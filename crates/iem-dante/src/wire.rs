// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from inferno (https://github.com/teodly/inferno at 9767558):
// inferno_aoip/src/protocol/req_resp.rs (the header layout) and
// inferno_aoip/src/util/bytes.rs (reading a string at an offset),
// Copyright (C) Teo and the inferno contributors, GPL-3.0-or-later OR
// AGPL-3.0-or-later, taken under GPL-3.0-or-later. Changes Copyright (C)
// 2026 iemmixer contributors.

//! The request/response datagram that ARC (UDP 4440) and CMC (UDP 8800)
//! share: a 10-byte big-endian header, then the body.
//!
//! | bytes | field | |
//! |---|---|---|
//! | 0–1 | `start` | the protocol's start code; a reply echoes it |
//! | 2–3 | `len` | the whole datagram's length, header included |
//! | 4–5 | `seq` | the requester's sequence number; a reply echoes it |
//! | 6–7 | `op1` | the opcode; a reply echoes it |
//! | 8–9 | `op2` | 0 in a request; a reply's status |
//!
//! Offsets inside a body (to strings, to descriptors) count from the
//! datagram's first byte, the header included, as inferno writes them.
//! Parsing never panics: a datagram shorter than the header, or whose
//! length field disagrees with its size, is an error.

/// The header's size.
pub const HEADER_LEN: usize = 10;
/// A reply's status: done.
pub const STATUS_OK: u16 = 0x0001;
/// A reply's status: one page of a list, and more follow.
pub const STATUS_MORE: u16 = 0x8112;
/// A reply's status: refused, nothing changed (inferno's answer to a
/// request it could not carry out).
pub const STATUS_ERROR: u16 = 0xffff;
/// The largest datagram a reply may be: one Ethernet frame's UDP payload
/// (1500 − 20 − 8), so no reply is fragmented.
pub const MAX_DATAGRAM: usize = 1472;

/// A datagram's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub start: u16,
    pub len: u16,
    pub seq: u16,
    pub op1: u16,
    pub op2: u16,
}

/// Why a datagram is no request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("{0} bytes, shorter than the {HEADER_LEN}-byte header")]
    Short(usize),
    #[error("the header says {header} bytes, the datagram has {actual}")]
    Length { header: u16, actual: usize },
}

impl Header {
    /// The header and the body of `buf`, a whole datagram.
    pub fn parse(buf: &[u8]) -> Result<(Header, &[u8]), WireError> {
        let Some((head, body)) = buf.split_first_chunk::<HEADER_LEN>() else {
            return Err(WireError::Short(buf.len()));
        };
        let field = |at: usize| u16::from_be_bytes([head[at], head[at + 1]]);
        let header = Header {
            start: field(0),
            len: field(2),
            seq: field(4),
            op1: field(6),
            op2: field(8),
        };
        if usize::from(header.len) != buf.len() {
            return Err(WireError::Length {
                header: header.len,
                actual: buf.len(),
            });
        }
        Ok((header, body))
    }

    /// The header's 10 bytes.
    pub fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let mut out = [0; HEADER_LEN];
        for (at, value) in [self.start, self.len, self.seq, self.op1, self.op2]
            .into_iter()
            .enumerate()
        {
            out[2 * at..2 * at + 2].copy_from_slice(&value.to_be_bytes());
        }
        out
    }
}

/// A datagram: the header with its length filled in, then `body`.
///
/// # Panics
///
/// When `body` is longer than a datagram can say (65 525 bytes); the
/// device's own bodies are bounded far below that ([`MAX_DATAGRAM`]).
pub fn request(start: u16, seq: u16, op1: u16, op2: u16, body: &[u8]) -> Vec<u8> {
    let len = u16::try_from(HEADER_LEN + body.len()).expect("a datagram body under 64 KiB");
    let header = Header {
        start,
        len,
        seq,
        op1,
        op2,
    };
    let mut out = Vec::with_capacity(usize::from(len));
    out.extend_from_slice(&header.to_bytes());
    out.extend_from_slice(body);
    out
}

/// The reply to `req`: its start code, sequence number and opcode, `status`
/// in op2, then `body`. Panics as [`request`] does.
pub fn reply(req: &Header, status: u16, body: &[u8]) -> Vec<u8> {
    request(req.start, req.seq, req.op1, status, body)
}

/// The big-endian `u16` at `at` in `body`, if `body` holds it.
pub fn read_u16(body: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    let bytes = body.get(at..end)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// The string at packet offset `offset` in `body`: up to its NUL, or to the
/// end of the body when it has none (as inferno reads it). `None` for an
/// offset inside the header or past the body, or bytes that are not UTF-8.
pub fn string_at(body: &[u8], offset: u16) -> Option<&str> {
    let start = usize::from(offset).checked_sub(HEADER_LEN)?;
    let rest = body.get(start..).filter(|rest| !rest.is_empty())?;
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    std::str::from_utf8(&rest[..end]).ok()
}

/// A reply body under construction. Its offsets are packet offsets (the
/// header counted), as the body's own pointers must be.
#[derive(Debug, Default)]
pub struct Body {
    bytes: Vec<u8>,
}

impl Body {
    pub fn new() -> Self {
        Self::default()
    }

    /// The packet offset of the next byte written.
    pub fn offset(&self) -> u16 {
        u16::try_from(HEADER_LEN + self.bytes.len()).expect("a reply body under 64 KiB")
    }

    pub fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub fn zeros(&mut self, n: usize) {
        self.bytes.resize(self.bytes.len() + n, 0);
    }

    pub fn bytes(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    /// Zeros up to the next packet offset that is a multiple of `n`
    /// (inferno's `align_wpos`, whose buffer holds the header too).
    pub fn align(&mut self, n: usize) {
        while usize::from(self.offset()) % n != 0 {
            self.bytes.push(0);
        }
    }

    /// Writes `s` and its NUL; returns the packet offset of its first byte.
    pub fn cstr(&mut self, s: &str) -> u16 {
        let at = self.offset();
        self.bytes.extend_from_slice(s.as_bytes());
        self.bytes.push(0);
        at
    }

    /// Overwrites the byte at packet offset `at`, already written.
    pub fn set_u8(&mut self, at: u16, value: u8) {
        self.bytes[usize::from(at) - HEADER_LEN] = value;
    }

    /// Overwrites the two bytes at packet offset `at`, already written.
    pub fn set_u16(&mut self, at: u16, value: u16) {
        self.set_bytes(at, &value.to_be_bytes());
    }

    /// Overwrites `bytes.len()` bytes at packet offset `at`, already written.
    pub fn set_bytes(&mut self, at: u16, bytes: &[u8]) {
        let at = usize::from(at) - HEADER_LEN;
        self.bytes[at..at + bytes.len()].copy_from_slice(bytes);
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
pub(crate) mod tests;
