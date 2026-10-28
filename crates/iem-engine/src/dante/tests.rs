//! The Dante backend's decisions (Gen 3 phase C, plan Task 3): the channel
//! map and its refusals, the timing, the device's options and errors, and
//! the driver over a fake device and a stand-in pump thread.

use super::*;

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc;

use iem_audio_io::{Block, Process};
use iem_dante::media::ring::Ring;
use iem_dante::media::rx::RxShared;
use iem_dante::media::tx::TxShared;

use crate::test_support::test_site;

const TEST_DEVICE: &str = include_str!("../../../../config/test-dante-device.toml");
/// Every wait in these tests ends.
const BOUND: Duration = Duration::from_secs(5);

/// The synthetic test device with `rx` RX and `tx` TX channels.
fn device(rx: usize, tx: usize) -> DeviceConfig {
    let mut cfg = DeviceConfig::from_toml(TEST_DEVICE).unwrap();
    cfg.rx = (1..=rx).map(|n| format!("RX {n}")).collect();
    cfg.tx = (1..=tx).map(|n| format!("TX {n}")).collect();
    cfg
}

/// Device channel numbers (from 1) as indices (from 0).
fn indices(channels: &[u16]) -> Vec<usize> {
    channels.iter().map(|&c| usize::from(c) - 1).collect()
}

// ---- the channel map ----

#[test]
fn channel_numbers_map_to_the_same_device_channels() {
    let topo = test_site();
    let highest_rx = *topo.rx.iter().max().unwrap();
    let highest_tx = *topo.tx.iter().max().unwrap();
    let map = check(
        &device(usize::from(highest_rx), usize::from(highest_tx)),
        &topo,
        &[],
    )
    .unwrap();
    // Site RX 101 is our RX channel 101, index 100; TX 71 is index 70.
    assert_eq!(topo.rx[0], 101);
    assert_eq!(map.rx[0], 100);
    let tx71 = topo.tx.iter().position(|&c| c == 71).unwrap();
    assert_eq!(map.tx[tx71], 70);
    assert_eq!(map.rx, indices(&topo.rx), "every input, in topology order");
    assert_eq!(map.tx, indices(&topo.tx), "every output, in topology order");
}

#[test]
fn hil_returns_follow_the_topology_rx() {
    let topo = test_site();
    let hil = [94, 95];
    let map = check(&device(132, 95), &topo, &hil).unwrap();
    let mut rx = indices(&topo.rx);
    rx.extend([93, 94]);
    let mut tx = indices(&topo.tx);
    tx.extend([93, 94]);
    assert_eq!(map.rx, rx, "the returns after the topology's RX");
    assert_eq!(map.tx, tx, "the spare outputs after the topology's TX");
    assert_eq!(map.rx.len(), topo.rx.len() + 2);
}

#[test]
fn a_site_channel_the_device_lacks_is_a_card_refusal() {
    let topo = test_site();
    let card = |r: Result<Map, EngineError>| match r {
        Err(EngineError::Card(why)) => why,
        other => panic!("{other:?}"),
    };
    let why = card(check(&device(131, 93), &topo, &[]));
    assert!(why.contains("rx channel 132"), "{why}");
    assert!(why.contains("131 channels"), "{why}");
    let why = card(check(&device(132, 92), &topo, &[]));
    assert!(why.contains("tx channel 93"), "{why}");
    // A spare output the device lacks, and its loopback return.
    let why = card(check(&device(132, 94), &topo, &[94, 95]));
    assert!(why.contains("tx channel 95"), "{why}");
    let why = card(check(&device(132, 140), &topo, &[133]));
    assert!(why.contains("HIL loopback return"), "{why}");
    assert!(why.contains("133"), "{why}");
}

#[test]
fn a_device_at_another_rate_is_refused() {
    let topo = test_site();
    for rate in [48_000, 192_000] {
        let mut cfg = device(132, 95);
        cfg.sample_rate = rate;
        match check(&cfg, &topo, &[]) {
            Err(EngineError::Card(why)) => assert!(why.contains(&format!("{rate} Hz")), "{why}"),
            other => panic!("{rate}: {other:?}"),
        }
    }
    assert!(check(&device(132, 95), &topo, &[]).is_ok(), "96 kHz runs");
}

#[test]
fn the_timing_is_32_samples_the_devices_latency_and_1_ms() {
    let mut cfg = device(2, 2);
    let t = timing(&cfg);
    assert_eq!((t.block, t.playout, t.margin), (32, 96, 96), "1 ms");
    assert_eq!(t.offset(), 224);
    cfg.latency_ns = 2_000_000;
    let t = timing(&cfg);
    assert_eq!((t.playout, t.offset()), (192, 320), "2 ms");
}

#[test]
fn the_work_margin_comes_from_the_device_config() {
    // 0.25 ms with the box's TX settings: the device refuses an advertised
    // latency shorter than tx_fpp_max frames + tx_margin_ns + its send slack.
    let text = TEST_DEVICE.replace(
        "latency_ns = 1000000",
        "latency_ns = 250000\nwork_margin = 48\ntx_fpp_max = 8\ntx_margin_ns = 30000",
    );
    assert_ne!(
        text, TEST_DEVICE,
        "the test config states latency_ns = 1000000"
    );
    let cfg = DeviceConfig::from_toml(&text).unwrap();
    let t = timing(&cfg);
    assert_eq!((t.block, t.playout, t.margin), (32, 24, 48));
    assert_eq!(t.offset(), 104, "D = 32 + 24 + 48, below 2.x's 129");
}

#[test]
fn the_dante_flags_become_the_devices_options() {
    let iface = DanteArgs {
        config: PathBuf::from("dev.toml"),
        place: DantePlace::Iface("eth9".into()),
        control: PathBuf::from("ctl.sock"),
    };
    assert_eq!(
        options(&iface),
        Options {
            config: PathBuf::from("dev.toml"),
            place: Place::Iface("eth9".into()),
            control: PathBuf::from("ctl.sock"),
        }
    );
    let ip = DanteArgs {
        place: DantePlace::Ip(Ipv4Addr::new(10, 0, 0, 10)),
        ..iface
    };
    assert_eq!(options(&ip).place, Place::Ip(Ipv4Addr::new(10, 0, 0, 10)));
}

#[test]
fn the_devices_errors_keep_their_exit_codes() {
    match host_error(HostError::Setup("no config".into())) {
        EngineError::Usage(why) => assert!(why.contains("no config"), "{why}"),
        other => panic!("{other:?}"),
    }
    match host_error(HostError::Run("no address".into())) {
        EngineError::Io(e) => assert!(e.to_string().contains("no address"), "{e}"),
        other => panic!("{other:?}"),
    }
}

// ---- the driver ----

/// What the stand-ins saw, in order.
type Log = Arc<Mutex<Vec<String>>>;

fn said(log: &Log, what: &str) {
    log.lock().unwrap().push(what.to_owned());
}

/// A device that ended by itself (`ended`), or not, and logs its stop
/// with whether the pump's thread had ended by then.
struct Fake {
    ended: Option<String>,
    log: Log,
    pump_ended: Arc<AtomicBool>,
    answer: Stopped,
}

impl Device for Fake {
    fn ended(&self) -> Option<String> {
        self.ended.clone()
    }

    fn stop(self: Box<Self>, within: Duration) -> Stopped {
        let pump = if self.pump_ended.load(Ordering::Acquire) {
            "pump ended"
        } else {
            "pump running"
        };
        said(&self.log, &format!("device stop within {within:?}, {pump}"));
        self.answer.clone()
    }
}

/// A stand-in pump thread that ends at its stop flag and says so.
fn pump_thread(stop: &Arc<AtomicBool>, ended: &Arc<AtomicBool>, log: &Log) -> JoinHandle<()> {
    let (stop, ended, log) = (Arc::clone(stop), Arc::clone(ended), Arc::clone(log));
    std::thread::spawn(move || {
        let start = Instant::now();
        while !stop.load(Ordering::Acquire) && start.elapsed() < BOUND {
            std::thread::sleep(Duration::from_millis(1));
        }
        said(&log, "pump ended");
        ended.store(true, Ordering::Release);
    })
}

struct Rig {
    driver: DanteDriver,
    stats: Arc<PumpStats>,
    reported: Arc<AtomicBool>,
    log: Log,
}

fn rig(ended: Option<&str>) -> Rig {
    rig_with(ended, Stopped::Done)
}

/// A driver whose device answers its stop with `answer`.
fn rig_with(ended: Option<&str>, answer: Stopped) -> Rig {
    let stats = Arc::new(PumpStats::new(timing(&device(2, 2)), 96_000));
    let (pump_stop, pump_ended) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let log: Log = Arc::default();
    let thread = pump_thread(&pump_stop, &pump_ended, &log);
    let reported = Arc::new(AtomicBool::new(false));
    let fake = Fake {
        ended: ended.map(str::to_owned),
        log: Arc::clone(&log),
        pump_ended,
        answer,
    };
    let driver = DanteDriver::new(
        Arc::clone(&stats),
        thread,
        pump_stop,
        Arc::clone(&reported),
        Box::new(fake),
    );
    Rig {
        driver,
        stats,
        reported,
        log,
    }
}

#[test]
fn a_reported_signal_is_a_stop_ending() {
    let r = rig(None);
    assert_eq!(r.driver.ending(), None, "serving, no signal");
    r.reported.store(true, Ordering::Release);
    assert_eq!(r.driver.ending(), Some(Ending::Stop));
    assert_eq!(r.driver.ending(), Some(Ending::Stop), "it stays");
    assert_eq!(Box::new(r.driver).stop(), StopOutcome::Released);
}

#[test]
fn a_device_that_ended_is_a_backend_ending() {
    let r = rig(Some("flow control on UDP 4455 ended"));
    assert_eq!(
        r.driver.ending(),
        Some(Ending::Backend("flow control on UDP 4455 ended".into()))
    );
    // The device's own end wins over a signal reported meanwhile.
    r.reported.store(true, Ordering::Release);
    assert_eq!(
        r.driver.ending(),
        Some(Ending::Backend("flow control on UDP 4455 ended".into()))
    );
    assert_eq!(Box::new(r.driver).stop(), StopOutcome::Released);
}

/// Runs `f` on a thread: its result, or a failure after [`BOUND`]
/// instead of a hang.
fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(BOUND).expect("returns within the bound")
}

#[test]
fn stop_ends_the_pump_before_the_device() {
    let r = rig(None);
    let log = Arc::clone(&r.log);
    let outcome = bounded(move || Box::new(r.driver).stop());
    assert_eq!(outcome, StopOutcome::Released);
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            "pump ended".to_owned(),
            format!("device stop within {STOP_WITHIN:?}, pump ended"),
        ]
    );
    assert_eq!(
        (PUMP_WITHIN, STOP_WITHIN),
        (Duration::from_secs(1), Duration::from_secs(7))
    );
}

#[test]
fn a_device_that_fails_or_stays_is_still_released() {
    for answer in [Stopped::Failed("goodbyes".into()), Stopped::StillRunning] {
        let r = rig_with(None, answer.clone());
        let log = Arc::clone(&r.log);
        let outcome = bounded(move || Box::new(r.driver).stop());
        assert_eq!(outcome, StopOutcome::Released, "{answer:?}");
        assert_eq!(log.lock().unwrap().len(), 2, "{answer:?}");
    }
}

/// Copies nothing: the pump's free blocks only need a processor.
struct Quiet;

impl Process for Quiet {
    fn process(&mut self, _block: &mut Block<'_>) {}
}

#[test]
fn stats_are_the_pumps() {
    let r = rig(None);
    // A pump counting into the driver's stats: three free blocks.
    let cfg = device(2, 2);
    let rings: Arc<[Ring]> = (0..2).map(|_| Ring::new()).collect::<Vec<_>>().into();
    let rx = Arc::new(RxShared::new(96_000, Ipv4Addr::LOCALHOST, Arc::clone(&rings)).unwrap());
    let tx = Arc::new(TxShared::new(cfg.clone(), Ipv4Addr::LOCALHOST, rings, 1, &[0, 1]).unwrap());
    let map = Map {
        rx: vec![0, 1],
        tx: vec![0, 1],
    };
    let mut pump = Pump::new(Quiet, map, timing(&cfg), 24, rx, tx, Arc::clone(&r.stats));
    for _ in 0..3 {
        pump.free();
    }
    let stats = r.driver.stats();
    assert_eq!(stats, r.stats.stream_stats());
    assert_eq!((stats.frames, stats.callbacks), (32, 3));
    let h = r.driver.histograms().expect("the pump's histograms");
    assert_eq!(h, r.stats.histograms());
    assert_eq!(h.process.iter().map(|e| e.1).sum::<u64>(), 3);
    assert!(h.top_us > 0, "{h:?}");
    assert_eq!(Box::new(r.driver).stop(), StopOutcome::Released);
}

#[test]
fn join_within_waits_for_a_thread_and_leaves_one_still_running() {
    // One that ends: joined.
    let done = std::thread::spawn(|| {});
    assert!(bounded(move || join_within(done, BOUND)));
    // One that ends during the wait (released 100 ms later): waited for,
    // joined.
    let (release, held) = mpsc::channel::<()>();
    let later = std::thread::spawn(move || {
        let _ = held.recv_timeout(BOUND);
    });
    let _releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        let _ = release.send(());
    });
    assert!(bounded(move || join_within(later, BOUND)));
    // One that is still running at the end of the wait: left, never joined
    // (a join would wait for it), and it ends afterwards by itself.
    let (go, wait) = mpsc::channel::<()>();
    let running = std::thread::spawn(move || {
        let _ = wait.recv_timeout(BOUND);
    });
    assert!(!bounded(move || join_within(
        running,
        Duration::from_millis(20)
    )));
    let _ = go.send(());
    // A panicked thread ended: joined, its panic logged.
    let panicked = std::thread::spawn(|| panic!("a test panic in a stand-in pump"));
    assert!(bounded(move || join_within(panicked, BOUND)));
}
