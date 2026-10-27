// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The engine's pump (Gen 3 phase C, plan
//! `docs/superpowers/plans/2026-10-26-gen3-phase-c-engine-on-linux.md` Task
//! 1; design §5.2): the engine's `Process` driven by the media clock
//! instead of a sound card's callback. It knows nothing about mixing.
//!
//! - Every [`BLOCK`] (32) frames, block k covers the input stamps
//!   [kB, kB + B) and runs once its last sample's playout time has passed:
//!   media now ≥ kB + B + P, P the device's latency in samples (the playout
//!   point the RX checks already read at). Its inputs are the RX rings at
//!   those stamps (an RX channel that is not routed to a flow is silence
//!   and reads nothing, so it counts no hole), its outputs go into the
//!   engine's TX rings ([`crate::media::tx::TxShared::engine_ring`]) at
//!   kB + D, **D = B + P + W** ([`Timing::offset`]), W the device's
//!   `work_margin` ([`WORK_MARGIN`], 1 ms, unless the config sets it): RX
//!   media time plus a fixed processing offset.
//!   The TX thread sends them as [`crate::media::source::Source::Engine`]:
//!   the packet stamped kB + D leaves at kB + D + fpp + the TX margin
//!   (the device's `tx_margin_ns`, 250 µs by default), so a block
//!   that ends within W of its due time is always in time, whatever either
//!   side's fpp.
//! - [`Blocks`] (pure) says what is due: a block, a wait, or a
//!   re-bootstrap when the pump is more than W behind (the skipped blocks'
//!   stamps go out as silence, counted as TX holes) or more than
//!   [`DISCONTINUITY`] ahead (the clock went back). A start never catches
//!   up: the first block is the one whose input ends at the playout point.
//! - [`Pump`]: the buffers, allocated once; [`Pump::media`] runs every due
//!   block at a media time (the first media step and a new clock epoch
//!   re-bootstrap; the processor's `discontinuity` is called once before
//!   the next block); [`Pump::free`] runs one block on silent inputs with
//!   its outputs dropped, while the clock is not ready, so commands, meters
//!   and the engine's status keep flowing and nothing is sent.
//!   [`Pump::block`] is the real-time part (I7: no allocation, lock, log or
//!   system call but the clock read; `tests/hot_path.rs`, rtsan): `process`
//!   inside `catch_unwind`, a panic zeroes the block and the processor is
//!   never called again (the backends' crash model).
//! - [`PumpStats`]: what the engine's status and the control socket read,
//!   as [`StreamStats`], the stream histograms ([`HistSnapshot`]) and a
//!   [`PumpView`].
//! - [`spawn`]: the `dante-engine` thread (`SCHED_FIFO` [`ENGINE_PRIORITY`]
//!   when allowed): on the media clock while it is ready, paced on its own
//!   ([`free_deadline`], NullRt's rule) while it is not.
//!
//! A sample is a left-justified `i32` (full scale 2³¹, as the rings keep
//! them): [`to_f64`] scales by 2⁻³¹ exactly, [`quantize`] rounds at the
//! flow's bits and saturates at full scale.

use std::any::Any;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use iem_audio_io::hist::{HistSnapshot, StreamHists};
use iem_audio_io::telemetry::period_ns;
use iem_audio_io::{Block, Process, StreamStats, panic_message};
use tracing::{info, warn};

use crate::clock::{PtpClock, to_samples};
use crate::media::ring::Ring;
use crate::media::rx::RxShared;
use crate::media::tx::{DISCONTINUITY, TxShared, ask_realtime, ns_of, place};

/// The engine's block, frames: the B = 32 of the RT contract.
pub const BLOCK: u64 = 32;
/// W: how late a block may run and still be sent in time, samples (1 ms
/// at 96 kHz). A pump later than this re-bootstraps. The default of the
/// device's `work_margin` (#72 phase F sweeps it without a build).
pub const WORK_MARGIN: u64 = 96;
/// The largest `work_margin` a config may set, samples (about 43 ms at
/// 96 kHz): far beyond any useful margin, well inside the rings.
pub const MAX_WORK_MARGIN: u64 = 4096;
/// The pump thread's `SCHED_FIFO` priority when `RLIMIT_RTPRIO` allows it:
/// below the TX and RX threads' 45 and dantesync's 50.
pub const ENGINE_PRIORITY: i32 = 40;
/// Free pacing more than this many blocks behind resynchronises instead
/// of bursting (NullRt's rule).
pub const FREE_RESYNC_BLOCKS: u32 = 8;

/// Full scale of a left-justified sample: 2³¹.
const FULL_SCALE: f64 = 2_147_483_648.0;

/// A left-justified sample as the engine's float: `s` · 2⁻³¹, exact.
pub fn to_f64(s: i32) -> f64 {
    f64::from(s) / FULL_SCALE
}

/// The engine's float as a left-justified sample of `bits` (1 to 32; others
/// read as the nearest): rounded at `bits`, saturating at full scale
/// (never wrapping), NaN as 0.
pub fn quantize(x: f64, bits: u8) -> i32 {
    let bits = bits.clamp(1, 32);
    let top = f64::from(1u32 << (bits - 1));
    // A float to int cast of NaN is 0; the clamp keeps ±∞ at the edges.
    let v = (x * top).round().clamp(-top, top - 1.0);
    ((v as i64) << (32 - bits)) as i32
}

/// Which device channels the engine's inputs and outputs are (indices from
/// 0): engine input i reads RX channel `rx[i]`, engine output j writes TX
/// channel `tx[j]`'s engine ring.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Map {
    pub rx: Vec<usize>,
    pub tx: Vec<usize>,
}

/// The pump's timing, samples. `block` is above 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// B: frames per block ([`BLOCK`]).
    pub block: u64,
    /// P: the playout point behind media now (the device's latency).
    pub playout: u64,
    /// W: the work margin (the device's `work_margin`).
    pub margin: u64,
}

impl Timing {
    /// D = B + P + W: an output stamped T is the input stamped T − D.
    pub fn offset(&self) -> u64 {
        self.block + self.playout + self.margin
    }
}

/// What [`Blocks::next`] says at a media time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// Run the block starting at `start`; `late` when one block or more
    /// behind its due time.
    Run { start: u64, late: bool },
    /// Nothing due before media time `until`.
    Wait { until: u64 },
    /// More than the margin behind, or more than [`DISCONTINUITY`] ahead:
    /// the schedule started again at now, `skipped` blocks never ran.
    Rebootstrap { skipped: u64 },
}

/// The pump's schedule (pure): the next block's start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blocks {
    start: u64,
    timing: Timing,
}

impl Blocks {
    /// The schedule at media time `now`: the first start is the smallest
    /// multiple of the block whose input ends at or after `now − playout`,
    /// so its block is due at or after `now` (no catch-up at start).
    pub fn bootstrap(now: u64, t: Timing) -> Blocks {
        let start = now.saturating_sub(t.playout + t.block).div_ceil(t.block) * t.block;
        Blocks { start, timing: t }
    }

    /// The next block's first input stamp.
    pub fn start(&self) -> u64 {
        self.start
    }

    /// When the next block is due: its last input sample's playout time.
    pub fn due(&self) -> u64 {
        self.start + self.timing.block + self.timing.playout
    }

    /// What is due at media time `now`; a run moves the schedule on by one
    /// block, a re-bootstrap starts it again at `now` (after which nothing
    /// is behind).
    pub fn next(&mut self, now: u64) -> Due {
        let due = self.due();
        if now < due {
            if due - now > DISCONTINUITY {
                return Due::Rebootstrap {
                    skipped: self.rebootstrap(now),
                };
            }
            return Due::Wait { until: due };
        }
        let behind = now - due;
        if behind > self.timing.margin {
            return Due::Rebootstrap {
                skipped: self.rebootstrap(now),
            };
        }
        let start = self.start;
        self.start += self.timing.block;
        Due::Run {
            start,
            late: behind >= self.timing.block,
        }
    }

    /// Starts the schedule again at media time `now` (a lag, a clock that
    /// went back, a new epoch); returns the blocks between the old next
    /// start and the new one that never run (0 when the new one is not
    /// later).
    pub fn rebootstrap(&mut self, now: u64) -> u64 {
        let new = Blocks::bootstrap(now, self.timing);
        let skipped = new.start.saturating_sub(self.start) / self.timing.block;
        *self = new;
        skipped
    }
}

/// The pump's counts: written by its thread, read by the engine's control
/// thread and the device's control socket.
pub struct PumpStats {
    frames: u32,
    offset: u64,
    playout: u64,
    /// One block's period, ns: a block longer than this is an overrun.
    period_ns: u64,
    /// The last step was on the media clock (not free pacing).
    media: AtomicBool,
    running: AtomicBool,
    faulted: AtomicBool,
    blocks: AtomicU64,
    late: AtomicU64,
    missed: AtomicU64,
    overruns: AtomicU64,
    rebootstraps: AtomicU64,
    max_process_ns: AtomicU64,
    /// The faulting block's own time; stored before `faulted`.
    fault_ns: AtomicU64,
    fault: Mutex<Option<String>>,
    hists: StreamHists,
}

/// The pump's numbers for the device's control socket (numbers only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PumpView {
    /// On the media clock (not free pacing).
    pub media: bool,
    /// D, samples.
    pub offset: u64,
    /// P, samples.
    pub playout: u64,
    pub blocks: u64,
    pub late: u64,
    /// Blocks skipped by re-bootstraps.
    pub missed: u64,
    /// Blocks longer than one period.
    pub overruns: u64,
    pub rebootstraps: u64,
    /// The longest block, whole µs.
    pub max_process_us: u64,
}

impl PumpStats {
    /// The counts of a pump with `timing` at `rate` Hz; the histograms are
    /// sized for one block at the rate (allocated here, before the thread).
    pub fn new(timing: Timing, rate: u32) -> Self {
        let frames = u32::try_from(timing.block).unwrap_or(u32::MAX);
        let period = period_ns(frames, f64::from(rate.max(1)));
        PumpStats {
            frames,
            offset: timing.offset(),
            playout: timing.playout,
            period_ns: period,
            media: AtomicBool::new(false),
            running: AtomicBool::new(false),
            faulted: AtomicBool::new(false),
            blocks: AtomicU64::new(0),
            late: AtomicU64::new(0),
            missed: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            rebootstraps: AtomicU64::new(0),
            max_process_ns: AtomicU64::new(0),
            fault_ns: AtomicU64::new(0),
            fault: Mutex::new(None),
            hists: StreamHists::new(period),
        }
    }

    /// One block's own time (the pump thread): counted, the longest kept,
    /// its histogram, an overrun when longer than one period.
    fn record(&self, took_ns: u64) {
        self.blocks.fetch_add(1, Ordering::Relaxed);
        self.max_process_ns.fetch_max(took_ns, Ordering::Relaxed);
        self.hists.process.record(took_ns);
        if took_ns > self.period_ns {
            self.overruns.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A re-bootstrap that skipped `skipped` blocks.
    fn rebootstrapped(&self, skipped: u64) {
        self.rebootstraps.fetch_add(1, Ordering::Relaxed);
        self.missed.fetch_add(skipped, Ordering::Relaxed);
    }

    /// The processor panicked in a block of `took_ns`: its message and its
    /// time, then the flag (a reader that sees the fault sees both).
    fn fault(&self, message: String, took_ns: u64) {
        *self.fault.lock().unwrap_or_else(PoisonError::into_inner) = Some(message);
        self.fault_ns.store(took_ns.max(1), Ordering::Release);
        self.faulted.store(true, Ordering::Release);
    }

    /// The pump as a stream: frames B, callbacks the blocks, late, missed
    /// the skipped blocks, overruns, resets the re-bootstraps; no card, so
    /// nothing parked and no reopen.
    pub fn stream_stats(&self) -> StreamStats {
        let faulted = self.faulted.load(Ordering::Acquire);
        StreamStats {
            frames: self.frames,
            callbacks: self.blocks.load(Ordering::Relaxed),
            late: self.late.load(Ordering::Relaxed),
            missed: self.missed.load(Ordering::Relaxed),
            overruns: self.overruns.load(Ordering::Relaxed),
            resets: self.rebootstraps.load(Ordering::Relaxed),
            parked: false,
            faulted,
            running: self.running.load(Ordering::Acquire),
            max_process_ns: self.max_process_ns.load(Ordering::Relaxed),
            fault: self
                .fault
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            last_reopen_us: 0,
            // Read after `faulted`, which is stored after it.
            fault_callback_ns: self.fault_ns.load(Ordering::Acquire),
        }
    }

    /// The stream histograms since the start: the interval between two
    /// blocks' starts and each block's own time.
    pub fn histograms(&self) -> HistSnapshot {
        self.hists.snapshot()
    }

    /// The numbers the device's control socket shows.
    pub fn view(&self) -> PumpView {
        PumpView {
            media: self.media.load(Ordering::Relaxed),
            offset: self.offset,
            playout: self.playout,
            blocks: self.blocks.load(Ordering::Relaxed),
            late: self.late.load(Ordering::Relaxed),
            missed: self.missed.load(Ordering::Relaxed),
            overruns: self.overruns.load(Ordering::Relaxed),
            rebootstraps: self.rebootstraps.load(Ordering::Relaxed),
            max_process_us: self.max_process_ns.load(Ordering::Relaxed) / 1_000,
        }
    }
}

/// A duration in ns, saturating.
fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// The engine's pump (see the module): every buffer it needs, allocated in
/// [`Pump::new`].
pub struct Pump<P: Process> {
    processor: P,
    map: Map,
    timing: Timing,
    bits: u8,
    rx: Arc<RxShared>,
    tx: Arc<TxShared>,
    stats: Arc<PumpStats>,
    frames: usize,
    /// The engine's inputs and outputs, channel-major.
    input: Vec<f64>,
    output: Vec<f64>,
    /// One channel's samples of a block.
    samples: Vec<i32>,
    /// `None` until the first media step, and after free pacing.
    blocks: Option<Blocks>,
    /// The clock's epoch at the last media step.
    epoch: Option<u32>,
    /// `Process::discontinuity` is due before the next block.
    discontinuity: bool,
    /// The processor panicked: never called again.
    faulted: bool,
    /// The last block's start, for the interval histogram.
    last_entry: Option<Instant>,
}

impl<P: Process> Pump<P> {
    /// A pump of `processor` over `map`'s channels with `timing`, its
    /// outputs quantized at `bits`, reading `rx`'s rings and writing `tx`'s
    /// engine rings, counting into `stats`. Every buffer is allocated here.
    pub fn new(
        processor: P,
        map: Map,
        timing: Timing,
        bits: u8,
        rx: Arc<RxShared>,
        tx: Arc<TxShared>,
        stats: Arc<PumpStats>,
    ) -> Self {
        let frames = usize::try_from(timing.block).unwrap_or(usize::MAX);
        Pump {
            input: vec![0.0; map.rx.len() * frames],
            output: vec![0.0; map.tx.len() * frames],
            samples: vec![0; frames],
            processor,
            map,
            timing,
            bits,
            rx,
            tx,
            stats,
            frames,
            blocks: None,
            epoch: None,
            discontinuity: false,
            faulted: false,
            last_entry: None,
        }
    }

    /// Every block due at media sample time `now` with the clock at
    /// `epoch`; returns the media sample time to wake at. The first media
    /// step (and the first after free pacing) bootstraps, a new epoch
    /// re-bootstraps (counted); either calls the processor's
    /// `discontinuity` once before the next block.
    pub fn media(&mut self, now: u64, epoch: u32) -> u64 {
        self.stats.media.store(true, Ordering::Relaxed);
        let mut blocks = match self.blocks {
            Some(mut b) if self.epoch != Some(epoch) => {
                self.stats.rebootstrapped(b.rebootstrap(now));
                self.discontinuity = true;
                b
            }
            Some(b) => b,
            None => {
                self.discontinuity = true;
                Blocks::bootstrap(now, self.timing)
            }
        };
        self.epoch = Some(epoch);
        let wake = loop {
            match blocks.next(now) {
                Due::Run { start, late } => {
                    if late {
                        self.stats.late.fetch_add(1, Ordering::Relaxed);
                    }
                    self.block(start);
                }
                Due::Wait { until } => break until,
                Due::Rebootstrap { skipped } => {
                    self.stats.rebootstrapped(skipped);
                    self.discontinuity = true;
                }
            }
        };
        self.blocks = Some(blocks);
        wake
    }

    /// One block while the clock is not ready: silent inputs, the RX rings
    /// not read, the outputs dropped (nothing reaches the engine rings);
    /// the next media step bootstraps.
    pub fn free(&mut self) {
        self.stats.media.store(false, Ordering::Relaxed);
        self.blocks = None;
        let entry = self.enter();
        self.input.fill(0.0);
        let fault = self.run_processor();
        self.leave(entry, fault);
    }

    /// The block whose inputs are stamped `start`…`start + B − 1` (the
    /// real-time part): each routed RX channel's ring read there (an
    /// unrouted one is silence and reads nothing), the processor, and each
    /// output quantized into its TX channel's engine ring at `start + D`.
    #[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
    pub fn block(&mut self, start: u64) {
        let entry = self.enter();
        let rings: &[Ring] = self.rx.rings();
        for (chan, &rx) in self.input.chunks_exact_mut(self.frames).zip(&self.map.rx) {
            match rings.get(rx).filter(|_| self.rx.routed(rx)) {
                Some(ring) => {
                    ring.read_at(start, &mut self.samples);
                    for (x, &s) in chan.iter_mut().zip(self.samples.iter()) {
                        *x = to_f64(s);
                    }
                }
                None => chan.fill(0.0),
            }
        }
        let fault = self.run_processor();
        let at = start + self.timing.offset();
        for (chan, &tx) in self.output.chunks_exact(self.frames).zip(&self.map.tx) {
            if let Some(ring) = self.tx.engine_ring(tx) {
                for (q, &y) in self.samples.iter_mut().zip(chan) {
                    *q = quantize(y, self.bits);
                }
                ring.put_at(at, &self.samples);
            }
        }
        self.leave(entry, fault);
    }

    /// A block's start: the interval since the last one's.
    fn enter(&mut self) -> Instant {
        let now = Instant::now();
        if let Some(before) = self.last_entry.replace(now) {
            self.stats
                .hists
                .interval
                .record(nanos(now.saturating_duration_since(before)));
        }
        now
    }

    /// A block's end: its own time recorded, and a fault after it.
    fn leave(&mut self, entry: Instant, fault: Option<Box<dyn Any + Send>>) {
        let took = nanos(entry.elapsed());
        self.stats.record(took);
        if let Some(payload) = fault {
            self.stats.fault(panic_message(&*payload), took);
        }
    }

    /// The processor on the block's buffers (the outputs zeroed first, a
    /// pending discontinuity before it) inside `catch_unwind`. A panic
    /// zeroes the outputs and is returned; from then on the processor is
    /// never called again and the outputs stay zero.
    fn run_processor(&mut self) -> Option<Box<dyn Any + Send>> {
        self.output.fill(0.0);
        if self.faulted {
            return None;
        }
        let discontinuity = std::mem::take(&mut self.discontinuity);
        let Pump {
            processor,
            input,
            output,
            frames,
            ..
        } = self;
        let caught = catch_unwind(AssertUnwindSafe(|| {
            if discontinuity {
                processor.discontinuity();
            }
            processor.process(&mut Block::new(*frames, input, output));
        }));
        let payload = caught.err()?;
        self.output.fill(0.0);
        self.faulted = true;
        Some(payload)
    }
}

/// The deadline of the free block after one due at `deadline`, at `now`: a
/// period later; more than [`FREE_RESYNC_BLOCKS`] periods behind it, `now`
/// (no burst; NullRt's rule).
pub fn free_deadline(deadline: Instant, now: Instant, period: Duration) -> Instant {
    let next = deadline + period;
    if now > next + period * FREE_RESYNC_BLOCKS {
        now
    } else {
        next
    }
}

/// Starts the `dante-engine` thread: `pump` on `clock` until `stop` is set
/// (within one block, or one free period, of it).
pub fn spawn<P: Process + 'static>(
    clock: Arc<dyn PtpClock>,
    pump: Pump<P>,
    stop: Arc<AtomicBool>,
) -> io::Result<JoinHandle<()>> {
    let stats = Arc::clone(&pump.stats);
    stats.running.store(true, Ordering::Release);
    let spawned = std::thread::Builder::new()
        .name("dante-engine".to_string())
        .spawn(move || run(clock.as_ref(), pump, &stop));
    if spawned.is_err() {
        stats.running.store(false, Ordering::Release);
    }
    spawned
}

/// The pump thread's loop (glue: every decision is [`Pump`]'s, [`Blocks`]'
/// and [`free_deadline`]'s): on the media clock while it is ready, paced on
/// `Instant` deadlines a block apart while it is not.
fn run<P: Process>(clock: &dyn PtpClock, mut pump: Pump<P>, stop: &AtomicBool) {
    if ask_realtime(ENGINE_PRIORITY) {
        info!(priority = ENGINE_PRIORITY, "engine pump runs SCHED_FIFO");
    } else {
        warn!(
            priority = ENGINE_PRIORITY,
            "engine pump runs at normal priority: RLIMIT_RTPRIO allows no SCHED_FIFO"
        );
    }
    place("engine pump", pump.tx.cfg.pump_cpu);
    let rate = pump.tx.cfg.sample_rate;
    let period = Duration::from_nanos(pump.stats.period_ns);
    let mut deadline: Option<Instant> = None;
    while !stop.load(Ordering::Acquire) {
        if let Some(now_ns) = clock.now_ns() {
            deadline = None;
            let wake = pump.media(to_samples(now_ns, rate), clock.epoch());
            clock.sleep_until(ns_of(wake, rate));
        } else {
            pump.free();
            let now = Instant::now();
            let next = free_deadline(deadline.unwrap_or(now), now, period);
            deadline = Some(next);
            std::thread::sleep(next.saturating_duration_since(now));
        }
    }
    pump.stats.running.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests;
