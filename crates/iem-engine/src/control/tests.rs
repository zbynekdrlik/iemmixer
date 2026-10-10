//! The control loop's tests: a `Control` on the test site with a fake
//! `Driver`; `peers` adds connections (Unix only), `s7` the HIL v2 times.

use std::sync::atomic::Ordering;

use iem_engine_proto::HilOut;

use super::connections::{engine_build, push_alarm};
use super::saving::unix_ms;
use super::status::meters_msg;
use super::*;

fn alarm(k: usize) -> Alarm {
    Alarm {
        code: AlarmCode::Sanitizer,
        detail: k.to_string(),
    }
}

#[test]
fn alarms_keep_the_newest_sixteen() {
    let mut list = Vec::new();
    for k in 0..MAX_ALARMS {
        push_alarm(&mut list, alarm(k));
    }
    assert_eq!(list.len(), MAX_ALARMS);
    assert_eq!(list[0].detail, "0");
    push_alarm(&mut list, alarm(16));
    assert_eq!(list.len(), MAX_ALARMS);
    assert_eq!(
        (list[0].detail.as_str(), list[15].detail.as_str()),
        ("1", "16")
    );
}

#[test]
fn meter_frames_convert_to_the_protocol() {
    let f = MeterFrame {
        seq: 3,
        inputs: vec![[0.5, 0.25]],
        mixes: vec![[1.0, 0.0], [0.125, 2.0]],
        groups: vec![[0.5, 0.0], [0.0, 0.75]],
        gr_db: vec![-3.0, 0.0],
        active: vec![96_000, 48_000],
        trips: 2,
        hil: vec![0.25],
    };
    let m = meters_msg(&f);
    assert_eq!(m.seq, 3);
    assert_eq!(m.inputs, vec![[0.5f32, 0.25]]);
    assert_eq!(m.mixes, vec![[1.0f32, 0.0], [0.125, 2.0]]);
    assert_eq!(m.groups, vec![[0.5f32, 0.0], [0.0, 0.75]]);
    assert_eq!(m.gr_db, vec![-3.0f32, 0.0]);
    assert_eq!(m.limiter_active_s, vec![1.0, 0.5]);
    assert_eq!(m.trips, 2);
}

#[test]
fn the_build_names_the_version() {
    assert!(engine_build().starts_with(concat!(env!("CARGO_PKG_VERSION"), "+")));
    assert!(unix_ms() > 1_700_000_000_000);
}

/// A backend that runs and never faults.
struct Idle;

impl Driver for Idle {
    fn stats(&self) -> StreamStats {
        StreamStats {
            running: true,
            ..StreamStats::default()
        }
    }

    fn stop(self: Box<Self>) -> StopOutcome {
        StopOutcome::Released
    }
}

struct Rig {
    c: Control,
    meters: triple_buffer::Input<MeterFrame>,
    status: Arc<RtStatus>,
    _ring: rtrb::Consumer<RtCmd>,
    dir: tempfile::TempDir,
}

/// A control loop on the test site with the processor's ends in hand.
fn rig() -> Rig {
    let flags = crate::core::Flags {
        test_signal: true,
        fault_injection: false,
    };
    rig_with(flags, false)
}

/// HIL's spare outputs of the test site (`[guard] hil_tx`).
const SPARE: [u16; 2] = [94, 95];

/// Like `run`, the engine opens HIL's spare outputs under the
/// test-signal flag only.
fn rig_with(flags: crate::core::Flags, hold: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let topo = Arc::new(crate::test_support::test_site());
    let hil = if flags.test_signal {
        SPARE.to_vec()
    } else {
        Vec::new()
    };
    let core = Core::new(
        Arc::clone(&topo),
        &iem_engine_proto::MixState::default(),
        0,
        flags,
    )
    .with_hil(hil);
    let (cmds, ring) = rtrb::RingBuffer::new(crate::rt::CMD_RING);
    let (meters_in, meters) = triple_buffer::triple_buffer(&MeterFrame::default());
    let status = Arc::new(RtStatus::default());
    let c = Control::new(Parts {
        core,
        store: Store::open(&dir.path().join("state")).unwrap(),
        cmds,
        meters,
        status: Arc::clone(&status),
        talkback_dropped: Arc::new(AtomicU64::new(0)),
        driver: Box::new(Idle),
        counters: vec![0; topo.mixes.len()],
        alarms: Vec::new(),
        settings: Settings {
            solo_grace: Duration::from_secs(10),
            block: 32,
            hold,
        },
    });
    Rig {
        c,
        meters: meters_in,
        status,
        _ring: ring,
        dir,
    }
}

/// `run` on a thread: its exit and how long it took; fails after 5 s
/// instead of hanging.
fn run_bounded(c: Control, rx: Receiver<CtlMsg>) -> (Exit, Duration) {
    let (tx, done) = std::sync::mpsc::channel();
    let _ = std::thread::spawn(move || {
        let t0 = Instant::now();
        let exit = c.run(&rx);
        let _ = tx.send((exit, t0.elapsed()));
    });
    done.recv_timeout(Duration::from_secs(5))
        .expect("run returns")
}

#[test]
fn shutdown_waits_for_the_fade_but_not_beyond_it() {
    // Already faded: saved, and the exit says so.
    let mut r = rig();
    r.status.faded_out.store(true, Ordering::Release);
    r.c.shutdown = true;
    let (_tx, rx) = std::sync::mpsc::channel();
    let (exit, _) = run_bounded(r.c, rx);
    assert_eq!(exit, Exit::Shutdown { faded: true });
    assert!(r.dir.path().join("state/current.json").exists(), "saved");
    // Never faded: the driver is released after FADE_WAIT.
    let mut r = rig();
    r.c.shutdown = true;
    let (_tx, rx) = std::sync::mpsc::channel();
    let (exit, took) = run_bounded(r.c, rx);
    assert_eq!(exit, Exit::Shutdown { faded: false });
    assert!(took >= FADE_WAIT, "{took:?}");
}

#[test]
fn the_fade_wait_ends_at_the_fade_or_after_fade_wait() {
    // Timed without the state save (its file I/O is slow on some hosts).
    let mut r = rig();
    r.status.faded_out.store(true, Ordering::Release);
    let t0 = Instant::now();
    assert!(r.c.fade_out());
    assert!(t0.elapsed() < FADE_WAIT / 2, "{:?}", t0.elapsed());
    let mut r = rig();
    let t0 = Instant::now();
    assert!(!r.c.fade_out());
    assert!(t0.elapsed() >= FADE_WAIT, "{:?}", t0.elapsed());
}

#[test]
fn status_reports_microseconds_counters_and_the_backlog() {
    let mut r = rig();
    r.c.pending.push_back(vec![RtOp::Nop; 3]);
    r.c.pending.push_back(vec![RtOp::Nop; 2]);
    r.status.trips.store(4, Ordering::Relaxed);
    let st = StreamStats {
        frames: 32,
        callbacks: 7,
        late: 1,
        missed: 0,
        overruns: 0,
        resets: 0,
        parked: false,
        faulted: false,
        running: true,
        max_process_ns: 2_500_000,
        fault: None,
        last_reopen_us: 0,
        fault_callback_ns: 0,
    };
    let s = r.c.status_msg(&st);
    assert_eq!((s.callbacks, s.late, s.faulted), (7, 1, false));
    assert_eq!(s.process_max_us, 2500.0);
    assert_eq!((s.trips, s.cmd_backlog), (4, 5));
}

#[test]
fn status_carries_the_measured_period_the_stream_counters_and_the_hold() {
    let r = rig_with(crate::core::Flags::default(), true);
    let st = StreamStats {
        frames: 32,
        callbacks: 7,
        missed: 2,
        overruns: 3,
        resets: 4,
        parked: true,
        running: true,
        ..StreamStats::default()
    };
    let s = r.c.status_msg(&st);
    assert_eq!((s.frames, s.missed, s.overruns, s.resets), (32, 2, 3, 4));
    assert!(s.parked, "parked");
    assert!(s.held, "held until Arm");
    assert!(!s.lock_failed);
    let s = rig().c.status_msg(&StreamStats {
        frames: 64,
        ..StreamStats::default()
    });
    assert_eq!((s.frames, s.missed, s.overruns, s.resets), (64, 0, 0, 0));
    assert!(!s.parked && !s.held);
}

/// HIL v1 proves the test signal from the engine (S6): each `Status`
/// carries every spare output's peak since the previous one, the
/// largest of the meter frames between them, which starts again after
/// it; an engine without spare outputs lists none.
#[test]
fn status_carries_the_hil_outputs_peaks_since_the_previous_status() {
    let mut r = rig();
    let st = StreamStats::default();
    let out = |tx: u16, peak: f32| HilOut { tx, peak };
    assert_eq!(r.c.status_msg(&st).hil, vec![out(94, 0.0), out(95, 0.0)]);
    r.c.note_hil(&[0.01, 0.0]);
    r.c.note_hil(&[0.03, 0.02]);
    r.c.note_hil(&[0.02, 0.01]);
    r.c.note_hil(&[0.5]);
    assert_eq!(r.c.next_status(&st).hil, vec![out(94, 0.5), out(95, 0.02)]);
    assert_eq!(r.c.next_status(&st).hil, vec![out(94, 0.0), out(95, 0.0)]);
    // The control loop feeds them from each meter frame it reads and
    // starts again after each Status it sends.
    let t0 = Instant::now();
    r.meters.write(MeterFrame {
        hil: vec![0.04, 0.001],
        ..MeterFrame::default()
    });
    assert!(r.c.tick(t0).is_none());
    assert_eq!(r.c.hil_peaks, [0.04, 0.001]);
    assert!(r.c.tick(t0 + Duration::from_secs(2)).is_none());
    assert_eq!(r.c.hil_peaks, [0.0, 0.0]);
    let quiet = rig_with(crate::core::Flags::default(), false);
    assert!(quiet.c.status_msg(&st).hil.is_empty());
    assert!(quiet.c.hil_peaks.is_empty());
}

/// A backend with scripted hooks: its ticks and forced reopens counted,
/// an ending, a lock failure.
struct Scripted {
    ticks: Arc<AtomicU64>,
    ending: Option<Ending>,
    lock_failed: bool,
}

impl Driver for Scripted {
    fn stats(&self) -> StreamStats {
        StreamStats {
            running: true,
            ..StreamStats::default()
        }
    }

    fn stop(self: Box<Self>) -> StopOutcome {
        StopOutcome::Released
    }

    fn tick(&mut self, _now: Instant) {
        self.ticks.fetch_add(1, Ordering::Relaxed);
    }

    fn ending(&self) -> Option<Ending> {
        self.ending.clone()
    }

    fn lock_failed(&self) -> bool {
        self.lock_failed
    }

    /// Counted with the ticks, [`REOPEN`] apiece, so one counter shows
    /// both.
    fn force_reopen(&self) -> bool {
        self.ticks.fetch_add(REOPEN, Ordering::Relaxed);
        true
    }
}

/// What one forced reopen adds to a scripted backend's counter.
const REOPEN: u64 = 1 << 32;

fn scripted(ending: Option<Ending>, lock_failed: bool) -> (Box<dyn Driver>, Arc<AtomicU64>) {
    let ticks = Arc::new(AtomicU64::new(0));
    let d = Scripted {
        ticks: Arc::clone(&ticks),
        ending,
        lock_failed,
    };
    (Box::new(d), ticks)
}

/// A running backend with stream histograms (S7).
struct Measured(HistSnapshot);

impl Driver for Measured {
    fn stats(&self) -> StreamStats {
        StreamStats {
            running: true,
            ..StreamStats::default()
        }
    }

    fn stop(self: Box<Self>) -> StopOutcome {
        StopOutcome::Released
    }

    fn histograms(&self) -> Option<HistSnapshot> {
        Some(self.0.clone())
    }
}

/// `Status` carries the stream's two histograms and their overflow
/// bucket (S7 design note §3); a backend without them, or no backend,
/// none and 0.
#[test]
fn status_carries_both_histograms_and_their_top() {
    let mut r = rig();
    let h = HistSnapshot {
        top_us: 667,
        interval: vec![(333, 2990), (667, 1)],
        process: vec![(40, 2991)],
    };
    r.c.driver = Some(Box::new(Measured(h.clone())));
    let s = r.c.status_msg(&StreamStats::default());
    assert_eq!(
        (s.hist_top_us, s.interval_hist, s.process_hist),
        (667, h.interval, h.process)
    );
    r.c.driver = None;
    let s = r.c.status_msg(&StreamStats::default());
    assert_eq!(s.hist_top_us, 0);
    assert!(s.interval_hist.is_empty() && s.process_hist.is_empty());
    let s = rig().c.status_msg(&StreamStats::default());
    assert_eq!(s.hist_top_us, 0);
    assert!(s.interval_hist.is_empty() && s.process_hist.is_empty());
    assert_eq!(Idle.histograms(), None);
}

#[test]
fn the_backend_is_ticked_and_its_lock_failure_is_reported() {
    let mut r = rig();
    let (d, ticks) = scripted(None, true);
    r.c.driver = Some(d);
    assert!(r.c.tick(Instant::now()).is_none());
    assert!(r.c.tick(Instant::now()).is_none());
    assert_eq!(ticks.load(Ordering::Relaxed), 2);
    assert!(r.c.status_msg(&StreamStats::default()).lock_failed);
    // NullRt (and any backend that does not say otherwise) locks nothing.
    assert!(!rig().c.status_msg(&StreamStats::default()).lock_failed);
    assert!(!Idle.lock_failed());
    assert_eq!(Idle.ending(), None);
}

#[test]
fn a_session_end_takes_the_shutdown_path() {
    let mut r = rig();
    r.status.faded_out.store(true, Ordering::Release);
    r.c.driver = Some(scripted(Some(Ending::Session), false).0);
    assert_eq!(
        r.c.tick(Instant::now()),
        Some(Exit::Shutdown { faded: true })
    );
    assert!(r.dir.path().join("state/current.json").exists(), "saved");
    assert!(r.c.driver.is_none(), "released");
}

#[test]
fn a_card_refused_while_running_ends_with_exit_3_and_no_fade() {
    let mut r = rig();
    let why = "the preferred buffer was not restored";
    r.c.driver = Some(scripted(Some(Ending::Card(why.into())), false).0);
    let t0 = Instant::now();
    assert_eq!(r.c.tick(Instant::now()), Some(Exit::Card(why.into())));
    assert!(t0.elapsed() < FADE_WAIT, "{:?}", t0.elapsed());
    assert!(r.dir.path().join("state/current.json").exists(), "saved");
    assert!(r.c.driver.is_none(), "released");
}

#[test]
fn only_new_sanitiser_trips_raise_an_alarm() {
    let mut r = rig();
    let now = Instant::now();
    for (trips, alarms) in [(0, 0), (2, 1), (2, 1), (3, 2)] {
        r.meters.input_buffer_mut().trips = trips;
        r.meters.publish();
        assert!(r.c.tick(now).is_none());
        assert_eq!(r.c.alarms.len(), alarms, "at {trips} trips");
    }
    assert_eq!(r.c.alarms[0].code, AlarmCode::Sanitizer);
    assert_eq!(r.c.alarms[0].detail, "sanitiser trips: 2");
    assert_eq!(r.c.alarms[1].detail, "sanitiser trips: 3");
}

#[test]
fn a_save_that_moves_a_save_tmp_aside_raises_an_alarm_naming_it() {
    // #32 MAJOR-1: a save.tmp the boot did not load (here a damaged
    // one the store never wrote) is kept aside, and the engineer hears
    // where.
    let mut r = rig();
    let state = r.dir.path().join("state");
    std::fs::write(state.join("save.tmp"), b"cut off").unwrap();
    r.c.save();
    let aside = state.join("save.tmp.orphan-1");
    assert_eq!(std::fs::read(&aside).unwrap(), b"cut off");
    assert_eq!(r.c.alarms.len(), 1, "{:?}", r.c.alarms);
    assert_eq!(r.c.alarms[0].code, AlarmCode::StateFallback);
    assert!(
        r.c.alarms[0].detail.contains(&aside.display().to_string()),
        "{:?}",
        r.c.alarms
    );
    // The next save moves nothing and raises nothing.
    r.c.save();
    assert_eq!(r.c.alarms.len(), 1, "{:?}", r.c.alarms);
}

/// Connections need a socket pair. Unix only: the harness's client
/// reads with a socket receive timeout, which Windows pipes do not have;
/// `tests/pipes.rs` runs the engine's pipes on Windows.
#[cfg(unix)]
mod peers;

/// S7 HIL v2 (#10, plan Task 28): the reopen's time and the faulting
/// callback's time in `Status`.
mod s7;
