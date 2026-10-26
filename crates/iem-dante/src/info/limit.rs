// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The info server's bound on its replies (the review of PR #78, #72).
//! Every reply goes multicast to [`INFO_GROUP`](super::INFO_GROUP), which
//! every Dante device on the band's network hears, so without a bound each
//! unicast request (a 32-byte datagram from any sender) would put a reply
//! of up to 368 bytes onto all of them. At most [`PER_SPAN`] replies of one
//! kind go out in any [`SPAN`] (a sliding window: the times of the last
//! [`PER_SPAN`] replies of that kind); the rest are dropped and counted, and
//! the counts are reported at most once a [`REPORT_EVERY`] ([`Limit::flush`]),
//! which the server logs as one warning, as ARC's gap guard does.
//!
//! A kind is the reply ([`Kind`]), not the request's whole opcode: a
//! request's byte 1 (and for 0x13 and 0x77 its last byte) may be anything,
//! so a sender that varies them still meets one bound. A request that gets
//! no reply (clock stats, an unknown opcode) is never bounded here. The
//! channel-change notification is a kind of its own: the server sends one
//! only when [`Limit::room`] says so and keeps the changes until then, so
//! it is never dropped.

use std::fmt;
use std::time::{Duration, Instant};

use super::Kind;

/// The most replies of one kind in any [`SPAN`].
pub const PER_SPAN: usize = 10;
/// The window the bound counts over.
pub const SPAN: Duration = Duration::from_secs(1);
/// How long one report of dropped replies counts.
pub const REPORT_EVERY: Duration = Duration::from_secs(60);
/// How often the server looks whether a report is due.
pub const CHECK: Duration = Duration::from_secs(1);

/// The kinds the server sends, in their report order.
pub const BOUNDED: [Kind; 5] = [
    Kind::BoardInfo,
    Kind::ProductInfo,
    Kind::NetworkInfo,
    Kind::Op0077,
    Kind::ChannelChange,
];

/// The replies dropped over the bound in one report's time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dropped {
    /// Each kind with a drop and its count, in [`BOUNDED`]'s order.
    pub counts: Vec<(Kind, u64)>,
}

impl Dropped {
    /// Every reply dropped.
    pub fn total(&self) -> u64 {
        self.counts.iter().map(|(_, n)| n).sum()
    }
}

/// A kind's request opcode byte and name, as the report names it.
fn label(kind: Kind) -> &'static str {
    match kind {
        Kind::BoardInfo => "0x61 board info",
        Kind::ProductInfo => "0xc1 product info",
        Kind::NetworkInfo => "0x13 network info",
        Kind::Op0077 => "0x77",
        Kind::ChannelChange => "channel change",
        // Never bounded, so never in a report.
        Kind::ClockStats | Kind::Unknown => "no reply",
    }
}

impl fmt::Display for Dropped {
    /// `0x61 board info 990, 0x13 network info 5, 0x77 1`: the request's
    /// opcode byte, the kind's name (0x77 has none), the count.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self
            .counts
            .iter()
            .map(|&(kind, n)| format!("{} {n}", label(kind)))
            .collect();
        f.write_str(&parts.join(", "))
    }
}

/// The bound's state: the last replies of each kind and the drops counted.
#[derive(Debug)]
pub struct Limit {
    /// Per [`BOUNDED`] kind: the times of its last [`PER_SPAN`] replies, a
    /// ring whose `next` slot holds the oldest (`None` while unused).
    sent: [[Option<Instant>; PER_SPAN]; BOUNDED.len()],
    next: [usize; BOUNDED.len()],
    /// Per [`BOUNDED`] kind: the replies dropped since the last report.
    dropped: [u64; BOUNDED.len()],
    /// The first drop since the last report; `None` while none.
    started: Option<Instant>,
}

/// `kind`'s place in [`BOUNDED`]; `None` for a kind without a reply.
fn slot(kind: Kind) -> Option<usize> {
    BOUNDED.iter().position(|&k| k == kind)
}

impl Limit {
    pub fn new() -> Self {
        Self {
            sent: [[None; PER_SPAN]; BOUNDED.len()],
            next: [0; BOUNDED.len()],
            dropped: [0; BOUNDED.len()],
            started: None,
        }
    }

    /// Whether a reply of `kind` may go out at `now`, nothing counted: the
    /// oldest of its kind's last [`PER_SPAN`] went out [`SPAN`] or more
    /// ago. A kind without a reply always may.
    pub fn room(&self, now: Instant, kind: Kind) -> bool {
        slot(kind).is_none_or(|k| {
            self.sent[k][self.next[k]].is_none_or(|t| now.saturating_duration_since(t) >= SPAN)
        })
    }

    /// Whether a reply of `kind` may go out at `now` ([`Limit::room`]). One
    /// that may is counted toward its kind's window; one that may not is a
    /// drop, counted for the report. A kind without a reply always may.
    pub fn allow(&mut self, now: Instant, kind: Kind) -> bool {
        let Some(k) = slot(kind) else {
            return true;
        };
        if !self.room(now, kind) {
            self.dropped[k] += 1;
            self.started.get_or_insert(now);
            return false;
        }
        self.sent[k][self.next[k]] = Some(now);
        self.next[k] = (self.next[k] + 1) % PER_SPAN;
        true
    }

    /// The drops counted, once [`REPORT_EVERY`] has run since the first
    /// of them; `None` before, or when nothing was dropped.
    pub fn flush(&mut self, now: Instant) -> Option<Dropped> {
        let started = self.started?;
        if now.saturating_duration_since(started) < REPORT_EVERY {
            return None;
        }
        self.started = None;
        let dropped = std::mem::take(&mut self.dropped);
        Some(Dropped {
            counts: BOUNDED
                .into_iter()
                .zip(dropped)
                .filter(|&(_, n)| n > 0)
                .collect(),
        })
    }
}

impl Default for Limit {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
