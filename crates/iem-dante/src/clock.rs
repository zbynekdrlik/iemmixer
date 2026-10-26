// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The media clock (Gen 3 phase B, plan Task 1; design §5.2, §5.5): PTP
//! time on the box, read from dantesync, the box's one PTP owner (a
//! follower). The device never opens UDP 319/320 and never sends PTP.
//!
//! dantesync does not keep `CLOCK_REALTIME` at PTP time: it holds
//! `wall = PTP + D`, `D` a fleet date offset that steps (whole 200 ms a
//! night, or a step or slew every ~20 s in its micro mode). So:
//!
//! - PTP ns = `CLOCK_MONOTONIC` + k. `CLOCK_MONOTONIC` runs at the rate
//!   dantesync's frequency word gives the kernel clock (the grandmaster's)
//!   and no step moves it.
//! - k = (`CLOCK_REALTIME` − D) − `CLOCK_MONOTONIC` ([`sample`]), with D
//!   from dantesync's time server ([`dsyx`]) once a second. A D step moves
//!   the wall and D together, so k stays.
//! - A reading counts only while dantesync is locked, no step is due within
//!   [`STEP_GUARD_NS`] and no slew runs (a slew moves the frequency word,
//!   so `CLOCK_MONOTONIC` leaves PTP time while it runs).
//! - [`Tracker`]: the first usable k anchors the clock; a k off by more than
//!   [`TOLERANCE_NS`] must hold for [`CONFIRM`] polls in a row before it
//!   replaces the old one (a grandmaster change, a re-acquisition): a
//!   re-anchor, counted by the clock's [`PtpClock::epoch`], after which the
//!   media threads re-align. A poll that cannot be used after the anchor is
//!   holdover: k stays.
//! - Media time = PTP ns × rate / 10⁹ ([`to_samples`], in `u128`); a
//!   packet's stamp is its seconds (mod 2³²) and the samples into that
//!   second ([`stamp`], [`from_stamp`]).
//!
//! dantesync measures no path delay, so this clock trails the grandmaster
//! by the one-way delay (0.1–0.5 ms): the safe side for TX, and it shows in
//! the RX lateness. Phase D replaces dantesync with our own follower.

pub mod dsyx;

#[cfg(target_os = "linux")]
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicI32, AtomicI64};

pub use dsyx::{DateState, Reading};

/// Two readings whose k differ by at most this agree (20 µs: far above
/// dantesync's reply jitter, two clock reads microseconds apart, and below
/// anything a receiver's latency notices).
pub const TOLERANCE_NS: i64 = 20_000;
/// The polls in a row a different k must hold to replace the anchored one.
pub const CONFIRM: u32 = 3;
/// No reading counts within this of a date step's instant, before or after
/// (dantesync applies a step in its next loop, and our reads of the wall
/// and of D may fall on either side of it).
pub const STEP_GUARD_NS: i64 = 2_000_000_000;

/// One reading as the tracker sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sample {
    /// A k the tracker may use.
    Usable { k_ns: i64 },
    /// dantesync is not locked.
    NotLocked,
    /// dantesync holds no date offset (the reply has no extension).
    NoDateState,
    /// A date step's instant is within [`STEP_GUARD_NS`].
    StepNear,
    /// A date slew runs.
    Slewing,
    /// dantesync did not answer the poll in time.
    NoReply,
}

/// What a reading `r` gives, with the box's `CLOCK_REALTIME` and
/// `CLOCK_MONOTONIC` read right after it arrived (back to back).
pub fn sample(r: &Reading, wall_ns: i64, mono_ns: i64) -> Sample {
    if !r.locked {
        return Sample::NotLocked;
    }
    let Some(date) = r.date else {
        return Sample::NoDateState;
    };
    if date.slewing {
        return Sample::Slewing;
    }
    let ptp_now = wall_ns.wrapping_sub(date.d_ns);
    if date
        .step_at_ptp_ns
        .is_some_and(|at| at.abs_diff(ptp_now) <= STEP_GUARD_NS.unsigned_abs())
    {
        return Sample::StepNear;
    }
    Sample::Usable {
        k_ns: ptp_now.wrapping_sub(mono_ns),
    }
}

/// What one poll did to the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The first k: the clock is ready.
    Ready { k_ns: i64 },
    /// k stays: the poll agreed, or a different k is held (not yet
    /// [`CONFIRM`] polls), or holdover ended.
    Held,
    /// A different k held for [`CONFIRM`] polls and replaced the old one.
    ReAnchored { delta_ns: i64 },
    /// After the anchor, a poll that cannot be used: k stays.
    Holdover,
    /// Before the anchor, a poll that cannot be used.
    NotReady,
}

/// The decisions on k ([`Event`]); the poller applies them to the clock.
#[derive(Debug, Default, Clone)]
pub struct Tracker {
    k: Option<i64>,
    /// A different k and the polls in a row it held.
    candidate: Option<(i64, u32)>,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// The anchored k.
    pub fn k(&self) -> Option<i64> {
        self.k
    }

    /// One poll's sample: what it does to k.
    pub fn feed(&mut self, s: Sample) -> Event {
        let Sample::Usable { k_ns } = s else {
            self.candidate = None;
            return if self.k.is_some() {
                Event::Holdover
            } else {
                Event::NotReady
            };
        };
        let Some(k) = self.k else {
            self.k = Some(k_ns);
            return Event::Ready { k_ns };
        };
        if agree(k_ns, k) {
            self.candidate = None;
            return Event::Held;
        }
        let (first, n) = match self.candidate {
            Some((first, n)) if agree(k_ns, first) => (first, n + 1),
            _ => (k_ns, 1),
        };
        if n < CONFIRM {
            self.candidate = Some((first, n));
            return Event::Held;
        }
        self.k = Some(k_ns);
        self.candidate = None;
        Event::ReAnchored {
            delta_ns: k_ns.wrapping_sub(k),
        }
    }
}

/// Two k agree within [`TOLERANCE_NS`].
fn agree(a: i64, b: i64) -> bool {
    a.abs_diff(b) <= TOLERANCE_NS.unsigned_abs()
}

/// One poll of dantesync and what it did (the poller's step; the
/// `iem-dante-dev clock` lines are made of these).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Polled {
    /// dantesync's reply; `None` when it did not answer in time.
    pub reading: Option<Reading>,
    /// What the reply gave the tracker.
    pub sample: Sample,
    /// What it did to k.
    pub event: Event,
    /// The anchored k after the poll; `None` before the clock is ready.
    pub k_ns: Option<i64>,
}

/// PTP time for the media threads.
pub trait PtpClock: Send + Sync {
    /// PTP ns now; `None` before the clock is ready.
    fn now_ns(&self) -> Option<u64>;
    /// Sleeps until PTP ns `ptp_ns` (at once when it is past). It may
    /// return early (a signal, or a clock that is not ready: then after at
    /// most [`NOT_READY_NAP_NS`]): callers read [`PtpClock::now_ns`] again.
    fn sleep_until(&self, ptp_ns: u64);
    /// The re-anchors so far: a change tells the media threads to re-align.
    fn epoch(&self) -> u32;
}

/// How long [`PtpClock::sleep_until`] waits on a clock that is not ready.
pub const NOT_READY_NAP_NS: u64 = 10_000_000;

/// A clock for tests: ready from its start time, and time moves only in
/// [`PtpClock::sleep_until`] (to the time asked), [`ManualClock::advance`]
/// and [`ManualClock::re_anchor`].
#[derive(Debug)]
pub struct ManualClock {
    now: AtomicU64,
    ready: AtomicBool,
    epoch: AtomicU32,
}

impl ManualClock {
    /// A ready clock at `start_ns`.
    pub fn new(start_ns: u64) -> Self {
        ManualClock {
            now: AtomicU64::new(start_ns),
            ready: AtomicBool::new(true),
            epoch: AtomicU32::new(0),
        }
    }

    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::SeqCst);
    }

    /// Moves time on by `ns`.
    pub fn advance(&self, ns: u64) {
        self.now.fetch_add(ns, Ordering::SeqCst);
    }

    /// A re-anchor: time jumps by `delta_ns` and the epoch counts it.
    pub fn re_anchor(&self, delta_ns: i64) {
        // Two's complement: adding the bits of a negative delta subtracts.
        self.now.fetch_add(delta_ns as u64, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }
}

impl PtpClock for ManualClock {
    fn now_ns(&self) -> Option<u64> {
        self.ready
            .load(Ordering::SeqCst)
            .then(|| self.now.load(Ordering::SeqCst))
    }

    fn sleep_until(&self, ptp_ns: u64) {
        self.now.fetch_max(ptp_ns, Ordering::SeqCst);
    }

    fn epoch(&self) -> u32 {
        self.epoch.load(Ordering::SeqCst)
    }
}

/// Samples at `rate` in `ptp_ns` (rounded down; `u128`, so years of a
/// grandmaster's uptime never overflow). A rate of 0 reads as 1.
pub fn to_samples(ptp_ns: u64, rate: u32) -> u64 {
    (u128::from(ptp_ns) * u128::from(rate.max(1)) / 1_000_000_000) as u64
}

/// A packet's stamp for sample index `samples`: its seconds (mod 2³²) and
/// the samples into that second. A rate of 0 reads as 1.
pub fn stamp(samples: u64, rate: u32) -> (u32, u32) {
    let rate = u64::from(rate.max(1));
    ((samples / rate) as u32, (samples % rate) as u32)
}

/// The sample index of the stamp (`secs`, `sub`) nearest `near` (the stamp
/// repeats every 2³² seconds; the later one on a tie; never below 0). A
/// rate of 0 reads as 1.
pub fn from_stamp(secs: u32, sub: u32, rate: u32, near: u64) -> u64 {
    let rate = i128::from(rate.max(1));
    let period = rate << 32;
    let base = i128::from(secs) * rate + i128::from(sub);
    let repeats = (i128::from(near) - base + period / 2).div_euclid(period);
    let index = base + repeats * period;
    (if index < 0 { index + period } else { index }) as u64
}

#[cfg(target_os = "linux")]
pub use linux::{DANTESYNC, MonoClock, POLL, Poller, monotonic_ns, poll_dantesync, realtime_ns};

#[cfg(target_os = "linux")]
mod linux {
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::time::Duration;

    use tokio::net::UdpSocket;
    use tokio::time::{Instant, MissedTickBehavior};
    use tracing::{debug, info, warn};

    use super::*;

    /// How often dantesync is asked.
    pub const POLL: Duration = Duration::from_secs(1);
    /// dantesync's time server on the box.
    pub const DANTESYNC: SocketAddr =
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, dsyx::PORT));
    /// How long one poll waits for its reply.
    pub const REPLY_TIMEOUT: Duration = Duration::from_millis(200);
    /// k before the clock is ready.
    const NOT_READY: i64 = i64::MIN;

    fn read(clock: libc::clockid_t) -> i64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a valid, writable timespec; clock_gettime only
        // writes it, and a clock id it does not know leaves it zero.
        unsafe { libc::clock_gettime(clock, &mut ts) };
        ts.tv_sec * 1_000_000_000 + ts.tv_nsec
    }

    /// The box's `CLOCK_MONOTONIC`, ns.
    pub fn monotonic_ns() -> i64 {
        read(libc::CLOCK_MONOTONIC)
    }

    /// The box's `CLOCK_REALTIME`, ns since 1970.
    pub fn realtime_ns() -> i64 {
        read(libc::CLOCK_REALTIME)
    }

    /// PTP time as `CLOCK_MONOTONIC` + k, k set by the poller.
    #[derive(Debug)]
    pub struct MonoClock {
        k_ns: AtomicI64,
        epoch: AtomicU32,
    }

    impl Default for MonoClock {
        fn default() -> Self {
            Self::new()
        }
    }

    impl MonoClock {
        /// Not ready until the first k.
        pub fn new() -> Self {
            MonoClock {
                k_ns: AtomicI64::new(NOT_READY),
                epoch: AtomicU32::new(0),
            }
        }

        /// The k in use.
        pub fn k_ns(&self) -> Option<i64> {
            let k = self.k_ns.load(Ordering::Acquire);
            (k != NOT_READY).then_some(k)
        }

        /// Sets k: the first makes the clock ready, a change re-anchors it.
        pub fn set_k(&self, k_ns: i64) {
            let old = self.k_ns.swap(k_ns, Ordering::AcqRel);
            if old != NOT_READY && old != k_ns {
                self.epoch.fetch_add(1, Ordering::Release);
            }
        }
    }

    impl PtpClock for MonoClock {
        fn now_ns(&self) -> Option<u64> {
            let k = self.k_ns()?;
            u64::try_from(i128::from(monotonic_ns()) + i128::from(k)).ok()
        }

        fn sleep_until(&self, ptp_ns: u64) {
            let Some(k) = self.k_ns() else {
                std::thread::sleep(Duration::from_nanos(NOT_READY_NAP_NS));
                return;
            };
            // A time before the box's boot is negative: clock_nanosleep
            // refuses it and returns at once, as for any past time.
            let target = i64::try_from(i128::from(ptp_ns) - i128::from(k)).unwrap_or(i64::MAX);
            let ts = libc::timespec {
                tv_sec: target / 1_000_000_000,
                tv_nsec: target % 1_000_000_000,
            };
            // SAFETY: `ts` is a valid timespec read by the call; the
            // remaining-time pointer may be null with TIMER_ABSTIME.
            unsafe {
                libc::clock_nanosleep(
                    libc::CLOCK_MONOTONIC,
                    libc::TIMER_ABSTIME,
                    &ts,
                    std::ptr::null_mut(),
                )
            };
        }

        fn epoch(&self) -> u32 {
            self.epoch.load(Ordering::Acquire)
        }
    }

    /// Polls dantesync on the box (127.0.0.1, [`dsyx::PORT`]) once a second
    /// and keeps `clock`'s k and `freq` (dantesync's frequency adjustment,
    /// ppb) up to date; runs until the task ends.
    pub async fn poll_dantesync(clock: Arc<MonoClock>, freq: Arc<AtomicI32>) {
        poll(DANTESYNC, clock, freq).await
    }

    /// The poller, aimed at `target` (a stand-in server in the tests).
    /// Glue: every decision is [`sample`]'s and [`Tracker`]'s. Logs name
    /// the clock's state, k and deltas in ns, never an address.
    pub(crate) async fn poll(target: SocketAddr, clock: Arc<MonoClock>, freq: Arc<AtomicI32>) {
        let mut poller = loop {
            match Poller::connect(target).await {
                Ok(p) => break p,
                Err(e) => {
                    warn!(error = %e, "media clock: no socket to dantesync, trying again");
                    tokio::time::sleep(POLL).await;
                }
            }
        };
        info!(
            port = target.port(),
            "media clock: polling dantesync's time server once a second"
        );
        let mut tick = tokio::time::interval(POLL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut last = Event::Held;
        let mut waiting: Option<Sample> = None;
        loop {
            tick.tick().await;
            let p = poller.once(&clock, &freq).await;
            log_event(p.event, last, p.sample, &mut waiting, clock.epoch());
            last = p.event;
        }
    }

    /// One poll at a time of dantesync's time server at a target: the
    /// socket, the request ids and the [`Tracker`]. The service's poller
    /// ([`poll_dantesync`]) and `iem-dante-dev clock` both step it once a
    /// second.
    #[derive(Debug)]
    pub struct Poller {
        socket: UdpSocket,
        tracker: Tracker,
        id: u32,
        buf: [u8; dsyx::REPLY_MAX],
    }

    impl Poller {
        /// A socket on loopback, connected to `target` ([`DANTESYNC`] on
        /// the box): only the target's datagrams reach it.
        pub async fn connect(target: SocketAddr) -> std::io::Result<Self> {
            let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
            socket.connect(target).await?;
            Ok(Poller {
                socket,
                tracker: Tracker::new(),
                // Any start; replies are matched by it.
                id: monotonic_ns() as u32,
                buf: [0u8; dsyx::REPLY_MAX],
            })
        }

        /// One request and its reply (at most [`REPLY_TIMEOUT`]): the
        /// reply's frequency adjustment goes to `freq`, its sample to the
        /// tracker, and a first or a replaced k to `clock`.
        pub async fn once(&mut self, clock: &MonoClock, freq: &AtomicI32) -> Polled {
            self.id = self.id.wrapping_add(1);
            let reply = ask(&self.socket, self.id, &mut self.buf).await;
            let s = match reply {
                Some((r, wall, mono)) => {
                    freq.store(r.freq_ppb, Ordering::Relaxed);
                    sample(&r, wall, mono)
                }
                None => Sample::NoReply,
            };
            let event = self.tracker.feed(s);
            let k_ns = self.tracker.k();
            if let (Event::Ready { .. } | Event::ReAnchored { .. }, Some(k)) = (event, k_ns) {
                clock.set_k(k);
            }
            Polled {
                reading: reply.map(|(r, _, _)| r),
                sample: s,
                event,
                k_ns,
            }
        }
    }

    /// One request and its reply within [`REPLY_TIMEOUT`], with the box's
    /// wall and monotonic clocks read the moment it arrived; a late reply to
    /// an earlier request is skipped.
    async fn ask(socket: &UdpSocket, id: u32, buf: &mut [u8]) -> Option<(Reading, i64, i64)> {
        if let Err(e) = socket.send(&dsyx::request(id)).await {
            debug!(error = %e, "media clock: the request to dantesync was not sent");
            return None;
        }
        let deadline = Instant::now() + REPLY_TIMEOUT;
        loop {
            let n = match tokio::time::timeout_at(deadline, socket.recv(buf)).await {
                Ok(Ok(n)) => n,
                Ok(Err(e)) => {
                    debug!(error = %e, "media clock: no reply from dantesync");
                    return None;
                }
                Err(_) => return None,
            };
            let wall = realtime_ns();
            let mono = monotonic_ns();
            match dsyx::parse(&buf[..n], id) {
                Ok(r) => return Some((r, wall, mono)),
                Err(e) => debug!(error = %e, "media clock: a datagram skipped"),
            }
        }
    }

    /// One line per change of the clock's state.
    fn log_event(ev: Event, last: Event, s: Sample, waiting: &mut Option<Sample>, epoch: u32) {
        match ev {
            Event::Ready { k_ns } => info!(k_ns, "media clock ready: PTP time from dantesync"),
            Event::ReAnchored { delta_ns } => warn!(
                delta_ns,
                epoch, "media clock re-anchored: a different k held for 3 polls"
            ),
            Event::Holdover if last != Event::Holdover => warn!(
                reason = ?s,
                "media clock in holdover: dantesync gives no usable time, k kept"
            ),
            Event::Held if last == Event::Holdover => info!("media clock out of holdover"),
            Event::NotReady if *waiting != Some(s) => {
                info!(reason = ?s, "media clock not ready: waiting for dantesync");
                *waiting = Some(s);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
