//! The RT processor (program spec §3.3 A1–A13, §3.4 X1–X4, X13–X15, Q1, I5,
//! I7; S3 design note §3.2, §3.4; #20 design note §3, §6): one callback runs
//! the fixed pipeline — the inputs, then every mix in declaration order (its
//! inputs directly or through their group's strip, the mixes it hears, then
//! EQ → limiter → volume/mute → Q1 safety → clamp → TX), then HIL's spare
//! outputs after the topology's TX (S6: the HIL test signal's sine while one
//! runs, zero otherwise).
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

use crate::cmd::{HilMask, MAX_HIL, RtCmd, RtOp};
use crate::core::reconcile;
use crate::latency::LatencyProbe;
use crate::params::{eq_params, input_params};
use crate::topology::Topology;
use crate::{MAX_CMDS_PER_BLOCK, SAMPLE_RATE, SEG, TALKBACK_GAIN, TEST_CAP};

mod render;

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
/// The output fade-in at start, after `Arm` and after a reopen (§4.4).
pub const FADE_IN_MS: f64 = 500.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Output fade-in after start (§4.4: 500 ms); 0 starts at full level.
    pub fade_in_ms: f64,
    /// `--hold` (S6 design note §4): every output stays silent until
    /// `RtOp::Arm`, then fades in.
    pub hold: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            fade_in_ms: FADE_IN_MS,
            hold: false,
        }
    }
}

/// One meter frame: peaks since the previous frame (inputs after their mute,
/// mixes after volume and mute, group strips after their fader, mix-major,
/// HIL's spare outputs as written), limiter GR in dB and X14 active samples
/// per mix.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MeterFrame {
    pub seq: u64,
    pub inputs: Vec<[f64; 2]>,
    pub mixes: Vec<[f64; 2]>,
    pub groups: Vec<[f64; 2]>,
    pub gr_db: Vec<f64>,
    pub active: Vec<u64>,
    pub trips: u64,
    /// HIL's spare outputs (S6), in the order the engine opened them: what
    /// HIL v1 reads to prove the signal reached them, and left them.
    pub hil: Vec<f64>,
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
    /// The D5(b) loopback round-trip, in samples, once measured (S6 test 5):
    /// 0 while no measurement (a loopback is never 0 samples). Cleared when a
    /// new HIL test signal starts.
    pub loopback_samples: AtomicU64,
}

/// The non-RT ends of the processor's rings.
pub struct RtHandles {
    pub cmds: Producer<RtCmd>,
    pub meters: triple_buffer::Output<MeterFrame>,
    /// Interleaved stereo 96 kHz: slot 0 the engineer, slot 1 one other mix
    /// (X3); silence while a HIL signal runs (S6).
    pub taps: [Consumer<f32>; 2],
    /// The listen probe's taps (S7), by slot like `taps`: the spare outputs'
    /// samples while a HIL signal with `listen` runs (`probe::push_probe`).
    pub probes: [Consumer<f32>; 2],
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

/// The output fade from silence to full level over `len` samples; at once
/// when `len` is 0.
fn rise(len: u32) -> Ramp {
    let mut fade = Ramp::new(0.0, len);
    if len > 0 {
        fade.set(1.0);
    } else {
        fade.jump(1.0);
    }
    fade
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
    /// The HIL signal's spare outputs, by HIL slot: until `end` they carry
    /// the sine and every mix's TX is zero.
    mask: Option<HilMask>,
    /// The listen probe (S7, HIL only): the listened slots' probe taps carry
    /// the spare outputs' samples until `end`.
    listen: bool,
}

impl TestRt {
    /// The sine of the next `out.len()` samples: the fade-in, the level,
    /// the fade-out once the TTL ran out.
    fn render(&mut self, out: &mut [f64]) {
        for y in out.iter_mut() {
            if self.left == 0 {
                self.fade.set(0.0);
            } else {
                self.left -= 1;
            }
            *y = self.amp * self.fade.tick() * (core::f64::consts::TAU * self.phase).sin();
            self.phase = (self.phase + self.inc).fract();
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
    probes: [Producer<f32>; 2],
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
    /// The test sine of the current segment: the input it replaces and HIL's
    /// spare outputs read it.
    test_buf: Vec<f64>,
    /// HIL's spare outputs (S6), the engine's outputs after the topology's
    /// TX: their peaks since the last meter frame.
    hil_peaks: Vec<PeakMeter<1>>,
    /// How many D5(b) loopback-return inputs the engine opened (S6 test 5),
    /// after the topology's `rx` in the input buffer; 0 unless opened.
    hil_rx: usize,
    /// The loopback round-trip measurement (test 5).
    latency: LatencyProbe,
    fade: Ramp,
    /// The fade-in's length in samples; 0 starts at full level.
    fade_in: u32,
    /// False while `Options::hold` keeps the output silent (until `Arm`).
    armed: bool,
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
        Self::with_hil(topo, state, counters, opts, 0, 0)
    }

    /// [`Processor::new`] with `hil` of HIL's spare card outputs (S6, at
    /// most [`MAX_HIL`]) as the engine's outputs after the topology's TX:
    /// they carry the HIL test signal's sine while one runs and zero
    /// otherwise (A1).
    pub fn with_hil(
        topo: Arc<Topology>,
        state: &MixState,
        counters: &[u64],
        opts: Options,
        hil: usize,
        hil_rx: usize,
    ) -> (Self, RtHandles) {
        let hil = hil.min(MAX_HIL);
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
            hil: vec![0.0; hil],
        };
        let (meter_in, meters) = triple_buffer::triple_buffer(&frame);
        let (cmd_tx, cmds) = RingBuffer::new(CMD_RING);
        let (tap0, tap0_rx) = RingBuffer::new(TAP_RING);
        let (tap1, tap1_rx) = RingBuffer::new(TAP_RING);
        let (probe0, probe0_rx) = RingBuffer::new(TAP_RING);
        let (probe1, probe1_rx) = RingBuffer::new(TAP_RING);
        let (talk_tx, talk) = RingBuffer::new(TALK_RING);
        let status = Arc::new(RtStatus::default());
        let fade_in = if opts.fade_in_ms > 0.0 {
            samples(opts.fade_in_ms, sr)
        } else {
            0
        };
        // Held: silent until `Arm` starts the fade-in.
        let fade = if opts.hold {
            Ramp::new(0.0, 1)
        } else {
            rise(fade_in)
        };
        let processor = Self {
            talkback_input: topo.inputs.iter().position(|n| n.talkback),
            topo,
            sr,
            inputs,
            mixes,
            cmds,
            meter_in,
            taps: [tap0, tap1],
            probes: [probe0, probe1],
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
            test_buf: vec![0.0; SEG],
            hil_peaks: vec![PeakMeter::new(); hil],
            hil_rx,
            latency: LatencyProbe::new(),
            fade,
            fade_in,
            armed: !opts.hold,
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
            probes: [probe0_rx, probe1_rx],
            talkback: talk_tx,
            status,
        };
        (processor, handles)
    }

    /// Samples rendered so far.
    pub fn time(&self) -> u64 {
        self.time
    }

    /// The engine's outputs: the topology's TX, then HIL's spare outputs.
    pub fn outputs(&self) -> usize {
        self.topo.tx.len() + self.hil_peaks.len()
    }

    /// The engine's inputs: the topology's RX, then the D5(b) loopback returns
    /// (`hil_rx`, after the topology's rx). The driver opens exactly this many.
    pub fn input_channels(&self) -> usize {
        self.topo.rx.len() + self.hil_rx
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

    /// The owner-approved SEH test (`--fault-injection` only, design §10): a
    /// structured exception on the RT thread that `catch_unwind` cannot
    /// catch, so the process's SEH filter runs (Windows). Off Windows there
    /// is no such filter, so it aborts (equally uncatchable). It never
    /// returns: `tests/pipes.rs` runs it in the binary off Windows (SIGABRT);
    /// on Windows `iemmode inject-seh` runs it on the PC.
    fn inject_seh() {
        #[cfg(windows)]
        iem_audio_io::asio::raise_test_seh();
        #[cfg(not(windows))]
        std::process::abort();
    }

    /// The parked-engine test (`--fault-injection` only, design §10 test #2,
    /// #35): the SEH test's exception under the ASIO backend's test hold, so
    /// the backend keeps the driver, the SEH filter parks this thread and the
    /// engine keeps running with its stream parked. Off Windows it aborts
    /// like `inject_seh` (no filter, no card to keep): `tests/pipes.rs` runs
    /// it in the binary there; on Windows `iemmode inject-park` runs it on
    /// the PC.
    fn inject_park() {
        #[cfg(windows)]
        iem_audio_io::asio::raise_test_park();
        #[cfg(not(windows))]
        std::process::abort();
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
            RtOp::TestSignal { i, hz, amp, ttl } => self.start_test(i, hz, amp, ttl, None, false),
            RtOp::HilTestSignal {
                i,
                hz,
                amp,
                ttl,
                mask,
                listen,
            } => self.start_test(i, hz, amp, ttl, Some(mask), listen),
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
            RtOp::Seh => Self::inject_seh(),
            RtOp::Park => Self::inject_park(),
            RtOp::Arm => {
                if !self.armed {
                    self.armed = true;
                    self.restart_fade();
                }
            }
        }
    }

    /// X13: a sine replaces input `i` for `ttl` samples and then fades out;
    /// every TX is capped meanwhile. With `mask` (the HIL signal) the sine
    /// sounds only on those spare outputs until it ended, and every mix's TX
    /// is zero; `listen` adds the listen probe (S7).
    fn start_test(
        &mut self,
        i: u16,
        hz: f64,
        amp: f64,
        ttl: u64,
        mask: Option<HilMask>,
        listen: bool,
    ) {
        // A new HIL signal restarts the loopback measurement (S6 test 5).
        if mask.is_some() {
            self.latency.reset();
            self.status.loopback_samples.store(0, Ordering::Relaxed);
        }
        let len = samples(FADE_MS, self.sr);
        let mut fade = Ramp::new(0.0, len);
        fade.set(1.0);
        self.test = Some(TestRt {
            input: usize::from(i),
            phase: 0.0,
            inc: hz / self.sr,
            amp: amp.min(TEST_CAP),
            left: ttl,
            fade,
            end: self.time.saturating_add(ttl).saturating_add(u64::from(len)),
            mask,
            listen,
        });
        self.set_caps(true);
    }

    /// The fade-in from silence, unless the output is fading out for good.
    fn restart_fade(&mut self) {
        if !self.fading_out {
            self.fade = rise(self.fade_in);
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

    fn render(&mut self, block: &mut Block<'_>, off: usize, n: usize) {
        if self.test.as_ref().is_some_and(|t| t.end <= self.time) {
            self.test = None;
            self.set_caps(false);
        }
        let fade = self.fade_buf.get_mut(..n).unwrap_or_default();
        for f in fade.iter_mut() {
            *f = self.fade.tick();
        }
        // The test sine of this segment, before the input it replaces (X13).
        if let Some(t) = self.test.as_mut() {
            t.render(self.test_buf.get_mut(..n).unwrap_or_default());
        }
        self.render_inputs(block, off, n);
        self.render_mixes(block, off, n);
        self.render_hil(block, off, n);
        self.probe_latency(block, off, n);
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
        for (d, peak) in f.hil.iter_mut().zip(self.hil_peaks.iter_mut()) {
            let [p] = peak.take();
            *d = p;
        }
        self.meter_in.publish();
    }
}

impl Process for Processor {
    /// A driver reopen (S6): the output fades in again (not while held or
    /// fading out).
    #[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
    fn discontinuity(&mut self) {
        if self.armed {
            self.restart_fade();
        }
    }

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
mod tests;
