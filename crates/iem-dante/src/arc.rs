// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from inferno (https://github.com/teodly/inferno at 9767558):
// inferno_aoip/src/protocol/proto_arc.rs (the opcodes, the body layouts and
// the paging) and inferno_aoip/src/device_server/arc_server.rs (the
// answers), Copyright (C) Teo and the inferno contributors,
// GPL-3.0-or-later OR AGPL-3.0-or-later, taken under GPL-3.0-or-later.
// Changes Copyright (C) 2026 iemmixer contributors.

//! ARC, Dante's control protocol on UDP 4440, for phase A: the device's
//! names, its channel counts and lists, and channel renames. Every opcode
//! inferno's server answers is answered as inferno answers it with no flows
//! and no subscriptions (a request Dante Controller gets refused it asks
//! again at once: #72, 2026-10-26, 0x2200 and 0x2204 about 2 250 times a
//! second while both were refused).
//!
//! | op1 | answer |
//! |---|---|
//! | 0x1000 | counts: TX, RX, flows (none in phase A) |
//! | 0x1002 | the device name |
//! | 0x1003 | the device names: current, factory, board, revision |
//! | 0x1100, 0x1102 | zeros, as inferno answers Dante Controller |
//! | 0x2000 | TX channels with their default names, paged |
//! | 0x2010 | TX channels' current names, paged |
//! | 0x2013 | TX rename |
//! | 0x2200 | TX flows: an empty page of [`FLOW_PAGE`] slots, status OK |
//! | 0x2201, 0x2202 | create, delete a multicast TX flow: nothing to create or delete, refused (inferno's answer without flows) |
//! | 0x2320 | status [`STATUS_2320`], no body (inferno's "???") |
//! | 0x3000 | RX channels with their names, paged; no subscriptions |
//! | 0x3001 | RX rename |
//! | 0x3010 | subscribe or unsubscribe RX channels: OK, no body; phase A subscribes nothing |
//! | 0x3014 | netaudio's unsubscribe: OK, no body |
//! | 0x3200 | RX flows: an empty page of [`FLOW_PAGE`] slots, status OK |
//! | 0x3300 | the fixed 8 bytes that keep Dante Controller from a clock-domain warning |
//!
//! No reply, as from inferno, to any other opcode (0x2204 and 0x1001
//! among them), to a request whose op2 is not 0, and to a body inferno
//! fails to read before it answers (0x2201 under 2 bytes, 0x2202 under 4,
//! 0x3010 whose items are cut, 0x3014 under 6): [`ArcOutcome::Unanswered`].
//! A datagram that is no request ([`Header::parse`]) is dropped. Refused
//! ([`STATUS_ERROR`]), changing nothing: a rename that renames nothing, a
//! page request without its first item, 0x2201 and 0x2202. A rename is all
//! or nothing: one invalid name refuses the whole request (inferno renames
//! the valid ones). An empty name is Dante's reset to the channel's default
//! name (netaudio's `reset_channel_name` sends one). Every refused or
//! unanswered request is counted per peer and opcode ([`gaps`]); the server
//! logs the counts as one warning a minute.
//!
//! Pages hold 32 TX or 16 RX channels, as real devices answer (netaudio
//! steps through pages by those counts, Dante Controller by the count each
//! page reports); with names of 31 bytes a page still fits one datagram
//! ([`MAX_DATAGRAM`](crate::wire::MAX_DATAGRAM)). Logs name opcodes,
//! sequences and channel numbers, never a name.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::time::MissedTickBehavior;
use tracing::{debug, error, info, warn};

use crate::device::{Device, DeviceConfig, DeviceState, channel_problems, valid_channel_name};
use crate::wire::{
    Body, HEADER_LEN, Header, STATUS_ERROR, STATUS_MORE, STATUS_OK, read_u16, reply, string_at,
};
use gaps::{Gap, Gaps};

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
/// inferno's status for 0x2320 ("???" in its source).
pub const STATUS_2320: u16 = 0x0030;

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
/// The counts' flags byte: bit 4, TX channels can be renamed (bit 5, TX
/// multicast, stays clear: no flows in phase A).
const FLAG_TX_RENAME: u8 = 0x10;
/// The device names' header (0x1003), before its strings.
const NAMES_HEADER: usize = 38;
/// The 16-byte descriptor of a page's channels.
const COMMON_DESCRIPTOR: usize = 16;
/// A datagram read from the socket; anything longer is cut and so fails
/// [`Header::parse`].
const RECV_BUFFER: usize = 2048;

/// What a request comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArcOutcome {
    /// Send this reply; nothing changed.
    Reply(Vec<u8>),
    /// Send this reply; the state changed, so the caller saves it and
    /// announces the new mDNS records.
    Changed(Vec<u8>),
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
    /// left unanswered or refused ([`STATUS_ERROR`]); `None` for any other
    /// answer and for a datagram that is no request.
    pub fn gap(&self) -> Option<(u16, Gap)> {
        match self {
            ArcOutcome::Unanswered { op1 } => Some((*op1, Gap::Unanswered)),
            ArcOutcome::Reply(bytes) => match Header::parse(bytes) {
                Ok((h, _)) if h.op2 == STATUS_ERROR => Some((h.op1, Gap::Refused)),
                _ => None,
            },
            ArcOutcome::Changed(_) | ArcOutcome::Misframed { .. } | ArcOutcome::Ignored => None,
        }
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

/// Answers one datagram from `state` and `cfg`. Only a valid rename
/// changes `state` (and is [`ArcOutcome::Changed`]); this function saves
/// nothing ([`respond`] does).
pub fn handle(state: &mut DeviceState, cfg: &DeviceConfig, req: &[u8]) -> ArcOutcome {
    let (h, body) = match Header::parse(req) {
        Ok(parsed) => parsed,
        Err(e) => {
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
        op::RX_CHANNELS => rx_channels(&h, body, state, cfg),
        op::RX_RENAME => rename(&h, body, state, cfg, Dir::Rx),
        op::TX_FLOWS | op::RX_FLOWS => no_flows(&h, body),
        op::TX_MULTICAST_CREATE => no_multicast(&h, body, 2),
        op::TX_MULTICAST_DELETE => no_multicast(&h, body, 4),
        op::OP_2320 => ArcOutcome::Reply(reply(&h, STATUS_2320, &[])),
        op::RX_SUBSCRIBE => subscribe(&h, body),
        op::RX_UNSUBSCRIBE => unsubscribe(&h, body),
        op::CLOCK => ok(&CLOCK_REPLY),
        _ => unanswered(&h, "an opcode inferno does not answer"),
    }
}

/// [`handle`], and a change saved to the state file before it is
/// answered: a rename that cannot be saved is undone and refused, so a
/// restart never loses a rename the device confirmed.
pub fn respond(device: &mut Device, req: &[u8]) -> ArcOutcome {
    let before = device.state.clone();
    match handle(&mut device.state, &device.cfg, req) {
        ArcOutcome::Changed(bytes) => match device.state.save(&device.cfg.state_file) {
            Ok(()) => ArcOutcome::Changed(bytes),
            Err(e) => {
                error!(error = %e, "a rename could not be saved: refused and undone");
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
/// and signals `changed` after each saved change (the mDNS announcer waits
/// on it). Ends only with an error receiving; a reply that cannot be sent
/// is logged and the server goes on. Refused and unanswered requests are
/// counted ([`Gaps`]) and their counts logged as one warning a minute,
/// looked at every [`GAP_CHECK`].
pub async fn serve(
    sock: UdpSocket,
    shared: Arc<Mutex<Device>>,
    changed: Arc<Notify>,
) -> io::Result<()> {
    let mut buf = vec![0u8; RECV_BUFFER];
    let mut gaps = Gaps::new(gaps::WINDOW);
    let mut check = tokio::time::interval(GAP_CHECK);
    check.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        let report = tokio::select! {
            got = sock.recv_from(&mut buf) => {
                let (n, peer) = got?;
                let outcome = {
                    let mut device = shared.lock().unwrap_or_else(PoisonError::into_inner);
                    respond(&mut device, &buf[..n])
                };
                let report = outcome
                    .gap()
                    .and_then(|(op1, gap)| gaps.record(Instant::now(), peer.ip(), op1, gap));
                let sent = match outcome {
                    ArcOutcome::Ignored
                    | ArcOutcome::Misframed { .. }
                    | ArcOutcome::Unanswered { .. } => None,
                    ArcOutcome::Reply(bytes) => Some((bytes, false)),
                    ArcOutcome::Changed(bytes) => Some((bytes, true)),
                };
                if let Some((bytes, is_change)) = sent {
                    if let Err(e) = sock.send_to(&bytes, peer).await {
                        warn!(error = %e, "an ARC reply could not be sent");
                    }
                    if is_change {
                        changed.notify_one();
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

/// 0x2200 and 0x3200 with no flows: inferno's page of [`FLOW_PAGE`] empty
/// two-byte slots, none used, status OK, for any first flow; refused
/// without one, as a channel list.
fn no_flows(h: &Header, req: &[u8]) -> ArcOutcome {
    if first_item(req).is_none() {
        return refuse(h, "a flow list request without its first flow");
    }
    let mut out = Body::new();
    out.u8(FLOW_PAGE);
    out.u8(0);
    out.zeros(2 * usize::from(FLOW_PAGE));
    ArcOutcome::Reply(reply(h, STATUS_OK, &out.into_vec()))
}

/// 0x2201 and 0x2202 with no flows: inferno creates or deletes nothing and
/// refuses. A body shorter than `readable` bytes inferno fails to read
/// before it answers: no reply.
fn no_multicast(h: &Header, req: &[u8], readable: usize) -> ArcOutcome {
    if req.len() < readable {
        return unanswered(h, "a multicast flow request too short to read");
    }
    refuse(h, "no multicast flows in phase A")
}

/// The local channels of 0x3010's items (6 bytes each: local channel, TX
/// channel name offset, TX device name offset), read as inferno's
/// `deserialize_items` reads them: `min(count, len / 6)` items after the
/// first two bytes. `None` where inferno fails: under 2 bytes, or those
/// items running past the end.
fn subscription_channels(req: &[u8]) -> Option<Vec<u16>> {
    const ITEM: usize = 6;
    let count = usize::from(*req.get(1)?).min(req.len() / ITEM);
    let items = req.get(2..)?.get(..count * ITEM)?;
    let (items, _) = items.as_chunks::<ITEM>();
    Some(
        items
            .iter()
            .map(|item| u16::from_be_bytes([item[0], item[1]]))
            .collect(),
    )
}

/// 0x3010: inferno answers OK, no body, to every request it can read, also
/// for a channel it passes over. Phase A has no subscriptions: nothing
/// changes, the RX list goes on showing none.
fn subscribe(h: &Header, req: &[u8]) -> ArcOutcome {
    let Some(channels) = subscription_channels(req) else {
        return unanswered(h, "a subscription request whose items are cut");
    };
    info!(
        seq = h.seq,
        channels = ?channels,
        "RX subscription request: phase A subscribes nothing"
    );
    ArcOutcome::Reply(reply(h, STATUS_OK, &[]))
}

/// 0x3014, netaudio's unsubscribe (`0001 0000 <local channel>`): OK, no
/// body; nothing is subscribed. Under 6 bytes inferno fails to read it: no
/// reply.
fn unsubscribe(h: &Header, req: &[u8]) -> ArcOutcome {
    let Some(channel) = read_u16(req, 4) else {
        return unanswered(h, "an unsubscribe request without its channel");
    };
    info!(
        seq = h.seq,
        channel, "RX unsubscribe request: nothing is subscribed in phase A"
    );
    ArcOutcome::Reply(reply(h, STATUS_OK, &[]))
}

/// A count or a channel number as the wire's u16 (the config allows far
/// fewer channels than that).
fn wire_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// 0x1000: inferno's layout, with no flows.
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
    // The most TX and RX flows: none in phase A.
    out.u16(0);
    out.u16(0);
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

/// One page of a channel list (inferno's `serialize_items`): `[slots,
/// count]`, `slots` descriptors of `item` bytes (the unused ones zero),
/// then what they point at. The request names the page's first channel
/// (from 1) in its body's bytes 2–3. `write` writes channel `index`'s
/// strings (and, for the page's first channel, any shared descriptor) and
/// returns its descriptor.
fn page(
    h: &Header,
    req: &[u8],
    total: usize,
    per_page: usize,
    item: usize,
    mut write: impl FnMut(usize, &mut Body) -> Vec<u8>,
) -> ArcOutcome {
    let Some(first) = first_item(req) else {
        return refuse(h, "a page request without its first channel");
    };
    let start = usize::from(first) - 1;
    let end = total.min(start.saturating_add(per_page));
    let slots = total.min(per_page);
    let mut out = Body::new();
    out.u8(slots as u8);
    out.u8(end.saturating_sub(start) as u8);
    let table = out.offset();
    out.zeros(slots * item);
    for (k, index) in (start..end).enumerate() {
        let descriptor = write(index, &mut out);
        debug_assert_eq!(descriptor.len(), item);
        out.set_bytes(wire_u16(usize::from(table) + k * item), &descriptor);
    }
    let status = if end < total { STATUS_MORE } else { STATUS_OK };
    ArcOutcome::Reply(reply(h, status, &out.into_vec()))
}

/// 0x2000: each TX channel with its default name.
fn tx_channels(h: &Header, req: &[u8], cfg: &DeviceConfig) -> ArcOutcome {
    let mut common = None;
    page(h, req, cfg.tx.len(), TX_PAGE, 8, |index, out| {
        let common = *common.get_or_insert_with(|| common_descriptor(out, cfg));
        let name = out.cstr(&cfg.tx[index]);
        descriptor(&[wire_u16(index + 1), 7, common, name])
    })
}

/// 0x2010: each TX channel with its current name.
fn tx_names(h: &Header, req: &[u8], state: &DeviceState) -> ArcOutcome {
    let mut opened = false;
    page(h, req, state.tx.len(), TX_PAGE, 6, |index, out| {
        if !opened {
            out.u32(0);
            opened = true;
        }
        let id = wire_u16(index + 1);
        let name = out.cstr(&state.tx[index]);
        descriptor(&[id, id, name])
    })
}

/// 0x3000: each RX channel with its name, unsubscribed.
fn rx_channels(h: &Header, req: &[u8], state: &DeviceState, cfg: &DeviceConfig) -> ArcOutcome {
    let mut common = None;
    page(h, req, state.rx.len(), RX_PAGE, 20, |index, out| {
        let common = *common.get_or_insert_with(|| common_descriptor(out, cfg));
        let name = out.cstr(&state.rx[index]);
        // No TX channel, no TX device, subscription status 0, then 0.
        descriptor(&[wire_u16(index + 1), 6, common, 0, 0, name, 0, 0, 0, 0])
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
    *names = proposed;
    info!(dir = label, channels = ?renamed, "channels renamed");
    ArcOutcome::Changed(reply(h, STATUS_OK, ok))
}

#[cfg(test)]
mod tests;
