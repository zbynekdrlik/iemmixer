//! Talkback receive (F18, X5; S5 design note §4): the browser's Opus frames
//! (mono, 48 kHz, 20 ms) are decoded on a 20 ms playout clock after a 40 ms
//! pre-roll; a frame that is not there in time is concealed by Opus' packet
//! loss concealment, and after three concealed frames the talker is taken as
//! silent and the pre-roll starts again. The engine holds its 120 ms cap and
//! the 5 ms gate.

use std::collections::VecDeque;

/// Frames buffered before playout starts (40 ms).
pub const PREROLL: usize = 2;
/// Frames buffered at most (120 ms); the oldest goes first.
pub const CAP: usize = 6;
/// Concealed frames in a row before playout stops.
pub const MAX_CONCEAL: u32 = 3;
pub const FRAME_MS: u32 = 20;
const FRAME: usize = 960;

pub struct TalkbackDecoder {
    dec: opus::Decoder,
    queue: VecDeque<Vec<u8>>,
    playing: bool,
    empty: u32,
    pub overflows: u64,
    pub concealed: u64,
    pub errors: u64,
}

impl TalkbackDecoder {
    pub fn new() -> Result<Self, opus::Error> {
        Ok(Self {
            dec: opus::Decoder::new(48_000, opus::Channels::Mono)?,
            queue: VecDeque::new(),
            playing: false,
            empty: 0,
            overflows: 0,
            concealed: 0,
            errors: 0,
        })
    }

    /// One Opus packet from the browser.
    pub fn push(&mut self, packet: Vec<u8>) {
        if self.queue.len() >= CAP {
            self.queue.pop_front();
            self.overflows += 1;
        }
        self.queue.push_back(packet);
    }

    pub fn fill_ms(&self) -> u32 {
        self.queue.len() as u32 * FRAME_MS
    }

    fn conceal(&mut self) -> Option<Vec<f32>> {
        let mut out = vec![0f32; FRAME];
        self.concealed += 1;
        match self.dec.decode_float(&[], &mut out, false) {
            Ok(n) => {
                out.truncate(n);
                Some(out)
            }
            Err(_) => Some(vec![0f32; FRAME]),
        }
    }

    /// The frame for this 20 ms tick, if the talker is playing.
    pub fn tick(&mut self) -> Option<Vec<f32>> {
        if !self.playing {
            if self.queue.len() < PREROLL {
                return None;
            }
            self.playing = true;
        }
        match self.queue.pop_front() {
            Some(p) => {
                self.empty = 0;
                let mut out = vec![0f32; FRAME];
                match self.dec.decode_float(&p, &mut out, false) {
                    Ok(n) => {
                        out.truncate(n);
                        Some(out)
                    }
                    Err(_) => {
                        self.errors += 1;
                        self.conceal()
                    }
                }
            }
            None => {
                self.empty += 1;
                if self.empty > MAX_CONCEAL {
                    self.playing = false;
                    self.empty = 0;
                    let _ = self.dec.reset_state();
                    None
                } else {
                    self.conceal()
                }
            }
        }
    }

    /// Drops what is buffered (the talker let go).
    pub fn clear(&mut self) {
        self.queue.clear();
        self.playing = false;
        self.empty = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packets(n: usize) -> Vec<Vec<u8>> {
        let mut enc =
            opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::Voip).unwrap();
        (0..n)
            .map(|k| {
                let frame: Vec<f32> = (0..FRAME)
                    .map(|i| {
                        let t = (k * FRAME + i) as f32 / 48_000.0;
                        0.3 * (std::f32::consts::TAU * 440.0 * t).sin()
                    })
                    .collect();
                enc.encode_vec_float(&frame, 4000).unwrap()
            })
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    fn playout_waits_for_the_preroll_then_gives_a_frame_per_tick() {
        let mut d = TalkbackDecoder::new().unwrap();
        let p = packets(6);
        assert_eq!(d.tick(), None);
        d.push(p[0].clone());
        assert_eq!(d.fill_ms(), 20);
        assert_eq!(d.tick(), None, "one frame is not the 40 ms pre-roll");
        d.push(p[1].clone());
        let a = d.tick().expect("playing");
        assert_eq!(a.len(), FRAME);
        let b = d.tick().expect("second frame");
        assert!(rms(&b) > 0.05, "a decoded tone");
        assert_eq!(d.concealed, 0);
    }

    #[test]
    fn a_late_frame_is_concealed_and_silence_stops_playout() {
        let mut d = TalkbackDecoder::new().unwrap();
        let p = packets(4);
        for x in &p {
            d.push(x.clone());
        }
        for _ in 0..4 {
            assert!(d.tick().is_some());
        }
        let hidden = d.tick().expect("concealed");
        assert_eq!(hidden.len(), FRAME);
        assert!(rms(&hidden) > 0.0, "PLC continues the tone");
        assert!(d.tick().is_some());
        assert!(d.tick().is_some());
        assert_eq!(d.concealed, 3);
        assert_eq!(d.tick(), None, "a fourth missing frame: silent");
        d.push(p[0].clone());
        assert_eq!(d.tick(), None, "pre-roll again");
    }

    #[test]
    fn the_buffer_is_capped_and_garbage_is_counted() {
        let mut d = TalkbackDecoder::new().unwrap();
        for x in packets(CAP + 2) {
            d.push(x);
        }
        assert_eq!(d.overflows, 2);
        assert_eq!(d.fill_ms(), CAP as u32 * FRAME_MS);
        d.clear();
        assert_eq!(d.fill_ms(), 0);
        d.push(vec![0xff; 3]);
        d.push(vec![0xff; 3]);
        let out = d.tick().expect("an error is concealed");
        assert_eq!(out.len(), FRAME);
        assert_eq!(d.errors, 1);
    }
}
