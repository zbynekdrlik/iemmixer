// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The ARC server's count of the requests it refuses or leaves unanswered
//! (#72, 2026-10-26: Dante Controller asked 0x2200 and 0x2204 about 2 250
//! times a second while the device refused both). Counted per peer and
//! opcode and reported at most once a [`WINDOW`]: [`Gaps::record`] and
//! [`Gaps::flush`] hand back a window's [`Report`] once it has run its
//! length, and the server logs it as one warning. A peer is its address,
//! named in the report by an id only (1 for the first peer seen, 2 for the
//! next, the same id for the server's life). A log, not a limiter: what the
//! server answers never depends on it.

use std::fmt;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// How long one report counts.
pub const WINDOW: Duration = Duration::from_secs(60);
/// The peers with an id of their own; any further peer counts as peer 0.
pub const MAX_PEERS: usize = 64;
/// The (peer, opcode, gap) entries one window counts; requests past them
/// are only summed ([`Report::uncounted`]).
pub const MAX_ENTRIES: usize = 256;
/// The entries a report's line names, the largest first.
pub const LISTED: usize = 16;

/// Why a request counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Gap {
    /// Answered with [`STATUS_ERROR`](crate::wire::STATUS_ERROR).
    Refused,
    /// Not answered at all.
    Unanswered,
}

/// One (peer, opcode, gap) count of a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub peer: usize,
    pub op1: u16,
    pub gap: Gap,
    pub count: u64,
}

/// A window's counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The largest count first; equal counts by peer, opcode, gap.
    pub entries: Vec<Entry>,
    /// Requests counted past [`MAX_ENTRIES`].
    pub uncounted: u64,
}

impl Report {
    /// Every request the window counted.
    pub fn requests(&self) -> u64 {
        0
    }
}

impl fmt::Display for Report {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Ok(())
    }
}

/// The counts of the window under way, and the peers' ids.
#[derive(Debug)]
pub struct Gaps {
    window: Duration,
}

impl Gaps {
    pub fn new(window: Duration) -> Self {
        Self { window }
    }

    /// Counts one request of `peer`; returns the report of the window
    /// before it when that one has run its length (the request then opens
    /// the next window).
    pub fn record(&mut self, _now: Instant, _peer: IpAddr, _op1: u16, _gap: Gap) -> Option<Report> {
        let _ = self.window;
        None
    }

    /// The report of the window under way once it has run its length;
    /// `None` before, or when nothing was counted.
    pub fn flush(&mut self, _now: Instant) -> Option<Report> {
        None
    }
}

#[cfg(test)]
mod tests;
