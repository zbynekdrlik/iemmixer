//! The RT processor (program spec §3.3 A1–A13, §3.4 X1–X4, X13–X15, Q1, I5,
//! I7; S3 design note §3.2, §3.4; #20 design note §3, §6): one callback runs
//! the fixed pipeline — the inputs, then every mix in declaration order (its
//! inputs directly or through their group's strip, the mixes it hears, then
//! EQ → limiter → volume/mute → Q1 safety → clamp → TX).
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
use iem_engine_proto::{MixState, db_to_lin};
use iem_limiter_mga::{DISABLE_MS, Limiter, Mga, Sliders};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::cmd::{RtCmd, RtOp};
use crate::core::reconcile;
use crate::params::{eq_params, input_params};
use crate::topology::Topology;
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

/// One meter frame: peaks since the previous frame (inputs after their mute,
/// mixes after volume and mute, group strips after their fader, mix-major),
/// limiter GR in dB and X14 active samples per mix.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MeterFrame {
    pub seq: u64,
    pub inputs: Vec<[f64; 2]>,
    pub mixes: Vec<[f64; 2]>,
    pub groups: Vec<[f64; 2]>,
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
    /// Blocks that left commands for the next block (the 512 budget): a
    /// command due in the block that it did not apply. Counted once per block.
    pub deferred: AtomicU64,
}

/// The non-RT ends of the processor's rings.
pub struct RtHandles {
    pub cmds: Producer<RtCmd>,
    pub meters: triple_buffer::Output<MeterFrame>,
    /// Interleaved stereo 96 kHz: slot 0 the engineer, slot 1 one other mix (X3).
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

/// Adds `g · src` to `dst` (a level).
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
    /// The input's signal P (post-FX, post-mute) every level reads.
    p: Stereo,
    trim: Ramp,
    /// 1 = processed (trim → EQ), 0 = dry (Q3); crossfades 20 ms.
    proc_mix: Ramp,
    eq: Equalizer<2>,
    /// 1 = open, 0 = muted (A3), 5 ms.
    gate: Ramp,
    trips: Trips,
    peak: PeakMeter<2>,
}

/// A mix limiter (A13, §4.4): enabling and lowering instant, raising over 10 ms.
struct MixLimiter {
    lim: Limiter,
    raise: Ramp,
    /// X14 samples counted before this run (Q4: persisted until reset).
    base: u64,
}

impl MixLimiter {
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

/// A group's strip in one mix (A7): Σ → EQ → fader → mute.
struct GroupRt {
    eq: Equalizer<2>,
    fader: StereoGain,
    trips: Trips,
    peak: PeakMeter<2>,
}

struct MixRt {
    /// The sum, then the mix's output in place: O (post-volume, post-mute),
    /// which later mixes that hear this one read (A9: unclipped).
    sum: Stereo,
    /// Level slots: every input, then the mixes it hears.
    levels: Vec<StereoGain>,
    groups: Vec<GroupRt>,
    eq: Equalizer<2>,
    limiter: MixLimiter,
    /// Volume and mute (A5 at pan 0).
    fader: StereoGain,
    safety: Mga,
    /// Output clamp: 1.0, or 0.1 while a test signal sounds (X13).
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
    topo: Arc<Topology>,
    sr: f64,
    inputs: Vec<InputRt>,
    mixes: Vec<MixRt>,
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
    /// A group strip's sum, one group at a time.
    group_buf: Stereo,
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
    /// A processor at `state` (reconciled against the topology), with the
    /// X14 counters `counters` per mix (topology order; missing ones are 0).
    pub fn new(
        topo: Arc<Topology>,
        state: &MixState,
        counters: &[u64],
        opts: Options,
    ) -> (Self, RtHandles) {
        let sr = f64::from(SAMPLE_RATE);
        let r = reconcile(&topo, state).0;
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
                    trips: Trips::default(),
                    peak: PeakMeter::new(),
                }
            })
            .collect();
        let mixes = r
            .mixes
            .iter()
            .enumerate()
            .map(|(m, rec)| MixRt {
                sum: Stereo::new(),
                levels: rec
                    .levels
                    .iter()
                    .map(|l| StereoGain::new(sr, db_to_lin(l.gain_db), l.muted, l.pan))
                    .collect(),
                groups: rec
                    .groups
                    .iter()
                    .map(|g| GroupRt {
                        eq: Equalizer::new(&eq_params(&g.eq), sr),
                        fader: StereoGain::new(sr, db_to_lin(g.gain_db), g.muted, 0.0),
                        trips: Trips::default(),
                        peak: PeakMeter::new(),
                    })
                    .collect(),
                eq: Equalizer::new(&eq_params(&rec.out.eq), sr),
                limiter: MixLimiter::new(
                    sr,
                    rec.out.limiter.enabled,
                    rec.out.limiter.limit_db,
                    counters.get(m).copied().unwrap_or(0),
                ),
                fader: StereoGain::new(sr, db_to_lin(rec.out.volume_db), rec.out.muted, 0.0),
                safety: safety(sr),
                cap: 1.0,
                trips: Trips::default(),
                peak: PeakMeter::new(),
            })
            .collect();
        let frame = MeterFrame {
            seq: 0,
            inputs: vec![[0.0; 2]; topo.inputs.len()],
            mixes: vec![[0.0; 2]; topo.mixes.len()],
            groups: vec![[0.0; 2]; topo.mixes.len() * topo.groups.len()],
            gr_db: vec![0.0; topo.mixes.len()],
            active: vec![0; topo.mixes.len()],
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
            talkback_input: topo.inputs.iter().position(|n| n.talkback),
            topo,
            sr,
            inputs,
            mixes,
            cmds,
            meter_in,
            taps: [tap0, tap1],
            talk,
            talk_gate: Ramp::new(0.0, samples(MUTE_MS, sr)),
            talk_f32: vec![0.0; SEG],
            talk_buf: vec![0.0; SEG],
            tap_buf: vec![0.0; 2 * SEG],
            dry: Stereo::new(),
            group_buf: Stereo::new(),
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

    /// X13: every mix hears every input, so a test signal caps every TX.
    fn set_caps(&mut self, on: bool) {
        for mix in &mut self.mixes {
            mix.cap = if on { TEST_CAP } else { 1.0 };
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
                }
            }
            RtOp::InputEq { i, eq } => {
                if let Some(n) = self.inputs.get_mut(usize::from(i)) {
                    n.eq.set(&eq);
                }
            }
            RtOp::MixOut { m, volume, muted } => {
                if let Some(n) = self.mixes.get_mut(usize::from(m)) {
                    n.fader.set(volume, muted, 0.0);
                }
            }
            RtOp::MixEq { m, eq } => {
                if let Some(n) = self.mixes.get_mut(usize::from(m)) {
                    n.eq.set(&eq);
                }
            }
            RtOp::Limiter {
                m,
                enabled,
                limit_db,
            } => {
                if let Some(n) = self.mixes.get_mut(usize::from(m)) {
                    n.limiter.set(enabled, limit_db);
                }
            }
            RtOp::ResetLimiter { m } => {
                if let Some(n) = self.mixes.get_mut(usize::from(m)) {
                    n.limiter.lim.reset_active();
                    n.limiter.base = 0;
                }
            }
            RtOp::Level {
                m,
                k,
                gain,
                pan,
                muted,
            } => {
                if let Some(g) = self
                    .mixes
                    .get_mut(usize::from(m))
                    .and_then(|n| n.levels.get_mut(usize::from(k)))
                {
                    g.set(gain, muted, pan);
                }
            }
            RtOp::Group { m, g, gain, muted } => {
                if let Some(s) = self
                    .mixes
                    .get_mut(usize::from(m))
                    .and_then(|n| n.groups.get_mut(usize::from(g)))
                {
                    s.fader.set(gain, muted, 0.0);
                }
            }
            RtOp::GroupEq { m, g, eq } => {
                if let Some(s) = self
                    .mixes
                    .get_mut(usize::from(m))
                    .and_then(|n| n.groups.get_mut(usize::from(g)))
                {
                    s.eq.set(&eq);
                }
            }
            RtOp::Listen { slot, mix } => {
                if let Some(l) = self.listen.get_mut(usize::from(slot)) {
                    *l = mix.map(usize::from);
                }
                if slot == 1 {
                    self.listen_lim.reset();
                }
            }
            RtOp::TestSignal { i, hz, amp, ttl } => {
                let len = samples(FADE_MS, sr);
                let mut fade = Ramp::new(0.0, len);
                fade.set(1.0);
                self.test = Some(TestRt {
                    input: usize::from(i),
                    phase: 0.0,
                    inc: hz / sr,
                    amp: amp.min(TEST_CAP),
                    left: ttl,
                    fade,
                    end: self.time.saturating_add(ttl).saturating_add(u64::from(len)),
                });
                self.set_caps(true);
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
    /// Returns whether a due group did not fit the budget and waits.
    fn apply_due(&mut self, budget: &mut usize) -> bool {
        loop {
            let (at, group) = match self.cmds.peek() {
                Ok(c) => (c.at, c.group),
                Err(_) => return false,
            };
            if at > self.time {
                return false;
            }
            let len = usize::from(group.max(1));
            if len > *budget {
                return true;
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
        let topo = Arc::clone(&self.topo);
        if self.talkback_input.is_some() {
            self.read_talkback(n);
        }
        for (i, spec) in topo.inputs.iter().enumerate() {
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

    fn render_mixes(&mut self, block: &mut Block<'_>, off: usize, n: usize) {
        let topo = Arc::clone(&self.topo);
        let Self {
            inputs,
            mixes,
            taps,
            tap_buf,
            group_buf,
            tx,
            listen,
            listen_lim,
            listen_buf,
            fade_buf,
            status,
            ..
        } = self;
        let heard_from = topo.inputs.len();
        let mut trips = 0;
        for (m, spec) in topo.mixes.iter().enumerate() {
            let (done, rest) = mixes.split_at_mut(m);
            let Some((mix, _)) = rest.split_first_mut() else {
                continue;
            };
            let MixRt {
                sum,
                levels,
                groups,
                eq,
                limiter,
                fader,
                safety,
                cap,
                trips: mix_trips,
                peak,
            } = mix;
            let (l, r) = sum.get_mut(n);
            l.fill(0.0);
            r.fill(0.0);
            // The inputs in no group, at their levels.
            for &i in &topo.direct {
                if let (Some(input), Some(g)) = (inputs.get(i), levels.get_mut(i)) {
                    accumulate(g, input.p.get(n), (&mut *l, &mut *r));
                }
            }
            // Each group's strip: its inputs at their levels → EQ → fader → mute.
            for (group, strip) in topo.groups.iter().zip(groups.iter_mut()) {
                let (gl, gr) = group_buf.get_mut(n);
                gl.fill(0.0);
                gr.fill(0.0);
                for &i in &group.inputs {
                    if let (Some(input), Some(g)) = (inputs.get(i), levels.get_mut(i)) {
                        accumulate(g, input.p.get(n), (&mut *gl, &mut *gr));
                    }
                }
                if !strip.eq.is_identity() {
                    strip.eq.process([&mut *gl, &mut *gr]);
                }
                stereo_gain(&mut strip.fader, gl, gr);
                if strip.trips.check([&mut *gl, &mut *gr]) {
                    strip.eq.reset();
                    trips += 1;
                }
                strip.peak.observe([&*gl, &*gr]);
                for ((a, c), (x, y)) in l.iter_mut().zip(r.iter_mut()).zip(gl.iter().zip(gr.iter()))
                {
                    *a += x;
                    *c += y;
                }
            }
            // The mixes it hears, after their mute and unclipped (A9).
            for (k, &s) in spec.mixes.iter().enumerate() {
                if let (Some(src), Some(g)) = (done.get(s), levels.get_mut(heard_from + k)) {
                    accumulate(g, src.sum.get(n), (&mut *l, &mut *r));
                }
            }
            if !eq.is_identity() {
                eq.process([&mut *l, &mut *r]);
            }
            limiter.process(l, r);
            if listen[0] == Some(m) {
                push_tap(&mut taps[0], (&*l, &*r), tap_buf, &status.tap_overruns);
            }
            stereo_gain(fader, l, r);
            if mix_trips.check([&mut *l, &mut *r]) {
                eq.reset();
                limiter.lim.reset();
                trips += 1;
            }
            peak.observe([&*l, &*r]);
            if listen[1] == Some(m) {
                let (ll, lr) = listen_buf.get_mut(n);
                copy(ll, l);
                copy(lr, r);
                listen_lim.process(ll, lr);
                push_tap(&mut taps[1], (&*ll, &*lr), tap_buf, &status.tap_overruns);
            }
            let (tl, tr) = tx.get_mut(n);
            let fade = fade_buf.get(..n).unwrap_or_default();
            for (((a, c), (x, y)), f) in tl
                .iter_mut()
                .zip(tr.iter_mut())
                .zip(l.iter().zip(r.iter()))
                .zip(fade)
            {
                let (inl, inr) = if spec.mono {
                    ((x + y) * 0.5, 0.0)
                } else {
                    (*x, *y)
                };
                let (yl, yr) = safety.tick(inl, inr);
                *a = yl.clamp(-*cap, *cap) * f;
                *c = yr.clamp(-*cap, *cap) * f;
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
            self.set_caps(false);
        }
        let fade = self.fade_buf.get_mut(..n).unwrap_or_default();
        for f in fade.iter_mut() {
            *f = self.fade.tick();
        }
        self.render_inputs(block, off, n);
        self.render_mixes(block, off, n);
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
        for ((d, (g, a)), mix) in f
            .mixes
            .iter_mut()
            .zip(f.gr_db.iter_mut().zip(f.active.iter_mut()))
            .zip(self.mixes.iter_mut())
        {
            *d = mix.peak.take();
            *g = mix.limiter.lim.gr_db();
            *a = mix.limiter.active();
        }
        for (d, strip) in f
            .groups
            .iter_mut()
            .zip(self.mixes.iter_mut().flat_map(|mix| mix.groups.iter_mut()))
        {
            *d = strip.peak.take();
        }
        self.meter_in.publish();
    }
}

impl Process for Processor {
    #[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
    fn process(&mut self, block: &mut Block<'_>) {
        let frames = block.frames();
        let mut budget = MAX_CMDS_PER_BLOCK;
        let mut left = false;
        let mut done = 0;
        while done < frames {
            left |= self.apply_due(&mut budget);
            let mut n = (frames - done).min(SEG);
            let next = self.cmds.peek().map(|c| c.at).ok();
            // A spent budget does not cut the block: a command due in this
            // segment waits for the next block.
            if budget == 0 && next.is_some_and(|at| at < self.time + n as u64) {
                left = true;
            }
            let mut cut = |at: u64| {
                if at > self.time {
                    let k = usize::try_from(at - self.time).unwrap_or(usize::MAX);
                    n = n.min(k);
                }
            };
            if budget > 0
                && let Some(at) = next
            {
                cut(at);
            }
            if let Some(t) = self.test.as_ref() {
                cut(t.end);
            }
            self.render(block, done, n);
            done += n;
            self.time += n as u64;
        }
        if left {
            self.status.deferred.fetch_add(1, Ordering::Relaxed);
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
