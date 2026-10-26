// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from inferno (https://github.com/teodly/inferno at 9767558):
// inferno_aoip/src/device_server/channels_subscriber.rs (the statuses and
// their codes; channels asking for the same TX channel share one slot; a TX
// channel a flow already carries is used without a request; a transmitter's
// channels in flows of min(nchan, 8, our RX channels) slots, the rest empty;
// frames per packet the transmitter's most that fit; the latency the larger
// of its and ours; the flow's name `<flow>_<process>`; a flow without a
// packet for 2 s, 6 s after it was asked for, is stopped and its channels
// resolved again; a flow no channel uses is stopped; unresolved channels
// tried again every 9 s; a transmitter whose channels disagree on nchan,
// bits or dbcp1 is not asked), Copyright (C) Teo and the inferno
// contributors, GPL-3.0-or-later OR AGPL-3.0-or-later, taken under
// GPL-3.0-or-later. Changes Copyright (C) 2026 iemmixer contributors.

//! Our RX subscriptions (Gen 3 phase B, plan Task 5): which TX channel of
//! which device each RX channel receives ([`crate::device::Subscription`],
//! set on our own device, kept in the state file), and the flows that
//! bring them.
//!
//! [`Manager`] is the pure core, driven by the control side: [`Manager::set`]
//! (a subscription changed), [`Manager::tick`] (every
//! [`TICK`]: the packets each flow slot received, the records the
//! [`resolve::Resolver`] knows), [`Manager::answered`] (a request's answer)
//! and [`Manager::stop_all`] (shutdown). It returns [`Action`]s, the only
//! datagrams our RX ever sends besides keepalives: a flow request to a
//! transmitter for its channels to one of our flow slots, and a stop. It
//! never changes another device's subscriptions: it asks transmitters for
//! flows of our own RX channels only.
//!
//! **What bounds our requests (network safety, plan Review Focus 1):** per
//! transmitter (by address) at most one request in flight, and at most one
//! per [`RETRY_NS`] (9 s), counted from the last request whatever its
//! answer; each failure in a row (a refusal, no answer within
//! [`GIVE_UP_NS`], a flow that stays silent) doubles the wait up to
//! [`RETRY_MAX_NS`] ([`retry_after`]), and audio received ends the row.
//! Stops are one datagram per flow we were granted. Nothing here depends on
//! what a transmitter answers to stay inside these bounds.
//!
//! **Flows.** Up to [`MAX_FLOWS`] (inferno's 32), each in one of our flow
//! slots: the slot's socket ([`crate::media::rx::RxShared`]) is where the
//! transmitter sends the audio. A flow has `min(nchan, 8, our RX count)`
//! channel slots, filled with the TX channels its transmitter's waiting
//! channels need (in RX order, one slot per TX channel however many RX
//! channels ask for it), the rest empty; the frames per packet are the
//! transmitter's most that fit one datagram; the latency is the larger of
//! its record's and ours. A flow no channel uses any more is stopped; a
//! flow that has received nothing for [`SILENT_NS`] once it is
//! [`YOUNG_NS`] old is stopped and its channels resolved again. A channel
//! whose subscription changes leaves its flow; phase B sends no update
//! (0x0102): a flow keeps the slots it was asked with.
//!
//! **Statuses** ([`SubStatus`], inferno's codes): a subscribed channel is
//! Unresolved without a flow (or TX fail / too many TX flows after its last
//! request failed), InProgress while its flow is asked for or quiet, and
//! Receiving while its flow's audio arrives. No multicast in phase B.
//!
//! The RX thread learns the flows through [`Manager::rx_plan`] and
//! [`Manager::rx_route`] (handed over by `RxShared::publish`). Times are the
//! control side's ns.

pub mod directory;
pub mod resolve;

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddrV4};

use crate::clock::to_samples;
use crate::cmc::PROCESS_ID;
use crate::device::{DeviceConfig, Subscription};
use crate::flows::client::ClientError;
use crate::flows::control::{FlowRequest, Handle, status};
use crate::media::packet::MEDIA_HEADER;
use crate::media::rx::{RxPlan, RxRoute};
use crate::media::tx::{FPP_MAX, MAX_CHANNELS, sendable};
use crate::wire::MAX_DATAGRAM;
use resolve::{Advertised, Resolver};

pub use crate::media::rx::MAX_FLOWS;

/// How often the control side calls [`Manager::tick`].
pub const TICK_NS: u64 = 250_000_000;
/// [`TICK_NS`] as a duration.
pub const TICK: std::time::Duration = std::time::Duration::from_nanos(TICK_NS);
/// The least time between two requests to one transmitter (inferno's 9 s
/// between resolves).
pub const RETRY_NS: u64 = 9_000_000_000;
/// The most time between two requests to a transmitter that keeps failing.
pub const RETRY_MAX_NS: u64 = 72_000_000_000;
/// A request without an answer for this long has failed (the client waits
/// 3 s; this catches an answer that was never handed over).
pub const GIVE_UP_NS: u64 = 4_000_000_000;
/// A flow without a packet for this long is silent (inferno's 2 s).
pub const SILENT_NS: u64 = 2_000_000_000;
/// A flow younger than this is never stopped for silence (inferno's 6 s).
pub const YOUNG_NS: u64 = 6_000_000_000;

/// The wait before the next request to a transmitter after `failures`
/// failures in a row: 9 s for none or one, then doubling to 72 s.
pub fn retry_after(failures: u32) -> u64 {
    (RETRY_NS << failures.saturating_sub(1).min(3)).min(RETRY_MAX_NS)
}

/// A subscribed channel's state, with inferno's codes (ARC 0x3000).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubStatus {
    Unresolved,
    InProgress,
    TooManyTxFlows,
    TxFail,
    /// Receiving unicast.
    Receiving,
}

impl SubStatus {
    /// The code Dante Controller reads.
    pub fn code(self) -> u32 {
        match self {
            SubStatus::Unresolved => 1,
            SubStatus::InProgress => 8,
            SubStatus::TooManyTxFlows => 0x14,
            SubStatus::TxFail => 0x15,
            SubStatus::Receiving => 0x0101_0009,
        }
    }
}

/// One flow: its slot and which of the slot's flows it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowId {
    pub slot: usize,
    pub generation: u32,
}

/// A datagram to send for the manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Ask the transmitter's flow control `to` for the flow `req`; hand
    /// the answer to [`Manager::answered`] with `flow`.
    Request {
        flow: FlowId,
        to: SocketAddrV4,
        req: FlowRequest,
    },
    /// Tell the transmitter's flow control `to` to end the flow `handle`
    /// (once; its answer changes nothing).
    Stop { to: SocketAddrV4, handle: Handle },
}

/// A request's answer, as the manager reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Granted(Handle),
    /// The transmitter's status.
    Refused(u16),
    /// No answer, or a socket error.
    Failed,
}

impl Answer {
    /// The client's result.
    pub fn of(r: &Result<Handle, ClientError>) -> Answer {
        match r {
            Ok(h) => Answer::Granted(*h),
            Err(ClientError::Refused(status)) => Answer::Refused(*status),
            Err(_) => Answer::Failed,
        }
    }
}

/// One of our flows.
#[derive(Debug, Clone)]
struct Flow {
    generation: u32,
    /// The transmitter's flow control.
    to: SocketAddrV4,
    /// Its TX channel in each slot.
    slots: Vec<Option<u16>>,
    bits: u32,
    fpp: u16,
    latency_ns: u64,
    /// When it was asked for.
    since: u64,
    /// The transmitter's handle; `None` while asked for.
    handle: Option<Handle>,
    /// When its slot last received a packet.
    heard: Option<u64>,
    /// The slot's packet count when last looked at.
    packets: u64,
}

/// A transmitter we ask, by address.
#[derive(Debug, Clone, Copy, Default)]
struct Peer {
    in_flight: Option<FlowId>,
    last_request: Option<u64>,
    /// Failures in a row.
    failures: u32,
}

/// Where a channel's audio comes from: its flow's slot and channel slot,
/// for the subscription it was routed for.
#[derive(Debug, Clone)]
struct Route {
    flow: FlowId,
    slot: usize,
    sub: Subscription,
}

/// A flow as the ARC lists show it (plan Task 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowView {
    pub id: FlowId,
    /// Our flow slot's UDP port, where the audio comes.
    pub port: u16,
    /// The transmitter's flow control.
    pub to: SocketAddrV4,
    /// `None` while asked for.
    pub handle: Option<Handle>,
    /// The TX channel of each channel slot.
    pub channels: Vec<Option<u16>>,
    /// Our RX channels (from 0) each channel slot feeds.
    pub rx: Vec<Vec<usize>>,
    pub bits: u32,
    pub fpp: u16,
    pub latency_ns: u64,
}

/// Our RX subscriptions and their flows.
#[derive(Debug)]
pub struct Manager {
    host: String,
    ip: Ipv4Addr,
    ports: [u16; MAX_FLOWS],
    rate: u32,
    latency_ns: u64,
    subs: Vec<Option<Subscription>>,
    routes: Vec<Option<Route>>,
    /// A channel's last request failed this way (until it is routed again
    /// or its subscription changes).
    failed: Vec<Option<SubStatus>>,
    flows: Vec<Option<Flow>>,
    /// When each flow slot was last freed: a new flow takes the slot freed
    /// longest ago (a transmitter still sending to an old flow's port finds
    /// no new flow there).
    freed: [Option<u64>; MAX_FLOWS],
    generation: u32,
    peers: BTreeMap<Ipv4Addr, Peer>,
    /// Requests given up on: a grant that comes after all is stopped.
    abandoned: Vec<(FlowId, SocketAddrV4)>,
    /// The statuses [`Manager::changed`] reported last.
    reported: Vec<Option<SubStatus>>,
    /// The last tick's time.
    now: u64,
}

impl Manager {
    /// The manager of the device `cfg` named `host` at `ip`, whose flow
    /// slots receive on `ports`; no subscription yet.
    pub fn new(cfg: &DeviceConfig, host: &str, ip: Ipv4Addr, ports: [u16; MAX_FLOWS]) -> Self {
        let n = cfg.rx.len();
        Manager {
            host: host.to_string(),
            ip,
            ports,
            rate: cfg.sample_rate,
            latency_ns: cfg.latency_ns,
            subs: vec![None; n],
            routes: vec![None; n],
            failed: vec![None; n],
            flows: vec![None; MAX_FLOWS],
            freed: [None; MAX_FLOWS],
            generation: 0,
            peers: BTreeMap::new(),
            abandoned: Vec::new(),
            reported: vec![None; n],
            now: 0,
        }
    }

    /// Sets RX channel `rx`'s subscription (from 0; `None` removes it);
    /// `false` for a channel we do not have. The flows follow at the next
    /// tick.
    pub fn set(&mut self, rx: usize, sub: Option<Subscription>) -> bool {
        let Some(slot) = self.subs.get_mut(rx) else {
            return false;
        };
        if *slot != sub {
            *slot = sub;
            self.failed[rx] = None;
        }
        true
    }

    /// Every subscription at once (the state file's at start).
    pub fn load(&mut self, subs: &[Option<Subscription>]) {
        for (rx, sub) in subs.iter().enumerate() {
            self.set(rx, sub.clone());
        }
    }

    /// Our address: where every flow slot receives (ARC 0x3200).
    pub fn ip(&self) -> Ipv4Addr {
        self.ip
    }

    /// RX channel `rx`'s subscription.
    pub fn subscription(&self, rx: usize) -> Option<&Subscription> {
        self.subs.get(rx)?.as_ref()
    }

    /// Whether any RX channel is subscribed (the mDNS browse starts then).
    pub fn subscribed(&self) -> bool {
        self.subs.iter().any(Option::is_some)
    }

    /// One round at `now`: `packets` holds each flow slot's packet count
    /// (`RxShared::packets`), `resolver` the channel records known. Flows
    /// that heard packets are alive; requests without an answer for
    /// [`GIVE_UP_NS`] have failed; flows no channel uses, and flows silent
    /// for [`SILENT_NS`] once [`YOUNG_NS`] old, are stopped; channels
    /// without a flow are resolved, routed to a flow that carries their TX
    /// channel already, or asked for (one request per transmitter allowed
    /// now).
    pub fn tick(
        &mut self,
        now: u64,
        packets: &[u64; MAX_FLOWS],
        resolver: &dyn Resolver,
    ) -> Vec<Action> {
        self.now = now;
        let mut out = Vec::new();
        self.hear(now, packets);
        self.give_up(now);
        // Channels whose subscription changed leave their flow.
        for (route, sub) in self.routes.iter_mut().zip(&self.subs) {
            if route.as_ref().is_some_and(|r| Some(&r.sub) != sub.as_ref()) {
                *route = None;
            }
        }
        self.close(now, &mut out);
        let pending = self.resolve(resolver);
        for (ip, waiting) in pending {
            if self.may_request(ip, now)
                && let Some(action) = self.request(&waiting, now, packets)
            {
                out.push(action);
            }
        }
        self.prune(now);
        out
    }

    /// The answer to the request of `flow`, at `now`. A grant for a request
    /// given up on, or for a slot that holds another flow by now, is
    /// stopped at once.
    pub fn answered(&mut self, flow: FlowId, answer: Answer, now: u64) -> Vec<Action> {
        self.now = self.now.max(now);
        for peer in self.peers.values_mut() {
            if peer.in_flight == Some(flow) {
                peer.in_flight = None;
            }
        }
        let current = self.flows[flow.slot]
            .as_ref()
            .is_some_and(|f| f.generation == flow.generation && f.handle.is_none());
        if !current {
            let stale = self.abandoned.iter().position(|(id, _)| *id == flow);
            let to = stale.map(|at| self.abandoned.remove(at).1);
            return match (answer, to) {
                (Answer::Granted(handle), Some(to)) => vec![Action::Stop { to, handle }],
                _ => Vec::new(),
            };
        }
        match answer {
            Answer::Granted(handle) => {
                if let Some(f) = self.flows[flow.slot].as_mut() {
                    f.handle = Some(handle);
                }
            }
            Answer::Refused(status::TOO_MANY_TX_FLOWS) => {
                self.fail(flow.slot, SubStatus::TooManyTxFlows, now)
            }
            Answer::Refused(_) | Answer::Failed => self.fail(flow.slot, SubStatus::TxFail, now),
        }
        Vec::new()
    }

    /// Shutdown: a stop for every flow we were granted; every flow and
    /// route dropped. A flow still asked for gets no keepalive, so it
    /// expires at its transmitter, and a grant of it that still comes is
    /// stopped ([`Manager::answered`]).
    pub fn stop_all(&mut self) -> Vec<Action> {
        let out = self
            .flows
            .iter()
            .flatten()
            .filter_map(|f| f.handle.map(|handle| Action::Stop { to: f.to, handle }))
            .collect();
        for slot in 0..MAX_FLOWS {
            self.abandon(slot);
            self.free(slot, self.now);
        }
        out
    }

    /// RX channel `rx`'s status; `None` when it is not subscribed. A route
    /// made for another subscription (the channel's changed since the last
    /// tick, which drops it) is none.
    pub fn status(&self, rx: usize) -> Option<SubStatus> {
        let sub = self.subs.get(rx)?.as_ref()?;
        let Some(route) = self.routes[rx].as_ref().filter(|r| r.sub == *sub) else {
            return Some(self.failed[rx].unwrap_or(SubStatus::Unresolved));
        };
        let flow = self.flows[route.flow.slot].as_ref()?;
        let fresh = flow
            .heard
            .is_some_and(|h| self.now.saturating_sub(h) < SILENT_NS);
        Some(if flow.handle.is_some() && fresh {
            SubStatus::Receiving
        } else {
            SubStatus::InProgress
        })
    }

    /// The RX channels whose status changed since the last call (the
    /// channel-change notification, plan Task 6).
    pub fn changed(&mut self) -> Vec<usize> {
        let mut out = Vec::new();
        for rx in 0..self.reported.len() {
            let now = self.status(rx);
            if self.reported[rx] != now {
                self.reported[rx] = now;
                out.push(rx);
            }
        }
        out
    }

    /// What the RX thread receives in flow slot `slot`.
    pub fn rx_plan(&self, slot: usize) -> RxPlan {
        let Some(Some(f)) = self.flows.get(slot) else {
            return RxPlan::default();
        };
        RxPlan {
            src: Some(*f.to.ip()),
            bps: (f.bits / 8) as u8,
            nslots: f.slots.len() as u8,
            latency: u32::try_from(to_samples(f.latency_ns, self.rate)).unwrap_or(u32::MAX),
            live: f.handle.is_some(),
            generation: f.generation,
        }
    }

    /// Where RX channel `rx`'s samples come from.
    pub fn rx_route(&self, rx: usize) -> Option<RxRoute> {
        let r = self.routes.get(rx)?.as_ref()?;
        Some(RxRoute {
            flow: r.flow.slot,
            slot: r.slot,
        })
    }

    /// The flow in flow slot `slot`.
    pub fn flow(&self, slot: usize) -> Option<FlowView> {
        let f = self.flows.get(slot)?.as_ref()?;
        let mut rx = vec![Vec::new(); f.slots.len()];
        for (channel, r) in self.routes.iter().enumerate() {
            if let Some(r) = r.as_ref().filter(|r| r.flow.slot == slot) {
                rx[r.slot].push(channel);
            }
        }
        Some(FlowView {
            id: FlowId {
                slot,
                generation: f.generation,
            },
            port: self.ports[slot],
            to: f.to,
            handle: f.handle,
            channels: f.slots.clone(),
            rx,
            bits: f.bits,
            fpp: f.fpp,
            latency_ns: f.latency_ns,
        })
    }

    /// Flows whose slot received packets since the last look heard them
    /// now; audio from a granted flow ends its transmitter's failures.
    fn hear(&mut self, now: u64, packets: &[u64; MAX_FLOWS]) {
        for (slot, flow) in self.flows.iter_mut().enumerate() {
            let Some(f) = flow.as_mut() else { continue };
            if packets[slot] != f.packets {
                f.packets = packets[slot];
                f.heard = Some(now);
                if f.handle.is_some()
                    && let Some(peer) = self.peers.get_mut(f.to.ip())
                {
                    peer.failures = 0;
                }
            }
        }
    }

    /// Requests without an answer for [`GIVE_UP_NS`] have failed.
    fn give_up(&mut self, now: u64) {
        for slot in 0..MAX_FLOWS {
            let Some(f) = &self.flows[slot] else { continue };
            if f.handle.is_none() && now.saturating_sub(f.since) >= GIVE_UP_NS {
                self.abandon(slot);
                self.fail(slot, SubStatus::TxFail, now);
            }
        }
    }

    /// Flow slot `slot`'s request is no longer waited for: a grant that
    /// still comes is stopped ([`Manager::answered`]); its transmitter may
    /// be asked again. The last [`MAX_FLOWS`] are kept.
    fn abandon(&mut self, slot: usize) {
        let Some(f) = self.flows[slot].as_ref().filter(|f| f.handle.is_none()) else {
            return;
        };
        let id = FlowId {
            slot,
            generation: f.generation,
        };
        if self.abandoned.len() >= MAX_FLOWS {
            self.abandoned.remove(0);
        }
        self.abandoned.push((id, f.to));
        if let Some(peer) = self.peers.get_mut(f.to.ip())
            && peer.in_flight == Some(id)
        {
            peer.in_flight = None;
        }
    }

    /// Stops the granted flows no channel uses, and those silent once old
    /// enough (a failure: their channels are resolved again).
    fn close(&mut self, now: u64, out: &mut Vec<Action>) {
        for slot in 0..MAX_FLOWS {
            let Some(f) = &self.flows[slot] else { continue };
            let Some(handle) = f.handle else { continue };
            let used = self.routes.iter().flatten().any(|r| r.flow.slot == slot);
            let silent = f.heard.is_none_or(|h| now.saturating_sub(h) >= SILENT_NS)
                && now.saturating_sub(f.since) >= YOUNG_NS;
            if used && !silent {
                continue;
            }
            out.push(Action::Stop { to: f.to, handle });
            if used && let Some(peer) = self.peers.get_mut(f.to.ip()) {
                peer.failures += 1;
            }
            self.free(slot, now);
        }
    }

    /// The subscribed channels without a flow: each routed to a flow that
    /// carries its TX channel already, or listed under its transmitter.
    fn resolve(&mut self, resolver: &dyn Resolver) -> BTreeMap<Ipv4Addr, Vec<(usize, Advertised)>> {
        let mut pending: BTreeMap<Ipv4Addr, Vec<(usize, Advertised)>> = BTreeMap::new();
        for rx in 0..self.subs.len() {
            let Some(sub) = self.subs[rx].as_ref().filter(|_| self.routes[rx].is_none()) else {
                continue;
            };
            let Some(a) = resolver.lookup(&sub.tx_device, &sub.tx_channel) else {
                continue;
            };
            if a.rate != self.rate {
                // It would refuse (0x0301): never asked.
                self.failed[rx] = Some(SubStatus::TxFail);
                continue;
            }
            match self.carrying(&a) {
                Some((flow, slot)) => self.route(rx, flow, slot),
                None => pending.entry(*a.control.ip()).or_default().push((rx, a)),
            }
        }
        pending
    }

    /// A flow from `a`'s transmitter that carries its TX channel, and the
    /// channel slot.
    fn carrying(&self, a: &Advertised) -> Option<(FlowId, usize)> {
        self.flows.iter().enumerate().find_map(|(slot, f)| {
            let f = f.as_ref().filter(|f| f.to.ip() == a.control.ip())?;
            let at = f.slots.iter().position(|&s| s == Some(a.tx_channel_id))?;
            Some((
                FlowId {
                    slot,
                    generation: f.generation,
                },
                at,
            ))
        })
    }

    /// Routes RX channel `rx` to channel slot `slot` of `flow`.
    fn route(&mut self, rx: usize, flow: FlowId, slot: usize) {
        if let Some(sub) = self.subs[rx].clone() {
            self.routes[rx] = Some(Route { flow, slot, sub });
            self.failed[rx] = None;
        }
    }

    /// Whether the transmitter at `ip` may be asked now.
    fn may_request(&self, ip: Ipv4Addr, now: u64) -> bool {
        self.peers.get(&ip).is_none_or(|p| {
            p.in_flight.is_none()
                && p.last_request
                    .is_none_or(|t| now >= t.saturating_add(retry_after(p.failures)))
        })
    }

    /// One flow for the first channels `waiting` (one transmitter's, in RX
    /// order) needs, and its request; `None` when the transmitter cannot be
    /// asked (its records disagree, no frame count fits, no flow slot is
    /// free).
    fn request(
        &mut self,
        waiting: &[(usize, Advertised)],
        now: u64,
        packets: &[u64; MAX_FLOWS],
    ) -> Option<Action> {
        let first = waiting.first()?.1;
        let format = |a: &Advertised| (a.nchan, a.bits, a.dbcp1);
        if waiting.iter().any(|(_, a)| format(a) != format(&first)) {
            return None;
        }
        let n = usize::from(first.nchan)
            .min(MAX_CHANNELS)
            .min(self.subs.len());
        let Some(fpp) = fpp_for(&first, n) else {
            for (rx, _) in waiting {
                self.failed[*rx] = Some(SubStatus::TxFail);
            }
            return None;
        };
        let slot = self.free_slot()?;
        // Each TX channel once, in RX order; the first n fill the slots, the
        // rest wait for the next flow.
        let mut ids: Vec<Option<u16>> = Vec::with_capacity(waiting.len());
        for (_, a) in waiting {
            if !ids.contains(&Some(a.tx_channel_id)) {
                ids.push(Some(a.tx_channel_id));
            }
        }
        ids.resize(n, None);
        self.generation = self.generation.wrapping_add(1);
        let id = FlowId {
            slot,
            generation: self.generation,
        };
        self.flows[slot] = Some(Flow {
            generation: id.generation,
            to: first.control,
            slots: ids.clone(),
            bits: first.bits,
            fpp,
            latency_ns: first.latency_ns.max(self.latency_ns),
            since: now,
            handle: None,
            heard: None,
            packets: packets[slot],
        });
        for (rx, a) in waiting {
            if let Some(at) = ids.iter().position(|&s| s == Some(a.tx_channel_id)) {
                self.route(*rx, id, at);
            }
        }
        let peer = self.peers.entry(*first.control.ip()).or_default();
        peer.in_flight = Some(id);
        peer.last_request = Some(now);
        Some(Action::Request {
            flow: id,
            to: first.control,
            req: FlowRequest {
                rx_host: self.host.clone(),
                rx_flow_name: format!("{}_{PROCESS_ID}", slot + 1),
                sample_rate: self.rate,
                bits: first.bits,
                channels: ids,
                fpp,
                rx: SocketAddrV4::new(self.ip, self.ports[slot]),
            },
        })
    }

    /// The free flow slot freed longest ago (never used first, then the
    /// lowest).
    fn free_slot(&self) -> Option<usize> {
        (0..MAX_FLOWS)
            .filter(|&s| self.flows[s].is_none())
            .min_by_key(|&s| (self.freed[s], s))
    }

    /// The request of flow slot `slot` failed with `why`: its channels
    /// leave it (and show `why`), its transmitter counts a failure.
    fn fail(&mut self, slot: usize, why: SubStatus, now: u64) {
        let Some(f) = &self.flows[slot] else { return };
        let ip = *f.to.ip();
        for (route, failed) in self.routes.iter_mut().zip(self.failed.iter_mut()) {
            if route.as_ref().is_some_and(|r| r.flow.slot == slot) {
                *route = None;
                *failed = Some(why);
            }
        }
        let peer = self.peers.entry(ip).or_default();
        peer.failures += 1;
        self.free(slot, now);
    }

    /// Flow slot `slot` holds no flow from `now`; its channels have none.
    fn free(&mut self, slot: usize, now: u64) {
        if self.flows[slot].take().is_some() {
            self.freed[slot] = Some(now);
        }
        for route in &mut self.routes {
            if route.as_ref().is_some_and(|r| r.flow.slot == slot) {
                *route = None;
            }
        }
    }

    /// Forgets transmitters that have nothing pending: no flow (a request
    /// in flight is one), no failure, and their wait over.
    fn prune(&mut self, now: u64) {
        let flows = &self.flows;
        self.peers.retain(|ip, p| {
            flows.iter().flatten().any(|f| f.to.ip() == ip)
                || p.failures > 0
                || p.last_request
                    .is_some_and(|t| now < t.saturating_add(RETRY_NS))
        });
    }
}

/// The frames per packet to ask `a`'s transmitter for in a flow of `n`
/// slots: its most, at most [`FPP_MAX`], within one datagram; `None` when
/// that is below its least.
pub fn fpp_for(a: &Advertised, n: usize) -> Option<u16> {
    let bps = usize::try_from(a.bits / 8).ok()?;
    let fit = (MAX_DATAGRAM - MEDIA_HEADER) / n.checked_mul(bps).filter(|&b| b > 0)?;
    let fpp = usize::from(a.fpp_max.min(FPP_MAX)).min(fit);
    let fpp = u16::try_from(fpp).ok()?;
    (fpp >= a.fpp_min.max(1) && sendable(n, bps, fpp)).then_some(fpp)
}

#[cfg(test)]
mod tests;
