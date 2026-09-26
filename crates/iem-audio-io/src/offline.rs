//! The `Offline` backend: runs a processor over a whole recording in blocks
//! of any size, deterministically (§3.5 parity harness, block-size invariance).

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{Block, Fault, Planar, Process, panic_message};

#[derive(Debug, Clone, Copy)]
pub struct Offline {
    /// Frames per call (0 is treated as 1); the last call may be shorter.
    pub block: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OfflineRun {
    pub output: Planar,
    /// Set when `process` panicked: the block from `frame` on is silent and
    /// the processor was not called again.
    pub fault: Option<Fault>,
}

fn copy(dst: &mut [f64], src: &[f64]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d = *s;
    }
}

impl Offline {
    pub fn run<P: Process>(&self, p: &mut P, input: &Planar, outputs: usize) -> OfflineRun {
        let block = self.block.max(1);
        let frames = input.frames();
        let inputs = input.channels();
        let mut output = Planar::new(outputs, frames);
        let mut ibuf = vec![0.0; inputs.saturating_mul(block)];
        let mut obuf = vec![0.0; outputs.saturating_mul(block)];
        let mut done = 0;
        while done < frames {
            let n = block.min(frames - done);
            for ch in 0..inputs {
                let src = input.channel(ch).get(done..done + n).unwrap_or_default();
                let dst = ibuf.get_mut(ch * n..(ch + 1) * n).unwrap_or_default();
                copy(dst, src);
            }
            let ins = ibuf.get(..inputs * n).unwrap_or_default();
            let outs = obuf.get_mut(..outputs * n).unwrap_or_default();
            outs.fill(0.0);
            let mut b = Block::new(n, ins, outs);
            if let Err(payload) = catch_unwind(AssertUnwindSafe(|| p.process(&mut b))) {
                return OfflineRun {
                    output,
                    fault: Some(Fault {
                        frame: done as u64,
                        message: panic_message(&*payload),
                    }),
                };
            }
            for ch in 0..outputs {
                let src = obuf.get(ch * n..(ch + 1) * n).unwrap_or_default();
                let dst = output
                    .channel_mut(ch)
                    .get_mut(done..done + n)
                    .unwrap_or_default();
                copy(dst, src);
            }
            done += n;
        }
        OfflineRun {
            output,
            fault: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records block sizes and writes `input + 10·channel` to each output.
    #[derive(Default)]
    struct Recorder {
        sizes: Vec<usize>,
        panic_at: Option<usize>,
    }

    impl Process for Recorder {
        fn process(&mut self, block: &mut Block<'_>) {
            self.sizes.push(block.frames());
            for ch in 0..block.outputs() {
                let src: Vec<f64> = block.input(ch % block.inputs().max(1)).to_vec();
                for (i, o) in block.output(ch).iter_mut().enumerate() {
                    *o = src.get(i).copied().unwrap_or(0.0) + 10.0 * ch as f64 + 1.0;
                }
            }
            if Some(self.sizes.len()) == self.panic_at {
                panic!("boom at call {}", self.sizes.len());
            }
        }
    }

    fn ramp(channels: usize, frames: usize) -> Planar {
        let mut p = Planar::new(channels, frames);
        for ch in 0..channels {
            for (i, x) in p.channel_mut(ch).iter_mut().enumerate() {
                *x = (ch * 1000 + i) as f64;
            }
        }
        p
    }

    #[test]
    fn offline_passes_blocks_of_the_requested_size() {
        let input = ramp(2, 100);
        let mut r = Recorder::default();
        let run = Offline { block: 32 }.run(&mut r, &input, 3);
        assert_eq!(r.sizes, vec![32, 32, 32, 4]);
        assert!(run.fault.is_none());
        assert_eq!(run.output.channels(), 3);
        for ch in 0..3 {
            for (i, y) in run.output.channel(ch).iter().enumerate() {
                let x = input.channel(ch % 2)[i];
                assert_eq!(*y, x + 10.0 * ch as f64 + 1.0, "ch {ch} frame {i}");
            }
        }
        let mut one = Recorder::default();
        let same = Offline { block: 0 }.run(&mut one, &input, 3);
        assert_eq!(one.sizes.len(), 100);
        assert_eq!(same.output, run.output);
        let mut big = Recorder::default();
        let whole = Offline { block: 1000 }.run(&mut big, &input, 3);
        assert_eq!(big.sizes, vec![100]);
        assert_eq!(whole.output, run.output);
    }

    #[test]
    fn offline_panic_zeroes_the_block_and_stops() {
        let input = ramp(1, 200);
        let mut r = Recorder {
            panic_at: Some(3),
            ..Recorder::default()
        };
        let run = Offline { block: 32 }.run(&mut r, &input, 1);
        assert_eq!(r.sizes.len(), 3);
        let fault = run.fault.expect("fault");
        assert_eq!(fault.frame, 64);
        assert!(
            fault.message.contains("boom at call 3"),
            "{}",
            fault.message
        );
        let out = run.output.channel(0);
        assert!(
            out[..64]
                .iter()
                .enumerate()
                .all(|(i, y)| *y == i as f64 + 1.0)
        );
        assert!(out[64..].iter().all(|y| *y == 0.0));
    }

    #[test]
    fn empty_input_calls_nothing() {
        let mut r = Recorder::default();
        let run = Offline { block: 8 }.run(&mut r, &Planar::new(2, 0), 2);
        assert!(r.sizes.is_empty());
        assert_eq!(run.output.frames(), 0);
    }
}
