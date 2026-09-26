//! The media path's pure parts (X3–X5; design note §3.4): listen taps from the
//! RT thread's 96 kHz rings into 20 ms 48 kHz frames, and talkback frames
//! from the server into the RT thread's 96 kHz ring. The media thread
//! (`control`) drives them; nothing here runs on the RT thread.

use iem_engine_proto::{FRAME_48K, MediaHeader};

use crate::resample::{Decimator2, Interpolator2};

/// One outgoing frame: header and interleaved stereo samples.
pub type Frame = (MediaHeader, Vec<f32>);

/// Decimates one tap's interleaved 96 kHz stereo into 960-frame 48 kHz frames.
#[derive(Debug, Clone)]
pub struct TapFramer {
    stream: u8,
    dec: Decimator2,
    seq: u64,
    buf: Vec<f32>,
}

impl TapFramer {
    pub fn new(stream: u8) -> Self {
        Self {
            stream,
            dec: Decimator2::new(),
            seq: 0,
            buf: Vec::with_capacity(2 * FRAME_48K),
        }
    }

    /// Drops a partial frame and the filter history (the tap changed source);
    /// the sequence keeps counting.
    pub fn restart(&mut self) {
        self.dec = Decimator2::new();
        self.buf.clear();
    }

    pub fn feed(&mut self, interleaved: &[f32], out: &mut Vec<Frame>) {
        for &[l, r] in interleaved.as_chunks::<2>().0 {
            let Some((a, b)) = self.dec.push(f64::from(l), f64::from(r)) else {
                continue;
            };
            self.buf.push(a as f32);
            self.buf.push(b as f32);
            if self.buf.len() >= 2 * FRAME_48K {
                let samples = std::mem::replace(&mut self.buf, Vec::with_capacity(2 * FRAME_48K));
                out.push((
                    MediaHeader {
                        stream: self.stream,
                        channels: 2,
                        seq: self.seq,
                        frames: FRAME_48K as u16,
                    },
                    samples,
                ));
                self.seq += 1;
            }
        }
    }
}

/// Upsamples talkback frames (mono 48 kHz) into the RT thread's ring.
#[derive(Debug, Clone, Default)]
pub struct TalkbackFeed {
    interp: Interpolator2,
    scratch: Vec<f32>,
}

impl TalkbackFeed {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the samples the full ring dropped (120 ms cap, X5).
    pub fn feed(&mut self, samples: &[f32], ring: &mut rtrb::Producer<f32>) -> usize {
        self.scratch.clear();
        for x in samples {
            let [a, b] = self.interp.push(f64::from(*x));
            self.scratch.push(a as f32);
            self.scratch.push(b as f32);
        }
        let (_, rest) = ring.push_partial_slice(&self.scratch);
        rest.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f64::consts::TAU;
    use iem_engine_proto::media::stream;

    #[test]
    fn tap_framer_emits_960_frame_stereo_frames_with_sequence() {
        let mut f = TapFramer::new(stream::MEMBER_LISTEN);
        let mut out = Vec::new();
        let tone: Vec<f32> = (0..2 * 96_00)
            .map(|i| {
                let n = (i / 2) as f64;
                let x = 0.5 * (TAU * 1000.0 * n / 96_000.0).sin();
                if i % 2 == 0 { x as f32 } else { -x as f32 }
            })
            .collect();
        // Input frames 0, 2, … yield outputs: 1918 frames give 959.
        f.feed(&tone[..3836], &mut out);
        assert!(out.is_empty());
        f.feed(&tone[3836..3840], &mut out);
        assert_eq!(out.len(), 1);
        f.feed(&tone[3840..], &mut out);
        assert_eq!(out.len(), 5);
        for (k, (h, s)) in out.iter().enumerate() {
            assert_eq!(
                *h,
                MediaHeader {
                    stream: stream::MEMBER_LISTEN,
                    channels: 2,
                    seq: k as u64,
                    frames: 960
                }
            );
            assert_eq!(s.len(), 1920);
        }
        let last = &out[4].1;
        let peak = last.iter().step_by(2).fold(0.0f32, |m, x| m.max(x.abs()));
        // 48 samples per cycle, sampled half a sample off the crest.
        assert!((peak - 0.5).abs() < 3e-3, "{peak}");
        assert!(last.chunks(2).all(|p| p[0] == -p[1]));
        f.restart();
        let mut more = Vec::new();
        f.feed(&tone[..3840], &mut more);
        assert_eq!(more[0].0.seq, 5);
    }

    #[test]
    fn talkback_feed_upsamples_and_counts_drops() {
        let (mut p, mut c) = rtrb::RingBuffer::new(100);
        let mut feed = TalkbackFeed::new();
        assert_eq!(feed.feed(&[0.5; 40], &mut p), 0);
        assert_eq!(c.slots(), 80);
        assert_eq!(feed.feed(&[0.5; 40], &mut p), 60);
        let got: Vec<f32> = std::iter::from_fn(|| c.pop().ok()).collect();
        assert_eq!(got.len(), 100);
        let mut steady = TalkbackFeed::new();
        let (mut p, mut c) = rtrb::RingBuffer::new(4000);
        steady.feed(&[0.25; 960], &mut p);
        let got: Vec<f32> = std::iter::from_fn(|| c.pop().ok()).collect();
        assert_eq!(got.len(), 1920);
        assert!(got[200..].iter().all(|x| (x - 0.25).abs() < 1e-6));
    }
}
