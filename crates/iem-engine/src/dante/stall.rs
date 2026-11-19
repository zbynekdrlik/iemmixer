//! The pump's stall watch (#130): the engine's safety net for a pump that
//! stopped while the media clock is ready (2026-11-19: after a holdover the
//! pump's blocks froze, the unit stayed active and every in-ear was silent
//! until a restart by hand).
//!
//! [`StallWatch::look`] (pure) is fed, at every control tick, the pump's
//! block count and whether the clock is ready. It is armed at the first
//! look the clock is ready, whatever the count, so a pump that never runs
//! its first block (right after a restart, the clock still in the state
//! that stalled it) is caught as one that froze later; a block count
//! unchanged for [`STALL_WITHIN`] while armed is a [`Stall`]. The
//! driver turns it into `Ending::Backend`: one error line, save, release,
//! exit 1, which the unit restarts.

use std::time::{Duration, Instant};

/// How long the pump's blocks may stand still while the clock is ready. A
/// block is 333 µs at B = 32 (167 µs at 16); since dstack v0.6.193
/// (dstack#440) `Blocks::next` never waits longer than the margin and a
/// block (W + B samples: at most 4 128, 43 ms at 96 kHz) and re-bootstraps a
/// clock gone back at once; the clock's not-ready nap is 10 ms. So 500 ms
/// (1500 blocks at B = 32) is no legitimate gap.
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
    /// `ready`. A clock not ready disarms the watch; a ready one arms it
    /// whatever the count (0 too: a pump that never ran a block); blocks
    /// that moved are remembered with `now`; blocks unchanged for the bound
    /// since they were first seen are a [`Stall`] (at every look from then).
    pub fn look(&mut self, now: Instant, blocks: u64, ready: bool) -> Option<Stall> {
        if !ready {
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
