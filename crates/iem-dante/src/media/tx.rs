// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from inferno (https://github.com/teodly/inferno at 9767558):
// inferno_aoip/src/device_server/flows_tx.rs (the TX flow table: 32 flows, a
// slot from a running sequence and a random cookie, a request for a
// destination already served sets its channels, an update revives an expired
// flow, 4 s without a datagram on the flow expires it, expired flows freed
// when a new destination needs a slot; the schedule's bootstrap on a multiple
// of fpp, its lag and jump checks; the keepalives read every 33 ms),
// Copyright (C) Teo and the inferno contributors, GPL-3.0-or-later OR
// AGPL-3.0-or-later, taken under GPL-3.0-or-later. Changes Copyright (C)
// 2026 iemmixer contributors.

//! Our TX flows (Gen 3 phase B, plan Task 4): the flows receivers asked
//! for on UDP 4455 ([`crate::flows::server`]) and the thread that sends
//! them.
//!
//! - [`TxTable`] (the control side: the 4455 server, later the ARC lists):
//!   at most [`MAX_FLOWS`] flows, each to the one receiver that asked for
//!   it, of at most [`MAX_CHANNELS`] slots. A flow without a datagram from
//!   its receiver for [`KEEPALIVE_TIMEOUT_NS`] expires: it sends nothing
//!   until an update or a new request for the same destination revives it,
//!   and its slot is freed when a new destination needs one (inferno).
//! - [`TxShared`]: what the control side and the TX thread share. The
//!   table sits behind a mutex only the control side takes; the TX thread
//!   reads each slot's [`Plan`] and each TX channel's [`Source`] from
//!   seqlocked atomics (never blocking: a read that meets a write keeps the
//!   last copy), and counts per slot what it sent and heard in atomics,
//!   and per TX channel the largest magnitude sent (the heartbeat's peaks,
//!   [`TxShared::take_peak`]).
//!   One UDP socket per slot, bound to the device's address: the flow's
//!   audio goes from it to the receiver, and the receiver's keepalives come
//!   back to it (only the receiver's own address counts, as a connected
//!   socket would see it).
//! - [`Schedule`]: a flow's packets in time. The packet stamped T carries
//!   the source's samples T…T+fpp−1 and goes out once the clock has passed
//!   T + fpp by [`TX_MARGIN_NS`]; the first stamp is a multiple of fpp, a
//!   lag past the device's latency or a jump of [`DISCONTINUITY`] samples
//!   starts it again (counted), and so does a re-anchor of the clock. A
//!   flow sends exactly rate / fpp packets a second and never more.
//! - [`Transmitter`] and [`spawn`]: the TX thread. Nothing leaves before the
//!   clock is ready. Every [`EVENTS_EVERY_NS`] it reads the plans, the
//!   sources and the keepalives; in between it sends what is due and
//!   sleeps until the next packet. Phase B sends the test sources of
//!   [`super::source`] (every channel silent at start); phase C adds
//!   [`Source::Engine`]: a TX channel the engine feeds has an engine ring
//!   ([`TxShared::engine_ring`]) that the engine's pump
//!   ([`crate::stream`]) writes and the TX thread reads at each packet's
//!   stamp, a hole counted in the slot's `holes` like the echo's.
//!
//! The hot path (a packet's samples, the packet, the send) allocates
//! nothing, takes no lock and logs nothing; its one system call is the send
//! (`tests/hot_path.rs`). Errors are counted per slot ([`LaneCounts`]).

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Instant;

use tracing::{info, warn};

use super::packet::{self, MEDIA_HEADER};
use super::ring::Ring;
use super::source::Source;
use crate::clock::{self, PtpClock};
use crate::device::DeviceConfig;
use crate::flows::control::{self, FlowRequest, Handle, status};
use crate::wire::MAX_DATAGRAM;

/// The most TX flows at once (inferno's `MAX_FLOWS`).
pub const MAX_FLOWS: usize = 32;
/// The most channel slots of one flow (inferno's `MAX_CHANNELS_IN_FLOW`;
/// the mDNS `nchan` and ARC say so).
pub const MAX_CHANNELS: usize = crate::arc::MAX_CHANNELS_IN_FLOW as usize;
/// The most frames per packet (inferno's `FPP_MAX`).
pub const FPP_MAX: u16 = 256;
/// A flow with no datagram from its receiver for this long expires
/// (inferno's 4 s).
pub const KEEPALIVE_TIMEOUT_NS: u64 = 4_000_000_000;
/// How long after a packet's last sample it is sent at the earliest: our
/// clock may run up to a few tens of µs ahead of the grandmaster's.
pub const TX_MARGIN_NS: u64 = 250_000;
/// A schedule this many samples ahead of the clock starts again (inferno's
/// "media clock jumped").
pub const DISCONTINUITY: u64 = 192_000;
/// How often the TX thread reads the plans, the sources and the keepalives
/// (inferno's 33 ms).
pub const EVENTS_EVERY_NS: u64 = 33_000_000;
/// The most datagrams read from one flow's socket a time (a flood never
/// holds the thread).
pub const KEEPALIVE_DRAIN: usize = 64;
/// The TX thread's `SCHED_FIFO` priority when `RLIMIT_RTPRIO` allows it:
/// below dantesync's 50, so the box's clock is never delayed by our sends.
pub const TX_PRIORITY: i32 = 45;
/// The tries of a seqlocked read before it keeps the last copy.
const READ_TRIES: usize = 16;

/// Whether a flow of `nch` slots, `bps` bytes a sample and `fpp` frames a
/// packet can be sent: at most [`MAX_CHANNELS`] slots, 2, 3 or 4 bytes, 1
/// to [`FPP_MAX`] frames, and a packet that fits one datagram
/// ([`MAX_DATAGRAM`], so it is never fragmented).
pub fn sendable(nch: usize, bps: usize, fpp: u16) -> bool {
    nch <= MAX_CHANNELS
        && (2..=4).contains(&bps)
        && (1..=FPP_MAX).contains(&fpp)
        && MEDIA_HEADER + nch * bps * usize::from(fpp) <= MAX_DATAGRAM
}

/// One TX flow, as the control side keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxFlow {
    /// The receiver: where the audio goes, and the only source of
    /// keepalives.
    pub dest: SocketAddrV4,
    /// The receiving device's name, as it asked (for the ARC lists).
    pub rx_host: String,
    /// The receiver's name of the flow.
    pub rx_flow_name: String,
    /// The slots: our TX channel number (from 1) or `None`.
    pub channels: Vec<Option<u16>>,
    pub fpp: u16,
    /// Bytes per sample: the request's bits / 8, as inferno takes them.
    pub bps: u8,
    pub cookie: u16,
    /// When a datagram from the receiver last kept the flow alive (the
    /// control side's ns).
    pub heard_ns: u64,
    /// No datagram for [`KEEPALIVE_TIMEOUT_NS`]: sends nothing.
    pub expired: bool,
}

/// The TX flows: the control side's table (pure; the times are the
/// control side's ns, [`TxShared::now_ns`]).
#[derive(Debug)]
pub struct TxTable {
    flows: Vec<Option<TxFlow>>,
    /// The next slot to try, mod [`MAX_FLOWS`] (inferno's `flow_seq_id`).
    seq: u32,
    /// The cookies' generator state.
    rng: u64,
}

impl TxTable {
    /// An empty table; `seed` starts the cookies (the daemon gives it the
    /// time, tests a constant).
    pub fn new(seed: u64) -> Self {
        TxTable {
            flows: vec![None; MAX_FLOWS],
            seq: 0,
            rng: seed,
        }
    }

    /// A flow for `req` (checked by the server): a destination already
    /// served gets its channels, fpp and width set and keeps its handle
    /// (revived when expired; inferno keeps its fpp and width, a "TODO
    /// FIXME" there); a new one takes the next free slot after expired
    /// flows are freed, or [`status::TOO_MANY_TX_FLOWS`].
    pub fn add(&mut self, req: &FlowRequest, now: u64) -> Result<(usize, Handle), u16> {
        let bps = u8::try_from(req.bits / 8).unwrap_or(u8::MAX);
        if let Some(slot) = self.slot_of(req.rx)
            && let Some(flow) = self.flows[slot].as_mut()
        {
            flow.rx_host.clone_from(&req.rx_host);
            flow.rx_flow_name.clone_from(&req.rx_flow_name);
            flow.channels.clone_from(&req.channels);
            flow.fpp = req.fpp;
            flow.bps = bps;
            flow.heard_ns = now;
            flow.expired = false;
            return Ok((slot, control::handle(slot as u32, flow.cookie)));
        }
        self.free_expired();
        for _ in 0..MAX_FLOWS {
            let slot = (self.seq % MAX_FLOWS as u32) as usize;
            self.seq = self.seq.wrapping_add(1);
            if self.flows[slot].is_none() {
                let cookie = self.cookie();
                self.flows[slot] = Some(TxFlow {
                    dest: req.rx,
                    rx_host: req.rx_host.clone(),
                    rx_flow_name: req.rx_flow_name.clone(),
                    channels: req.channels.clone(),
                    fpp: req.fpp,
                    bps,
                    cookie,
                    heard_ns: now,
                    expired: false,
                });
                return Ok((slot, control::handle(slot as u32, cookie)));
            }
        }
        Err(status::TOO_MANY_TX_FLOWS)
    }

    /// The slot of the flow to `dest`, expired or not.
    pub fn slot_of(&self, dest: SocketAddrV4) -> Option<usize> {
        self.flows
            .iter()
            .position(|f| f.as_ref().is_some_and(|f| f.dest == dest))
    }

    /// The flow with handle `h`, expired or not.
    pub fn get(&self, h: &Handle) -> Option<(usize, &TxFlow)> {
        let (slot, cookie) = control::split(h);
        let slot = usize::try_from(slot).ok()?;
        let flow = self.flows.get(slot)?.as_ref()?;
        (flow.cookie == cookie).then_some((slot, flow))
    }

    /// Removes the flow with handle `h` (expired or not); returns its slot,
    /// or [`status::FLOW_NOT_FOUND`].
    pub fn stop(&mut self, h: &Handle) -> Result<usize, u16> {
        let (slot, _) = self.get(h).ok_or(status::FLOW_NOT_FOUND)?;
        self.flows[slot] = None;
        Ok(slot)
    }

    /// Sets the channels of the flow with handle `h` (checked by the
    /// server) and keeps it alive, an expired flow revived (inferno);
    /// returns its slot, or [`status::FLOW_NOT_FOUND`].
    pub fn update(&mut self, h: &Handle, channels: &[Option<u16>], now: u64) -> Result<usize, u16> {
        let (slot, _) = self.get(h).ok_or(status::FLOW_NOT_FOUND)?;
        if let Some(flow) = self.flows[slot].as_mut() {
            flow.channels = channels.to_vec();
            flow.heard_ns = now;
            flow.expired = false;
        }
        Ok(slot)
    }

    /// A datagram from the receiver of `slot`'s flow: alive until
    /// [`KEEPALIVE_TIMEOUT_NS`] after `now`. An expired flow stays expired
    /// (inferno reads nothing for it).
    pub fn keepalive(&mut self, slot: usize, now: u64) {
        if let Some(flow) = self
            .flows
            .get_mut(slot)
            .and_then(Option::as_mut)
            .filter(|f| !f.expired)
        {
            flow.heard_ns = now;
        }
    }

    /// Expires every flow not heard from for [`KEEPALIVE_TIMEOUT_NS`];
    /// returns the slots it expired now, one bit each.
    pub fn expire(&mut self, now: u64) -> u32 {
        let mut expired = 0u32;
        for (slot, flow) in self.flows.iter_mut().enumerate() {
            if let Some(flow) = flow
                .as_mut()
                .filter(|f| !f.expired && now.saturating_sub(f.heard_ns) >= KEEPALIVE_TIMEOUT_NS)
            {
                flow.expired = true;
                expired += 1 << slot;
            }
        }
        expired
    }

    /// The flow in `slot`.
    pub fn flow(&self, slot: usize) -> Option<&TxFlow> {
        self.flows.get(slot)?.as_ref()
    }

    /// Every flow with its slot, expired ones too.
    pub fn flows(&self) -> impl Iterator<Item = (usize, &TxFlow)> {
        self.flows
            .iter()
            .enumerate()
            .filter_map(|(slot, f)| Some((slot, f.as_ref()?)))
    }

    /// What the TX thread sends for `slot`: nothing for an empty slot, an
    /// expired flow or one that cannot be sent.
    pub fn plan(&self, slot: usize) -> Plan {
        match self.flow(slot) {
            Some(f) if !f.expired && sendable(f.channels.len(), usize::from(f.bps), f.fpp) => {
                let mut channels = [0u16; MAX_CHANNELS];
                for (c, id) in channels.iter_mut().zip(&f.channels) {
                    *c = id.unwrap_or(0);
                }
                Plan {
                    dest: Some(f.dest),
                    fpp: f.fpp,
                    bps: f.bps,
                    nch: f.channels.len() as u8,
                    channels,
                }
            }
            _ => Plan::default(),
        }
    }

    fn free_expired(&mut self) {
        for flow in &mut self.flows {
            if flow.as_ref().is_some_and(|f| f.expired) {
                *flow = None;
            }
        }
    }

    /// The next cookie (SplitMix64; any 16 bits will do, inferno's are
    /// random).
    fn cookie(&mut self) -> u16 {
        self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z >> 48) as u16
    }
}

/// What the TX thread sends for one slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Plan {
    /// The receiver; `None`: send nothing.
    pub dest: Option<SocketAddrV4>,
    pub fpp: u16,
    pub bps: u8,
    /// The slots used, at most [`MAX_CHANNELS`].
    pub nch: u8,
    /// Each slot's TX channel number (from 1), 0 for an empty slot.
    pub channels: [u16; MAX_CHANNELS],
}

impl Plan {
    /// The plan as four words (the seqlocked cell's).
    pub fn words(&self) -> [u64; 4] {
        let Some(dest) = self.dest else {
            return [0; 4];
        };
        [
            (u64::from(dest.ip().to_bits()) << 32)
                + (u64::from(dest.port()) << 16)
                + u64::from(self.fpp),
            (1 << 63) + (u64::from(self.bps) << 8) + u64::from(self.nch),
            pack(&self.channels[..4]),
            pack(&self.channels[4..]),
        ]
    }

    /// The plan [`Plan::words`] wrote.
    pub fn from_words(w: [u64; 4]) -> Plan {
        if w[1] >> 63 == 0 {
            return Plan::default();
        }
        let mut channels = [0u16; MAX_CHANNELS];
        for (i, c) in channels.iter_mut().enumerate() {
            *c = (w[2 + i / 4] >> (16 * (i % 4))) as u16;
        }
        Plan {
            dest: Some(SocketAddrV4::new(
                Ipv4Addr::from_bits((w[0] >> 32) as u32),
                (w[0] >> 16) as u16,
            )),
            fpp: w[0] as u16,
            bps: (w[1] >> 8) as u8,
            nch: w[1] as u8,
            channels,
        }
    }

    /// Whether going from `before` to this plan starts the schedule again:
    /// another receiver (or none), fpp or width. Changed channels keep it.
    pub fn restarts(&self, before: &Plan) -> bool {
        self.dest != before.dest || self.fpp != before.fpp || self.bps != before.bps
    }
}

/// Four channel numbers in one word, the first lowest.
fn pack(ids: &[u16]) -> u64 {
    ids.iter()
        .enumerate()
        .map(|(i, &id)| u64::from(id) << (16 * i))
        .sum()
}

/// A source as three words (the seqlocked cell's).
fn source_words(s: Source) -> [u64; 3] {
    match s {
        Source::Silence => [0, 0, 0],
        Source::Counter => [1, 0, 0],
        Source::Sine { hz, dbfs } => [2, hz.to_bits(), dbfs.to_bits()],
        Source::Echo { rx, offset } => [3, rx as u64, u64::from(offset)],
        Source::Engine => [4, 0, 0],
    }
}

/// The source [`source_words`] wrote.
fn source_from(w: [u64; 3]) -> Source {
    match w[0] {
        1 => Source::Counter,
        2 => Source::Sine {
            hz: f64::from_bits(w[1]),
            dbfs: f64::from_bits(w[2]),
        },
        3 => Source::Echo {
            rx: usize::try_from(w[1]).unwrap_or(usize::MAX),
            offset: w[2] as u32,
        },
        4 => Source::Engine,
        _ => Source::Silence,
    }
}

/// `N` words behind a seqlock: one writer at a time (the control side,
/// holding [`TxShared`]'s lock), readers that never block.
#[derive(Debug)]
pub(crate) struct Cell<const N: usize> {
    /// Odd while a write is under way.
    seq: AtomicU32,
    words: [AtomicU64; N],
}

impl<const N: usize> Cell<N> {
    pub(crate) fn new() -> Self {
        Cell {
            seq: AtomicU32::new(0),
            words: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Writes the words (the caller is the only writer).
    pub(crate) fn write(&self, w: [u64; N]) {
        let s = self.seq.load(Ordering::Relaxed);
        self.seq.store(s.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        for (word, value) in self.words.iter().zip(w) {
            word.store(value, Ordering::Relaxed);
        }
        self.seq.store(s.wrapping_add(2), Ordering::Release);
    }

    /// The version a reader compares to see a change.
    pub(crate) fn version(&self) -> u32 {
        self.seq.load(Ordering::Acquire)
    }

    /// The words and their version, all of one write; `None` when every
    /// try met a write under way.
    pub(crate) fn read(&self) -> Option<(u32, [u64; N])> {
        for _ in 0..READ_TRIES {
            let s = self.seq.load(Ordering::Acquire);
            if s.is_multiple_of(2) {
                let w = std::array::from_fn(|i| self.words[i].load(Ordering::Relaxed));
                fence(Ordering::Acquire);
                if self.seq.load(Ordering::Relaxed) == s {
                    return Some((s, w));
                }
            }
            std::hint::spin_loop();
        }
        None
    }

    /// The words as last written (for the writer).
    pub(crate) fn peek(&self) -> [u64; N] {
        std::array::from_fn(|i| self.words[i].load(Ordering::Relaxed))
    }
}

/// One slot's counts, written by the TX thread.
#[derive(Debug, Default)]
pub struct LaneStats {
    packets: AtomicU64,
    send_errors: AtomicU64,
    keepalives: AtomicU64,
    rebootstraps: AtomicU64,
    holes: AtomicU64,
}

/// One slot's counts since the device started (every flow it carried).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LaneCounts {
    /// Packets sent whole.
    pub packets: u64,
    /// Packets the socket refused or cut.
    pub send_errors: u64,
    /// Datagrams from the flow's receiver.
    pub keepalives: u64,
    /// Schedules started again: a lag past the latency, a jump, a
    /// re-anchor.
    pub rebootstraps: u64,
    /// Echo samples the RX ring did not hold, and engine samples the
    /// engine ring did not (sent as silence).
    pub holes: u64,
}

/// The control side's half of [`TxShared`]: the table, and the keepalive
/// counts it has seen per slot.
#[derive(Debug)]
pub struct Control {
    pub table: TxTable,
    seen: [u64; MAX_FLOWS],
}

/// What the control side and the TX thread share.
#[derive(Debug)]
pub struct TxShared {
    pub cfg: DeviceConfig,
    control: Mutex<Control>,
    plans: [Cell<4>; MAX_FLOWS],
    sources: Box<[Cell<3>]>,
    sockets: Box<[UdpSocket]>,
    rings: Arc<[Ring]>,
    /// Each TX channel's engine ring (phase C): the engine's pump writes
    /// it, [`Source::Engine`] sends it; `None` for a channel the engine
    /// does not feed.
    engine: Box<[Option<Ring>]>,
    lanes: [LaneStats; MAX_FLOWS],
    /// Each TX channel's largest magnitude sent since the last take (the
    /// heartbeat's peaks).
    peaks: Box<[AtomicU32]>,
    stop: AtomicBool,
    base: Instant,
}

impl TxShared {
    /// The shared state of the device `cfg` at `ip`: an empty table
    /// (cookies from `seed`), every TX channel silent, the RX channels'
    /// `rings` for the echo, an engine ring for each TX channel index in
    /// `engine` (from 0; the standalone device passes none), and one socket
    /// per slot bound to `ip`. An `engine` index the device does not have
    /// is refused (`InvalidInput`).
    pub fn new(
        cfg: DeviceConfig,
        ip: Ipv4Addr,
        rings: Arc<[Ring]>,
        seed: u64,
        engine: &[usize],
    ) -> io::Result<Self> {
        if engine.iter().any(|&tx| tx >= cfg.tx.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "an engine channel past the device's {} TX channels",
                    cfg.tx.len()
                ),
            ));
        }
        let sockets = (0..MAX_FLOWS)
            .map(|_| {
                let s = UdpSocket::bind(SocketAddrV4::new(ip, 0))?;
                s.set_nonblocking(true)?;
                Ok(s)
            })
            .collect::<io::Result<Box<[UdpSocket]>>>()?;
        Ok(TxShared {
            sources: (0..cfg.tx.len()).map(|_| Cell::new()).collect(),
            peaks: (0..cfg.tx.len()).map(|_| AtomicU32::new(0)).collect(),
            engine: (0..cfg.tx.len())
                .map(|tx| engine.contains(&tx).then(Ring::new))
                .collect(),
            cfg,
            control: Mutex::new(Control {
                table: TxTable::new(seed),
                seen: [0; MAX_FLOWS],
            }),
            plans: std::array::from_fn(|_| Cell::new()),
            sockets,
            rings,
            lanes: std::array::from_fn(|_| LaneStats::default()),
            stop: AtomicBool::new(false),
            base: Instant::now(),
        })
    }

    /// The control side's lock (never taken by the TX thread).
    pub fn lock(&self) -> MutexGuard<'_, Control> {
        self.control.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The control side's clock: ns since this state was built.
    pub fn now_ns(&self) -> u64 {
        u64::try_from(self.base.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// Hands the table's plans to the TX thread (the slots that changed).
    /// The caller holds the lock it got `c` from, so it is the only writer.
    pub fn publish(&self, c: &Control) {
        for (slot, cell) in self.plans.iter().enumerate() {
            let w = c.table.plan(slot).words();
            if cell.peek() != w {
                cell.write(w);
            }
        }
    }

    /// The control side's tick: a slot whose receiver was heard since the
    /// last tick is kept alive, flows not heard from for
    /// [`KEEPALIVE_TIMEOUT_NS`] expire, and the plans are handed over.
    /// Returns the slots expired now, one bit each.
    pub fn sync(&self, now: u64) -> u32 {
        let mut c = self.lock();
        for (slot, lane) in self.lanes.iter().enumerate() {
            let heard = lane.keepalives.load(Ordering::Relaxed);
            if heard != c.seen[slot] {
                c.seen[slot] = heard;
                c.table.keepalive(slot, now);
            }
        }
        let expired = c.table.expire(now);
        self.publish(&c);
        expired
    }

    /// Sets TX channel `tx`'s source (from 0); `false` when there is no
    /// such channel, or for [`Source::Engine`] on a channel without an
    /// engine ring. Runtime only: every channel is silent at start.
    pub fn set_source(&self, tx: usize, source: Source) -> bool {
        let _writer = self.lock();
        let Some(cell) = self.sources.get(tx) else {
            return false;
        };
        if source == Source::Engine && self.engine_ring(tx).is_none() {
            return false;
        }
        cell.write(source_words(source));
        true
    }

    /// TX channel `tx`'s engine ring (from 0; phase C): what the engine's
    /// pump writes and [`Source::Engine`] sends. `None` for a channel the
    /// engine does not feed.
    pub fn engine_ring(&self, tx: usize) -> Option<&Ring> {
        self.engine.get(tx)?.as_ref()
    }

    /// TX channel `tx`'s source (from 0).
    pub fn source(&self, tx: usize) -> Option<Source> {
        Some(source_from(self.sources.get(tx)?.read()?.1))
    }

    /// TX channel `tx`'s largest magnitude sent since the last call (from
    /// 0; 0 when nothing was sent, or no such channel).
    pub fn take_peak(&self, tx: usize) -> u32 {
        self.peaks
            .get(tx)
            .map_or(0, |p| p.swap(0, Ordering::Relaxed))
    }

    /// Slot `slot`'s counts.
    pub fn lane(&self, slot: usize) -> LaneCounts {
        let Some(l) = self.lanes.get(slot) else {
            return LaneCounts::default();
        };
        LaneCounts {
            packets: l.packets.load(Ordering::Relaxed),
            send_errors: l.send_errors.load(Ordering::Relaxed),
            keepalives: l.keepalives.load(Ordering::Relaxed),
            rebootstraps: l.rebootstraps.load(Ordering::Relaxed),
            holes: l.holes.load(Ordering::Relaxed),
        }
    }

    /// Where slot `slot`'s audio comes from and its keepalives go.
    pub fn local_addr(&self, slot: usize) -> io::Result<SocketAddr> {
        self.sockets
            .get(slot)
            .ok_or_else(|| io::Error::other("no such slot"))?
            .local_addr()
    }

    /// The RX channels' rings the echo reads.
    pub fn rings(&self) -> &Arc<[Ring]> {
        &self.rings
    }

    /// Asks the TX thread to end (within one of its sleeps, at most
    /// [`EVENTS_EVERY_NS`]).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// A flow's packets in time, in media samples ([`send_clock`]): `next` is
/// the stamp of the next packet, due once the clock reaches `next + fpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    pub next: u64,
    pub fpp: u64,
    /// The lag past which the schedule starts again: the device's latency
    /// in samples (a packet that late is of no use to the receiver).
    pub max_lag: u64,
    /// Starts again since the last [`Schedule::take_rebootstraps`].
    rebootstraps: u64,
}

impl Schedule {
    /// A schedule whose first stamp is the first multiple of `fpp` after
    /// `now` (inferno's bootstrap).
    pub fn bootstrap(now: u64, fpp: u16, max_lag: u64) -> Self {
        let fpp = u64::from(fpp.max(1));
        Schedule {
            next: aligned_after(now, fpp),
            fpp,
            max_lag,
            rebootstraps: 0,
        }
    }

    /// The stamp of the packet due at `now`, if one is (the schedule moves
    /// on by fpp). A lag past `max_lag`, or a schedule [`DISCONTINUITY`]
    /// samples ahead of the clock, starts it again (counted) and sends
    /// nothing now.
    pub fn due(&mut self, now: u64) -> Option<u64> {
        let send = self.next.saturating_add(self.fpp);
        if now > send.saturating_add(self.max_lag) || send > now.saturating_add(DISCONTINUITY) {
            self.realign(now);
            return None;
        }
        if now < send {
            return None;
        }
        let stamp = self.next;
        self.next = send;
        Some(stamp)
    }

    /// Starts the schedule again at `now` (counted): a lag, a jump, a
    /// re-anchor of the clock.
    pub fn realign(&mut self, now: u64) {
        self.next = aligned_after(now, self.fpp);
        self.rebootstraps += 1;
    }

    /// The starts again since the last call.
    pub fn take_rebootstraps(&mut self) -> u64 {
        std::mem::take(&mut self.rebootstraps)
    }

    /// The PTP ns at which the next packet is due at `rate`.
    pub fn wake_ns(&self, rate: u32) -> u64 {
        ns_of(self.next.saturating_add(self.fpp), rate).saturating_add(TX_MARGIN_NS)
    }
}

/// The first multiple of `fpp` after `now`.
fn aligned_after(now: u64, fpp: u64) -> u64 {
    now - now % fpp + fpp
}

/// The first PTP ns by which `samples` samples at `rate` have passed
/// (rounded up, so [`send_clock`] there reads them all). A rate of 0 reads
/// as 1.
pub fn ns_of(samples: u64, rate: u32) -> u64 {
    let rate = u128::from(rate.max(1));
    u64::try_from((u128::from(samples) * 1_000_000_000).div_ceil(rate)).unwrap_or(u64::MAX)
}

/// The media clock a schedule runs on: the samples at `rate` up to
/// [`TX_MARGIN_NS`] before `now_ns`.
pub fn send_clock(now_ns: u64, rate: u32) -> u64 {
    clock::to_samples(now_ns.saturating_sub(TX_MARGIN_NS), rate)
}

/// One slot as the TX thread sees it.
#[derive(Debug, Clone, Copy, Default)]
struct Lane {
    /// The plan cell's version read last.
    seen: Option<u32>,
    plan: Plan,
    /// `None` until the clock is ready, and after the plan restarted.
    schedule: Option<Schedule>,
}

/// The TX thread's state: everything it needs, allocated before its loop.
#[derive(Debug)]
pub struct Transmitter {
    shared: Arc<TxShared>,
    lanes: Vec<Lane>,
    sources: Vec<Source>,
    sources_seen: Vec<Option<u32>>,
    /// One packet's samples, slot by slot.
    frames: Box<[[i32; FPP_MAX as usize]; MAX_CHANNELS]>,
    packet: Box<[u8; MAX_DATAGRAM]>,
    /// The clock's epoch at the last pass.
    epoch: Option<u32>,
    /// When the next read of the plans, sources and keepalives is due.
    next_events: Option<u64>,
    rate: u32,
    max_lag: u64,
}

impl Transmitter {
    /// The thread's state for `shared` (every allocation it will need).
    pub fn new(shared: Arc<TxShared>) -> Self {
        let rate = shared.cfg.sample_rate;
        let channels = shared.sources.len();
        Transmitter {
            lanes: vec![Lane::default(); MAX_FLOWS],
            sources: vec![Source::Silence; channels],
            sources_seen: vec![None; channels],
            frames: Box::new([[0; FPP_MAX as usize]; MAX_CHANNELS]),
            packet: Box::new([0; MAX_DATAGRAM]),
            epoch: None,
            next_events: None,
            rate,
            max_lag: clock::to_samples(shared.cfg.latency_ns, rate),
            shared,
        }
    }

    /// One pass at PTP time `now_ns` (`None`: the clock is not ready) and
    /// the clock's `epoch`: the plans, sources and keepalives when due
    /// (every pass while the clock is not ready, and at once when the clock
    /// went back), every flow realigned after a re-anchor, then every
    /// packet due sent. Nothing is sent before the clock is ready.
    pub fn step(&mut self, now_ns: Option<u64>, epoch: u32) {
        let events_due = match (now_ns, self.next_events) {
            // Past the next read, or the clock went back before the last.
            (Some(now), Some(at)) => now >= at || at - now > EVENTS_EVERY_NS,
            _ => true,
        };
        if events_due {
            self.events();
            self.next_events = now_ns.map(|now| now.saturating_add(EVENTS_EVERY_NS));
        }
        let Some(now_ns) = now_ns else {
            return;
        };
        let now = send_clock(now_ns, self.rate);
        if self.epoch.is_some_and(|e| e != epoch) {
            for schedule in self.lanes.iter_mut().filter_map(|l| l.schedule.as_mut()) {
                schedule.realign(now);
            }
        }
        self.epoch = Some(epoch);
        self.send_due(now);
    }

    /// When the next pass is due, PTP ns: the next packet of any flow or
    /// the next read of the plans; 0 (at once: a clock that is not ready
    /// naps) when the clock is not ready.
    pub fn wake_ns(&self, now_ns: Option<u64>) -> u64 {
        if now_ns.is_none() {
            return 0;
        }
        // A lane has a schedule only while its plan sends.
        self.lanes
            .iter()
            .filter_map(|l| l.schedule.map(|s| s.wake_ns(self.rate)))
            .fold(self.next_events.unwrap_or(0), u64::min)
    }

    /// The plans and sources that changed, and the keepalives of every
    /// flow that sends.
    fn events(&mut self) {
        let shared = &*self.shared;
        for (slot, lane) in self.lanes.iter_mut().enumerate() {
            let cell = &shared.plans[slot];
            if lane.seen != Some(cell.version())
                && let Some((version, w)) = cell.read()
            {
                let plan = Plan::from_words(w);
                if plan.restarts(&lane.plan) {
                    lane.schedule = None;
                }
                lane.plan = plan;
                lane.seen = Some(version);
            }
            let Some(dest) = lane.plan.dest else {
                continue;
            };
            let mut heard = 0;
            for _ in 0..KEEPALIVE_DRAIN {
                match shared.sockets[slot].recv_from(&mut self.packet[..]) {
                    Ok((_, SocketAddr::V4(from))) if from == dest => heard += 1,
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            shared.lanes[slot]
                .keepalives
                .fetch_add(heard, Ordering::Relaxed);
        }
        for (tx, (source, seen)) in self
            .sources
            .iter_mut()
            .zip(self.sources_seen.iter_mut())
            .enumerate()
        {
            let cell = &shared.sources[tx];
            if *seen != Some(cell.version())
                && let Some((version, w)) = cell.read()
            {
                *source = source_from(w);
                *seen = Some(version);
            }
        }
    }

    /// Every packet due at media time `now`, each flow in turn (the hot
    /// path: no allocation, no lock, no log; the send is its one system
    /// call).
    fn send_due(&mut self, now: u64) {
        let Transmitter {
            shared,
            lanes,
            sources,
            frames,
            packet,
            rate,
            max_lag,
            ..
        } = self;
        let rings: &[Ring] = &shared.rings;
        for (slot, lane) in lanes.iter_mut().enumerate() {
            let Some(dest) = lane.plan.dest else {
                continue;
            };
            let plan = lane.plan;
            let stats = &shared.lanes[slot];
            let socket = &shared.sockets[slot];
            let schedule = lane
                .schedule
                .get_or_insert_with(|| Schedule::bootstrap(now, plan.fpp, *max_lag));
            let fpp = usize::from(plan.fpp);
            let nch = usize::from(plan.nch);
            let mut holes = 0;
            while let Some(stamp) = schedule.due(now) {
                for (frame, &id) in frames.iter_mut().zip(&plan.channels).take(nch) {
                    let out = &mut frame[..fpp];
                    match usize::from(id).checked_sub(1).and_then(|i| {
                        let own = shared.engine.get(i).and_then(Option::as_ref);
                        Some((sources.get_mut(i)?, shared.peaks.get(i)?, own))
                    }) {
                        Some((source, peak, own)) => {
                            holes += source.fill(stamp, *rate, out, rings, own).holes;
                            let loudest = out.iter().fold(0, |m: u32, s| m.max(s.unsigned_abs()));
                            peak.fetch_max(loudest, Ordering::Relaxed);
                        }
                        None => out.fill(0),
                    }
                }
                let (secs, sub) = clock::stamp(stamp, *rate);
                // The plan is sendable (TxTable::plan), so the packet is
                // whole and fits the buffer.
                let n = packet::write(
                    &mut packet[..],
                    secs,
                    sub,
                    nch,
                    usize::from(plan.bps),
                    fpp,
                    |f, s| frames[s][f],
                );
                // A datagram socket sends the datagram whole or not at all
                // (the packet fits one: `wire::MAX_DATAGRAM`), so the byte
                // count says nothing more than `Ok`.
                let counter = match socket.send_to(&packet[..n], dest) {
                    Ok(_) => &stats.packets,
                    Err(_) => &stats.send_errors,
                };
                counter.fetch_add(1, Ordering::Relaxed);
            }
            stats.holes.fetch_add(holes as u64, Ordering::Relaxed);
            stats
                .rebootstraps
                .fetch_add(schedule.take_rebootstraps(), Ordering::Relaxed);
        }
    }
}

/// Starts the TX thread: it sends `shared`'s flows on `clock` until
/// [`TxShared::stop`].
pub fn spawn(clock: Arc<dyn PtpClock>, shared: Arc<TxShared>) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("dante-tx".to_string())
        .spawn(move || run(clock.as_ref(), shared))
}

/// The TX thread's loop (glue: every decision is [`Transmitter`]'s).
fn run(clock: &dyn PtpClock, shared: Arc<TxShared>) {
    if ask_realtime(TX_PRIORITY) {
        info!(priority = TX_PRIORITY, "TX thread runs SCHED_FIFO");
    } else {
        warn!(
            priority = TX_PRIORITY,
            "TX thread runs at normal priority: RLIMIT_RTPRIO allows no SCHED_FIFO"
        );
    }
    let mut tx = Transmitter::new(Arc::clone(&shared));
    while !shared.stopping() {
        let now = clock.now_ns();
        tx.step(now, clock.epoch());
        clock.sleep_until(tx.wake_ns(now));
    }
}

/// Asks for `SCHED_FIFO` at `priority` for the calling thread (reset in
/// any child it forks); `false` when the kernel refuses it. The RX thread
/// asks too ([`super::rx`]).
#[cfg(target_os = "linux")]
pub(crate) fn ask_realtime(priority: i32) -> bool {
    let param = libc::sched_param {
        sched_priority: priority,
    };
    // SAFETY: `param` is a valid sched_param the call only reads; pid 0 is
    // the calling thread.
    unsafe {
        libc::sched_setscheduler(0, libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, &param) == 0
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn ask_realtime(_priority: i32) -> bool {
    false
}

#[cfg(test)]
mod tests;
