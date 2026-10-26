// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from inferno (https://github.com/teodly/inferno at 9767558):
// inferno_aoip/src/protocol/proto_arc.rs (the opcodes, the body layouts and
// the paging) and inferno_aoip/src/device_server/arc_server.rs (the
// answers), Copyright (C) Teo and the inferno contributors,
// GPL-3.0-or-later OR AGPL-3.0-or-later, taken under GPL-3.0-or-later.
// Changes Copyright (C) 2026 iemmixer contributors.

//! ARC, Dante's control protocol on UDP 4440: the device's names, its
//! channel counts and lists, channel renames, its flows (phase B: the TX
//! flows receivers asked for on 4455, our RX flows) and its RX
//! subscriptions. Every opcode inferno's server answers is answered as
//! inferno answers it with flows and a subscriber (a request Dante
//! Controller gets refused it asks again at once: #72, 2026-10-26, 0x2200
//! and 0x2204 about 2 250 times a second while both were refused).
//!
//! | op1 | answer |
//! |---|---|
//! | 0x1000 | counts: TX, RX, inferno's 32 TX and 32 RX flows |
//! | 0x1002 | the device name |
//! | 0x1003 | the device names: current, factory, board, revision |
//! | 0x1100, 0x1102 | zeros, as inferno answers Dante Controller |
//! | 0x2000 | TX channels with their default names, paged |
//! | 0x2010 | TX channels' current names, paged |
//! | 0x2013 | TX rename |
//! | 0x2200 | TX flows, [`FLOW_PAGE`] slots a page (inferno's `query_tx_flows`) |
//! | 0x2201, 0x2202 | create, delete a multicast TX flow: no multicast, refused (inferno's answer when it creates or deletes none) |
//! | 0x2320 | status [`STATUS_2320`], no body (inferno's "???") |
//! | 0x3000 | RX channels with their names, subscriptions and statuses, paged |
//! | 0x3001 | RX rename |
//! | 0x3010 | subscribe or unsubscribe RX channels: OK, no body |
//! | 0x3014 | netaudio's unsubscribe: OK, no body |
//! | 0x3200 | RX flows, [`FLOW_PAGE`] slots a page (inferno's `query_rx_flows`) |
//! | 0x3300 | the fixed 8 bytes that keep Dante Controller from a clock-domain warning |
//!
//! No reply, as from inferno, to any other opcode (0x2204 and 0x1001
//! among them), to a request whose op2 is not 0, and to a body inferno
//! fails to read before it answers (0x2201 under 2 bytes, 0x2202 under 4,
//! 0x3010 whose items are cut, 0x3014 under 6): [`ArcOutcome::Unanswered`].
//! A datagram that is no request ([`Header::parse`]) is dropped; one whose
//! length field disagrees with its size is counted as well
//! ([`ArcOutcome::Misframed`]). Refused
//! ([`STATUS_ERROR`]), changing nothing: a rename that renames nothing, a
//! page request without its first item, 0x2201 and 0x2202, and a change
//! the state file cannot keep ([`respond`]). A rename is all or nothing:
//! one invalid name refuses the whole request (inferno renames the valid
//! ones). An empty name is Dante's reset to the channel's default name
//! (netaudio's `reset_channel_name` sends one). Every refused or
//! unanswered request is counted per peer and opcode ([`gaps`]); the server
//! logs the counts as one warning a minute.
//!
//! **Subscriptions** (0x3010, 0x3014) change our own RX channels only, and
//! are kept in the state file before the OK goes out; the channel reads
//! Unresolved at once (Dante Controller reads 0x3000 right after), and the
//! server hands the new subscriptions to the manager ([`Flows::subscribe`]),
//! which asks the transmitters for flows. A subscription whose names
//! Dante's rules refuse is not made (inferno makes it; our state file
//! could not keep it): the requester still gets inferno's OK.
//!
//! **Pages** (inferno's `serialize_items`): `[slots, count]`, `slots`
//! descriptors (the unused ones zero), then what they point at. Channel
//! lists hold 32 TX or 16 RX channels a page, as real devices answer
//! (netaudio steps through pages by those counts, Dante Controller by the
//! count each page reports), flow lists [`FLOW_PAGE`] of the 32 flow slots
//! (an empty slot is passed over, not counted). A page ends early, status
//! `0x8112`, once an item has brought it to [`SOFT_LIMIT`] bytes: that
//! item's descriptor is written but not counted (inferno), so the next page
//! starts with it. Every page fits one datagram
//! ([`MAX_DATAGRAM`](crate::wire::MAX_DATAGRAM)): no item is longer than
//! 600 bytes (a TX flow's names are cut to [`MAX_REMOTE_NAME`], ours are 31
//! at most). Logs name opcodes, sequences and channel numbers, never a
//! name.
//!
//! Our own reads of another device's ARC (a transmitter's channel list, for
//! a device that publishes no `_netaudio-chan` record, plan Task 9b) are
//! [`client`]'s: read-only, the same page format.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::time::MissedTickBehavior;
use tracing::{debug, error, info, warn};

use crate::cmc::PROCESS_ID;
use crate::ctl::{Requests, header_op};
use crate::device::{
    Device, DeviceConfig, DeviceState, Subscription, channel_problems, valid_channel_name,
};
use crate::info::Changes;
use crate::media::tx::{TxShared, TxTable};
use crate::subscribe::{Manager, SubStatus};
use crate::wire::{
    Body, HEADER_LEN, Header, STATUS_ERROR, STATUS_MORE, STATUS_OK, WireError, read_u16, reply,
    string_at,
};
use gaps::{Gap, Gaps};

pub mod client;
pub mod gaps;

/// ARC's UDP port.
pub const PORT: u16 = 4440;
/// How often the server looks whether a minute of [`Gaps`] is to be
/// reported.
pub const GAP_CHECK: Duration = Duration::from_secs(1);

/// The opcodes (op1) phase A answers.
pub mod op {
    pub const COUNTS: u16 = 0x1000;
    pub const DEVICE_NAME: u16 = 0x1002;
    pub const DEVICE_NAMES: u16 = 0x1003;
    /// Asked by Dante Controller; inferno answers 110 zero bytes.
    pub const ZEROS_1100: u16 = 0x1100;
    /// Asked by Dante Controller; inferno answers 94 zero bytes.
    pub const ZEROS_1102: u16 = 0x1102;
    pub const TX_CHANNELS: u16 = 0x2000;
    pub const TX_NAMES: u16 = 0x2010;
    pub const TX_RENAME: u16 = 0x2013;
    pub const TX_FLOWS: u16 = 0x2200;
    pub const TX_MULTICAST_CREATE: u16 = 0x2201;
    pub const TX_MULTICAST_DELETE: u16 = 0x2202;
    /// Asked by Dante Controller; inferno answers status
    /// [`STATUS_2320`](super::STATUS_2320) and no body.
    pub const OP_2320: u16 = 0x2320;
    pub const RX_CHANNELS: u16 = 0x3000;
    pub const RX_RENAME: u16 = 0x3001;
    pub const RX_SUBSCRIBE: u16 = 0x3010;
    /// netaudio's unsubscribe.
    pub const RX_UNSUBSCRIBE: u16 = 0x3014;
    pub const RX_FLOWS: u16 = 0x3200;
    pub const CLOCK: u16 = 0x3300;
}

/// The slots of a flow list's page (0x2200, 0x3200): inferno's 32 flows,
/// at most 16 a page.
pub const FLOW_PAGE: u8 = 16;
/// The most flows each way (inferno's TX and RX `MAX_FLOWS`, 0x1000).
pub const MAX_FLOWS: u16 = 32;
/// A page ends once an item has brought the reply to this many bytes, the
/// header counted (inferno's `PACKET_SIZE_SOFT_LIMIT`).
pub const SOFT_LIMIT: usize = 800;
/// The most bytes of a receiver's device name and of its flow name a TX
/// flow list writes (inferno writes them whole). A Dante name has 31 at
/// most, but the 4455 request that brought them may hold a kilobyte; cut
/// here, every item of a page stays far below a datagram.
pub const MAX_REMOTE_NAME: usize = 63;
/// inferno's status for 0x2320 ("???" in its source).
pub const STATUS_2320: u16 = 0x0030;
/// A flow's socket descriptor (`0x8002`, port, IPv4 address).
const SOCKET_DESCRIPTOR: u16 = 0x8002;
/// A TX flow descriptor's type: unicast (2 is multicast).
const FLOW_UNICAST: u16 = 0x11;

/// TX channels per page.
pub const TX_PAGE: usize = 32;
/// RX channels per page.
pub const RX_PAGE: usize = 16;
/// The PCM type of the channel descriptor and the mDNS `pcm` entry
/// (inferno: "usually 0xe, in older devices 4").
pub const PCM_TYPE: u8 = 0x0e;
/// The most channels one flow carries (inferno's `MAX_CHANNELS_IN_FLOW`).
pub const MAX_CHANNELS_IN_FLOW: u16 = 8;
/// The answer to 0x3300 (inferno: "necessary to avoid 'clock domain
/// mismatch' error in DC").
pub const CLOCK_REPLY: [u8; 8] = [0x38, 0x00, 0x38, 0xfd, 0x38, 0xfe, 0x38, 0xff];
/// The counts' flags byte: bit 4, TX channels can be renamed. Bit 5, TX
/// multicast, stays clear (inferno sets it): the device creates no
/// multicast flow (0x2201 refused), so Dante Controller is never offered one.
const FLAG_TX_RENAME: u8 = 0x10;
/// The device names' header (0x1003), before its strings.
const NAMES_HEADER: usize = 38;
/// The 16-byte descriptor of a page's channels.
const COMMON_DESCRIPTOR: usize = 16;
/// A datagram read from the socket; anything longer is cut and so fails
/// [`Header::parse`].
const RECV_BUFFER: usize = 2048;

/// What a saved change changed besides the names the mDNS records carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    /// RX channels (from 0) renamed: the channel-change notification names
    /// them (inferno).
    pub renamed_rx: Vec<usize>,
    /// RX channels (from 0) whose subscription changed: the manager takes
    /// them ([`Flows::subscribe`]), and the notification names them.
    pub subscriptions: Vec<usize>,
}

/// What a request comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArcOutcome {
    /// Send this reply; nothing changed.
    Reply(Vec<u8>),
    /// Send this reply; the state changed, so the caller saves it, announces
    /// the new mDNS records and passes the [`Change`] on.
    Changed(Vec<u8>, Change),
    /// A request inferno leaves unanswered: no reply.
    Unanswered { op1: u16 },
    /// A datagram whose length field disagrees with its size (`op1` as its
    /// header's bytes 6–7 say): no request, no reply, but counted
    /// ([`Gap::Misframed`]), so a sender we misread shows in the warning.
    Misframed { op1: u16 },
    /// Shorter than the header: no request, no reply.
    Ignored,
}

impl ArcOutcome {
    /// What [`gaps::Gaps`] counts of this outcome: the opcode of a request
    /// left unanswered or refused ([`STATUS_ERROR`]), or of a datagram whose
    /// length field disagrees; `None` for any other answer and for a
    /// datagram shorter than the header.
    pub fn gap(&self) -> Option<(u16, Gap)> {
        match self {
            ArcOutcome::Unanswered { op1 } => Some((*op1, Gap::Unanswered)),
            ArcOutcome::Misframed { op1 } => Some((*op1, Gap::Misframed)),
            ArcOutcome::Reply(bytes) => match Header::parse(bytes) {
                Ok((h, _)) if h.op2 == STATUS_ERROR => Some((h.op1, Gap::Refused)),
                _ => None,
            },
            ArcOutcome::Changed(..) | ArcOutcome::Ignored => None,
        }
    }
}

/// The running flows as the ARC lists show them: the TX flows receivers
/// asked for on 4455 and our RX subscriptions' flows and statuses.
#[derive(Debug, Clone, Copy)]
pub struct Live<'a> {
    pub tx: &'a TxTable,
    pub rx: &'a Manager,
}

/// Where the ARC server reads the running flows and leaves the saved
/// subscriptions. Locks are taken in one order across the device: the
/// device, then the TX table, then the manager; nothing holding a later one
/// takes an earlier one.
pub trait Flows: Send + Sync + 'static {
    /// Calls `f` with the flows as they are now.
    fn view<R>(&self, f: impl FnOnce(&Live<'_>) -> R) -> R;
    /// The saved subscriptions changed to `subs`: the manager takes them.
    /// Returns the RX channels whose status moved
    /// ([`Manager::changed`]).
    fn subscribe(&self, subs: &[Option<Subscription>]) -> Vec<usize>;
}

/// The daemon's flows (plan Task 7): the TX side's table and the manager.
#[derive(Debug, Clone)]
pub struct Running {
    pub tx: Arc<TxShared>,
    pub rx: Arc<Mutex<Manager>>,
}

impl Flows for Running {
    fn view<R>(&self, f: impl FnOnce(&Live<'_>) -> R) -> R {
        let control = self.tx.lock();
        let rx = self.rx.lock().unwrap_or_else(PoisonError::into_inner);
        f(&Live {
            tx: &control.table,
            rx: &rx,
        })
    }

    fn subscribe(&self, subs: &[Option<Subscription>]) -> Vec<usize> {
        let mut rx = self.rx.lock().unwrap_or_else(PoisonError::into_inner);
        rx.load(subs);
        rx.changed()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Rx,
    Tx,
}

impl Dir {
    fn label(self) -> &'static str {
        match self {
            Dir::Rx => "RX",
            Dir::Tx => "TX",
        }
    }
}

/// Answers one datagram from `state`, `cfg` and the running flows `live`.
/// Only a valid rename or a subscription change changes `state` (and is
/// [`ArcOutcome::Changed`]); this function saves nothing ([`respond`] does)
/// and tells nobody (the server does).
pub fn handle(
    state: &mut DeviceState,
    cfg: &DeviceConfig,
    live: &Live<'_>,
    req: &[u8],
) -> ArcOutcome {
    let (h, body) = match Header::parse(req) {
        Ok(parsed) => parsed,
        Err(e @ WireError::Length { .. }) => {
            // The header is there (Length comes only after it was read):
            // its opcode names the datagram in the gap guard's count.
            let op1 = read_u16(req, 6).unwrap_or(0);
            debug!(error = %e, op = format_args!("{op1:#06x}"), "ARC datagram dropped, counted");
            return ArcOutcome::Misframed { op1 };
        }
        Err(e @ WireError::Short(_)) => {
            debug!(error = %e, "ARC datagram dropped");
            return ArcOutcome::Ignored;
        }
    };
    debug!(
        op = format_args!("{:#06x}", h.op1),
        seq = h.seq,
        "ARC request"
    );
    if h.op2 != 0 {
        return unanswered(&h, "op2 is not 0");
    }
    let ok = |body: &[u8]| ArcOutcome::Reply(reply(&h, STATUS_OK, body));
    match h.op1 {
        op::COUNTS => ok(&counts(cfg)),
        op::DEVICE_NAME => {
            let mut out = Body::new();
            out.cstr(&state.name);
            ok(&out.into_vec())
        }
        op::DEVICE_NAMES => ok(&device_names(state, cfg)),
        op::ZEROS_1100 => ok(&[0; 110]),
        op::ZEROS_1102 => ok(&[0; 94]),
        op::TX_CHANNELS => tx_channels(&h, body, cfg),
        op::TX_NAMES => tx_names(&h, body, state),
        op::TX_RENAME => rename(&h, body, state, cfg, Dir::Tx),
        op::RX_CHANNELS => rx_channels(&h, body, state, cfg, live.rx),
        op::RX_RENAME => rename(&h, body, state, cfg, Dir::Rx),
        op::TX_FLOWS => tx_flows(&h, body, cfg, live.tx),
        op::RX_FLOWS => rx_flows(&h, body, cfg, live.rx),
        op::TX_MULTICAST_CREATE => no_multicast(&h, body, 2),
        op::TX_MULTICAST_DELETE => no_multicast(&h, body, 4),
        op::OP_2320 => ArcOutcome::Reply(reply(&h, STATUS_2320, &[])),
        op::RX_SUBSCRIBE => subscribe(&h, body, state, cfg),
        op::RX_UNSUBSCRIBE => unsubscribe(&h, body, state, cfg),
        op::CLOCK => ok(&CLOCK_REPLY),
        _ => unanswered(&h, "an opcode inferno does not answer"),
    }
}

/// [`handle`], and a change saved to the state file before it is
/// answered: a rename or a subscription that cannot be saved is undone and
/// refused, so a restart never loses a change the device confirmed.
pub fn respond(device: &mut Device, live: &Live<'_>, req: &[u8]) -> ArcOutcome {
    let before = device.state.clone();
    match handle(&mut device.state, &device.cfg, live, req) {
        ArcOutcome::Changed(bytes, change) => match device.state.save(&device.cfg.state_file) {
            Ok(()) => ArcOutcome::Changed(bytes, change),
            Err(e) => {
                error!(error = %e, "a change could not be saved: refused and undone");
                device.state = before;
                match Header::parse(req) {
                    Ok((h, _)) => ArcOutcome::Reply(reply(&h, STATUS_ERROR, &[])),
                    Err(_) => ArcOutcome::Ignored,
                }
            }
        },
        other => other,
    }
}

/// The ARC server: answers every datagram on `sock` from the shared device
/// and the running `flows`. After each saved change it signals `changed`
/// (the mDNS announcer waits on it), hands changed subscriptions to the
/// manager ([`Flows::subscribe`]) and adds the RX channels renamed, whose
/// subscription changed or whose status moved to `rx_changes` (the info
/// server's channel-change notification). Ends only with an error
/// receiving; a reply that cannot be sent is logged and the server goes
/// on. Refused and unanswered requests are counted ([`Gaps`]) and their
/// counts logged as one warning a minute, looked at every [`GAP_CHECK`];
/// every datagram is counted in `requests` (the control socket's status).
pub async fn serve<F: Flows>(
    sock: UdpSocket,
    shared: Arc<Mutex<Device>>,
    flows: Arc<F>,
    changed: Arc<Notify>,
    rx_changes: Arc<Changes>,
    requests: Arc<Requests>,
) -> io::Result<()> {
    let mut buf = vec![0u8; RECV_BUFFER];
    let mut gaps = Gaps::new(gaps::WINDOW);
    let mut check = tokio::time::interval(GAP_CHECK);
    check.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        let report = tokio::select! {
            got = sock.recv_from(&mut buf) => {
                let (n, peer) = got?;
                requests.received(PORT, peer.ip(), header_op(&buf[..n]));
                let (outcome, moved) = {
                    let mut device = shared.lock().unwrap_or_else(PoisonError::into_inner);
                    let outcome = flows.view(|live| respond(&mut device, live, &buf[..n]));
                    let moved = match &outcome {
                        ArcOutcome::Changed(_, c) if !c.subscriptions.is_empty() => {
                            flows.subscribe(&device.state.subscriptions)
                        }
                        _ => Vec::new(),
                    };
                    (outcome, moved)
                };
                let report = outcome
                    .gap()
                    .and_then(|(op1, gap)| gaps.record(Instant::now(), peer.ip(), op1, gap));
                let sent = match outcome {
                    ArcOutcome::Ignored
                    | ArcOutcome::Misframed { .. }
                    | ArcOutcome::Unanswered { .. } => None,
                    ArcOutcome::Reply(bytes) => Some((bytes, None)),
                    ArcOutcome::Changed(bytes, change) => Some((bytes, Some(change))),
                };
                if let Some((bytes, change)) = sent {
                    // The notification is queued before the reply goes, as
                    // inferno sends its own before answering a rename.
                    if let Some(change) = change {
                        rx_changes.add(
                            change
                                .renamed_rx
                                .into_iter()
                                .chain(change.subscriptions)
                                .chain(moved),
                        );
                        changed.notify_one();
                    }
                    if let Err(e) = sock.send_to(&bytes, peer).await {
                        warn!(error = %e, "an ARC reply could not be sent");
                    }
                }
                report
            }
            _ = check.tick() => gaps.flush(Instant::now()),
        };
        if let Some(report) = report {
            warn!(
                requests = report.requests(),
                counts = %report,
                "ARC requests refused or not answered in a minute (peer id, opcode)"
            );
        }
    }
}

fn refuse(h: &Header, why: &str) -> ArcOutcome {
    debug!(
        op = format_args!("{:#06x}", h.op1),
        seq = h.seq,
        why,
        "ARC request refused"
    );
    ArcOutcome::Reply(reply(h, STATUS_ERROR, &[]))
}

/// No reply, as inferno sends none.
fn unanswered(h: &Header, why: &str) -> ArcOutcome {
    debug!(
        op = format_args!("{:#06x}", h.op1),
        seq = h.seq,
        why,
        "ARC request not answered"
    );
    ArcOutcome::Unanswered { op1: h.op1 }
}

/// The first item a page request names (from 1), in its body's bytes 2–3
/// (inferno's `extract_start_index`); `None` without one.
fn first_item(req: &[u8]) -> Option<u16> {
    read_u16(req, 2).filter(|&first| first > 0)
}

/// `s` cut to at most [`MAX_REMOTE_NAME`] bytes, at a character's start.
fn capped(s: &str) -> &str {
    let mut end = s.len().min(MAX_REMOTE_NAME);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A flow socket descriptor: `0x8002`, the port, the IPv4 address
/// (inferno's `DestinationSocketDescriptor`).
fn socket_descriptor(out: &mut Body, at: std::net::SocketAddrV4) {
    out.u16(SOCKET_DESCRIPTOR);
    out.u16(at.port());
    out.bytes(&at.ip().octets());
}

/// 0x2200: the TX flows receivers asked for on 4455, expired ones too
/// (inferno lists a flow until a new destination frees its slot), by slot
/// (inferno's `query_tx_flows`). Each flow: its name `<flow>_<process>`, the
/// receiver's device name and flow name (cut to [`MAX_REMOTE_NAME`]), at a
/// multiple of 4 its socket descriptor (the receiver's address), the names
/// descriptor (`0x0a00, 1`, the receiver's names, `0x10`, ours, 8 zeros),
/// the flow descriptor (flow, `0x11` unicast, our rate, 0, our encoding,
/// 1, the slots, the socket descriptor), each slot's TX channel (0 for
/// none), the names descriptor's offset. The page's item is the flow
/// descriptor's offset.
fn tx_flows(h: &Header, req: &[u8], cfg: &DeviceConfig, tx: &TxTable) -> ArcOutcome {
    page(
        h,
        req,
        usize::from(FLOW_PAGE),
        2,
        0..usize::from(MAX_FLOWS),
        |slot, out| {
            let f = tx.flow(slot)?;
            let id = wire_u16(slot + 1);
            let local = out.cstr(&format!("{id}_{PROCESS_ID}"));
            let host = out.cstr(capped(&f.rx_host));
            let name = out.cstr(capped(&f.rx_flow_name));
            out.align(4);
            let socket = out.offset();
            socket_descriptor(out, f.dest);
            let names = out.offset();
            for field in [0x0a00, 1, host, name, 0x10, local] {
                out.u16(field);
            }
            out.zeros(8);
            let main = out.offset();
            out.u16(id);
            out.u16(FLOW_UNICAST);
            out.u32(cfg.sample_rate);
            out.u16(0);
            out.u16(u16::from(cfg.encoding));
            out.u16(1);
            out.u16(wire_u16(f.channels.len()));
            out.u16(socket);
            for channel in &f.channels {
                out.u16(channel.unwrap_or(0));
            }
            out.u16(names);
            Some(main.to_be_bytes().to_vec())
        },
    )
}

/// A flow latency of `latency_ns` as an RX flow list states it: in whole
/// samples at `rate`, then back in ns (inferno keeps it in samples), at
/// most `u32::MAX`.
fn stated_latency_ns(latency_ns: u64, rate: u32) -> u32 {
    let samples = crate::clock::to_samples(latency_ns, rate);
    let ns = u128::from(samples) * 1_000_000_000 / u128::from(rate.max(1));
    u32::try_from(ns).unwrap_or(u32::MAX)
}

/// 0x3200: our RX flows a transmitter granted, by slot (inferno lists a
/// flow from its grant; inferno's `query_rx_flows`). Each flow: at a
/// multiple of 4 its socket descriptor (our address and the slot's port),
/// descriptor 2 (`9, 1, 0x800, 0`, the latency in ns, 0), each channel
/// slot's bitmask of the RX channels it feeds (16 a word, as many words as
/// our RX channels need), at a multiple of 4 the flow descriptor (flow, 1,
/// our rate, 0, our encoding, 1, the slots, the words a bitmask, the socket
/// descriptor), the bitmasks' offsets, descriptor 2's offset. The page's
/// item is the flow descriptor's offset.
fn rx_flows(h: &Header, req: &[u8], cfg: &DeviceConfig, rx: &Manager) -> ArcOutcome {
    page(
        h,
        req,
        usize::from(FLOW_PAGE),
        2,
        0..usize::from(MAX_FLOWS),
        |slot, out| {
            let f = rx.flow(slot).filter(|f| f.handle.is_some())?;
            out.align(4);
            let socket = out.offset();
            socket_descriptor(out, std::net::SocketAddrV4::new(rx.ip(), f.port));
            let descriptor2 = out.offset();
            for field in [9, 1, 0x800, 0] {
                out.u16(field);
            }
            out.u32(stated_latency_ns(f.latency_ns, cfg.sample_rate));
            out.u32(0);
            // inferno: every slot's mask has a bit per RX channel, so as many
            // words as they need; a flow without slots has one.
            let words = if f.rx.is_empty() {
                1
            } else {
                cfg.rx.len().div_ceil(16).max(1)
            };
            let masks: Vec<u16> =
                f.rx.iter()
                    .map(|fed| {
                        let at = out.offset();
                        for word in bitmask(fed, words) {
                            out.u16(word);
                        }
                        at
                    })
                    .collect();
            out.align(4);
            let main = out.offset();
            out.u16(wire_u16(slot + 1));
            out.u16(1);
            out.u32(cfg.sample_rate);
            out.u16(0);
            out.u16(u16::from(cfg.encoding));
            out.u16(1);
            out.u16(wire_u16(f.rx.len()));
            out.u16(wire_u16(words));
            out.u16(socket);
            for mask in masks {
                out.u16(mask);
            }
            out.u16(descriptor2);
            Some(main.to_be_bytes().to_vec())
        },
    )
}

/// The RX channels `channels` (from 0) as `words` 16-bit words: channel
/// `c` is bit `c mod 16` of word `c / 16` (inferno's bit vector: a channel
/// named twice is one bit); channels past the words are left out.
fn bitmask(channels: &[usize], words: usize) -> Vec<u16> {
    let mut out = vec![0u16; words];
    for &c in channels {
        if let Some(word) = out.get_mut(c / 16) {
            *word |= 1 << (c % 16);
        }
    }
    out
}

/// 0x2201 and 0x2202: the device makes no multicast flow, so it creates or
/// deletes none and refuses, as inferno does when it creates or deletes
/// none. A body shorter than `readable` bytes inferno fails to read before
/// it answers: no reply.
fn no_multicast(h: &Header, req: &[u8], readable: usize) -> ArcOutcome {
    if req.len() < readable {
        return unanswered(h, "a multicast flow request too short to read");
    }
    refuse(h, "no multicast flows")
}

/// 0x3010's items (6 bytes each: local channel, TX channel name offset,
/// TX device name offset), read as inferno's `deserialize_items` reads
/// them: `min(count, len / 6)` items after the first two bytes. `None`
/// where inferno fails: under 2 bytes, or those items running past the end.
fn subscription_items(req: &[u8]) -> Option<Vec<[u16; 3]>> {
    const ITEM: usize = 6;
    let count = usize::from(*req.get(1)?).min(req.len() / ITEM);
    let items = req.get(2..)?.get(..count * ITEM)?;
    let (items, _) = items.as_chunks::<ITEM>();
    Some(
        items
            .iter()
            .map(|i| {
                [
                    u16::from_be_bytes([i[0], i[1]]),
                    u16::from_be_bytes([i[2], i[3]]),
                    u16::from_be_bytes([i[4], i[5]]),
                ]
            })
            .collect(),
    )
}

/// Sets RX channel `rx`'s subscription to `sub` in `state`; whether it
/// changed.
fn set_subscription(
    state: &mut DeviceState,
    cfg: &DeviceConfig,
    rx: usize,
    sub: Option<Subscription>,
) -> bool {
    state.subscription(rx) != sub.as_ref() && state.set_subscription(cfg, rx, sub)
}

/// OK, no body; [`ArcOutcome::Changed`] when `changed` names a channel.
fn subscribed(h: &Header, changed: Vec<usize>) -> ArcOutcome {
    let bytes = reply(h, STATUS_OK, &[]);
    if changed.is_empty() {
        return ArcOutcome::Reply(bytes);
    }
    ArcOutcome::Changed(
        bytes,
        Change {
            subscriptions: changed,
            ..Change::default()
        },
    )
}

/// 0x3010 (inferno's `set_channels_subscriptions`): each item subscribes
/// its RX channel to the TX channel and device its two names say, or, when
/// either name's offset is 0, unsubscribes it. Channel 0 and channels we do
/// not have are passed over; names inferno cannot read (an offset inside
/// the header or past the datagram, bytes that are not UTF-8) change
/// nothing, and so do names Dante's rules refuse (inferno subscribes to
/// them; our state file cannot keep them). OK, no body, to every request
/// inferno can read (also when nothing changed); a cut one gets no reply.
fn subscribe(h: &Header, req: &[u8], state: &mut DeviceState, cfg: &DeviceConfig) -> ArcOutcome {
    let Some(items) = subscription_items(req) else {
        return unanswered(h, "a subscription request whose items are cut");
    };
    let mut changed = Vec::new();
    for [id, channel_at, device_at] in items {
        // Channel 0 is passed over here, a channel we do not have where the
        // state refuses it.
        let Some(rx) = usize::from(id).checked_sub(1) else {
            continue;
        };
        let sub = if channel_at > 0 && device_at > 0 {
            let (Some(tx_channel), Some(tx_device)) =
                (string_at(req, channel_at), string_at(req, device_at))
            else {
                debug!(channel = id, "subscription whose names cannot be read");
                continue;
            };
            let sub = Subscription {
                tx_channel: tx_channel.to_string(),
                tx_device: tx_device.to_string(),
            };
            if !sub.is_valid() {
                info!(
                    channel = id,
                    "subscription to a name Dante's rules refuse: not made"
                );
                continue;
            }
            Some(sub)
        } else {
            None
        };
        let made = sub.is_some();
        if set_subscription(state, cfg, rx, sub) && !changed.contains(&rx) {
            info!(
                seq = h.seq,
                channel = id,
                subscribed = made,
                "RX subscription changed"
            );
            changed.push(rx);
        }
    }
    subscribed(h, changed)
}

/// 0x3014, netaudio's unsubscribe (`0001 0000 <local channel>`): the
/// channel unsubscribed; OK, no body, also for channel 0, a channel we do
/// not have or one not subscribed. Under 6 bytes inferno fails to read it:
/// no reply.
fn unsubscribe(h: &Header, req: &[u8], state: &mut DeviceState, cfg: &DeviceConfig) -> ArcOutcome {
    let Some(channel) = read_u16(req, 4) else {
        return unanswered(h, "an unsubscribe request without its channel");
    };
    // A channel we do not have the state refuses.
    let changed = usize::from(channel)
        .checked_sub(1)
        .filter(|&rx| set_subscription(state, cfg, rx, None));
    if changed.is_some() {
        info!(seq = h.seq, channel, "RX unsubscribed");
    }
    subscribed(h, changed.into_iter().collect())
}

/// A count or a channel number as the wire's u16 (the config allows far
/// fewer channels than that).
fn wire_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// 0x1000: inferno's layout.
fn counts(cfg: &DeviceConfig) -> Vec<u8> {
    let tx = wire_u16(cfg.tx.len());
    let rx = wire_u16(cfg.rx.len());
    let mut out = Body::new();
    out.u8(0);
    out.u8(FLAG_TX_RENAME);
    out.u16(tx);
    out.u16(rx);
    out.u16(4);
    out.u16(MAX_CHANNELS_IN_FLOW.min(tx));
    out.u16(8);
    // The most TX and RX flows.
    out.u16(MAX_FLOWS);
    out.u16(MAX_FLOWS);
    out.u16(tx.saturating_add(rx));
    out.u16(1);
    out.u16(1);
    out.zeros(12);
    out.into_vec()
}

/// 0x1003: a 38-byte header of offsets and constants, then the current
/// name, the factory name, the board name and the revision.
fn device_names(state: &DeviceState, cfg: &DeviceConfig) -> Vec<u8> {
    let mut out = Body::new();
    out.zeros(NAMES_HEADER);
    let name = out.cstr(&state.name);
    let factory = out.cstr(&cfg.name);
    let board = out.cstr(&cfg.model);
    let revision = out.cstr(":705");
    #[rustfmt::skip]
    let fields: [u16; NAMES_HEADER / 2] = [
        0, 0, 0, board, revision, 0, name, factory, name,
        0, 0, 0, 0, 0, 0,
        0x2729, 0, 0x1102, 0,
    ];
    for (i, value) in fields.into_iter().enumerate() {
        out.set_u16(wire_u16(HEADER_LEN + 2 * i), value);
    }
    out.into_vec()
}

/// The descriptor every channel of a page points at: rate, encoding, PCM
/// type.
fn common_descriptor(out: &mut Body, cfg: &DeviceConfig) -> u16 {
    let at = out.offset();
    let bits = u16::from(cfg.encoding);
    out.u32(cfg.sample_rate);
    out.u8(1);
    out.u8(1);
    out.u16(bits);
    out.u16(0x400);
    out.u16(bits);
    out.u16(bits);
    out.u16(u16::from(PCM_TYPE));
    debug_assert_eq!(usize::from(out.offset() - at), COMMON_DESCRIPTOR);
    at
}

/// One page of a list (inferno's `paginate_make_response` and
/// `serialize_items`): `[slots, count]`, `slots` descriptors of `item`
/// bytes (the unused ones zero), then what they point at. The request names
/// the page's first item (from 1) in its body's bytes 2–3; the page lists
/// `items` from there. `write` writes an item's strings and descriptors
/// (the page's first may write a shared one) and returns its `item`-byte
/// descriptor, or `None` for an item not listed (an empty flow slot: passed
/// over, not counted). The page says more follow (`0x8112`) when an item is
/// left after its slots are full, or when an item has brought the reply to
/// [`SOFT_LIMIT`] bytes: that item's descriptor stays in its slot, not
/// counted, as inferno leaves it.
fn page<I>(
    h: &Header,
    req: &[u8],
    slots: usize,
    item: usize,
    items: impl IntoIterator<Item = I>,
    mut write: impl FnMut(I, &mut Body) -> Option<Vec<u8>>,
) -> ArcOutcome {
    let Some(first) = first_item(req) else {
        return refuse(h, "a page request without its first item");
    };
    let mut out = Body::new();
    out.u8(slots as u8);
    out.u8(0);
    let table = usize::from(out.offset());
    out.zeros(slots * item);
    let mut count = 0;
    let mut more = false;
    for it in items.into_iter().skip(usize::from(first) - 1) {
        if count >= slots {
            more = true;
            break;
        }
        let Some(descriptor) = write(it, &mut out) else {
            continue;
        };
        debug_assert_eq!(descriptor.len(), item);
        out.set_bytes(wire_u16(table + count * item), &descriptor);
        if usize::from(out.offset()) >= SOFT_LIMIT {
            more = true;
            break;
        }
        count += 1;
    }
    out.set_u8(wire_u16(HEADER_LEN + 1), count as u8);
    let status = if more { STATUS_MORE } else { STATUS_OK };
    ArcOutcome::Reply(reply(h, status, &out.into_vec()))
}

/// 0x2000: each TX channel with its default name.
fn tx_channels(h: &Header, req: &[u8], cfg: &DeviceConfig) -> ArcOutcome {
    let mut common = None;
    let n = cfg.tx.len();
    page(h, req, n.min(TX_PAGE), 8, 0..n, |index, out| {
        let common = *common.get_or_insert_with(|| common_descriptor(out, cfg));
        let name = out.cstr(&cfg.tx[index]);
        Some(descriptor(&[wire_u16(index + 1), 7, common, name]))
    })
}

/// 0x2010: each TX channel with its current name.
fn tx_names(h: &Header, req: &[u8], state: &DeviceState) -> ArcOutcome {
    let mut opened = false;
    let n = state.tx.len();
    page(h, req, n.min(TX_PAGE), 6, 0..n, |index, out| {
        if !opened {
            out.u32(0);
            opened = true;
        }
        let id = wire_u16(index + 1);
        let name = out.cstr(&state.tx[index]);
        Some(descriptor(&[id, id, name]))
    })
}

/// RX channel `rx`'s subscription as 0x3000 lists it, with its status
/// code: the manager's status when it holds the same subscription, else
/// Unresolved (a subscription saved a moment ago, not handed over yet).
fn listed_subscription<'s>(
    state: &'s DeviceState,
    rx: usize,
    manager: &Manager,
) -> Option<(&'s Subscription, u32)> {
    let sub = state.subscription(rx)?;
    let status = match manager.subscription(rx) {
        Some(known) if known == sub => manager.status(rx).unwrap_or(SubStatus::Unresolved),
        _ => SubStatus::Unresolved,
    };
    Some((sub, status.code()))
}

/// 0x3000 (inferno's `get_receive_channels`): each RX channel with, when
/// subscribed, the TX channel's and the TX device's names and the status
/// code, then its own name.
fn rx_channels(
    h: &Header,
    req: &[u8],
    state: &DeviceState,
    cfg: &DeviceConfig,
    manager: &Manager,
) -> ArcOutcome {
    let mut common = None;
    let n = state.rx.len();
    page(h, req, n.min(RX_PAGE), 20, 0..n, |index, out| {
        let common = *common.get_or_insert_with(|| common_descriptor(out, cfg));
        let (tx_channel, tx_device, status) = match listed_subscription(state, index, manager) {
            Some((sub, status)) => (out.cstr(&sub.tx_channel), out.cstr(&sub.tx_device), status),
            None => (0, 0, 0),
        };
        let name = out.cstr(&state.rx[index]);
        let [s0, s1] = [(status >> 16) as u16, status as u16];
        Some(descriptor(&[
            wire_u16(index + 1),
            6,
            common,
            tx_channel,
            tx_device,
            name,
            s0,
            s1,
            0,
            0,
        ]))
    })
}

fn descriptor(fields: &[u16]) -> Vec<u8> {
    fields.iter().flat_map(|f| f.to_be_bytes()).collect()
}

/// 0x2013 and 0x3001: `[?, count]`, then `count` items (TX: `0, channel,
/// name offset`; RX: `channel, name offset`), the names behind them. A
/// refusal's reason is a debug line only: the refusal is a gap the server's
/// once-a-minute warning counts, and Dante Controller asks again at once,
/// so a warning per request would be a flood.
fn rename(
    h: &Header,
    req: &[u8],
    state: &mut DeviceState,
    cfg: &DeviceConfig,
    dir: Dir,
) -> ArcOutcome {
    let (item, names, defaults) = match dir {
        Dir::Tx => (6, &mut state.tx, &cfg.tx),
        Dir::Rx => (4, &mut state.rx, &cfg.rx),
    };
    let label = dir.label();
    let Some(count) = req.get(1).map(|&c| usize::from(c)).filter(|&c| c > 0) else {
        return refuse(h, "a rename without a channel");
    };
    let mut proposed = names.clone();
    let mut renamed = Vec::with_capacity(count);
    for k in 0..count {
        let at = 2 + k * item + if dir == Dir::Tx { 2 } else { 0 };
        let (Some(id), Some(offset)) = (read_u16(req, at), read_u16(req, at + 2)) else {
            return refuse(h, "a rename whose items are cut short");
        };
        let Some(index) = usize::from(id)
            .checked_sub(1)
            .filter(|&i| i < proposed.len())
        else {
            debug!(dir = label, channel = id, "rename refused: no such channel");
            return refuse(h, "no such channel");
        };
        let Some(name) = string_at(req, offset) else {
            debug!(
                dir = label,
                channel = id,
                "rename refused: no name at its offset"
            );
            return refuse(h, "no name at its offset");
        };
        let name = if name.is_empty() {
            defaults[index].as_str()
        } else {
            name
        };
        if !valid_channel_name(name) {
            debug!(
                dir = label,
                channel = id,
                "rename refused: not a valid Dante channel name"
            );
            return refuse(h, "not a valid Dante channel name");
        }
        proposed[index] = name.to_string();
        renamed.push(id);
    }
    let problems = channel_problems(
        label,
        &proposed,
        (dir == Dir::Tx).then_some(defaults.as_slice()),
    );
    if !problems.is_empty() {
        debug!(dir = label, problems = %problems.join("; "), "rename refused");
        return refuse(h, "the names would clash");
    }
    let ok: &[u8] = match dir {
        Dir::Tx => &[0, 0],
        Dir::Rx => &[],
    };
    if proposed == *names {
        return ArcOutcome::Reply(reply(h, STATUS_OK, ok));
    }
    // The channel-change notification names the RX channels renamed (inferno
    // also sends one, empty, for a rename that renamed nothing).
    let renamed_rx = match dir {
        Dir::Rx => (0..names.len())
            .filter(|&i| names[i] != proposed[i])
            .collect(),
        Dir::Tx => Vec::new(),
    };
    *names = proposed;
    info!(dir = label, channels = ?renamed, "channels renamed");
    ArcOutcome::Changed(
        reply(h, STATUS_OK, ok),
        Change {
            renamed_rx,
            ..Change::default()
        },
    )
}

#[cfg(test)]
mod tests;
