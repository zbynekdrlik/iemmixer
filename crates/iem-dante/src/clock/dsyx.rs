// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from dantesync 1.16.0 (https://github.com/zbynekdrlik/dantesync
// at 8d77132): src/time_server.rs (the "DSYX" request and the reply's base
// layout), src/date_offset/wire.rs (the date-offset extension) and
// src/date_offset/slew.rs (a slew's rate bounds and duration),
// Copyright (c) 2024-2025 DanteSync Contributors, under the MIT License:
//
//   Permission is hereby granted, free of charge, to any person obtaining a
//   copy of this software and associated documentation files (the
//   "Software"), to deal in the Software without restriction, including
//   without limitation the rights to use, copy, modify, merge, publish,
//   distribute, sublicense, and/or sell copies of the Software, and to
//   permit persons to whom the Software is furnished to do so, subject to
//   the following conditions:
//
//   The above copyright notice and this permission notice shall be included
//   in all copies or substantial portions of the Software.
//
//   THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
//   OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
//   MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
//   IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
//   CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
//   TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
//   SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
//
// Changes Copyright (C) 2026 iemmixer contributors, GPL-3.0-or-later.

//! dantesync's time server on the box (UDP 31900): one `"DSYX"` request a
//! second and its reply, read for the media clock (plan Task 1).
//!
//! The request is the magic `"DSYX"` and a request id, zero-padded to
//! [`REQUEST_LEN`] (dantesync answers no shorter `DSYX`: no amplification).
//! The reply (big-endian) is a 64-byte base, then the date-offset extension
//! once dantesync holds a date state:
//!
//! | bytes | field |
//! |---|---|
//! | 0–3 | `"DSYR"` |
//! | 4–7 | the request id, echoed |
//! | 8–15 | the wall clock when the base was built, ns since 1970 |
//! | 36–39 | the frequency adjustment, ppm × 1000 (ppb) |
//! | 40 | the mode: 0 init, 1 acquiring, 2 production, 3 lock, 4 nano, 5 NTP only |
//! | 41 | locked: 0 or 1 |
//! | 62 | flags: bit 1 settled |
//! | 64 | the extension's version (0: none) |
//! | 65 | flags: bit 1 the announce is a slew (version 2 on) |
//! | 66–67 | a slew's rate, ppm |
//! | 68–75 | the announced `D` (wall = PTP + `D`); a slew's end |
//! | 76–83 | the PTP instant it takes effect; a slew's start |
//! | 96–103 | the replier's PTP "now": its wall less the `D` in effect |
//! | 104–111 | a slew's starting `D` (version 2 on) |
//!
//! The `D` in effect is the base's wall less the extension's PTP "now"
//! (two clock reads of dantesync's, microseconds apart). A step announce
//! names its instant, a past one the last step: [`DateState::step_at_ptp_ns`].
//! A backward slew (dantesync's `as_slew`; a forward one is a step) runs
//! from its start for `ceil(|from − to| · 10⁶ / ppm)` ns of PTP time:
//! [`DateState::slewing`] while the reply's PTP "now" lies within it, or
//! within [`STEP_GUARD_NS`] of either end. Parsing never panics.

use super::STEP_GUARD_NS;

/// dantesync's time server port.
pub const PORT: u16 = 31_900;
/// The request's length: dantesync answers a `DSYX` only at the base's size.
pub const REQUEST_LEN: usize = 64;
/// The reply's base.
pub const BASE_LEN: usize = 64;
/// The extension's version 1 size (a version 1 reader decodes these of any
/// version).
pub const EXT_LEN: usize = 40;
/// The extension from version 2 (the slew's starting `D`).
pub const EXT_LEN_V2: usize = 48;
/// A buffer every reply fits (dantesync writes 112 bytes).
pub const REPLY_MAX: usize = 256;

const REQUEST_MAGIC: [u8; 4] = *b"DSYX";
const REPLY_MAGIC: [u8; 4] = *b"DSYR";
const FLAG_SETTLED: u8 = 0x02;
const EXT_FLAG_SLEW: u8 = 0x02;
/// dantesync's slew rate for a rate of 0, and its bounds.
const DEFAULT_SLEW_PPM: u32 = 100;
const MIN_SLEW_PPM: u32 = 10;
const MAX_SLEW_PPM: u32 = 500;

/// What one reply says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// dantesync's wall clock when it built the reply, ns since 1970.
    pub wall_ns: i64,
    /// Its PTP lock.
    pub locked: bool,
    /// Its servo settled.
    pub settled: bool,
    /// Its mode (the table above).
    pub mode: u8,
    /// Its frequency adjustment, ppb: the heartbeat's clock record later.
    pub freq_ppb: i32,
    /// The date offset; `None` before dantesync holds one (or from a
    /// dantesync without the extension).
    pub date: Option<DateState>,
}

/// The date offset `D` of a reply (wall = PTP + `D`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateState {
    /// The `D` in effect: the reply's wall less its PTP "now".
    pub d_ns: i64,
    /// A step's instant in PTP ns (future: due; past: the last one); `None`
    /// when the announce is a slew.
    pub step_at_ptp_ns: Option<i64>,
    /// A slew runs (or starts or ends within [`STEP_GUARD_NS`]).
    pub slewing: bool,
}

/// Why a datagram is not the reply to our request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DsyxError {
    #[error("a reply of {len} bytes, under the {BASE_LEN}-byte base")]
    Short { len: usize },
    #[error("not a DSYR reply")]
    Magic,
    #[error("the reply to request {got}, not ours")]
    RequestId { got: u32 },
}

/// The request with `id`, padded to [`REQUEST_LEN`].
pub fn request(id: u32) -> [u8; REQUEST_LEN] {
    let mut out = [0u8; REQUEST_LEN];
    out[0..4].copy_from_slice(&REQUEST_MAGIC);
    out[4..8].copy_from_slice(&id.to_be_bytes());
    out
}

/// Reads the reply to the request `id`.
pub fn parse(reply: &[u8], id: u32) -> Result<Reading, DsyxError> {
    let base: &[u8; BASE_LEN] = reply
        .get(..BASE_LEN)
        .and_then(|b| b.try_into().ok())
        .ok_or(DsyxError::Short { len: reply.len() })?;
    if base[0..4] != REPLY_MAGIC {
        return Err(DsyxError::Magic);
    }
    let got = u32::from_be_bytes([base[4], base[5], base[6], base[7]]);
    if got != id {
        return Err(DsyxError::RequestId { got });
    }
    let wall_ns = i64_at(base, 8);
    Ok(Reading {
        wall_ns,
        locked: base[41] != 0,
        settled: base[62] & FLAG_SETTLED != 0,
        mode: base[40],
        freq_ppb: i32::from_be_bytes([base[36], base[37], base[38], base[39]]),
        date: date_state(&reply[BASE_LEN..], wall_ns),
    })
}

/// The extension's date state, `None` when it is absent or of version 0.
fn date_state(ext: &[u8], wall_ns: i64) -> Option<DateState> {
    if ext.len() < EXT_LEN || ext[0] == 0 {
        return None;
    }
    let now_ptp = i64_at(ext, 32);
    let announced = i64_at(ext, 4);
    let effective = i64_at(ext, 12);
    let slew = (ext[0] >= 2 && ext.len() >= EXT_LEN_V2 && ext[1] & EXT_FLAG_SLEW != 0)
        .then(|| {
            (
                i64_at(ext, 40),
                u32::from(u16::from_be_bytes([ext[2], ext[3]])),
            )
        })
        .filter(|&(from, _)| announced < from);
    let (step_at_ptp_ns, slewing) = match slew {
        Some((from, ppm)) => {
            let end = effective.saturating_add(slew_duration_ns(from.abs_diff(announced), ppm));
            let runs = now_ptp >= effective.saturating_sub(STEP_GUARD_NS)
                && now_ptp < end.saturating_add(STEP_GUARD_NS);
            (None, runs)
        }
        None => (Some(effective), false),
    };
    Some(DateState {
        d_ns: wall_ns.wrapping_sub(now_ptp),
        step_at_ptp_ns,
        slewing,
    })
}

/// How long a slew of `amount` ns at `ppm` runs, PTP ns (dantesync's
/// `DateSlew::duration_ns`: rounded up, the rate as it clamps it).
fn slew_duration_ns(amount: u64, ppm: u32) -> i64 {
    let rate = if ppm == 0 {
        DEFAULT_SLEW_PPM
    } else {
        ppm.clamp(MIN_SLEW_PPM, MAX_SLEW_PPM)
    };
    let rate = u128::from(rate);
    let ns = (u128::from(amount) * 1_000_000).div_ceil(rate);
    i64::try_from(ns).unwrap_or(i64::MAX)
}

fn i64_at(bytes: &[u8], at: usize) -> i64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[at..at + 8]);
    i64::from_be_bytes(b)
}

#[cfg(test)]
pub(crate) mod tests;
