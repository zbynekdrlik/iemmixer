// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! Our reads of another device's ARC (UDP 4440, Gen 3 phase B, plan Task
//! 9b): the channel directory of a transmitter that publishes no
//! `_netaudio-chan` record (the band's Yamaha Dante cards publish only
//! `_netaudio-arc`, `_netaudio-cmc` and `_netaudio-dbc`, #72 2026-10-26), so
//! a subscription to one of its channels can be resolved
//! ([`crate::subscribe::directory`]).
//!
//! A read asks three read-only opcodes, the pages our own server answers
//! ([`super::handle`]) and netaudio reads, with netaudio's request
//! ([`page_request`]: start code [`START`], body `00 01 <first item> 00
//! 00`):
//!
//! - 0x1000, the counts: the TX channel count at body bytes 2–3.
//! - 0x2000, the TX channels, paged: `[slots, count]`, then `slots` 8-byte
//!   descriptors `id, flags, common, name`; `common` points at the
//!   16-byte descriptor of the channel's format (rate u32, then two bytes,
//!   then the bits per sample u16), `name` at its default name.
//! - 0x2010, the TX channels' own names, paged: `[slots, count]`, then
//!   6-byte descriptors `index, id, name`. A real card lists only the
//!   channels with a name of their own (from index 1, its ids in any order);
//!   ours lists every channel, `[id, id, name]`. The id is the second field
//!   (netaudio reads it there).
//!
//! Offsets count from the datagram's first byte, the header included. A
//! list's next page starts at the first item plus the page's count, while
//! the page says more follow (`0x8112`) or was full with fewer items read
//! than the TX count; a page that says neither, a count of 0 or
//! [`MAX_PAGES`] pages end it. A channel whose descriptor does not read (an
//! offset past the reply, a rate of 0, bits not 16, 24 or 32, no name) is
//! counted, not listed. Each exchange is one datagram and one wait of at
//! most [`REPLY_TIMEOUT`] for the reply of its sequence number and opcode;
//! a refusal, no reply or a page that does not read ends the read with a
//! [`ReadError`]: nothing here ever asks again (when to read again is the
//! directory's call, bounded per device). Every request is counted in the
//! status's `sent` table ([`Requests::sent`]).

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::time::{Duration, Instant};

use super::op;
use crate::ctl::Requests;
use crate::wire::{HEADER_LEN, STATUS_MORE, STATUS_OK, read_u16, request, string_at};

/// netaudio's start code (Dante Controller's is 0x2729); the captured
/// replies answered it.
pub const START: u16 = 0x27ff;
/// How long one request waits for its reply.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(1);
/// The most pages one list is read in (512 channels at 16 a page; the IEM
/// card's 128 come in 4).
pub const MAX_PAGES: usize = 32;
/// A 0x2000 descriptor's size.
const CHANNEL_ITEM: usize = 8;
/// A 0x2010 descriptor's size.
const NAME_ITEM: usize = 6;
/// A reply read from the socket; a page fits one datagram (1 472 bytes).
const RECV_BUFFER: usize = 2048;

/// One TX channel of another device, as its pages list it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxChannel {
    /// Its id in a flow request (from 1).
    pub id: u16,
    pub rate: u32,
    /// Bits per sample: 16, 24 or 32.
    pub bits: u32,
    /// Its default name (0x2000).
    pub default: String,
    /// Its own name (0x2010), when it has one.
    pub name: Option<String>,
}

/// A device's TX channels, read through its ARC.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Directory {
    channels: Vec<TxChannel>,
}

impl Directory {
    /// The channels of `listed` (the 0x2000 items; an id listed twice is
    /// kept once) with the names `names` gives them by id (the first for an
    /// id).
    pub fn new(listed: Vec<TxChannel>, names: &[(u16, String)]) -> Self {
        let mut channels: Vec<TxChannel> = Vec::with_capacity(listed.len());
        for mut ch in listed {
            if channels.iter().any(|c| c.id == ch.id) {
                continue;
            }
            ch.name = names
                .iter()
                .find(|(id, _)| *id == ch.id)
                .map(|(_, n)| n.clone());
            channels.push(ch);
        }
        Directory { channels }
    }

    pub fn channels(&self) -> &[TxChannel] {
        &self.channels
    }

    /// The channels with a name of their own.
    pub fn named(&self) -> usize {
        self.channels.iter().filter(|c| c.name.is_some()).count()
    }

    /// The channel called `name` (case aside, as mDNS compares names): by
    /// its own name first, then by its default name (Dante's records carry
    /// both).
    pub fn find(&self, name: &str) -> Option<&TxChannel> {
        let own = |c: &&TxChannel| {
            c.name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        };
        self.channels.iter().find(own).or_else(|| {
            self.channels
                .iter()
                .find(|c| c.default.eq_ignore_ascii_case(name))
        })
    }
}

/// Why a read ended without a directory: the opcode it was at, never an
/// address or a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    #[error("{op:#06x}: socket: {kind}")]
    Io { op: u16, kind: io::ErrorKind },
    #[error("{op:#06x}: no reply within the wait")]
    Timeout { op: u16 },
    #[error("{op:#06x}: refused with status {status:#06x}")]
    Refused { op: u16, status: u16 },
    #[error("{op:#06x}: a reply that does not read")]
    Malformed { op: u16 },
    #[error("the read was stopped")]
    Stopped,
}

impl ReadError {
    /// The fixed word the minute's report counts it under.
    pub fn label(&self) -> &'static str {
        match self {
            ReadError::Io { .. } => "socket",
            ReadError::Timeout { .. } => "no_reply",
            ReadError::Refused { .. } => "refused",
            ReadError::Malformed { .. } => "malformed",
            ReadError::Stopped => "stopped",
        }
    }
}

/// One exchange with the device: the page of `op1` from item `start`;
/// its reply's status and body.
pub trait Exchange {
    fn ask(&mut self, op1: u16, start: u16) -> Result<(u16, Vec<u8>), ReadError>;
}

/// netaudio's request for the page of `op1` from item `start` (from 1).
pub fn page_request(seq: u16, op1: u16, start: u16) -> Vec<u8> {
    let [hi, lo] = start.to_be_bytes();
    request(START, seq, op1, 0, &[0, 1, hi, lo, 0, 0])
}

/// The reply to (`seq`, `op1`) in `d`: its status and body; `None` for any
/// other datagram (another sequence number or opcode, status 0, shorter
/// than the header). The start code and the length field are not read (as
/// our flow-control client and inferno).
pub fn reply_to(d: &[u8], seq: u16, op1: u16) -> Option<(u16, &[u8])> {
    let (head, body) = d.split_first_chunk::<HEADER_LEN>()?;
    let field = |at: usize| u16::from_be_bytes([head[at], head[at + 1]]);
    let status = field(8);
    (field(4) == seq && field(6) == op1 && status != 0).then_some((status, body))
}

/// One page of a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// It says more follow (`0x8112`).
    pub more: bool,
    pub slots: usize,
    /// The items it counts, read or not.
    pub count: usize,
    /// The items that read.
    pub items: Vec<T>,
}

/// The page table of `body` with `item`-byte descriptors: its slots and
/// count; `None` when the count passes the slots or the table the body.
fn table(body: &[u8], item: usize) -> Option<(usize, usize)> {
    let (&slots, &count) = (body.first()?, body.get(1)?);
    let (slots, count) = (usize::from(slots), usize::from(count));
    (count <= slots && 2 + slots * item <= body.len()).then_some((slots, count))
}

/// A page of 0x2000 (the TX channels) with status `status`.
pub fn tx_channels(status: u16, body: &[u8]) -> Option<Page<TxChannel>> {
    let (slots, count) = table(body, CHANNEL_ITEM)?;
    let items = (0..count)
        .filter_map(|i| {
            let at = 2 + i * CHANNEL_ITEM;
            let id = read_u16(body, at).filter(|&id| id > 0)?;
            let format = usize::from(read_u16(body, at + 4)?).checked_sub(HEADER_LEN)?;
            let rate = u32::from_be_bytes(body.get(format..format + 4)?.try_into().ok()?);
            let bits = u32::from(read_u16(body, format + 6)?);
            let default = string_at(body, read_u16(body, at + 6)?).filter(|n| !n.is_empty())?;
            (rate > 0 && [16, 24, 32].contains(&bits)).then(|| TxChannel {
                id,
                rate,
                bits,
                default: default.to_string(),
                name: None,
            })
        })
        .collect();
    Some(Page {
        more: status == STATUS_MORE,
        slots,
        count,
        items,
    })
}

/// A page of 0x2010 (the TX channels' own names) with status `status`: each
/// item's id and name.
pub fn tx_names(status: u16, body: &[u8]) -> Option<Page<(u16, String)>> {
    let (slots, count) = table(body, NAME_ITEM)?;
    let items = (0..count)
        .filter_map(|i| {
            let at = 2 + i * NAME_ITEM;
            let id = read_u16(body, at + 2).filter(|&id| id > 0)?;
            let name = string_at(body, read_u16(body, at + 4)?).filter(|n| !n.is_empty())?;
            Some((id, name.to_string()))
        })
        .collect();
    Some(Page {
        more: status == STATUS_MORE,
        slots,
        count,
        items,
    })
}

/// Reads the device's TX channels through `ex`: the counts, the 0x2000
/// pages, then (with any channel) the 0x2010 pages.
pub fn read_directory(ex: &mut impl Exchange) -> Result<Directory, ReadError> {
    let (status, body) = ex.ask(op::COUNTS, 1)?;
    if status != STATUS_OK {
        return Err(ReadError::Refused {
            op: op::COUNTS,
            status,
        });
    }
    let total = usize::from(read_u16(&body, 2).ok_or(ReadError::Malformed { op: op::COUNTS })?);
    let channels = read_list(ex, op::TX_CHANNELS, total, tx_channels)?;
    let names = if channels.is_empty() {
        Vec::new()
    } else {
        read_list(ex, op::TX_NAMES, total, tx_names)?
    };
    Ok(Directory::new(channels, &names))
}

/// The items of the list `op1`, page after page (see the module), of a
/// device with `total` TX channels.
fn read_list<T>(
    ex: &mut impl Exchange,
    op1: u16,
    total: usize,
    parse: fn(u16, &[u8]) -> Option<Page<T>>,
) -> Result<Vec<T>, ReadError> {
    let mut out = Vec::new();
    let mut counted = 0;
    let mut start: u16 = 1;
    for _ in 0..MAX_PAGES {
        if counted >= total {
            break;
        }
        let (status, body) = ex.ask(op1, start)?;
        if status != STATUS_OK && status != STATUS_MORE {
            return Err(ReadError::Refused { op: op1, status });
        }
        let page = parse(status, &body).ok_or(ReadError::Malformed { op: op1 })?;
        counted += page.count;
        out.extend(page.items);
        let full = page.count == page.slots;
        if page.count == 0 || !(page.more || full) {
            break;
        }
        match u16::try_from(usize::from(start) + page.count) {
            Ok(next) => start = next,
            Err(_) => break,
        }
    }
    Ok(out)
}

/// The exchange over UDP: one socket bound to our address and connected to
/// the device's ARC (only its datagrams are read; a closed port is an error
/// at once), each request counted, then one wait for its reply.
#[derive(Debug)]
pub struct Udp<'a> {
    sock: UdpSocket,
    to: IpAddr,
    seq: &'a AtomicU16,
    wait: Duration,
    requests: &'a Requests,
    stop: &'a AtomicBool,
    /// The requests sent so far.
    pub sent: usize,
    /// The device's peer id in the status (0 before the first request).
    pub peer: usize,
}

impl<'a> Udp<'a> {
    /// The exchange from our address `ip` with the ARC at `to`, sequence
    /// numbers from `seq`, each reply awaited `wait` at most, requests
    /// counted in `requests`; none is sent once `stop` is set.
    pub fn open(
        ip: Ipv4Addr,
        to: SocketAddrV4,
        seq: &'a AtomicU16,
        wait: Duration,
        requests: &'a Requests,
        stop: &'a AtomicBool,
    ) -> io::Result<Self> {
        let sock = UdpSocket::bind(SocketAddrV4::new(ip, 0))?;
        sock.connect(SocketAddr::V4(to))?;
        Ok(Udp {
            sock,
            to: IpAddr::V4(*to.ip()),
            seq,
            wait,
            requests,
            stop,
            sent: 0,
            peer: 0,
        })
    }
}

impl Exchange for Udp<'_> {
    fn ask(&mut self, op1: u16, start: u16) -> Result<(u16, Vec<u8>), ReadError> {
        if self.stop.load(Ordering::Relaxed) {
            return Err(ReadError::Stopped);
        }
        let io = |e: io::Error| ReadError::Io {
            op: op1,
            kind: e.kind(),
        };
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.peer = self.requests.sent(self.to, op1);
        self.sent += 1;
        self.sock.send(&page_request(seq, op1, start)).map_err(io)?;
        let deadline = Instant::now() + self.wait;
        let mut buf = [0u8; RECV_BUFFER];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(ReadError::Timeout { op: op1 });
            }
            self.sock.set_read_timeout(Some(left)).map_err(io)?;
            match self.sock.recv(&mut buf) {
                Ok(n) => {
                    if let Some((status, body)) = reply_to(&buf[..n], seq, op1) {
                        return Ok((status, body.to_vec()));
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(ReadError::Timeout { op: op1 });
                }
                Err(e) => return Err(io(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests;
