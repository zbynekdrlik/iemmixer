// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from inferno (https://github.com/teodly/inferno at 9767558):
// inferno_aoip/src/device_server/flows_tx.rs (the header a TX packet
// carries), inferno_aoip/src/device_server/flows_rx.rs (its parse) and
// inferno_aoip/src/device_server/samples_utils.rs (the sample widths),
// Copyright (C) Teo and the inferno contributors, GPL-3.0-or-later OR
// AGPL-3.0-or-later, taken under GPL-3.0-or-later. Changes Copyright (C)
// 2026 iemmixer contributors.

//! A Dante audio packet (one UDP datagram of a flow):
//!
//! | bytes | field |
//! |---|---|
//! | 0 | [`KIND`], 2 as inferno sends it (it does not read it) |
//! | 1–4 | the stamp's seconds (mod 2³²), big-endian |
//! | 5–8 | the samples into that second, big-endian |
//! | 9… | the frames: each slot's sample in turn, big-endian, 2, 3 or 4 bytes |
//!
//! The stamp names the packet's first frame. Samples are left-justified
//! `i32`: a 24-bit sample is written from its top three bytes and read
//! back `<< 8` (its sign is the `i32`'s), a 16-bit one from its top two,
//! as inferno's writers truncate (no rounding, no dither here). Never a
//! panic, never an allocation.

/// The header's size.
pub const MEDIA_HEADER: usize = 9;
/// Byte 0 of a packet we send.
pub const KIND: u8 = 2;

/// A packet that is not a whole number of frames (or shorter than the
/// header, or of a width or slot count no flow has).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a whole number of frames")]
pub struct Ragged;

/// Writes a packet into `out`: the stamp (`secs`, `sub`), then `frames`
/// frames of `slots` samples `bps` bytes wide, `sample(frame, slot)` each.
/// Returns its length, or 0 (nothing written) when `bps` is not 2, 3 or 4
/// or the packet does not fit `out`.
pub fn write(
    out: &mut [u8],
    secs: u32,
    sub: u32,
    slots: usize,
    bps: usize,
    frames: usize,
    mut sample: impl FnMut(usize, usize) -> i32,
) -> usize {
    if !WIDTHS.contains(&bps) {
        return 0;
    }
    let Some(packet) = frames
        .checked_mul(slots)
        .and_then(|n| n.checked_mul(bps))
        .and_then(|n| n.checked_add(MEDIA_HEADER))
        .and_then(|len| out.get_mut(..len))
    else {
        return 0;
    };
    packet[0] = KIND;
    packet[1..5].copy_from_slice(&secs.to_be_bytes());
    packet[5..9].copy_from_slice(&sub.to_be_bytes());
    // One cell a sample, frame by frame (no cell at all without slots).
    for (i, cell) in packet[MEDIA_HEADER..].chunks_exact_mut(bps).enumerate() {
        cell.copy_from_slice(&sample(i / slots, i % slots).to_be_bytes()[..bps]);
    }
    packet.len()
}

/// The sample widths a flow has, bytes.
const WIDTHS: [usize; 3] = [2, 3, 4];

/// The stamp (`secs`, `sub`) of a packet; `None` when it is shorter than
/// the header.
pub fn read_header(buf: &[u8]) -> Option<(u32, u32)> {
    let h = buf.get(..MEDIA_HEADER)?;
    Some((
        u32::from_be_bytes([h[1], h[2], h[3], h[4]]),
        u32::from_be_bytes([h[5], h[6], h[7], h[8]]),
    ))
}

/// Reads a packet's frames of `slots` samples `bps` bytes wide, calling
/// `f(frame, slot, sample)` for each; returns the frame count. A packet
/// that is not a whole number of frames calls nothing.
pub fn read_frames(
    buf: &[u8],
    slots: usize,
    bps: usize,
    mut f: impl FnMut(usize, usize, i32),
) -> Result<usize, Ragged> {
    if !WIDTHS.contains(&bps) {
        return Err(Ragged);
    }
    let audio = buf.get(MEDIA_HEADER..).ok_or(Ragged)?;
    let stride = slots.checked_mul(bps).filter(|&s| s > 0).ok_or(Ragged)?;
    if audio.len() % stride != 0 {
        return Err(Ragged);
    }
    for (frame, bytes) in audio.chunks_exact(stride).enumerate() {
        for (slot, b) in bytes.chunks_exact(bps).enumerate() {
            let mut word = [0u8; 4];
            word[..bps].copy_from_slice(b);
            f(frame, slot, i32::from_be_bytes(word));
        }
    }
    Ok(audio.len() / stride)
}

#[cfg(test)]
mod tests;
