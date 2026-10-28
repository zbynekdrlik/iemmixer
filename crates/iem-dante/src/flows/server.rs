// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from inferno (https://github.com/teodly/inferno at 9767558):
// inferno_aoip/src/device_server/flows_control_server.rs (which request is
// answered how, and in which order the request's values are checked),
// Copyright (C) Teo and the inferno contributors, GPL-3.0-or-later OR
// AGPL-3.0-or-later, taken under GPL-3.0-or-later. Changes Copyright (C)
// 2026 iemmixer contributors.

//! Our answers on UDP 4455, the transmitter's side of flow control. Every
//! request inferno answers is answered as inferno answers it (a receiver
//! or Dante Controller asks again at once after an answer it did not
//! expect: #72, 2026-10-26), with one answer where inferno sends two or
//! panics, and our network-safety refusals where inferno would send what
//! a band's network must not carry. In order:
//!
//! | request | condition | answer |
//! |---|---|---|
//! | any | from a source that is not unicast, or port 0 | none |
//! | any | shorter than the header | none |
//! | any | its length field disagrees with its size | none, counted `misframed` |
//! | any | op2 not 0 (a reply, too: we never answer one) | none, counted |
//! | any | op1 not 0x0100, 0x0101, 0x0102 | none, counted |
//! | 0x0100 | unreadable ([`control::Unread`]: inferno logs or panics) | none, counted |
//! | 0x0100 | a channel id past our TX channels | 0x0302 (inferno: 0x0302, then goes on to a second answer) |
//! | 0x0100 | another sample rate | 0x0301 |
//! | 0x0100 | more than 256 frames a packet | 0x0302 |
//! | 0x0100 | ours: more frames a packet than our channels offer (the config's `tx_fpp_max`, which its advertised latency covers) | 0x0302 (inferno would accept) |
//! | 0x0100 | ours: fpp 0, bits / 8 not 2–4, more than 8 slots, a packet past one datagram, audio to another address than the requester's, port 0 | 0x0302 (inferno would accept: then divide by zero, send malformed packets, index past its buffer or send to anyone) |
//! | 0x0100 | no free slot once expired flows are freed | 0x0315 |
//! | 0x0100 | otherwise | OK, the 6-byte handle (a destination already served keeps its handle) |
//! | 0x0101 | a body that is not exactly the handle | none, counted |
//! | 0x0101 | an unknown handle | 0x0103 |
//! | 0x0101 | otherwise | OK, no body: the flow ends |
//! | 0x0102 | the handle, count or ids cut | none, counted (inferno panics) |
//! | 0x0102 | an unknown handle | 0x0103 |
//! | 0x0102 | ours: a channel id past ours, more than 8 slots, a packet past one datagram | 0x0302 (inferno: OK, then its TX thread panics) |
//! | 0x0102 | otherwise | OK, no body: the channels set, an expired flow revived |
//!
//! Every answer is unicast to the requester, never anywhere else, and only
//! the requester's own address can receive the audio. At most
//! [`PER_SPAN`] requests of a peer are read in any [`SPAN`] ([`PeerLimit`]);
//! the rest are dropped unread and counted `limited`. Refused (any status
//! but OK), unanswered, misframed and limited requests are counted per peer
//! and opcode in the gap guard ([`crate::arc::gaps`]), logged as one warning
//! a minute. Logs name slots, counts, opcodes and peer ids, never a name or
//! an address.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, warn};

use super::control::{self, Unread, op, status};
use crate::arc::gaps::{self, Gap, Gaps};
use crate::ctl::{Requests, header_op};
use crate::device::DeviceConfig;
use crate::media::tx::{FPP_MAX, MAX_FLOWS, TxShared, TxTable, sendable};
use crate::wire::{Header, WireError, read_u16, reply};

/// The most requests of one peer read in any [`SPAN`].
pub const PER_SPAN: usize = 10;
/// The window [`PER_SPAN`] counts over.
pub const SPAN: Duration = Duration::from_secs(1);
/// The peers with a window of their own; any further peer shares one
/// window with every other such peer.
pub const MAX_PEERS: usize = 64;
/// How often the server looks whether a minute of gaps is to be reported.
pub const GAP_CHECK: Duration = Duration::from_secs(1);
/// How often the server keeps heard flows alive and expires the rest
/// ([`TxShared::sync`]).
pub const EXPIRE_CHECK: Duration = Duration::from_millis(100);
/// A datagram read from the socket; anything longer is cut and so fails
/// [`Header::parse`].
const RECV_BUFFER: usize = 2048;

/// What a datagram on UDP 4455 comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowOutcome {
    /// Send this reply to the requester.
    Reply(Vec<u8>),
    /// A request inferno leaves unanswered: no reply.
    Unanswered { op: u16 },
    /// Its length field disagrees with its size (`op` as its header says):
    /// no reply.
    Misframed { op: u16 },
    /// Over the peer's bound: dropped unread, no reply.
    Limited { op: u16 },
    /// Shorter than the header, or from a source no reply can go to.
    Ignored,
}

impl FlowOutcome {
    /// What the gap guard counts of it: a refusal (any status but OK), no
    /// answer, a misframed or a limited datagram, with its opcode.
    pub fn gap(&self) -> Option<(u16, Gap)> {
        match self {
            FlowOutcome::Reply(bytes) => match Header::parse(bytes) {
                Ok((h, _)) if h.op2 != status::OK => Some((h.op1, Gap::Refused)),
                _ => None,
            },
            FlowOutcome::Unanswered { op } => Some((*op, Gap::Unanswered)),
            FlowOutcome::Misframed { op } => Some((*op, Gap::Misframed)),
            FlowOutcome::Limited { op } => Some((*op, Gap::Limited)),
            FlowOutcome::Ignored => None,
        }
    }
}

/// Answers one datagram `req` from `from` at the control side's `now`
/// (ns), changing `table` only for an OK. Pure apart from its debug and
/// info lines.
pub fn handle(
    table: &mut TxTable,
    cfg: &DeviceConfig,
    from: SocketAddrV4,
    req: &[u8],
    now: u64,
) -> FlowOutcome {
    if !unicast(*from.ip()) || from.port() == 0 {
        debug!("flow-control datagram from a source no reply can go to: dropped");
        return FlowOutcome::Ignored;
    }
    let (h, body) = match Header::parse(req) {
        Ok(parsed) => parsed,
        Err(e @ WireError::Length { .. }) => {
            let op = read_u16(req, 6).unwrap_or(0);
            debug!(error = %e, op = format_args!("{op:#06x}"), "flow-control datagram dropped, counted");
            return FlowOutcome::Misframed { op };
        }
        Err(e @ WireError::Short(_)) => {
            debug!(error = %e, "flow-control datagram dropped");
            return FlowOutcome::Ignored;
        }
    };
    debug!(
        op = format_args!("{:#06x}", h.op1),
        seq = h.seq,
        "flow-control request"
    );
    if h.op2 != 0 {
        return unanswered(&h, "op2 is not 0 (a reply, or no request)");
    }
    match h.op1 {
        op::REQUEST => request(table, cfg, &h, from, req, now),
        op::STOP => match control::parse_stop(body) {
            Err(e) => unread(&h, e),
            Ok(handle) => match table.stop(&handle) {
                Ok(slot) => {
                    info!(slot, "TX flow stopped by its receiver");
                    ok(&h, &[])
                }
                Err(code) => refused(&h, code, "stop of an unknown flow"),
            },
        },
        op::UPDATE => update(table, cfg, &h, body, now),
        _ => unanswered(&h, "an opcode inferno does not answer"),
    }
}

/// [`handle`] behind the peer's bound: a request over it is dropped
/// unread ([`FlowOutcome::Limited`]); `at` is the bound's clock.
pub fn answer(
    limit: &mut PeerLimit,
    table: &mut TxTable,
    cfg: &DeviceConfig,
    from: SocketAddrV4,
    req: &[u8],
    at: Instant,
    now: u64,
) -> FlowOutcome {
    if !limit.allow(at, *from.ip()) {
        return FlowOutcome::Limited {
            op: read_u16(req, 6).unwrap_or(0),
        };
    }
    handle(table, cfg, from, req, now)
}

/// A flow request: inferno's checks in inferno's order, then ours.
fn request(
    table: &mut TxTable,
    cfg: &DeviceConfig,
    h: &Header,
    from: SocketAddrV4,
    req: &[u8],
    now: u64,
) -> FlowOutcome {
    let r = match control::parse_request(req) {
        Ok(r) => r,
        Err(e) => return unread(h, e),
    };
    let (code, why) = if !ids_ok(&r.channels, cfg) {
        (status::BAD_REQUEST, "a channel id past our TX channels")
    } else if r.sample_rate != cfg.sample_rate {
        (status::SAMPLE_RATE_MISMATCH, "another sample rate")
    } else if r.fpp > FPP_MAX {
        (status::BAD_REQUEST, "more than 256 frames a packet")
    } else if r.fpp > cfg.tx_fpp_max {
        (
            status::BAD_REQUEST,
            "more frames a packet than our channels offer",
        )
    } else if !sendable(r.channels.len(), (r.bits / 8) as usize, r.fpp) {
        (status::BAD_REQUEST, "a flow that cannot be sent")
    } else if r.rx.ip() != from.ip() || r.rx.port() == 0 {
        (
            status::BAD_REQUEST,
            "audio to another address than the requester's",
        )
    } else {
        let served = table.slot_of(r.rx).is_some();
        return match table.add(&r, now) {
            Ok((slot, handle)) => {
                if served {
                    debug!(slot, "TX flow asked again: its channels set");
                } else {
                    info!(
                        slot,
                        slots = r.channels.len(),
                        fpp = r.fpp,
                        bits = r.bits,
                        "TX flow started"
                    );
                }
                ok(h, &handle)
            }
            Err(code) => refused(h, code, "no free TX flow"),
        };
    };
    refused(h, code, why)
}

/// A flow update: inferno's lookup, then our checks of the channels.
fn update(
    table: &mut TxTable,
    cfg: &DeviceConfig,
    h: &Header,
    body: &[u8],
    now: u64,
) -> FlowOutcome {
    let (handle, channels) = match control::parse_update(body) {
        Ok(parsed) => parsed,
        Err(e) => return unread(h, e),
    };
    let Some((_, flow)) = table.get(&handle) else {
        return refused(h, status::FLOW_NOT_FOUND, "update of an unknown flow");
    };
    if !ids_ok(&channels, cfg) || !sendable(channels.len(), usize::from(flow.bps), flow.fpp) {
        return refused(h, status::BAD_REQUEST, "channels that cannot be sent");
    }
    match table.update(&handle, &channels, now) {
        Ok(slot) => {
            debug!(slot, slots = channels.len(), "TX flow's channels set");
            ok(h, &[])
        }
        Err(code) => refused(h, code, "update of an unknown flow"),
    }
}

/// Every channel id names one of our TX channels (inferno: `id − 1` below
/// their count).
fn ids_ok(ids: &[Option<u16>], cfg: &DeviceConfig) -> bool {
    ids.iter()
        .flatten()
        .all(|&id| usize::from(id) <= cfg.tx.len())
}

/// An address a reply may go to: not multicast, broadcast or 0.0.0.0.
fn unicast(ip: Ipv4Addr) -> bool {
    !ip.is_multicast() && !ip.is_broadcast() && !ip.is_unspecified()
}

fn ok(h: &Header, body: &[u8]) -> FlowOutcome {
    FlowOutcome::Reply(reply(h, status::OK, body))
}

fn refused(h: &Header, code: u16, why: &str) -> FlowOutcome {
    debug!(
        op = format_args!("{:#06x}", h.op1),
        seq = h.seq,
        status = format_args!("{code:#06x}"),
        why,
        "flow-control request refused"
    );
    FlowOutcome::Reply(reply(h, code, &[]))
}

fn unanswered(h: &Header, why: &str) -> FlowOutcome {
    debug!(
        op = format_args!("{:#06x}", h.op1),
        seq = h.seq,
        why,
        "flow-control request not answered"
    );
    FlowOutcome::Unanswered { op: h.op1 }
}

fn unread(h: &Header, e: Unread) -> FlowOutcome {
    debug!(
        op = format_args!("{:#06x}", h.op1),
        seq = h.seq,
        error = %e,
        "flow-control request not read"
    );
    FlowOutcome::Unanswered { op: h.op1 }
}

/// The requests of one peer in the last [`SPAN`]: the times of the last
/// [`PER_SPAN`], a ring whose `next` slot holds the oldest.
#[derive(Debug, Clone, Default)]
struct Window {
    times: [Option<Instant>; PER_SPAN],
    next: usize,
}

impl Window {
    /// Whether a request at `now` is within the bound (then it counts).
    fn allow(&mut self, now: Instant) -> bool {
        let oldest = &mut self.times[self.next];
        if oldest.is_some_and(|t| now.saturating_duration_since(t) < SPAN) {
            return false;
        }
        *oldest = Some(now);
        self.next = (self.next + 1) % PER_SPAN;
        true
    }

    /// No request in the last [`SPAN`].
    fn idle(&self, now: Instant) -> bool {
        self.times
            .iter()
            .flatten()
            .all(|&t| now.saturating_duration_since(t) >= SPAN)
    }
}

/// The bound on each peer's requests (ours, not inferno's: a sender that
/// loops, or one that forges many sources, gets at most [`PER_SPAN`]
/// requests a second read per address, and the peers past [`MAX_PEERS`]
/// share one window).
#[derive(Debug, Default)]
pub struct PeerLimit {
    peers: Vec<(Ipv4Addr, Window)>,
    others: Window,
}

impl PeerLimit {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a request of `peer` at `now` is read (then it counts).
    pub fn allow(&mut self, now: Instant, peer: Ipv4Addr) -> bool {
        let at = match self.peers.iter().position(|(p, _)| *p == peer) {
            Some(at) => at,
            None if self.peers.len() < MAX_PEERS => {
                self.peers.push((peer, Window::default()));
                self.peers.len() - 1
            }
            // A peer quiet for a whole span gives its window up.
            None => match self.peers.iter().position(|(_, w)| w.idle(now)) {
                Some(at) => {
                    self.peers[at] = (peer, Window::default());
                    at
                }
                None => return self.others.allow(now),
            },
        };
        self.peers[at].1.allow(now)
    }
}

/// The flow-control server: answers every datagram on `sock` (bound to
/// the device's address, [`control::PORT`]) from `shared`'s table and hands
/// each change to the TX thread; every [`EXPIRE_CHECK`] keeps heard flows
/// alive and expires the rest. Every datagram is counted in `requests`
/// (the control socket's status), over the bound or not. Ends only with an
/// error receiving; a reply that cannot be sent is logged and the server
/// goes on. Glue: every decision is [`answer`]'s and [`TxShared::sync`]'s.
pub async fn serve(
    sock: UdpSocket,
    shared: Arc<TxShared>,
    requests: Arc<Requests>,
) -> io::Result<()> {
    let mut buf = vec![0u8; RECV_BUFFER];
    let mut gaps = Gaps::new(gaps::WINDOW);
    let mut limit = PeerLimit::new();
    let mut expiry = tokio::time::interval(EXPIRE_CHECK);
    expiry.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut check = tokio::time::interval(GAP_CHECK);
    check.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        let report = tokio::select! {
            got = sock.recv_from(&mut buf) => {
                let (n, peer) = got?;
                requests.received(control::PORT, peer.ip(), header_op(&buf[..n]));
                match peer {
                    SocketAddr::V4(from) => {
                        let at = Instant::now();
                        let outcome = {
                            let mut c = shared.lock();
                            let outcome = answer(&mut limit, &mut c.table, &shared.cfg, from, &buf[..n], at, shared.now_ns());
                            shared.publish(&c);
                            outcome
                        };
                        if let FlowOutcome::Reply(bytes) = &outcome
                            && let Err(e) = sock.send_to(bytes, peer).await
                        {
                            warn!(error = %e, "a flow-control reply could not be sent");
                        }
                        outcome
                            .gap()
                            .and_then(|(op, gap)| gaps.record(at, IpAddr::V4(*from.ip()), op, gap))
                    }
                    SocketAddr::V6(_) => None,
                }
            }
            _ = expiry.tick() => {
                let expired = shared.sync(shared.now_ns());
                for slot in (0..MAX_FLOWS).filter(|s| expired >> s & 1 == 1) {
                    info!(slot, "TX flow expired: nothing from its receiver for 4 s");
                }
                None
            }
            _ = check.tick() => gaps.flush(Instant::now()),
        };
        if let Some(report) = report {
            warn!(
                requests = report.requests(),
                counts = %report,
                "flow-control requests refused, not answered or over the bound in a minute (peer id, opcode)"
            );
        }
    }
}

#[cfg(test)]
mod tests;
