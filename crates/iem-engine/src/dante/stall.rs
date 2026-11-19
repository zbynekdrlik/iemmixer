//! The pump's stall watch (#130): the engine's safety net for a pump that
//! stopped while the media clock is ready (2026-11-19: after a holdover the
//! pump's blocks froze, the unit stayed active and every in-ear was silent
//! until a restart by hand).
//!
//! [`StallWatch::look`] (pure) is fed, at every control tick, the pump's
//! block count and whether the clock is ready. It is armed only while the
//! clock is ready and after the pump's first block; a block count unchanged
//! for [`STALL_WITHIN`] while armed is a [`Stall`]. The driver turns it into
//! `Ending::Backend`: one error line, save, release, exit 1, which the unit
//! restarts.

use std::time::{Duration, Instant};

/// How long the pump's blocks may stand still while the clock is ready: a
/// block is 333 µs at B = 32 (167 µs at 16), a re-bootstrap or a new
/// latency runs the next block at once and the clock's not-ready nap is 10
/// ms, so 500 ms (1500 blocks at B = 32) is no legitimate gap.
pub const STALL_WITHIN: Duration = Duration::from_millis(500);

/// A pump whose blocks stood still while the clock was ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stall {
    /// The frozen block count.
    pub blocks: u64,
    /// How long it stood still, at the look that saw it.
    pub for_: Duration,
}

impl Stall {
    /// The ending's reason (numbers only).
    pub fn reason(&self) -> String {
        format!(
            "the engine pump stalled: blocks frozen at {} for {} ms while the media clock is ready",
            self.blocks,
            self.for_.as_millis()
        )
    }
}

/// The watch's state between looks (see the module).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StallWatch {
    within: Duration,
    /// The block count last seen and since when, while armed.
    seen: Option<(u64, Instant)>,
}

impl StallWatch {
    /// A watch that reports blocks standing still for `within`.
    pub fn new(within: Duration) -> Self {
        StallWatch { within, seen: None }
    }

    /// One look at `now`: the pump's `blocks` and whether the clock is
    /// `ready`. A clock not ready or no block yet disarms the watch; blocks
    /// that moved are remembered with `now`; blocks unchanged for the bound
    /// since they were first seen are a [`Stall`] (at every look from then).
    pub fn look(&mut self, now: Instant, blocks: u64, ready: bool) -> Option<Stall> {
        if !ready || blocks == 0 {
            self.seen = None;
            return None;
        }
        match self.seen {
            Some((seen, since)) if seen == blocks => {
                let for_ = now.saturating_duration_since(since);
                (for_ >= self.within).then_some(Stall { blocks, for_ })
            }
            _ => {
                self.seen = Some((blocks, now));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests;
