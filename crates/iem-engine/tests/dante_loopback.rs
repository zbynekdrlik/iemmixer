//! A mix through our own Dante device on loopback, bit for bit (Gen 3
//! phase C, plan `docs/superpowers/plans/2026-10-26-gen3-phase-c-engine-on-linux.md`
//! Task 4): the engine's real `Processor` inside the device's pump
//! (`iem_dante::stream::Pump`) on the channels `iem_engine::dante::check`
//! maps, the device's TX and RX on 127.0.0.1, our own flow-control server
//! granting our own RX one flow of our own TX (the self-loop: RX 1 and RX 2
//! hear TX 1 and TX 2, the engine's inputs; RX 5 and RX 6 hear TX 3 and
//! TX 4, the engine's outputs), resolved from our own records (`Own`, at
//! the test's server).
//!
//! The site is the smallest `[engine]` table `compile` accepts: two mono
//! inputs on RX 1 and RX 2, the engineer's stereo mix on TX 3 and TX 4
//! hearing both. The device is built in code: 6 RX, 4 TX, 96 kHz, 24 bits,
//! latency 1 ms, so P = 96 and D = 224.
//!
//! The media clock is a `ManualClock` the test moves; the pump, the TX pass
//! and the RX pass are stepped as their threads step them (the threads'
//! loops are glue), on a thread of the test's own: every packet sent is
//! received before time moves on, and every wait is bounded (5 s: the
//! flow's grant, each packet, and each step the test thread waits for, so a
//! step that never returns fails in seconds).

#![cfg(target_os = "linux")]

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use iem_audio_io::{Block, Offline, Planar, Process};
use iem_dante::announce::FPP_MAX;
use iem_dante::clock::{ManualClock, PtpClock, to_samples};
use iem_dante::daemon::media::{Clock, Driver, Media};
use iem_dante::device::{Device, DeviceConfig, Subscription};
use iem_dante::flows::client::Client;
use iem_dante::flows::server;
use iem_dante::info::Changes;
use iem_dante::media::ring::Ring;
use iem_dante::media::rx::{Receiver, RxCounts};
use iem_dante::media::source::Source;
use iem_dante::media::tx::{LaneCounts, TX_MARGIN_NS, Transmitter, ns_of};
use iem_dante::stream::{Blocks, Map, Pump, PumpStats, PumpView, Timing, WORK_MARGIN};
use iem_dante::subscribe::Answer;
use iem_dante::subscribe::resolve::{Advertised, Own, Resolver};
use iem_engine::cmd::push_group;
use iem_engine::core::{Core, Flags};
use iem_engine::dante::{check, timing};
use iem_engine::rt::{CMD_RING, Options, Processor, RtHandles};
use iem_engine::site::parse;
use iem_engine::topology::{Topology, compile};
use iem_engine_proto::{BandKind, Cmd, Eq, EqBand, EqTarget, InputId, MixId, MixState};
use tokio::net::UdpSocket;

/// Every wait's bound.
const BOUND: Duration = Duration::from_secs(5);
/// PTP time at the start: 1000 s, a whole second of samples at 96 kHz.
const START: u64 = 1_000_000_000_000;
const SECOND: u64 = 1_000_000_000;
const RATE: u32 = 96_000;
const BITS: u8 = 24;
/// Our device's name: our own records resolve it.
const DEVICE: &str = "test-device";

/// The smallest `[engine]` table `compile` accepts: two mono inputs on
/// RX 1 and RX 2, the engineer's stereo mix on TX 3 and TX 4 (every mix
/// hears every input, at its level).
const SITE: &str = r#"
[engine]
channels = 4
engineer = "engineer"

[[engine.inputs]]
id = "in1"
rx = [1]

[[engine.inputs]]
id = "in2"
rx = [2]

[[engine.mixes]]
id = "engineer"
tx = [3, 4]
"#;

/// Our device's RX channels (indices from 0): the engine's inputs and its
/// outputs heard back.
const RX1: usize = 0;
const RX2: usize = 1;
const RX3: usize = 2;
const RX5: usize = 4;
const RX6: usize = 5;

/// Full scale of a left-justified sample: 2³¹.
const FULL_SCALE: f64 = 2_147_483_648.0;

/// A left-justified sample as the engine's float, written out from its
/// definition (`stream::to_f64`): the test's own oracle, so a fault in the
/// code it checks cannot hide in both renders.
fn sample_to_f64(s: i32) -> f64 {
    f64::from(s) / FULL_SCALE
}

/// The engine's float on the 24-bit grid, left-justified, written out from
/// its definition (`stream::quantize` at 24 bits: rounded half away from
/// zero, saturating at full scale): the test's own oracle, as above.
fn on_24_bits(x: f64) -> i32 {
    let top = f64::from(1u32 << 23);
    let v = (x * top).round().clamp(-top, top - 1.0);
    (v as i32) << 8
}

fn topology() -> Arc<Topology> {
    Arc::new(compile(&parse(SITE).unwrap()).unwrap())
}

/// The device, built in code: 6 RX, 4 TX, 96 kHz, 24 bits, 1 ms.
fn device(dir: &Path) -> Device {
    Device::load(DeviceConfig {
        name: DEVICE.into(),
        manufacturer: "iemmixer".into(),
        model: "iem-dante".into(),
        rx: (1..=6).map(|n| format!("RX {n}")).collect(),
        tx: (1..=4).map(|n| format!("TX {n}")).collect(),
        sample_rate: RATE,
        encoding: BITS,
        latency_ns: 1_000_000,
        work_margin: WORK_MARGIN,
        tx_latency_ns: None,
        tx_margin_ns: TX_MARGIN_NS,
        tx_fpp_max: FPP_MAX,
        pump_cpu: None,
        io_cpu: None,
        state_file: dir.join("state.toml"),
    })
    .unwrap()
}

/// Our own records (`Own`: what the device announces), their flow control
/// at the test's server instead of UDP 4455.
struct OwnAt {
    own: Own,
    control: SocketAddrV4,
}

impl Resolver for OwnAt {
    fn lookup(&self, tx_device: &str, tx_channel: &str) -> Option<Advertised> {
        let control = self.control;
        self.own
            .lookup(tx_device, tx_channel)
            .map(|a| Advertised { control, ..a })
    }
}

/// The device on loopback with its self-loop granted: RX 1 and RX 2 hear
/// TX 1 and TX 2 (sources `tx1`, `tx2`), RX 5 and RX 6 the engine's TX 3
/// and TX 4, in one flow each way.
struct Rig {
    _dir: tempfile::TempDir,
    clock: Arc<ManualClock>,
    media: Arc<Media>,
    map: Map,
    timing: Timing,
    tx_slot: usize,
    rx_slot: usize,
    _driver: Driver,
    serving: tokio::task::AbortHandle,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.serving.abort();
    }
}

async fn rig(tx1: Source, tx2: Source) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let device = device(dir.path());
    let topo = topology();
    // Site RX n is our RX channel n, site TX n our TX channel n.
    let map = check(&device.cfg, &topo, &[]).unwrap();
    assert_eq!(
        map,
        Map {
            rx: vec![RX1, RX2],
            tx: vec![2, 3]
        }
    );
    let timing = timing(&device.cfg);
    assert_eq!(
        (timing.block, timing.playout, timing.offset()),
        (32, 96, 224)
    );
    let clock = Arc::new(ManualClock::new(START));
    let media = Arc::new(
        Media::new(
            &device,
            Ipv4Addr::LOCALHOST,
            Clock {
                ptp: clock.clone(),
                freq: Arc::default(),
                stats: Arc::default(),
            },
            7,
            &map.tx,
        )
        .unwrap(),
    );
    // Our flow-control server, at a port of its own (the resolver says
    // where): what the device serves on UDP 4455.
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let SocketAddr::V4(control) = sock.local_addr().unwrap() else {
        panic!("an IPv4 socket");
    };
    let serving = tokio::spawn(server::serve(
        sock,
        Arc::clone(&media.tx),
        Arc::clone(&media.requests),
    ))
    .abort_handle();
    {
        let mut manager = media.manager.lock().unwrap();
        for (rx, tx) in [(RX1, 1), (RX2, 2), (RX5, 3), (RX6, 4)] {
            assert!(manager.set(
                rx,
                Some(Subscription {
                    tx_channel: format!("TX {tx}"),
                    tx_device: DEVICE.into(),
                })
            ));
        }
    }
    assert!(media.tx.set_source(0, tx1));
    assert!(media.tx.set_source(1, tx2));
    for &tx in &map.tx {
        assert!(media.tx.set_source(tx, Source::Engine));
    }

    // Our client asks our server for the flow: all four channels in one.
    let resolver = OwnAt {
        own: Own::new(&device.state, &device.cfg, Ipv4Addr::LOCALHOST),
        control,
    };
    let (mut driver, mut answers) = Driver::new(
        Arc::clone(&media),
        Client::new(Ipv4Addr::LOCALHOST),
        Arc::new(Changes::default()),
    );
    driver.tick(&resolver);
    let (flow, answer) = tokio::time::timeout(BOUND, answers.recv())
        .await
        .expect("an answer within the bound")
        .expect("the driver holds a sender");
    assert!(matches!(answer, Answer::Granted(_)), "{answer:?}");
    driver.answered(flow, answer);
    let tx_slots: Vec<usize> = media.tx.lock().table.flows().map(|(s, _)| s).collect();
    let [tx_slot] = tx_slots[..] else {
        panic!("one TX flow: {tx_slots:?}");
    };
    let routed: Vec<usize> = (0..6).filter(|&c| media.rx.routed(c)).collect();
    assert_eq!(routed, [RX1, RX2, RX5, RX6]);
    Rig {
        _dir: dir,
        clock,
        media,
        map,
        timing,
        tx_slot,
        rx_slot: flow.slot,
        _driver: driver,
        serving,
    }
}

/// A command for the core: `input`'s level in the engineer's mix.
fn level(input: &str, gain_db: f64, pan: f64) -> Cmd {
    Cmd::SetLevel {
        mix: MixId::new("engineer"),
        source: iem_engine_proto::Source::Input(InputId::new(input)),
        gain_db: Some(gain_db),
        pan: Some(pan),
        muted: None,
    }
}

/// The core with `cmds` applied (each must apply).
fn core(cmds: &[Cmd]) -> Core {
    let mut core = Core::new(topology(), &MixState::default(), 0, Flags::default());
    for c in cmds {
        core.apply(c).unwrap();
    }
    core
}

/// A processor at the core's state, fading in over `fade_in_ms`.
fn processor(core: &Core, fade_in_ms: f64) -> (Processor, RtHandles) {
    Processor::new(
        topology(),
        &core.state(),
        &[],
        Options {
            fade_in_ms,
            hold: false,
        },
    )
}

/// The pump of `processor` on the rig's device, as `dante::start` builds
/// it (outputs quantized at the device's encoding).
fn pump<P: Process>(r: &Rig, processor: P) -> (Pump<P>, Arc<PumpStats>) {
    let stats = Arc::new(PumpStats::new(r.timing, RATE));
    let pump = Pump::new(
        processor,
        r.map.clone(),
        r.timing,
        BITS,
        Arc::clone(&r.media.rx),
        Arc::clone(&r.media.tx),
        Arc::clone(&stats),
    );
    (pump, stats)
}

/// Says the work on the test's thread made a step.
struct Beat(Sender<()>);

impl Beat {
    fn beat(&self) {
        let _ = self.0.send(());
    }
}

/// Runs `work` on a thread of its own and waits for it: each of its beats
/// within [`BOUND`] of the one before (a step that never returns fails the
/// test in 5 s, whatever the run's length); its result, or its panic.
fn bounded<T: Send + 'static>(work: impl FnOnce(&Beat) -> T + Send + 'static) -> T {
    let (beats, waits) = mpsc::channel();
    let worker = std::thread::spawn(move || work(&Beat(beats)));
    loop {
        match waits.recv_timeout(BOUND) {
            Ok(()) => {}
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => panic!("no step within {BOUND:?}"),
        }
    }
    match worker.join() {
        Ok(out) => out,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// The device's TX and RX passes and the engine's pump, stepped as their
/// threads step them on the manual clock.
struct Steps {
    clock: Arc<ManualClock>,
    media: Arc<Media>,
    tx: Transmitter,
    rx: Receiver,
    tx_slot: usize,
    rx_slot: usize,
}

/// What one step did.
struct Step {
    /// Its PTP ns (none: the clock was not ready).
    now: Option<u64>,
    /// The pump's next block's first input stamp (none while free pacing):
    /// every block before it ran.
    next_start: Option<u64>,
}

impl Steps {
    fn new(clock: &Arc<ManualClock>, media: &Arc<Media>, tx_slot: usize, rx_slot: usize) -> Steps {
        Steps {
            clock: Arc::clone(clock),
            tx: Transmitter::new(Arc::clone(&media.tx)),
            rx: Receiver::new(Arc::clone(&media.rx)).unwrap(),
            media: Arc::clone(media),
            tx_slot,
            rx_slot,
        }
    }

    /// One step at the clock's now: the pump (every block due on the media
    /// clock, or one free block while it is not ready), the TX pass, every
    /// packet it sent received; then the clock moves on to the earlier of
    /// the TX's and the pump's wake.
    fn step<P: Process>(&mut self, pump: &mut Pump<P>, timing: &Timing) -> Step {
        let now = self.clock.now_ns();
        let epoch = self.clock.epoch();
        let wake = match now {
            Some(ns) => Some(pump.media(to_samples(ns, RATE), epoch)),
            None => {
                pump.free();
                None
            }
        };
        self.tx.step(now, epoch);
        let sent = self.media.tx.lane(self.tx_slot).packets;
        let deadline = Instant::now() + BOUND;
        while self.media.rx.lane(self.rx_slot).packets < sent {
            assert!(
                Instant::now() < deadline,
                "sent {sent}, received {:?}",
                self.media.rx.lane(self.rx_slot)
            );
            self.rx.wait(Duration::from_millis(10)).unwrap();
            self.rx.pass(self.clock.now_ns(), self.media.rx.now_ns());
        }
        if let Some(wake) = wake {
            self.clock
                .sleep_until(self.tx.wake_ns(now).min(ns_of(wake, RATE)));
        }
        Step {
            now,
            // The pump wakes when its next block is due: start + B + P.
            next_start: wake.map(|w| w - timing.block - timing.playout),
        }
    }
}

/// One RX channel's ring from a position on, read back one sample at a
/// time (a position never written reads 0 and is marked so).
#[derive(Debug, Clone)]
struct Tape {
    ring: usize,
    from: u64,
    samples: Vec<i32>,
    written: Vec<bool>,
}

impl Tape {
    fn new(ring: usize, from: u64) -> Tape {
        Tape {
            ring,
            from,
            samples: Vec::new(),
            written: Vec::new(),
        }
    }

    fn end(&self) -> u64 {
        self.from + self.samples.len() as u64
    }

    /// Reads on up to `to` (positions the device has for good).
    fn read_to(&mut self, rings: &[Ring], to: u64) {
        let mut one = [0];
        for pos in self.end()..to {
            let read = rings[self.ring].read_at(pos, &mut one);
            self.samples.push(one[0]);
            self.written.push(read.holes == 0);
        }
    }

    fn index(&self, pos: u64) -> usize {
        usize::try_from(pos - self.from).unwrap()
    }

    fn at(&self, pos: u64) -> i32 {
        self.samples[self.index(pos)]
    }

    fn was_written(&self, pos: u64) -> bool {
        self.written[self.index(pos)]
    }

    /// The first position from `pos` on that a packet wrote.
    fn first_written(&self, pos: u64) -> Option<u64> {
        (pos..self.end()).find(|&p| self.was_written(p))
    }
}

/// What a run leaves for the test thread's checks.
struct Run {
    inputs: Vec<Tape>,
    outputs: Vec<Tape>,
    pump: PumpView,
    tx: LaneCounts,
    rx: RxCounts,
}

impl Run {
    /// The tapes, and the counts now: the pump's, the flow's TX and RX
    /// lanes.
    fn new(steps: &Steps, inputs: Vec<Tape>, outputs: Vec<Tape>, pump: &PumpStats) -> Run {
        Run {
            inputs,
            outputs,
            pump: pump.view(),
            tx: steps.media.tx.lane(steps.tx_slot),
            rx: steps.media.rx.lane(steps.rx_slot),
        }
    }
}

/// Steps from the clock's now until PTP ns `until`, reading the tapes on
/// after each step: the inputs up to what the pump has read, the outputs
/// up to the playout point (what every packet sent has reached).
fn run_until<P: Process>(
    steps: &mut Steps,
    pump: &mut Pump<P>,
    timing: &Timing,
    until: u64,
    inputs: &mut [Tape],
    outputs: &mut [Tape],
    beat: &Beat,
) -> Step {
    let mut last = None;
    while steps.clock.now_ns().unwrap() < until {
        let step = steps.step(pump, timing);
        let rings = steps.media.rx.rings();
        let next_start = step.next_start.unwrap();
        for t in inputs.iter_mut() {
            t.read_to(rings, next_start);
        }
        let playout = to_samples(step.now.unwrap(), RATE) - timing.playout;
        for t in outputs.iter_mut() {
            t.read_to(rings, playout);
        }
        beat.beat();
        last = Some(step);
    }
    last.expect("a step")
}

/// The device's RX lane saw every packet whole and in time.
fn assert_clean_rx(rx: &RxCounts) {
    assert_eq!(
        (rx.late, rx.out_of_order, rx.refused, rx.stray, rx.unplaced),
        (0, 0, 0, 0, 0),
        "{rx:?}"
    );
}

/// TX 3 and TX 4 (the engine's) sent silence counted as holes exactly for
/// their stamps from the flow's first, `first`, before the engine's first
/// output, `engine`: 2 channels each.
fn engine_holes(first: u64, engine: u64) -> u64 {
    assert!(first <= engine, "{first} {engine}");
    2 * (engine - first)
}

#[tokio::test]
async fn the_mix_through_our_device_is_the_offline_render_bit_for_bit() {
    // TX 1 the counter, TX 2 a sine: RX 1 and RX 2, the engine's inputs.
    let r = rig(
        Source::Counter,
        Source::Sine {
            hz: 997.0,
            dbfs: -20.0,
        },
    )
    .await;
    // A mix with something to do: both inputs at their levels and pans,
    // and a band of the mix's EQ.
    let mut eq = Eq::default();
    eq.bands[2] = EqBand {
        kind: BandKind::Peak,
        enabled: true,
        freq_hz: 1_000.0,
        gain_db: 6.0,
        bw_oct: 1.0,
    };
    let core = core(&[
        level("in1", -3.0, -0.4),
        level("in2", 0.0, 0.6),
        Cmd::SetEq {
            target: EqTarget::Mix(MixId::new("engineer")),
            eq,
        },
    ]);
    // Two processors of the same state: one in the device's pump, the
    // other for the offline render.
    let (live, _live_handles) = processor(&core, 0.0);
    let (mut reference, _reference_handles) = processor(&core, 0.0);
    let (mut pump, stats) = pump(&r, live);
    let timing = r.timing;
    let d = timing.offset();
    // The pump's first block: the first media step is at START.
    let s0 = Blocks::bootstrap(to_samples(START, RATE), timing).start();
    let (clock, media) = (Arc::clone(&r.clock), Arc::clone(&r.media));
    let (tx_slot, rx_slot) = (r.tx_slot, r.rx_slot);
    let run = bounded(move |beat| {
        let mut steps = Steps::new(&clock, &media, tx_slot, rx_slot);
        let mut inputs = vec![Tape::new(RX1, s0), Tape::new(RX2, s0)];
        let mut outputs = vec![Tape::new(RX5, s0), Tape::new(RX6, s0)];
        run_until(
            &mut steps,
            &mut pump,
            &timing,
            START + SECOND,
            &mut inputs,
            &mut outputs,
            beat,
        );
        Run::new(&steps, inputs, outputs, &stats)
    });

    // The offline render over the samples RX 1 and RX 2 held, block 32
    // from the pump's first block on.
    let frames = run.inputs[0].samples.len();
    let mut input = Planar::new(2, frames);
    for (c, tape) in run.inputs.iter().enumerate() {
        for (x, &s) in input.channel_mut(c).iter_mut().zip(&tape.samples) {
            *x = sample_to_f64(s);
        }
    }
    let offline = Offline { block: 32 }.run(&mut reference, &input, 2);
    assert!(offline.fault.is_none());

    // RX 5 and RX 6 at stamp T are quantize(offline(T − D), 24) for every
    // T from the engine's first output on; silence before it.
    let (left, right) = (&run.outputs[0], &run.outputs[1]);
    assert_eq!(left.end(), right.end());
    let mut compared = 0;
    let mut loudest = 0i64;
    let mut differ = false;
    for t in s0..left.end() {
        let got = [left.at(t), right.at(t)];
        let Some(i) = (t - s0).checked_sub(d) else {
            assert_eq!(got, [0, 0], "before the engine's first output, at {t}");
            continue;
        };
        let i = usize::try_from(i).unwrap();
        assert!(i < frames, "{i} {frames}");
        let want = [
            on_24_bits(offline.output.channel(0)[i]),
            on_24_bits(offline.output.channel(1)[i]),
        ];
        assert_eq!(got, want, "stamp {t}: s0 + D + {i}");
        assert!(left.was_written(t) && right.was_written(t), "{t}");
        compared += 1;
        loudest = got.iter().fold(loudest, |m, &s| m.max(i64::from(s).abs()));
        differ |= got[0] != got[1];
    }
    // One simulated second, a real mix: loud, and the pans set it apart.
    assert!(compared > 95_000, "{compared}");
    assert!(loudest > 1 << 26, "{loudest}");
    assert!(differ);

    // The flow: about 3 000 packets, all received in time; TX 3 and TX 4
    // sent holes only before the engine's first output.
    assert!((2_990..=3_000).contains(&run.tx.packets), "{:?}", run.tx);
    assert_eq!(run.rx.packets, run.tx.packets);
    assert_clean_rx(&run.rx);
    let first = left.first_written(s0).unwrap();
    assert_eq!(first, right.first_written(s0).unwrap());
    assert_eq!(
        (run.tx.send_errors, run.tx.rebootstraps, run.tx.holes),
        (0, 0, engine_holes(first, s0 + d)),
        "{:?}",
        run.tx
    );
    // The pump: every block on time, none skipped.
    let v = run.pump;
    assert!(v.media, "{v:?}");
    assert_eq!(
        (v.offset, v.playout, v.late, v.missed, v.rebootstraps),
        (224, 96, 0, 0, 0),
        "{v:?}"
    );
    assert_eq!(v.blocks, frames as u64 / 32, "{v:?}");
}

#[tokio::test]
async fn the_test_mix_adds_no_delay() {
    // TX 1 echoes RX 3, which holds an impulse the test wrote (silence
    // around it); RX 1 hears TX 1, so the impulse arrives on RX 1 at its
    // own stamp.
    let r = rig(Source::Echo { rx: RX3, offset: 0 }, Source::Silence).await;
    let at = to_samples(START, RATE);
    let impulse = 0x2345_6700; // on the 24-bit grid, about −11 dBFS
    let stamp = at + 9_601;
    let mut written = vec![0; 32_768];
    written[usize::try_from(stamp - at + 2_048).unwrap()] = impulse;
    r.media.rx.rings()[RX3].write_at(at - 2_048, &written);
    // The test mix: RX 1 at 0 dB, centre pan (√2·sin π/4 = 1), flat EQ,
    // the default limiter (−6 dB, zero lookahead) never reached.
    let core = core(&[level("in1", 0.0, 0.0)]);
    let (live, _handles) = processor(&core, 0.0);
    let (mut pump, stats) = pump(&r, live);
    let timing = r.timing;
    let d = timing.offset();
    let s0 = Blocks::bootstrap(at, timing).start();
    let (clock, media) = (Arc::clone(&r.clock), Arc::clone(&r.media));
    let (tx_slot, rx_slot) = (r.tx_slot, r.rx_slot);
    let run = bounded(move |beat| {
        let mut steps = Steps::new(&clock, &media, tx_slot, rx_slot);
        let mut inputs = vec![Tape::new(RX1, s0)];
        let mut outputs = vec![Tape::new(RX5, s0), Tape::new(RX6, s0)];
        run_until(
            &mut steps,
            &mut pump,
            &timing,
            START + SECOND / 5,
            &mut inputs,
            &mut outputs,
            beat,
        );
        Run::new(&steps, inputs, outputs, &stats)
    });

    let rx1 = &run.inputs[0];
    assert_eq!(rx1.at(stamp), impulse);
    for out in &run.outputs {
        // The first sample that is not silence is the impulse's own stamp
        // + D, of equal value; every output sample is RX 1's at T − D.
        let first = (s0..out.end()).find(|&t| out.at(t) != 0);
        assert_eq!(first, Some(stamp + d), "RX {}", out.ring + 1);
        assert_eq!(out.at(stamp + d), impulse);
        for t in s0 + d..out.end() {
            assert_eq!(out.at(t), rx1.at(t - d), "RX {} at {t}", out.ring + 1);
        }
    }
    // The echo read every sample it sent (no hole); TX 3 and TX 4 sent
    // holes only before the engine's first output.
    let first = run.outputs[0].first_written(s0).unwrap();
    assert_eq!(run.tx.holes, engine_holes(first, s0 + d), "{:?}", run.tx);
    assert_eq!(run.rx.packets, run.tx.packets);
    assert_clean_rx(&run.rx);
    assert_eq!((run.pump.late, run.pump.rebootstraps), (0, 0));
}

/// The real processor, counting the pump's `discontinuity` calls.
struct Counted {
    inner: Processor,
    discontinuities: Arc<AtomicU64>,
}

impl Process for Counted {
    fn process(&mut self, block: &mut Block<'_>) {
        self.inner.process(block);
    }

    fn discontinuity(&mut self) {
        self.discontinuities.fetch_add(1, Ordering::Relaxed);
        self.inner.discontinuity();
    }
}

/// The fade-in of the re-anchor test: 5 ms, 480 samples. A (re-)start's
/// first blocks read the RX ring before the TX's first stamp (silence, up
/// to B + P samples), so the fade must last longer to be heard.
const FADE_MS: f64 = 5.0;
const FADE: u64 = 480;

/// From `from` to `to`, RX 5 is RX 1 at T − D, bit for bit (the test mix:
/// RX 1 at 0 dB, centre pan, flat EQ, the limiter never reached).
fn assert_identity(input: &Tape, output: &Tape, d: u64, from: u64, to: u64) {
    assert!(from < to, "{from} {to}");
    for t in from..to {
        assert_eq!(output.at(t), input.at(t - d), "at {t}");
    }
}

/// From `from`, a fade-in of [`FADE`] samples: never louder than RX 1 at
/// T − D, nor of the other sign, and its first sound well below the input
/// (silence at its start: the ring before the TX's first stamp).
fn assert_fading_in(input: &Tape, output: &Tape, d: u64, from: u64) {
    for t in from..from + FADE {
        let (y, x) = (i64::from(output.at(t)), i64::from(input.at(t - d)));
        assert!(y.abs() <= x.abs() && y * x >= 0, "at {t}: {y} {x}");
    }
    let sound = (from..from + FADE / 2)
        .find(|&t| input.at(t - d) != 0)
        .expect("the input sounds in the fade's first half");
    let (y, x) = (output.at(sound), input.at(sound - d));
    assert!(
        i64::from(y).abs() * 2 < i64::from(x).abs(),
        "at {sound}: {y} {x}"
    );
}

#[tokio::test]
async fn a_re_anchor_rebootstraps_the_pump_and_restarts_the_fade() {
    // The counter through the test mix, fading in over 5 ms (`FADE_MS`).
    let r = rig(Source::Counter, Source::Silence).await;
    let core = core(&[level("in1", 0.0, 0.0)]);
    let (live, _handles) = processor(&core, FADE_MS);
    let discontinuities = Arc::new(AtomicU64::new(0));
    let counted = Counted {
        inner: live,
        discontinuities: Arc::clone(&discontinuities),
    };
    let (mut pump, stats) = pump(&r, counted);
    let timing = r.timing;
    let d = timing.offset();
    let s0 = Blocks::bootstrap(to_samples(START, RATE), timing).start();
    // The re-anchor: 10 ms forward, the epoch counts it.
    let jump_ns = 10_000_000;
    let (clock, media) = (Arc::clone(&r.clock), Arc::clone(&r.media));
    let (tx_slot, rx_slot) = (r.tx_slot, r.rx_slot);
    let seen = Arc::clone(&discontinuities);
    let (run, before, after) = bounded(move |beat| {
        let mut steps = Steps::new(&clock, &media, tx_slot, rx_slot);
        let mut inputs = vec![Tape::new(RX1, s0)];
        let mut outputs = vec![Tape::new(RX5, s0)];
        let last = run_until(
            &mut steps,
            &mut pump,
            &timing,
            START + SECOND / 5,
            &mut inputs,
            &mut outputs,
            beat,
        );
        let before = (
            last.next_start.unwrap(),
            outputs[0].end(),
            media.tx.lane(tx_slot),
            stats.view(),
            seen.load(Ordering::Relaxed),
        );
        clock.re_anchor(jump_ns);
        // The pump starts again at the first step's media time.
        let again = Blocks::bootstrap(to_samples(clock.now_ns().unwrap(), RATE), timing).start();
        let until = clock.now_ns().unwrap() + SECOND / 5;
        run_until(
            &mut steps,
            &mut pump,
            &timing,
            until,
            &mut inputs,
            &mut outputs,
            beat,
        );
        let after = (again, seen.load(Ordering::Relaxed));
        (Run::new(&steps, inputs, outputs, &stats), before, after)
    });
    let (old_next, old_end, old_tx, old_pump, old_seen) = before;
    let (again, seen) = after;
    let (rx1, rx5) = (&run.inputs[0], &run.outputs[0]);

    // Before: one discontinuity (the first media step), no re-bootstrap;
    // the fade-in at the start, then the input bit for bit.
    assert_eq!(old_seen, 1);
    assert_eq!(
        (old_pump.rebootstraps, old_pump.missed),
        (0, 0),
        "{old_pump:?}"
    );
    let first = rx5.first_written(s0).unwrap();
    assert_eq!(old_tx.holes, engine_holes(first, s0 + d), "{old_tx:?}");
    assert_fading_in(rx1, rx5, d, s0 + d);
    assert_identity(rx1, rx5, d, s0 + d + FADE, old_end);

    // The re-anchor: one re-bootstrap skipping the blocks from the old
    // next start to the new start, one discontinuity, the TX realigned
    // once.
    let v = run.pump;
    assert_eq!(seen, old_seen + 1);
    assert_eq!(v.rebootstraps, 1, "{v:?}");
    assert!(again > old_next, "{again} {old_next}");
    assert_eq!(v.missed, (again - old_next) / timing.block, "{v:?}");
    assert_eq!(v.late, 0, "{v:?}");
    assert_eq!(run.tx.rebootstraps, 1, "{:?}", run.tx);
    // TX holes only for the skipped stamps: the TX's first stamp after the
    // jump (the first written past the gap the jump left on RX 5) lies at
    // or after the first skipped block's output stamp, its holes end at
    // the new start's first output, and they are silence.
    let gap = (old_end..rx5.end())
        .find(|&t| !rx5.was_written(t))
        .expect("the jump leaves a gap");
    let resumed = rx5.first_written(gap).unwrap();
    assert!(resumed >= old_next + d, "{resumed} {old_next}");
    assert!((resumed..again + d).all(|t| rx5.was_written(t) && rx5.at(t) == 0));
    assert_eq!(
        run.tx.holes - old_tx.holes,
        engine_holes(resumed, again + d),
        "{:?}",
        run.tx
    );
    // The fade restarts at the new start's output, then the input again.
    assert_fading_in(rx1, rx5, d, again + d);
    assert_identity(rx1, rx5, d, again + d + FADE, rx5.end());
    assert_eq!(run.rx.packets, run.tx.packets);
    assert_clean_rx(&run.rx);
}

#[tokio::test]
async fn nothing_is_sent_before_the_clock_is_ready_yet_commands_apply() {
    let r = rig(Source::Counter, Source::Silence).await;
    r.clock.set_ready(false);
    // A processor that hears nothing; a level command opens RX 1 in the
    // engineer's mix (0 dB, centre: the test mix).
    let mut core = core(&[]);
    let (live, mut handles) = processor(&core, 0.0);
    let open = core.apply(&level("in1", 0.0, 0.0)).unwrap();
    assert!(push_group(&mut handles.cmds, 0, &open.rt));
    let (mut pump, stats) = pump(&r, live);
    let timing = r.timing;
    let d = timing.offset();
    let (clock, media) = (Arc::clone(&r.clock), Arc::clone(&r.media));
    let (tx_slot, rx_slot, engine) = (r.tx_slot, r.rx_slot, r.map.tx.clone());
    // 64 free blocks: 2 048 samples, past the level's 10 ms ramp.
    let free = 64;
    let (run, waiting, s0) = bounded(move |beat| {
        let mut steps = Steps::new(&clock, &media, tx_slot, rx_slot);
        for _ in 0..free {
            let step = steps.step(&mut pump, &timing);
            assert_eq!(step.now, None);
            beat.beat();
        }
        let waiting = (
            stats.view(),
            (0..32).map(|s| media.tx.lane(s).packets).sum::<u64>(),
            media.rx.lane(rx_slot),
            engine
                .iter()
                .map(|&tx| media.tx.engine_ring(tx).unwrap().counters().written)
                .sum::<u64>(),
            handles.cmds.slots(),
        );
        clock.set_ready(true);
        let s0 = Blocks::bootstrap(to_samples(clock.now_ns().unwrap(), RATE), timing).start();
        let mut inputs = vec![Tape::new(RX1, s0)];
        let mut outputs = vec![Tape::new(RX5, s0), Tape::new(RX6, s0)];
        run_until(
            &mut steps,
            &mut pump,
            &timing,
            START + SECOND / 10,
            &mut inputs,
            &mut outputs,
            beat,
        );
        (Run::new(&steps, inputs, outputs, &stats), waiting, s0)
    });

    // While the clock was not ready: free blocks only, nothing sent or
    // received, nothing in the engine rings, and the command drained.
    let (v, tx_packets, rx, engine_written, slots) = waiting;
    assert!(!v.media, "{v:?}");
    assert_eq!(v.blocks, free, "{v:?}");
    assert_eq!((tx_packets, rx.packets, rx.unplaced), (0, 0, 0), "{rx:?}");
    assert_eq!(engine_written, 0);
    assert_eq!(slots, CMD_RING);

    // Once ready: from the engine's first output on, RX 5 and RX 6 are RX 1
    // at T − D at full level: the command and its ramp were done before.
    let rx1 = &run.inputs[0];
    for out in &run.outputs {
        assert_identity(rx1, out, d, s0 + d, out.end());
    }
    assert!(run.outputs[0].samples.iter().any(|&s| s != 0));
    let v = run.pump;
    assert!(v.media, "{v:?}");
    assert_eq!((v.late, v.rebootstraps), (0, 0), "{v:?}");
    assert!(run.tx.packets > 0);
    assert_eq!(run.rx.packets, run.tx.packets);
    assert_clean_rx(&run.rx);
}
