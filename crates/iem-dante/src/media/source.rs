// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! What a TX channel sends in phase B (set at runtime through the local
//! control socket, Task 7; every channel silent at start). Each source
//! writes the samples of the positions `stamp`, `stamp + 1`, … : a function
//! of the position, so a packet stamped T always carries the source's
//! samples T…T+fpp−1 and a receiver that places by stamp sees exactly
//! what was sent.
//!
//! - [`Source::Counter`]: `(position mod 65536) << 8`, a ramp at about
//!   −42 dBFS whose every sample names its own position: the
//!   [`super::check::CounterCheck`] reads it back. It needs 24 or 32 bits
//!   (a 16-bit flow keeps only its top 8 bits).
//! - [`Source::Sine`]: at most [`SINE_CAP_DBFS`] (the HIL rule), however it
//!   is asked; its phase is the position times the frequency in 2⁻⁶⁴
//!   cycles a sample, exact modulo 2⁶⁴, so it never drifts and never
//!   jumps. NaN or out-of-range values give silence or the cap, never a
//!   panic.
//! - [`Source::Echo`]: RX channel `rx`'s ring at `position − offset`: the
//!   TX sample stamped T is the RX sample stamped T − offset; a hole is
//!   silence, counted.
//! - [`Source::Engine`] (phase C): the TX channel's own engine ring at the
//!   position, which the engine's pump ([`crate::stream`]) writes stamped
//!   RX time plus its offset; a hole (a block not written in time, or no
//!   ring) is silence, counted.

use super::ring::Ring;

/// The loudest a sine may be, dBFS.
pub const SINE_CAP_DBFS: f64 = -20.0;

/// A TX channel's source.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Source {
    #[default]
    Silence,
    Counter,
    Sine {
        hz: f64,
        dbfs: f64,
    },
    Echo {
        rx: usize,
        offset: u32,
    },
    /// The engine's output for this TX channel (phase C).
    Engine,
}

/// What one fill found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fill {
    /// Echo samples the ring did not hold (sent as silence).
    pub holes: usize,
}

impl Source {
    /// Writes the samples of positions `stamp`, `stamp + 1`, … into `out`,
    /// at `rate` (the sine's only use of it); `rings` are the RX channels',
    /// `own` this TX channel's engine ring (`None`: it has none).
    pub fn fill(
        &mut self,
        stamp: u64,
        rate: u32,
        out: &mut [i32],
        rings: &[Ring],
        own: Option<&Ring>,
    ) -> Fill {
        match *self {
            Source::Silence => out.fill(0),
            Source::Counter => {
                for (i, o) in out.iter_mut().enumerate() {
                    *o = ((stamp.wrapping_add(i as u64) % 65_536) << 8) as i32;
                }
            }
            Source::Sine { hz, dbfs } => {
                // The phase step in 2⁻⁶⁴ cycles a sample (a float to int cast
                // saturates, NaN gives 0), the level at most the cap.
                let step = (hz / f64::from(rate.max(1)) * 2f64.powi(64)) as u64;
                let amp = f64::from(i32::MAX) * 10f64.powf(dbfs.min(SINE_CAP_DBFS) / 20.0);
                for (i, o) in out.iter_mut().enumerate() {
                    let phase = stamp.wrapping_add(i as u64).wrapping_mul(step);
                    let cycles = phase as f64 / 2f64.powi(64);
                    *o = (amp * (std::f64::consts::TAU * cycles).sin()).round() as i32;
                }
            }
            Source::Echo { rx, offset } => {
                let at = stamp.checked_sub(u64::from(offset));
                return match (rings.get(rx), at) {
                    (Some(ring), Some(at)) => Fill {
                        holes: ring.read_at(at, out).holes,
                    },
                    // No such RX channel, or a position before time began.
                    _ => {
                        out.fill(0);
                        Fill { holes: out.len() }
                    }
                };
            }
            Source::Engine => {
                return match own {
                    Some(ring) => Fill {
                        holes: ring.read_at(stamp, out).holes,
                    },
                    // A channel without an engine ring (refused by
                    // `TxShared::set_source`): all silence, all holes.
                    None => {
                        out.fill(0);
                        Fill { holes: out.len() }
                    }
                };
            }
        }
        Fill::default()
    }
}

#[cfg(test)]
mod tests;
