// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The ARC server's count of the requests it refuses or leaves unanswered,
//! and of the datagrams whose length field disagrees with their size
//! (#72, 2026-10-26: Dante Controller asked 0x2200 and 0x2204 about 2 250
//! times a second while the device refused both); the flow-control server
//! (UDP 4455, [`crate::flows::server`]) keeps one of its own, with the
//! requests over its per-peer bound. Counted per peer and
//! opcode and reported at most once a [`WINDOW`]: [`Gaps::record`] and
//! [`Gaps::flush`] hand back a window's [`Report`] once it has run its
//! length, and the server logs it as one warning. A peer is its address,
//! named in the report by an id only (1 for the first peer seen, 2 for the
//! next, the same id for the server's life). A log, not a limiter: what the
//! server answers never depends on it.

use std::collections::BTreeMap;
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
    /// No request: its length field disagrees with its size.
    Misframed,
    /// Dropped unread: over the server's bound on one peer's requests
    /// (the flow-control server, [`crate::flows::server::PeerLimit`]).
    Limited,
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
        self.entries.iter().map(|e| e.count).sum::<u64>() + self.uncounted
    }
}

impl Gap {
    fn label(self) -> &'static str {
        match self {
            Gap::Refused => "refused",
            Gap::Unanswered => "unanswered",
            Gap::Misframed => "misframed",
            Gap::Limited => "limited",
        }
    }
}

impl fmt::Display for Report {
    /// `peer 1 0x2204 unanswered 134999, peer 1 0x2200 refused 3`: the
    /// first [`LISTED`] entries, then one sum for the rest and one for the
    /// requests past the table.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<String> = self
            .entries
            .iter()
            .take(LISTED)
            .map(|e| {
                format!(
                    "peer {} {:#06x} {} {}",
                    e.peer,
                    e.op1,
                    e.gap.label(),
                    e.count
                )
            })
            .collect();
        let rest = self.entries.get(LISTED..).unwrap_or_default();
        if !rest.is_empty() {
            let requests: u64 = rest.iter().map(|e| e.count).sum();
            parts.push(format!(
                "{} more entries with {requests} requests",
                rest.len()
            ));
        }
        if self.uncounted > 0 {
            parts.push(format!("{} requests past the table", self.uncounted));
        }
        f.write_str(&parts.join(", "))
    }
}

/// The counts of the window under way, and the peers' ids.
#[derive(Debug)]
pub struct Gaps {
    window: Duration,
    /// The window's first request; `None` while nothing is counted.
    started: Option<Instant>,
    /// The peers seen, in order: peer `k + 1` is `peers[k]`.
    peers: Vec<IpAddr>,
    counts: BTreeMap<(usize, u16, Gap), u64>,
    uncounted: u64,
}

impl Gaps {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            started: None,
            peers: Vec::new(),
            counts: BTreeMap::new(),
            uncounted: 0,
        }
    }

    /// Counts one request of `peer`; returns the report of the window
    /// before it when that one has run its length (the request then opens
    /// the next window).
    pub fn record(&mut self, now: Instant, peer: IpAddr, op1: u16, gap: Gap) -> Option<Report> {
        let report = self.flush(now);
        self.started.get_or_insert(now);
        let key = (self.peer_id(peer), op1, gap);
        if let Some(count) = self.counts.get_mut(&key) {
            *count += 1;
        } else if self.counts.len() < MAX_ENTRIES {
            self.counts.insert(key, 1);
        } else {
            self.uncounted += 1;
        }
        report
    }

    /// The report of the window under way once it has run its length;
    /// `None` before, or when nothing was counted.
    pub fn flush(&mut self, now: Instant) -> Option<Report> {
        let started = self.started?;
        if now.saturating_duration_since(started) < self.window {
            return None;
        }
        self.started = None;
        let mut entries: Vec<Entry> = std::mem::take(&mut self.counts)
            .into_iter()
            .map(|((peer, op1, gap), count)| Entry {
                peer,
                op1,
                gap,
                count,
            })
            .collect();
        // Stable: equal counts stay in the map's (peer, opcode, gap) order.
        entries.sort_by_key(|e| std::cmp::Reverse(e.count));
        Some(Report {
            entries,
            uncounted: std::mem::take(&mut self.uncounted),
        })
    }

    /// `peer`'s id: its place among the peers seen, from 1; 0 once
    /// [`MAX_PEERS`] have one.
    fn peer_id(&mut self, peer: IpAddr) -> usize {
        if let Some(k) = self.peers.iter().position(|&p| p == peer) {
            return k + 1;
        }
        if self.peers.len() < MAX_PEERS {
            self.peers.push(peer);
            return self.peers.len();
        }
        0
    }
}

#[cfg(test)]
mod tests;
