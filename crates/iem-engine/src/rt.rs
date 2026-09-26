//! The RT processor (program spec §3.3 A1–A13, §3.4 X1–X4, X13–X15, Q1, I5,
//! I7; design note §3.2, §3.4): one callback renders the whole graph.
//!
//! A block is cut into segments of at most [`SEG`] samples at every command
//! timestamp and test-signal end, and every ramp steps per sample, so the
//! output depends only on sample indices, never on the block size. Nothing
//! here allocates, locks, makes a syscall or logs: buffers are preallocated,
//! commands arrive through an `rtrb` ring, meters leave through a triple
//! buffer and taps through rings.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use iem_audio_io::{Block, Process};
use iem_dsp::eq::Equalizer;
use iem_dsp::meter::PeakMeter;
use iem_dsp::pan::StereoGain;
use iem_dsp::ramp::{EQ_MS, GAIN_MS, MUTE_MS, Ramp, samples};
use iem_dsp::sanitize::Trips;
use iem_engine_proto::{BusKind, MixState, db_to_lin};
use iem_limiter_mga::{DISABLE_MS, Limiter, Mga, Sliders};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::cmd::{RtCmd, RtOp};
use crate::core::reconcile;
use crate::graph::{Graph, Src};
use crate::params::{eq_params, input_params};
use crate::{MAX_CMDS_PER_BLOCK, SAMPLE_RATE, SEG, TALKBACK_GAIN, TEST_CAP};

/// Samples between meter frames: 30 Hz at 96 kHz.
pub const METER_PERIOD: u64 = 3_200;
/// Command ring capacity.
pub const CMD_RING: usize = 4_096;
/// Listen tap rings: 200 ms of interleaved stereo f32 at 96 kHz.
pub const TAP_RING: usize = 2 * 19_200;
/// Talkback ring: 120 ms at 96 kHz (X5 cap).
pub const TALK_RING: usize = 11_520;
/// Engine fade-out on `Shutdown`, and the test signal's fades.
pub const FADE_MS: f64 = 50.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Output fade-in after start (§4.4: 500 ms); 0 starts at full level.
    pub fade_in_ms: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self { fade_in_ms: 500.0 }
    }
}

/// One meter frame: peaks since the previous frame (inputs at the pre-fader
/// tap, buses post-fader), limiter GR in dB and X14 active samples per bus.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MeterFrame {
    pub seq: u64,
    pub inputs: Vec<[f64; 2]>,
    pub buses: Vec<[f64; 2]>,
    pub gr_db: Vec<f64>,
    pub active: Vec<u64>,
    pub trips: u64,
}

/// Counters the control loop reads.
#[derive(Debug, Default)]
pub struct RtStatus {
    pub faded_out: AtomicBool,
    pub trips: AtomicU64,
    pub tap_overruns: AtomicU64,
    pub talkback_underruns: AtomicU64,
    /// Blocks that left commands for the next block (the 512 budget).
    pub deferred: AtomicU64,
}

/// The non-RT ends of the processor's rings.
pub struct RtHandles {
    pub cmds: Producer<RtCmd>,
    pub meters: triple_buffer::Output<MeterFrame>,
    /// Interleaved stereo 96 kHz: slot 0 engineer, slot 1 member (X3).
    pub taps: [Consumer<f32>; 2],
    /// Mono 96 kHz talkback into the talkback input (A4).
    pub talkback: Producer<f32>,
    pub status: Arc<RtStatus>,
}

#[derive(Debug, Clone)]
struct Stereo {
    l: Vec<f64>,
    r: Vec<f64>,
}

impl Stereo {
    fn new() -> Self {
        Self {
            l: vec![0.0; SEG],
            r: vec![0.0; SEG],
        }
    }

    fn get(&self, n: usize) -> (&[f64], &[f64]) {
        (
            self.l.get(..n).unwrap_or_default(),
            self.r.get(..n).unwrap_or_default(),
        )
    }

    fn get_mut(&mut self, n: usize) -> (&mut [f64], &mut [f64]) {
        (
            self.l.get_mut(..n).unwrap_or_default(),
            self.r.get_mut(..n).unwrap_or_default(),
        )
    }
}

fn scale(x: &mut [f64], g: f64) {
    if g == 0.0 {
        x.fill(0.0);
    } else if g != 1.0 {
        for v in x {
            *v *= g;
        }
    }
}

/// Applies a mono gain ramp to both channels.
fn ramp_gain(ramp: &mut Ramp, l: &mut [f64], r: &mut [f64]) {
    if !ramp.is_moving() {
        let g = ramp.value();
        scale(l, g);
        scale(r, g);
        return;
    }
    for (a, b) in l.iter_mut().zip(r.iter_mut()) {
        let g = ramp.tick();
        *a *= g;
        *b *= g;
    }
}

/// Applies a fader/send gain (A5) in place.
fn stereo_gain(g: &mut StereoGain, l: &mut [f64], r: &mut [f64]) {
    if let Some((gl, gr)) = g.steady() {
        scale(l, gl);
        scale(r, gr);
        return;
    }
    for (a, b) in l.iter_mut().zip(r.iter_mut()) {
        let (gl, gr) = g.tick();
        *a *= gl;
        *b *= gr;
    }
}

/// Adds `g · src` to `dst` (a send, or an input's post-fader signal into master).
fn accumulate(g: &mut StereoGain, src: (&[f64], &[f64]), dst: (&mut [f64], &mut [f64])) {
    let ((sl, sr), (dl, dr)) = (src, dst);
    if let Some((gl, gr)) = g.steady() {
        if gl != 0.0 {
            for (d, s) in dl.iter_mut().zip(sl) {
                *d += gl * s;
            }
        }
        if gr != 0.0 {
            for (d, s) in dr.iter_mut().zip(sr) {
                *d += gr * s;
            }
        }
        return;
    }
    for ((a, b), (x, y)) in dl.iter_mut().zip(dr.iter_mut()).zip(sl.iter().zip(sr)) {
        let (gl, gr) = g.tick();
        *a += gl * x;
        *b += gr * y;
    }
}

/// Copies `src` into `dst` element by element (equal lengths by construction).
fn copy(dst: &mut [f64], src: &[f64]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d = *s;
    }
}

fn push_tap(
    p: &mut Producer<f32>,
    (l, r): (&[f64], &[f64]),
    scratch: &mut [f32],
    overruns: &AtomicU64,
) {
    let mut used = 0;
    for (pair, (a, b)) in scratch
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip(l.iter().zip(r))
    {
        *pair = [*a as f32, *b as f32];
        used += 2;
    }
    let (_, rest) = p.push_partial_slice(scratch.get(..used).unwrap_or_default());
    if !rest.is_empty() {
        overruns.fetch_add(1, Ordering::Relaxed);
    }
}

/// The Q1 safety stage: the MGA core at 0 dB, fully linked.
fn safety(sr: f64) -> Mga {
    Mga::new(
        sr,
        Sliders {
            threshold_db: 0.0,
            release_ms: 50.0,
            link_pct: 100.0,
            ceiling_db: 0.0,
        },
    )
}

struct InputRt {
    /// The pre-fader tap P (post-FX, post-mute).
    p: Stereo,
    trim: Ramp,
    /// 1 = processed (trim → EQ), 0 = dry (Q3); crossfades 20 ms.
    proc_mix: Ramp,
    eq: Equalizer<2>,
    /// 1 = open, 0 = muted (A3), 5 ms.
    gate: Ramp,
    /// Track fader and pan into the master (A11).
    fader: StereoGain,
    trips: Trips,
    peak: PeakMeter<2>,
}

/// A bus limiter (A13, §4.4): enabling and lowering instant, raising over 10 ms.
struct BusLimiter {
    lim: Limiter,
    raise: Ramp,
    /// X14 samples counted before this run (Q4: persisted until reset).
    base: u64,
}

impl BusLimiter {
    fn new(sr: f64, enabled: bool, limit_db: f64, base: u64) -> Self {
        let mut lim = Limiter::new(sr, limit_db);
        lim.set_enabled(enabled);
        Self {
            raise: Ramp::new(lim.limit_db(), samples(DISABLE_MS, sr)),
            lim,
            base,
        }
    }

    fn set(&mut self, enabled: bool, limit_db: f64) {
        self.lim.set_enabled(enabled);
        if limit_db <= self.lim.limit_db() {
            self.raise.jump(limit_db);
            self.lim.set_limit_db(limit_db);
        } else {
            self.raise.set(limit_db);
        }
    }

    fn process(&mut self, l: &mut [f64], r: &mut [f64]) {
        if !self.raise.is_moving() {
            self.lim.process(l, r);
            return;
        }
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            self.lim.set_limit_db(self.raise.tick());
            self.lim
                .process(core::slice::from_mut(a), core::slice::from_mut(b));
        }
    }

    fn active(&self) -> u64 {
        self.base.saturating_add(self.lim.active_samples())
    }
}

struct BusRt {
    /// The sum, then the node's output in place: O (post-fader, post-mute).
    sum: Stereo,
    eq: Option<Equalizer<2>>,
    limiter: Option<BusLimiter>,
    fader: StereoGain,
    safety: Option<Mga>,
    /// Output clamp: 1.0, or 0.1 while a test signal reaches this bus (X13).
    cap: f64,
    trips: Trips,
    peak: PeakMeter<2>,
}

/// A running test signal (X13): replaces one input.
struct TestRt {
    input: usize,
    phase: f64,
    inc: f64,
    amp: f64,
    /// Samples until the fade-out starts.
    left: u64,
    fade: Ramp,
    /// Sample time after which it is silent and the caps lift.
    end: u64,
}

impl TestRt {
    fn render(&mut self, l: &mut [f64], r: &mut [f64]) {
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            if self.left == 0 {
                self.fade.set(0.0);
            } else {
                self.left -= 1;
            }
            let x = self.amp * self.fade.tick() * (core::f64::consts::TAU * self.phase).sin();
            self.phase = (self.phase + self.inc).fract();
            *a = x;
            *b = x;
        }
    }
}

pub struct Processor {
    graph: Arc<Graph>,
    sr: f64,
    inputs: Vec<InputRt>,
    buses: Vec<BusRt>,
    sends: Vec<StereoGain>,
    talkback_input: Option<usize>,
    cmds: Consumer<RtCmd>,
    meter_in: triple_buffer::Input<MeterFrame>,
    taps: [Producer<f32>; 2],
    talk: Consumer<f32>,
    talk_gate: Ramp,
    talk_f32: Vec<f32>,
    talk_buf: Vec<f64>,
    tap_buf: Vec<f32>,
    dry: Stereo,
    tx: Stereo,
    listen_buf: Stereo,
    fade_buf: Vec<f64>,
    listen: [Option<usize>; 2],
    listen_lim: Limiter,
    test: Option<TestRt>,
    fade: Ramp,
    fading_out: bool,
    time: u64,
    since_meter: u64,
    seq: u64,
    trips: u64,
    status: Arc<RtStatus>,
}

impl Processor {
    /// A processor at `state` (reconciled against the graph), with the X14
    /// counters `counters` per bus (graph order; missing ones are 0).
    pub fn new(
        graph: Arc<Graph>,
        state: &MixState,
        counters: &[u64],
        opts: Options,
    ) -> (Self, RtHandles) {
        let sr = f64::from(SAMPLE_RATE);
        let r = reconcile(&graph, state).0;
        let inputs = r
            .inputs
            .iter()
            .map(|s| {
                let p = input_params(s);
                InputRt {
                    p: Stereo::new(),
                    trim: Ramp::new(p.trim, samples(GAIN_MS, sr)),
                    proc_mix: Ramp::new(if p.processing { 1.0 } else { 0.0 }, samples(EQ_MS, sr)),
                    eq: Equalizer::new(&eq_params(&s.eq), sr),
                    gate: Ramp::new(if p.muted { 0.0 } else { 1.0 }, samples(MUTE_MS, sr)),
                    fader: StereoGain::new(sr, p.fader, false, p.pan),
                    trips: Trips::default(),
                    peak: PeakMeter::new(),
                }
            })
            .collect();
        let buses = r
            .buses
            .iter()
            .enumerate()
            .map(|(b, s)| BusRt {
                sum: Stereo::new(),
                eq: graph
                    .has_eq(b)
                    .then(|| Equalizer::new(&eq_params(&s.eq), sr)),
                limiter: graph.has_limiter(b).then(|| {
                    BusLimiter::new(
                        sr,
                        s.limiter.enabled,
                        s.limiter.limit_db,
                        counters.get(b).copied().unwrap_or(0),
                    )
                }),
                fader: StereoGain::new(sr, db_to_lin(s.fader_db), s.muted, s.pan),
                safety: (graph.kind(b) != Some(BusKind::Stems)).then(|| safety(sr)),
                cap: 1.0,
                trips: Trips::default(),
                peak: PeakMeter::new(),
            })
            .collect();
        let sends = r
            .sends
            .iter()
            .map(|s| StereoGain::new(sr, db_to_lin(s.gain_db), s.muted, s.pan))
            .collect();
        let frame = MeterFrame {
            seq: 0,
            inputs: vec![[0.0; 2]; graph.inputs.len()],
            buses: vec![[0.0; 2]; graph.buses.len()],
            gr_db: vec![0.0; graph.buses.len()],
            active: vec![0; graph.buses.len()],
            trips: 0,
        };
        let (meter_in, meters) = triple_buffer::triple_buffer(&frame);
        let (cmd_tx, cmds) = RingBuffer::new(CMD_RING);
        let (tap0, tap0_rx) = RingBuffer::new(TAP_RING);
        let (tap1, tap1_rx) = RingBuffer::new(TAP_RING);
        let (talk_tx, talk) = RingBuffer::new(TALK_RING);
        let status = Arc::new(RtStatus::default());
        let mut fade = Ramp::new(0.0, samples(opts.fade_in_ms, sr));
        if opts.fade_in_ms > 0.0 {
            fade.set(1.0);
        } else {
            fade.jump(1.0);
        }
        let processor = Self {
            talkback_input: graph.inputs.iter().position(|n| n.talkback),
            graph,
            sr,
            inputs,
            buses,
            sends,
            cmds,
            meter_in,
            taps: [tap0, tap1],
            talk,
            talk_gate: Ramp::new(0.0, samples(MUTE_MS, sr)),
            talk_f32: vec![0.0; SEG],
            talk_buf: vec![0.0; SEG],
            tap_buf: vec![0.0; 2 * SEG],
            dry: Stereo::new(),
            tx: Stereo::new(),
            listen_buf: Stereo::new(),
            fade_buf: vec![1.0; SEG],
            listen: [None, None],
            listen_lim: Limiter::new(sr, 0.0),
            test: None,
            fade,
            fading_out: false,
            time: 0,
            since_meter: 0,
            seq: 0,
            trips: 0,
            status: Arc::clone(&status),
        };
        let handles = RtHandles {
            cmds: cmd_tx,
            meters,
            taps: [tap0_rx, tap1_rx],
            talkback: talk_tx,
            status,
        };
        (processor, handles)
    }

    /// Samples rendered so far.
    pub fn time(&self) -> u64 {
        self.time
    }

    fn set_caps(&mut self, input: Option<usize>) {
        let reach = input.and_then(|i| self.graph.reach.get(i));
        for (b, bus) in self.buses.iter_mut().enumerate() {
            let capped = reach.and_then(|r| r.get(b)).copied().unwrap_or(false);
            bus.cap = if capped { TEST_CAP } else { 1.0 };
        }
    }

    /// The deliberate fault of `--fault-injection` (§2.4: automation injects
    /// panics only).
    #[allow(clippy::panic)]
    fn inject_fault() {
        panic!("fault injection: panic on the RT thread");
    }

    fn apply(&mut self, op: RtOp) {
        let sr = self.sr;
        match op {
            RtOp::Nop => {}
            RtOp::Input { i, p } => {
                if let Some(n) = self.inputs.get_mut(usize::from(i)) {
                    n.trim.set(p.trim);
                    n.gate.set(if p.muted { 0.0 } else { 1.0 });
                    let dry = !n.proc_mix.is_moving() && n.proc_mix.value() == 0.0;
                    if p.processing && dry {
                        // Re-entering from fully dry: trim and EQ restart from
                        // their targets, whatever ran while fading out.
                        n.trim.jump(n.trim.target());
                        n.eq = Equalizer::new(&n.eq.params(), sr);
                    }
                    n.proc_mix.set(if p.processing { 1.0 } else { 0.0 });
                    n.fader.set(p.fader, false, p.pan);
                }
            }
            RtOp::InputEq { i, eq } => {
                if let Some(n) = self.inputs.get_mut(usize::from(i)) {
                    n.eq.set(&eq);
                }
            }
            RtOp::Bus {
                b,
                fader,
                pan,
                muted,
            } => {
                if let Some(n) = self.buses.get_mut(usize::from(b)) {
                    n.fader.set(fader, muted, pan);
                }
            }
            RtOp::BusEq { b, eq } => {
                if let Some(e) = self
                    .buses
                    .get_mut(usize::from(b))
                    .and_then(|n| n.eq.as_mut())
                {
                    e.set(&eq);
                }
            }
            RtOp::Limiter {
                b,
                enabled,
                limit_db,
            } => {
                if let Some(l) = self
                    .buses
                    .get_mut(usize::from(b))
                    .and_then(|n| n.limiter.as_mut())
                {
                    l.set(enabled, limit_db);
                }
            }
            RtOp::ResetLimiter { b } => {
                if let Some(l) = self
                    .buses
                    .get_mut(usize::from(b))
                    .and_then(|n| n.limiter.as_mut())
                {
                    l.lim.reset_active();
                    l.base = 0;
                }
            }
            RtOp::Send {
                s,
                gain,
                pan,
                muted,
            } => {
                if let Some(g) = self.sends.get_mut(usize::from(s)) {
                    g.set(gain, muted, pan);
                }
            }
            RtOp::Listen { slot, bus } => {
                if let Some(l) = self.listen.get_mut(usize::from(slot)) {
                    *l = bus.map(usize::from);
                }
                if slot == 1 {
                    self.listen_lim.reset();
                }
            }
            RtOp::TestSignal { i, hz, amp, ttl } => {
                let len = samples(FADE_MS, sr);
                let mut fade = Ramp::new(0.0, len);
                fade.set(1.0);
                let input = usize::from(i);
                self.test = Some(TestRt {
                    input,
                    phase: 0.0,
                    inc: hz / sr,
                    amp: amp.min(TEST_CAP),
                    left: ttl,
                    fade,
                    end: self.time.saturating_add(ttl).saturating_add(u64::from(len)),
                });
                self.set_caps(Some(input));
            }
            RtOp::StopTestSignal => {
                let now = self.time;
                if let Some(t) = self.test.as_mut() {
                    t.left = 0;
                    let len = samples(FADE_MS, sr);
                    let mut fade = Ramp::new(t.fade.value(), len);
                    fade.set(0.0);
                    t.fade = fade;
                    t.end = now.saturating_add(u64::from(len));
                }
            }
            RtOp::FadeOut => {
                let mut fade = Ramp::new(self.fade.value(), samples(FADE_MS, sr));
                fade.set(0.0);
                self.fade = fade;
                self.fading_out = true;
            }
            RtOp::Panic => Self::inject_fault(),
        }
    }

    /// Applies the commands due now within the block's remaining budget.
    fn apply_due(&mut self, budget: &mut usize) {
        loop {
            let (at, group) = match self.cmds.peek() {
                Ok(c) => (c.at, c.group),
                Err(_) => return,
            };
            if at > self.time {
                return;
            }
            let len = usize::from(group.max(1));
            if len > *budget {
                self.status.deferred.fetch_add(1, Ordering::Relaxed);
                return;
            }
            for _ in 0..len {
                if let Ok(c) = self.cmds.pop() {
                    self.apply(c.op);
                }
            }
            *budget -= len;
        }
    }

    /// Reads up to `n` talkback samples; the gate closes on underrun.
    fn read_talkback(&mut self, n: usize) {
        let buf = self.talk_buf.get_mut(..n).unwrap_or_default();
        let raw = self.talk_f32.get_mut(..n).unwrap_or_default();
        let (got, _) = self.talk.pop_partial_slice(raw);
        let got = got.len();
        for (d, s) in buf.iter_mut().zip(raw.iter()) {
            *d = f64::from(*s);
        }
        if let Some(rest) = buf.get_mut(got..) {
            rest.fill(0.0);
        }
        if got < n {
            self.talk_gate.set(0.0);
            if got > 0 {
                self.status
                    .talkback_underruns
                    .fetch_add(1, Ordering::Relaxed);
            }
        } else {
            self.talk_gate.set(1.0);
        }
        for x in buf.iter_mut() {
            *x *= TALKBACK_GAIN * self.talk_gate.tick();
        }
    }

    fn trip(&mut self) {
        self.trips += 1;
        self.status.trips.fetch_add(1, Ordering::Relaxed);
    }

    fn render_inputs(&mut self, block: &Block<'_>, off: usize, n: usize) {
        let graph = Arc::clone(&self.graph);
        if self.talkback_input.is_some() {
            self.read_talkback(n);
        }
        for (i, spec) in graph.inputs.iter().enumerate() {
            let mut tripped = false;
            let Some(node) = self.inputs.get_mut(i) else {
                continue;
            };
            let (l, r) = node.p.get_mut(n);
            for (dst, ch) in [(&mut *l, spec.rx[0]), (&mut *r, spec.rx[1])] {
                match block.input(ch).get(off..off + n) {
                    Some(src) => copy(dst, src),
                    None => dst.fill(0.0),
                }
            }
            // X1 on the card input.
            if node.trips.check([&mut *l, &mut *r]) {
                node.eq.reset();
                tripped = true;
            }
            if let Some(t) = self.test.as_mut().filter(|t| t.input == i) {
                t.render(l, r);
            }
            let mix = &mut node.proc_mix;
            let dry = !mix.is_moving() && mix.value() == 0.0;
            if !dry {
                let fading = mix.is_moving();
                let (dl, dr) = self.dry.get_mut(n);
                if fading {
                    copy(dl, l);
                    copy(dr, r);
                }
                ramp_gain(&mut node.trim, l, r);
                if !node.eq.is_identity() {
                    node.eq.process([&mut *l, &mut *r]);
                }
                if fading {
                    for ((a, b), (x, y)) in
                        l.iter_mut().zip(r.iter_mut()).zip(dl.iter().zip(dr.iter()))
                    {
                        // At the ends the result is exactly wet or dry, as in a
                        // segment that starts after the fade (block-size invariance).
                        let m = mix.tick();
                        if m != 1.0 {
                            *a = x + m * (*a - x);
                            *b = y + m * (*b - y);
                        }
                    }
                }
            }
            if Some(i) == self.talkback_input {
                let tb = self.talk_buf.get(..n).unwrap_or_default();
                for ((a, b), t) in l.iter_mut().zip(r.iter_mut()).zip(tb) {
                    *a += t;
                    *b += t;
                }
            }
            ramp_gain(&mut node.gate, l, r);
            // X1 after the node.
            if node.trips.check([&mut *l, &mut *r]) {
                node.eq.reset();
                tripped = true;
            }
            node.peak.observe([&*l, &*r]);
            if tripped {
                self.trip();
            }
        }
    }

    fn render_buses(&mut self, block: &mut Block<'_>, off: usize, n: usize) {
        let graph = Arc::clone(&self.graph);
        let Self {
            inputs,
            buses,
            sends,
            taps,
            tap_buf,
            tx,
            listen,
            listen_lim,
            listen_buf,
            fade_buf,
            status,
            ..
        } = self;
        let mut trips = 0;
        if let Some(m) = graph.master.and_then(|m| buses.get_mut(m)) {
            let (ml, mr) = m.sum.get_mut(n);
            ml.fill(0.0);
            mr.fill(0.0);
            for node in inputs.iter_mut() {
                let (pl, pr) = node.p.get(n);
                accumulate(&mut node.fader, (pl, pr), (&mut *ml, &mut *mr));
            }
        }
        for (b, spec) in graph.buses.iter().enumerate() {
            let (done, rest) = buses.split_at_mut(b);
            let Some((bus, _)) = rest.split_first_mut() else {
                continue;
            };
            let (l, r) = bus.sum.get_mut(n);
            if spec.kind == BusKind::Master {
                for (s, stems) in graph.buses.iter().zip(done.iter()) {
                    if s.kind == BusKind::Stems {
                        let (sl, sr) = stems.sum.get(n);
                        for ((a, c), (x, y)) in
                            l.iter_mut().zip(r.iter_mut()).zip(sl.iter().zip(sr))
                        {
                            *a += x;
                            *c += y;
                        }
                    }
                }
            } else {
                l.fill(0.0);
                r.fill(0.0);
            }
            for (edge, gain) in graph
                .sends
                .get(spec.sends.clone())
                .unwrap_or_default()
                .iter()
                .zip(sends.get_mut(spec.sends.clone()).unwrap_or_default())
            {
                let src = match edge.src {
                    Src::Pre(i) => inputs.get(i).map(|x| x.p.get(n)),
                    Src::Post(j) => done.get(j).map(|x| x.sum.get(n)),
                };
                if let Some(src) = src {
                    accumulate(gain, src, (&mut *l, &mut *r));
                }
            }
            if let Some(eq) = bus.eq.as_mut().filter(|e| !e.is_identity()) {
                eq.process([&mut *l, &mut *r]);
            }
            if let Some(lim) = bus.limiter.as_mut() {
                lim.process(l, r);
            }
            if listen[0] == Some(b) {
                push_tap(&mut taps[0], (&*l, &*r), tap_buf, &status.tap_overruns);
            }
            stereo_gain(&mut bus.fader, l, r);
            if bus.trips.check([&mut *l, &mut *r]) {
                if let Some(eq) = bus.eq.as_mut() {
                    eq.reset();
                }
                if let Some(lim) = bus.limiter.as_mut() {
                    lim.lim.reset();
                }
                trips += 1;
            }
            bus.peak.observe([&*l, &*r]);
            if listen[1] == Some(b) {
                let (ll, lr) = listen_buf.get_mut(n);
                copy(ll, l);
                copy(lr, r);
                listen_lim.process(ll, lr);
                push_tap(&mut taps[1], (&*ll, &*lr), tap_buf, &status.tap_overruns);
            }
            let Some(safety) = bus.safety.as_mut() else {
                continue;
            };
            let (tl, tr) = tx.get_mut(n);
            let cap = bus.cap;
            let fade = fade_buf.get(..n).unwrap_or_default();
            let mono = spec.kind == BusKind::Translator;
            for (((a, c), (x, y)), f) in tl
                .iter_mut()
                .zip(tr.iter_mut())
                .zip(l.iter().zip(r.iter()))
                .zip(fade)
            {
                let (inl, inr) = if mono { ((x + y) * 0.5, 0.0) } else { (*x, *y) };
                let (yl, yr) = safety.tick(inl, inr);
                *a = yl.clamp(-cap, cap) * f;
                *c = yr.clamp(-cap, cap) * f;
            }
            for (ch, src) in spec.tx.iter().zip([&*tl, &*tr]) {
                let Some(ch) = *ch else {
                    continue;
                };
                if let Some(out) = block.output(ch).get_mut(off..off + n) {
                    copy(out, src);
                }
            }
        }
        if trips > 0 {
            self.trips += trips;
            self.status.trips.fetch_add(trips, Ordering::Relaxed);
        }
    }

    fn render(&mut self, block: &mut Block<'_>, off: usize, n: usize) {
        if self.test.as_ref().is_some_and(|t| t.end <= self.time) {
            self.test = None;
            self.set_caps(None);
        }
        let fade = self.fade_buf.get_mut(..n).unwrap_or_default();
        for f in fade.iter_mut() {
            *f = self.fade.tick();
        }
        self.render_inputs(block, off, n);
        self.render_buses(block, off, n);
        // After `FadeOut` the fade's target is 0: at rest it is silent.
        if self.fading_out && !self.fade.is_moving() {
            self.status.faded_out.store(true, Ordering::Release);
        }
    }

    fn publish_meters(&mut self) {
        self.seq += 1;
        let f = self.meter_in.input_buffer_mut();
        f.seq = self.seq;
        f.trips = self.trips;
        for (d, n) in f.inputs.iter_mut().zip(self.inputs.iter_mut()) {
            *d = n.peak.take();
        }
        for ((d, (g, a)), bus) in f
            .buses
            .iter_mut()
            .zip(f.gr_db.iter_mut().zip(f.active.iter_mut()))
            .zip(self.buses.iter_mut())
        {
            *d = bus.peak.take();
            *g = bus.limiter.as_ref().map_or(0.0, |l| l.lim.gr_db());
            *a = bus.limiter.as_ref().map_or(0, BusLimiter::active);
        }
        self.meter_in.publish();
    }
}

impl Process for Processor {
    #[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
    fn process(&mut self, block: &mut Block<'_>) {
        let frames = block.frames();
        let mut budget = MAX_CMDS_PER_BLOCK;
        let mut done = 0;
        while done < frames {
            self.apply_due(&mut budget);
            let mut n = (frames - done).min(SEG);
            let mut cut = |at: u64| {
                if at > self.time {
                    let k = usize::try_from(at - self.time).unwrap_or(usize::MAX);
                    n = n.min(k);
                }
            };
            if budget > 0
                && let Ok(c) = self.cmds.peek()
            {
                cut(c.at);
            }
            if let Some(t) = self.test.as_ref() {
                cut(t.end);
            }
            self.render(block, done, n);
            done += n;
            self.time += n as u64;
        }
        self.since_meter += frames as u64;
        if self.since_meter >= METER_PERIOD {
            self.since_meter %= METER_PERIOD;
            self.publish_meters();
        }
    }
}

#[cfg(test)]
#[path = "rt_tests.rs"]
mod tests;
