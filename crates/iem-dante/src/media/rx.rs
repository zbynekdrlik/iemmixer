// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from inferno (https://github.com/teodly/inferno at 9767558):
// inferno_aoip/src/device_server/flows_rx.rs (one thread receiving every RX
// flow's socket through mio's poll, up to 32 flows, each socket on the
// device's address at an ephemeral port; a packet's stamp places its samples
// in the subscribed channels' buffers; the keepalive `13 37` to the source
// of a flow's audio every 250 ms per flow; the largest lateness of a flow's
// packets since it was last read, its heartbeat record 0x8003), Copyright (C)
// Teo and the inferno contributors, GPL-3.0-or-later OR AGPL-3.0-or-later,
// taken under GPL-3.0-or-later. Changes Copyright (C) 2026 iemmixer
// contributors.

//! Our RX flows (Gen 3 phase B, plan Task 5): the thread that receives the
//! audio transmitters send us, and what it shares with the control side.
//!
//! - [`RxShared`]: [`MAX_FLOWS`] flow slots, each a UDP socket on our
//!   address (an ephemeral port, a [`RECV_BUFFER_BYTES`] receive buffer
//!   asked for) whose port the flow request names. The control side
//!   ([`crate::subscribe::Manager`]) hands each slot's [`RxPlan`] (the
//!   transmitter, the width, the channel slots, the latency, whether it was
//!   granted) and each RX channel's [`RxRoute`] (which flow slot and channel
//!   slot feed its ring) over through seqlocked atomics
//!   ([`RxShared::publish`]); the thread never blocks on them.
//! - [`Receiver`] and [`spawn`]: the thread. It waits on every flow socket
//!   at once (mio's poll), at most [`WAKE_NS`] at a time. A datagram counts
//!   only from its slot's transmitter (by address; anything else is
//!   `stray`); a well-formed one is a packet: its stamp, read as the sample
//!   index nearest our clock's now ([`clock::from_stamp`], across the 32-bit
//!   seconds wrap and any uptime), places each channel slot's samples in
//!   the rings of the RX channels routed to it ([`super::ring::Ring`]: a
//!   missing packet reads as a hole, a late one is the ring's to count). Its
//!   lateness (our now less its first sample) is measured, the largest kept
//!   until read; one later than the flow's latency counts `late`, one
//!   before the end of the last `out_of_order`. Before our clock is ready a
//!   packet is read and counted but placed nowhere (`unplaced`).
//! - Keepalives: `13 37` from the slot's socket to the address its audio
//!   comes from, every [`KEEPALIVE_NS`] (four a second, never more: a late
//!   one moves the schedule on, it never catches up), only for a flow its
//!   transmitter granted and whose audio came within [`LIVE_NS`]. Nothing
//!   is ever sent to a source that sent no audio of a granted flow.
//!
//! The per-packet path (the receive, the parse, the ring writes, a
//! keepalive's send) allocates nothing, takes no lock and logs nothing
//! (`tests/hot_path.rs`); every count is an atomic of its slot
//! ([`RxCounts`]).

use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mio::{Events, Interest, Poll, Token};
use tracing::{info, warn};

use super::packet::{self, MEDIA_HEADER};
use super::ring::Ring;
use super::tx::{Cell, MAX_CHANNELS, ask_realtime};
use crate::clock::{self, PtpClock};
use crate::wire::MAX_DATAGRAM;

/// Our RX flow slots (inferno's `MAX_FLOWS`).
pub const MAX_FLOWS: usize = 32;
/// A keepalive's bytes (inferno's).
pub const KEEPALIVE: [u8; 2] = [0x13, 0x37];
/// A granted flow's keepalives, one per this (inferno's 250 ms).
pub const KEEPALIVE_NS: u64 = 250_000_000;
/// Keepalives go out only while the flow's audio came within this.
pub const LIVE_NS: u64 = 2_000_000_000;
/// The receive buffer asked for each flow socket (the kernel gives at most
/// its `rmem_max`).
pub const RECV_BUFFER_BYTES: usize = 4 << 20;
/// The longest the thread waits for a datagram: it reads the stop flag and
/// new plans at least this often.
pub const WAKE_NS: u64 = 33_000_000;
/// The most datagrams read from one socket in a pass (a flood never holds
/// the thread); the rest wait for the next pass, at once.
pub const DRAIN: usize = 64;
/// The RX thread's `SCHED_FIFO` priority when `RLIMIT_RTPRIO` allows it:
/// the TX thread's, below dantesync's 50.
pub const RX_PRIORITY: i32 = 45;
/// The most frames one channel slot of a packet holds (16-bit, one slot).
const FRAMES_MAX: usize = (MAX_DATAGRAM - MEDIA_HEADER) / 2;
/// A datagram read; one longer than [`MAX_DATAGRAM`] is refused.
const BUFFER: usize = 2048;

/// What the RX thread receives in one flow slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RxPlan {
    /// The transmitter: only its datagrams are this flow's. `None`: no
    /// flow.
    pub src: Option<Ipv4Addr>,
    /// Bytes per sample.
    pub bps: u8,
    /// Channel slots per frame.
    pub nslots: u8,
    /// The flow's latency, samples.
    pub latency: u32,
    /// Granted by its transmitter: keepalives go out.
    pub live: bool,
    /// Which flow (a new flow in the slot is a new plan).
    pub generation: u32,
}

impl RxPlan {
    /// The plan as two words (the seqlocked cell's).
    pub fn words(&self) -> [u64; 2] {
        let Some(src) = self.src else {
            return [0; 2];
        };
        [
            (u64::from(src.to_bits()) << 32) + u64::from(self.latency),
            (1 << 63)
                + (u64::from(self.live) << 62)
                + (u64::from(self.generation) << 16)
                + (u64::from(self.bps) << 8)
                + u64::from(self.nslots),
        ]
    }

    /// The plan [`RxPlan::words`] wrote.
    pub fn from_words(w: [u64; 2]) -> RxPlan {
        if w[1] >> 63 == 0 {
            return RxPlan::default();
        }
        RxPlan {
            src: Some(Ipv4Addr::from_bits((w[0] >> 32) as u32)),
            bps: (w[1] >> 8) as u8,
            nslots: w[1] as u8,
            latency: w[0] as u32,
            live: (w[1] >> 62) & 1 == 1,
            generation: (w[1] >> 16) as u32,
        }
    }
}

/// Where an RX channel's samples come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxRoute {
    /// The flow slot.
    pub flow: usize,
    /// The channel slot in its frames.
    pub slot: usize,
}

impl RxRoute {
    /// A route as one word, 0 for none.
    pub fn word(route: Option<RxRoute>) -> u64 {
        route.map_or(0, |r| {
            (1 << 63) + ((r.flow as u64 & 0xff) << 8) + (r.slot as u64 & 0xff)
        })
    }

    /// The route [`RxRoute::word`] wrote.
    pub fn from_word(w: u64) -> Option<RxRoute> {
        (w >> 63 == 1).then_some(RxRoute {
            flow: ((w >> 8) & 0xff) as usize,
            slot: (w & 0xff) as usize,
        })
    }
}

/// One flow slot's counts, written by the RX thread.
#[derive(Debug, Default)]
struct RxStats {
    packets: AtomicU64,
    refused: AtomicU64,
    stray: AtomicU64,
    out_of_order: AtomicU64,
    late: AtomicU64,
    unplaced: AtomicU64,
    keepalives: AtomicU64,
    keepalive_errors: AtomicU64,
    recv_errors: AtomicU64,
    max_lateness: AtomicU64,
    /// What the last [`RxShared::take_max_lateness`] took.
    last_lateness: AtomicU64,
}

/// One flow slot's counts since the device started (every flow it held).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RxCounts {
    /// Well-formed packets from the flow's transmitter.
    pub packets: u64,
    /// Datagrams from the transmitter that were no packet of the flow
    /// (shorter than the header, longer than a datagram, no whole frame).
    pub refused: u64,
    /// Datagrams from another address, or to a slot without a flow.
    pub stray: u64,
    /// Packets that began before the end of an earlier one.
    pub out_of_order: u64,
    /// Packets later than the flow's latency.
    pub late: u64,
    /// Packets read before our clock was ready.
    pub unplaced: u64,
    pub keepalives: u64,
    pub keepalive_errors: u64,
    pub recv_errors: u64,
}

/// What the control side and the RX thread share.
#[derive(Debug)]
pub struct RxShared {
    rate: u32,
    plans: [Cell<2>; MAX_FLOWS],
    routes: Box<[AtomicU64]>,
    routes_version: AtomicU32,
    sockets: Box<[UdpSocket]>,
    rings: Arc<[Ring]>,
    lanes: [RxStats; MAX_FLOWS],
    /// Held by [`RxShared::publish`] only (one writer at a time); the RX
    /// thread never takes it.
    writer: Mutex<()>,
    poll_errors: AtomicU64,
    stop: AtomicBool,
    base: Instant,
    recv_buffer: usize,
}

impl RxShared {
    /// The flow slots of a device at `ip` running at `rate`: one socket each
    /// bound to `ip`, and the RX channels' `rings` (one per RX channel,
    /// shared with the TX side's echo).
    pub fn new(rate: u32, ip: Ipv4Addr, rings: Arc<[Ring]>) -> io::Result<Self> {
        let mut recv_buffer = 0;
        let sockets = (0..MAX_FLOWS)
            .map(|_| {
                let s = UdpSocket::bind(SocketAddrV4::new(ip, 0))?;
                s.set_nonblocking(true)?;
                recv_buffer = set_recv_buffer(&s, RECV_BUFFER_BYTES);
                Ok(s)
            })
            .collect::<io::Result<Box<[UdpSocket]>>>()?;
        Ok(RxShared {
            rate,
            plans: std::array::from_fn(|_| Cell::new()),
            routes: (0..rings.len()).map(|_| AtomicU64::new(0)).collect(),
            routes_version: AtomicU32::new(0),
            sockets,
            rings,
            lanes: std::array::from_fn(|_| RxStats::default()),
            writer: Mutex::new(()),
            poll_errors: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            base: Instant::now(),
            recv_buffer,
        })
    }

    /// Hands the flow slots' plans and the RX channels' routes to the RX
    /// thread (what changed). Routes that leave a flow go first and routes
    /// that join one last, so no channel ever reads another flow's samples.
    pub fn publish(
        &self,
        plan: impl Fn(usize) -> RxPlan,
        route: impl Fn(usize) -> Option<RxRoute>,
    ) {
        let _writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let mut changed = false;
        for (rx, cell) in self.routes.iter().enumerate() {
            let have = cell.load(Ordering::Relaxed);
            if have != 0 && have != RxRoute::word(route(rx)) {
                cell.store(0, Ordering::Release);
                changed = true;
            }
        }
        for (slot, cell) in self.plans.iter().enumerate() {
            let w = plan(slot).words();
            if cell.peek() != w {
                cell.write(w);
            }
        }
        for (rx, cell) in self.routes.iter().enumerate() {
            let want = RxRoute::word(route(rx));
            if want != cell.load(Ordering::Relaxed) {
                cell.store(want, Ordering::Release);
                changed = true;
            }
        }
        if changed {
            self.routes_version.fetch_add(1, Ordering::Release);
        }
    }

    /// Each flow slot's UDP port, where its transmitter sends the audio.
    pub fn ports(&self) -> io::Result<[u16; MAX_FLOWS]> {
        let mut ports = [0; MAX_FLOWS];
        for (port, s) in ports.iter_mut().zip(self.sockets.iter()) {
            *port = s.local_addr()?.port();
        }
        Ok(ports)
    }

    /// Flow slot `slot`'s address.
    pub fn local_addr(&self, slot: usize) -> io::Result<SocketAddr> {
        self.sockets
            .get(slot)
            .ok_or_else(|| io::Error::other("no such slot"))?
            .local_addr()
    }

    /// Flow slot `slot`'s counts.
    pub fn lane(&self, slot: usize) -> RxCounts {
        let Some(l) = self.lanes.get(slot) else {
            return RxCounts::default();
        };
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        RxCounts {
            packets: get(&l.packets),
            refused: get(&l.refused),
            stray: get(&l.stray),
            out_of_order: get(&l.out_of_order),
            late: get(&l.late),
            unplaced: get(&l.unplaced),
            keepalives: get(&l.keepalives),
            keepalive_errors: get(&l.keepalive_errors),
            recv_errors: get(&l.recv_errors),
        }
    }

    /// Every flow slot's packets (the manager's [`crate::subscribe::Manager::tick`]).
    pub fn packets(&self) -> [u64; MAX_FLOWS] {
        std::array::from_fn(|slot| self.lanes[slot].packets.load(Ordering::Relaxed))
    }

    /// Flow slot `slot`'s largest lateness since the last call, samples
    /// (the heartbeat's 0x8003, plan Task 6); kept for
    /// [`RxShared::last_lateness`].
    pub fn take_max_lateness(&self, slot: usize) -> u64 {
        self.lanes.get(slot).map_or(0, |l| {
            let taken = l.max_lateness.swap(0, Ordering::Relaxed);
            l.last_lateness.store(taken, Ordering::Relaxed);
            taken
        })
    }

    /// What the last [`RxShared::take_max_lateness`] of flow slot `slot`
    /// took (the heartbeat's last second), samples: the control socket's
    /// status reads it and takes nothing (plan Task 7).
    pub fn last_lateness(&self, slot: usize) -> u64 {
        self.lanes
            .get(slot)
            .map_or(0, |l| l.last_lateness.load(Ordering::Relaxed))
    }

    /// The receive buffer the kernel gave the flow sockets, bytes.
    pub fn recv_buffer(&self) -> usize {
        self.recv_buffer
    }

    /// The thread's failed waits.
    pub fn poll_errors(&self) -> u64 {
        self.poll_errors.load(Ordering::Relaxed)
    }

    /// The RX channels' rings.
    pub fn rings(&self) -> &Arc<[Ring]> {
        &self.rings
    }

    /// The control side's clock and the thread's for keepalives: ns since
    /// this state was built.
    pub fn now_ns(&self) -> u64 {
        u64::try_from(self.base.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// Asks the RX thread to end (within [`WAKE_NS`]).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// Asks for a receive buffer of `bytes` on `s`; returns what the kernel
/// gave (Linux doubles what it grants, up to its `rmem_max`; 0 when it
/// cannot say). A refusal leaves the kernel's default.
fn set_recv_buffer(s: &UdpSocket, bytes: usize) -> usize {
    let sock = socket2::SockRef::from(s);
    let _refused = sock.set_recv_buffer_size(bytes);
    sock.recv_buffer_size().unwrap_or(0)
}

/// One flow slot as the RX thread sees it.
#[derive(Debug)]
struct Lane {
    /// The plan cell's version read last.
    seen: Option<u32>,
    plan: RxPlan,
    /// (channel slot, RX channel) for each RX channel routed here;
    /// allocated for every RX channel, so a rebuild never allocates.
    targets: Vec<(usize, usize)>,
    /// Where the flow's audio comes from: the keepalives' destination.
    source: Option<SocketAddrV4>,
    /// When its last packet came (the thread's clock, ns).
    heard: Option<u64>,
    next_keepalive: Option<u64>,
    /// The end of the furthest packet so far, samples.
    end: Option<u64>,
}

impl Lane {
    fn new(channels: usize) -> Self {
        Lane {
            seen: None,
            plan: RxPlan::default(),
            targets: Vec::with_capacity(channels),
            source: None,
            heard: None,
            next_keepalive: None,
            end: None,
        }
    }
}

/// The RX thread's state: everything it needs, allocated before its loop.
#[derive(Debug)]
pub struct Receiver {
    shared: Arc<RxShared>,
    poll: Poll,
    events: Events,
    /// The flow sockets (the shared ones' duplicates, registered with the
    /// poll).
    sockets: Vec<mio::net::UdpSocket>,
    lanes: Vec<Lane>,
    /// The routes' version read last.
    routes_seen: Option<u32>,
    /// Slots with datagrams waiting, one bit each.
    ready: u32,
    buf: Box<[u8; BUFFER]>,
    /// One packet's samples, channel slot by channel slot.
    frames: Box<[[i32; FRAMES_MAX]; MAX_CHANNELS]>,
}

impl Receiver {
    /// The thread's state for `shared`: its poll with every flow socket.
    pub fn new(shared: Arc<RxShared>) -> io::Result<Self> {
        let poll = Poll::new()?;
        let mut sockets = Vec::with_capacity(MAX_FLOWS);
        for (slot, s) in shared.sockets.iter().enumerate() {
            let mut s = mio::net::UdpSocket::from_std(s.try_clone()?);
            poll.registry()
                .register(&mut s, Token(slot), Interest::READABLE)?;
            sockets.push(s);
        }
        let channels = shared.rings.len();
        Ok(Receiver {
            poll,
            events: Events::with_capacity(MAX_FLOWS),
            sockets,
            lanes: (0..MAX_FLOWS).map(|_| Lane::new(channels)).collect(),
            routes_seen: None,
            ready: 0,
            buf: Box::new([0; BUFFER]),
            frames: Box::new([[0; FRAMES_MAX]; MAX_CHANNELS]),
            shared,
        })
    }

    /// Waits up to `timeout` for datagrams; the slots that have some are
    /// read at the next [`Receiver::pass`]. A failed wait is the poll's
    /// error (an interrupt is nothing to the thread; any other is counted,
    /// [`RxShared::poll_errors`], and the thread naps).
    pub fn wait(&mut self, timeout: Duration) -> io::Result<()> {
        self.poll.poll(&mut self.events, Some(timeout))?;
        for ev in &self.events {
            if let Some(bit) = u32::try_from(ev.token().0)
                .ok()
                .and_then(|t| 1u32.checked_shl(t))
            {
                self.ready |= bit;
            }
        }
        Ok(())
    }

    /// How long the next [`Receiver::wait`] may last at the thread's clock
    /// `mono`: none while datagrams wait, else until the next keepalive is
    /// due, at most [`WAKE_NS`].
    pub fn timeout(&self, mono: u64) -> Duration {
        if self.ready != 0 {
            return Duration::ZERO;
        }
        let due = self
            .lanes
            .iter()
            .filter_map(|l| l.next_keepalive)
            .fold(mono.saturating_add(WAKE_NS), u64::min);
        Duration::from_nanos(due.saturating_sub(mono))
    }

    /// One pass at PTP time `now_ns` (`None`: our clock is not ready) and
    /// the thread's clock `mono`: the plans and routes that changed, every
    /// slot with datagrams waiting read, the keepalives due sent.
    pub fn pass(&mut self, now_ns: Option<u64>, mono: u64) {
        self.refresh();
        let mut ready = self.ready;
        while ready != 0 {
            let slot = ready.trailing_zeros() as usize;
            ready &= ready - 1;
            if self.receive(slot, now_ns, mono) {
                self.ready &= !(1 << slot);
            }
        }
        self.keepalives(mono);
    }

    /// Reads the plans that changed (a new one resets its slot) and, after
    /// the routes changed (their version moves once [`RxShared::publish`]
    /// wrote them all), which RX channels each slot feeds.
    fn refresh(&mut self) {
        let shared = &*self.shared;
        for (lane, cell) in self.lanes.iter_mut().zip(&shared.plans) {
            if lane.seen != Some(cell.version())
                && let Some((version, w)) = cell.read()
            {
                let plan = RxPlan::from_words(w);
                if plan != lane.plan {
                    lane.plan = plan;
                    lane.source = None;
                    lane.heard = None;
                    lane.next_keepalive = None;
                    lane.end = None;
                }
                lane.seen = Some(version);
            }
        }
        let version = shared.routes_version.load(Ordering::Acquire);
        if self.routes_seen == Some(version) {
            return;
        }
        for lane in &mut self.lanes {
            lane.targets.clear();
        }
        for (rx, cell) in shared.routes.iter().enumerate() {
            if let Some(r) = RxRoute::from_word(cell.load(Ordering::Acquire))
                && r.slot < MAX_CHANNELS
                && let Some(lane) = self.lanes.get_mut(r.flow)
            {
                lane.targets.push((r.slot, rx));
            }
        }
        self.routes_seen = Some(version);
    }

    /// Reads up to [`DRAIN`] datagrams of flow slot `slot`; `true` when
    /// none is left. The hot path: no allocation, no lock, no log.
    fn receive(&mut self, slot: usize, now_ns: Option<u64>, mono: u64) -> bool {
        let Receiver {
            shared,
            sockets,
            lanes,
            buf,
            frames,
            ..
        } = self;
        let (Some(socket), Some(lane), Some(stats)) = (
            sockets.get(slot),
            lanes.get_mut(slot),
            shared.lanes.get(slot),
        ) else {
            return true;
        };
        let rate = shared.rate;
        let rings: &[Ring] = &shared.rings;
        let now = now_ns.map(|ns| clock::to_samples(ns, rate));
        let count = |a: &AtomicU64| {
            a.fetch_add(1, Ordering::Relaxed);
        };
        for _ in 0..DRAIN {
            let (n, from) = match socket.recv_from(&mut buf[..]) {
                Ok((n, SocketAddr::V4(from))) => (n, from),
                Ok(_) => {
                    count(&stats.stray);
                    continue;
                }
                Err(e) => {
                    // Would block: none left. Anything else (an unconnected
                    // UDP socket reports none we know of) is counted and
                    // ends this slot's reads for the pass.
                    if e.kind() != ErrorKind::WouldBlock {
                        count(&stats.recv_errors);
                    }
                    return true;
                }
            };
            if lane.plan.src != Some(*from.ip()) {
                count(&stats.stray);
                continue;
            }
            let nslots = usize::from(lane.plan.nslots);
            let datagram = &buf[..n];
            let header = packet::read_header(datagram);
            let read = (n <= MAX_DATAGRAM && nslots <= MAX_CHANNELS)
                .then(|| {
                    packet::read_frames(datagram, nslots, usize::from(lane.plan.bps), |f, s, v| {
                        frames[s][f] = v;
                    })
                    .ok()
                })
                .flatten();
            let (Some((secs, sub)), Some(nframes @ 1..)) = (header, read) else {
                count(&stats.refused);
                continue;
            };
            count(&stats.packets);
            lane.source = Some(from);
            lane.heard = Some(mono);
            let Some(now) = now else {
                count(&stats.unplaced);
                continue;
            };
            let pos = clock::from_stamp(secs, sub, rate, now);
            let lateness = now.saturating_sub(pos);
            stats.max_lateness.fetch_max(lateness, Ordering::Relaxed);
            if lateness > u64::from(lane.plan.latency) {
                count(&stats.late);
            }
            let end = pos.saturating_add(nframes as u64);
            match lane.end {
                Some(e) if pos < e => {
                    count(&stats.out_of_order);
                    lane.end = Some(e.max(end));
                }
                _ => lane.end = Some(end),
            }
            for &(s, rx) in &lane.targets {
                if s < nslots
                    && let Some(ring) = rings.get(rx)
                {
                    ring.write_at(pos, &frames[s][..nframes]);
                }
            }
        }
        false
    }

    /// Sends the keepalives due at the thread's clock `mono`: one per
    /// [`KEEPALIVE_NS`] per granted flow whose audio came within
    /// [`LIVE_NS`], to that audio's source.
    fn keepalives(&mut self, mono: u64) {
        for (slot, lane) in self.lanes.iter_mut().enumerate() {
            let live =
                lane.plan.live && lane.heard.is_some_and(|h| mono.saturating_sub(h) < LIVE_NS);
            let (true, Some(to)) = (live, lane.source) else {
                lane.next_keepalive = None;
                continue;
            };
            let due = *lane
                .next_keepalive
                .get_or_insert(mono.saturating_add(KEEPALIVE_NS));
            if mono < due {
                continue;
            }
            let stats = &self.shared.lanes[slot];
            let counter = match self.sockets[slot].send_to(&KEEPALIVE, SocketAddr::V4(to)) {
                Ok(_) => &stats.keepalives,
                Err(_) => &stats.keepalive_errors,
            };
            counter.fetch_add(1, Ordering::Relaxed);
            let next = due.saturating_add(KEEPALIVE_NS);
            lane.next_keepalive = Some(if next <= mono {
                mono.saturating_add(KEEPALIVE_NS)
            } else {
                next
            });
        }
    }
}

/// Starts the RX thread: it receives `shared`'s flows on `clock` until
/// [`RxShared::stop`].
pub fn spawn(clock: Arc<dyn PtpClock>, shared: Arc<RxShared>) -> io::Result<JoinHandle<()>> {
    let rx = Receiver::new(Arc::clone(&shared))?;
    std::thread::Builder::new()
        .name("dante-rx".to_string())
        .spawn(move || run(clock.as_ref(), &shared, rx))
}

/// What a failed wait means for the RX thread: an interrupt (a signal) is
/// nothing; any other failure is counted ([`RxShared::poll_errors`]) and
/// the thread naps [`WAKE_NS`] (`true`), so a failing poll is never a spin.
fn failed_wait(shared: &RxShared, e: &io::Error) -> bool {
    if e.kind() == ErrorKind::Interrupted {
        return false;
    }
    shared.poll_errors.fetch_add(1, Ordering::Relaxed);
    true
}

/// The RX thread's loop (glue: every decision is [`Receiver`]'s and
/// [`failed_wait`]'s).
fn run(clock: &dyn PtpClock, shared: &RxShared, mut rx: Receiver) {
    if ask_realtime(RX_PRIORITY) {
        info!(priority = RX_PRIORITY, "RX thread runs SCHED_FIFO");
    } else {
        warn!(
            priority = RX_PRIORITY,
            "RX thread runs at normal priority: RLIMIT_RTPRIO allows no SCHED_FIFO"
        );
    }
    info!(
        bytes = shared.recv_buffer(),
        asked = RECV_BUFFER_BYTES,
        "RX flow sockets' receive buffer"
    );
    while !shared.stopping() {
        if let Err(e) = rx.wait(rx.timeout(shared.now_ns()))
            && failed_wait(shared, &e)
        {
            std::thread::sleep(Duration::from_nanos(WAKE_NS));
        }
        rx.pass(clock.now_ns(), shared.now_ns());
    }
}

#[cfg(test)]
mod tests;
