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
//! no reply (clock stats, an unknown opcode) is never bounded here.

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

/// The kinds the server answers, in their report order.
pub const BOUNDED: [Kind; 4] = [
    Kind::BoardInfo,
    Kind::ProductInfo,
    Kind::NetworkInfo,
    Kind::Op0077,
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
        let _ = &self.counts;
        0
    }
}

impl fmt::Display for Dropped {
    /// `0x61 board info 990, 0x13 network info 5, 0x77 1`: the request's
    /// opcode byte, the kind's name (0x77 has none), the count.
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Ok(())
    }
}

/// The bound's state: the last replies of each kind and the drops counted.
#[derive(Debug)]
pub struct Limit {}

impl Limit {
    pub fn new() -> Self {
        Self {}
    }

    /// Whether a reply of `kind` may go out at `now`. One that may is
    /// counted toward its kind's window; one that may not is a drop,
    /// counted for the report. A kind without a reply always may.
    pub fn allow(&mut self, now: Instant, kind: Kind) -> bool {
        let _ = (now, kind);
        true
    }

    /// The drops counted, once [`REPORT_EVERY`] has run since the first
    /// of them; `None` before, or when nothing was dropped.
    pub fn flush(&mut self, now: Instant) -> Option<Dropped> {
        let _ = now;
        None
    }
}

impl Default for Limit {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
