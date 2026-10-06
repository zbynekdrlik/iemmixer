//! Tests of the spike's portable parts: the parser, the run guards, the
//! stress threads and the hwlat scanner. They run on every OS
//! (`[[example]] test = true`).

use super::*;
use std::cell::Cell;

/// Their exit codes are `spike_run::exit_code`'s (tested there).
#[test]
fn early_ends_have_their_outcomes() {
    assert_eq!(
        [End::Stopped, End::RateChanged, End::BandActivity].map(End::outcome),
        ["stopped", "rate-changed", "band-activity"]
    );
}

#[test]
fn a_placement_reports_null_its_ids_or_its_error() {
    assert_eq!(applied_json(&[14], &Applied::Nothing), Value::Null);
    assert_eq!(
        applied_json(&[6, 7], &Applied::Ids(vec![0x106, 0x107])),
        serde_json::json!({ "lps": [6, 7], "ids": [0x106, 0x107] })
    );
    assert_eq!(
        applied_json(&[14], &Applied::Failed("denied".to_owned())),
        serde_json::json!({ "lps": [14], "error": "denied" })
    );
}

#[test]
fn the_watch_stops_on_the_stop_file_a_rate_change_and_band_activity() {
    let t0 = Instant::now();
    let s = Duration::from_secs(1);
    let mut w = Watch::new(t0, Watched::All);
    assert_eq!(w.poll(t0, true, true, || vec![0.01]), Some(End::Stopped));
    assert_eq!(
        w.poll(t0, false, true, || vec![0.01]),
        Some(End::RateChanged)
    );
    // The peaks are read once a second; three loud seconds in a row are band activity.
    let reads = Cell::new(0);
    let peaks = || {
        reads.set(reads.get() + 1);
        vec![0.0, 0.01]
    };
    assert_eq!(w.poll(t0 + s / 2, false, false, peaks), None);
    assert_eq!(reads.get(), 0);
    assert_eq!(w.poll(t0 + s, false, false, peaks), None);
    assert_eq!(w.poll(t0 + s, false, false, peaks), None);
    assert_eq!(reads.get(), 1);
    assert_eq!(w.poll(t0 + 2 * s, false, false, peaks), None);
    assert_eq!(
        w.poll(t0 + 3 * s, false, false, peaks),
        Some(End::BandActivity)
    );
    assert_eq!(reads.get(), 3);
}

#[test]
fn a_quiet_second_resets_the_band_guard_and_a_late_poll_reads_once() {
    let t0 = Instant::now();
    let s = Duration::from_secs(1);
    let mut w = Watch::new(t0, Watched::All);
    assert_eq!(w.poll(t0 + s, false, false, || vec![0.5]), None);
    assert_eq!(w.poll(t0 + 2 * s, false, false, || vec![0.0]), None);
    assert_eq!(w.poll(t0 + 3 * s, false, false, || vec![0.5]), None);
    // A pause of 10 s (a reopen): one read, the next one a second later.
    let reads = Cell::new(0);
    let peaks = || {
        reads.set(reads.get() + 1);
        vec![0.5]
    };
    assert_eq!(w.poll(t0 + 13 * s, false, false, peaks), None);
    assert_eq!(w.poll(t0 + 13 * s, false, false, peaks), None);
    assert_eq!(reads.get(), 1);
    assert_eq!(
        w.poll(t0 + 14 * s, false, false, peaks),
        Some(End::BandActivity)
    );
}

#[test]
fn a_loud_input_outside_the_stage_inputs_does_not_stop_the_run_but_is_reported() {
    let t0 = Instant::now();
    let s = Duration::from_secs(1);
    // Stage inputs 2 and 3 (card numbers); input 1 carries program material.
    let mut w = Watch::new(t0, Watched::parse("2-3").unwrap());
    for k in 1..=10 {
        assert_eq!(
            w.poll(t0 + k * s, false, false, || vec![0.76, 0.001, 0.0, 0.02]),
            None,
            "second {k}"
        );
    }
    let mut report = serde_json::json!({ "tool": "asio_spike" });
    w.record_levels(&mut report);
    assert_eq!(report["loudest_input_dbfs"], dbfs(0.76));
    assert_eq!(report["loudest_watched_dbfs"], dbfs(0.001));
    assert_eq!(
        report["loudest_inputs"],
        serde_json::json!([
            { "channel": 1, "index": 0, "dbfs": dbfs(0.76) },
            { "channel": 4, "index": 3, "dbfs": dbfs(0.02) },
            { "channel": 2, "index": 1, "dbfs": dbfs(0.001) },
        ])
    );
    assert_eq!(report["activity_channels"], serde_json::json!([2, 3]));
    // The stage inputs get loud: three seconds in a row stop the run.
    for k in 11..=12 {
        assert_eq!(
            w.poll(t0 + k * s, false, false, || vec![0.0, 0.0, 0.5]),
            None
        );
    }
    assert_eq!(
        w.poll(t0 + 13 * s, false, false, || vec![0.0, 0.0, 0.5]),
        Some(End::BandActivity)
    );
}

#[test]
fn the_levels_list_the_five_loudest_inputs_and_all_when_every_input_is_watched() {
    let t0 = Instant::now();
    let mut w = Watch::new(t0, Watched::All);
    let peaks: Vec<f64> = (0..8).map(|i| f64::from(i) / 8.0).collect();
    assert_eq!(
        w.poll(t0 + Duration::from_secs(1), false, false, || peaks),
        None
    );
    let mut progress = serde_json::json!({ "elapsed_s": 5 });
    w.record_levels(&mut progress);
    let listed: Vec<u64> = progress["loudest_inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["channel"].as_u64().unwrap())
        .collect();
    assert_eq!(listed, [8, 7, 6, 5, 4]);
    assert_eq!(progress["activity_channels"], "all");
    assert_eq!(progress["loudest_watched_dbfs"], dbfs(7.0 / 8.0));
    assert_eq!(progress["elapsed_s"], 5);
}

/// A watch whose first second ends at `t0` + 1 s.
fn a_watch(t0: Instant) -> Watch {
    Watch::new(t0, Watched::All)
}

/// #38 (owner, 2026-10-06): no input level ends a run. Other devices on the
/// Dante network feed the card's inputs, and only the owner's signal decides
/// whether the PC may be used; a loud input is listed in the report only.
#[test]
fn loud_inputs_never_end_a_run() {
    let t0 = Instant::now();
    let s = Duration::from_secs(1);
    let mut w = a_watch(t0);
    for k in 1..=20 {
        assert_eq!(
            w.poll(t0 + k * s, false, false, || vec![1.0, 0.9, 0.5]),
            None,
            "second {k}"
        );
    }
    let mut report = serde_json::json!({ "tool": "asio_spike" });
    w.record_levels(&mut report);
    assert_eq!(report["loudest_input_dbfs"], dbfs(1.0));
    assert_eq!(report["loudest_inputs"][0]["channel"], 1);
    // The stop file and a rate change still end it.
    assert_eq!(
        w.poll(t0 + 21 * s, false, true, || vec![1.0]),
        Some(End::RateChanged)
    );
    assert_eq!(
        w.poll(t0 + 21 * s, true, false, || vec![1.0]),
        Some(End::Stopped)
    );
}

/// #38: a duplex or reopen run names no inputs to listen to, and the flag
/// that named them is gone.
#[test]
fn a_streaming_run_needs_no_inputs_to_listen_to() {
    for line in [
        "duplex --driver D1 --report r --stop-file s --frames 32",
        "reopen --driver D1 --report r --stop-file s --frames 48 --cycles 2",
    ] {
        let parsed = parse(&argv(line));
        assert!(parsed.is_ok(), "{line:?}: {parsed:?}");
    }
    let named = parse(&argv(
        "duplex --driver D1 --report r --stop-file s --frames 32 --activity-channels all",
    ));
    assert!(
        named
            .as_ref()
            .is_err_and(|e| e.contains("unknown flag --activity-channels")),
        "{named:?}"
    );
}

#[test]
fn measurements_are_kept_as_they_complete() {
    let mut r = serde_json::json!({ "tool": "asio_spike" });
    push(&mut r, "segments", serde_json::json!({ "seconds": 1 }));
    push(&mut r, "segments", serde_json::json!({ "seconds": 2 }));
    assert_eq!(
        r,
        serde_json::json!({ "tool": "asio_spike", "segments": [{ "seconds": 1 }, { "seconds": 2 }] })
    );
}

#[test]
fn stress_threads_stop_when_dropped() {
    let (s, applied) = Stress::start(2, &[], never_placed).unwrap();
    let flag = Arc::clone(&s.stop);
    assert_eq!((s.threads.len(), applied), (2, Applied::Nothing));
    drop(s);
    assert!(flag.load(Ordering::Relaxed));
}

/// A stand-in for `os::set_thread_cpus`: the CPU Set ID of processor n
/// is 0x100 + n.
fn placed(cpus: &[u8]) -> Result<Vec<u32>, String> {
    Ok(cpus.iter().map(|&lp| 0x100 + u32::from(lp)).collect())
}

fn refused(cpus: &[u8]) -> Result<Vec<u32>, String> {
    Err(format!("processors {cpus:?} are not in group 0"))
}

/// Without CPUs nothing is placed: a call fails its thread's start.
fn never_placed(cpus: &[u8]) -> Result<Vec<u32>, String> {
    Err(format!(
        "placed on {cpus:?} although no CPUs were asked for"
    ))
}

/// Refuses the second placement of this test only (threads place
/// themselves concurrently: any one of them is the second).
fn second_refused(cpus: &[u8]) -> Result<Vec<u32>, String> {
    static CALLS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    match CALLS.fetch_add(1, Ordering::SeqCst) {
        1 => refused(cpus),
        _ => placed(cpus),
    }
}

#[test]
fn stress_threads_are_placed_on_their_cpus_and_report_the_ids() {
    let (s, applied) = Stress::start(3, &[6, 7], placed).unwrap();
    assert_eq!(
        (s.threads.len(), applied),
        (3, Applied::Ids(vec![0x106, 0x107]))
    );
    let flag = Arc::clone(&s.stop);
    drop(s);
    assert!(flag.load(Ordering::Relaxed));
    // No threads: nothing to place, nothing applied.
    let (none, applied) = Stress::start(0, &[6], refused).unwrap();
    assert_eq!((none.threads.len(), applied), (0, Applied::Nothing));
}

/// A busy thread that could not be placed would load other processors
/// than the run reports: the start fails and every thread is ended.
#[test]
fn a_failed_stress_placement_fails_the_start() {
    let e = Stress::start(2, &[6, 7], refused).err().unwrap();
    assert!(
        e.contains("stress") && e.contains("[6, 7] are not in group 0"),
        "{e}"
    );
    let e = Stress::start(3, &[6], second_refused).err().unwrap();
    assert!(e.contains("[6] are not in group 0"), "{e}");
}

fn raised() -> Result<(), String> {
    Ok(())
}

fn not_raised() -> Result<(), String> {
    Err("access denied".to_owned())
}

fn never_raised() -> Result<(), String> {
    panic!("raised a scanner that was not placed")
}

/// hwlat measures one processor at TIME_CRITICAL: a scanner that could
/// not be placed or raised measures something else, so it never scans
/// and the run reports why (outcome "error", exit 1).
#[test]
fn hwlat_scans_only_on_its_cpu_at_time_critical() {
    let go = AtomicBool::new(false);
    let stopped = AtomicBool::new(true);
    let long = Duration::from_secs(5);
    // Placed and raised: it reads the clock until stopped (here at once)
    // or until its end.
    let (s, ids) = hwlat_scan(3, 10_000, long, &stopped, placed, raised).unwrap();
    assert_eq!((s.reads, ids), (1, vec![0x103]));
    let (s, _) = hwlat_scan(3, 10_000, Duration::ZERO, &go, placed, raised).unwrap();
    assert_eq!(s.reads, 1);
    // Not placed: not raised either, no scan.
    let e = hwlat_scan(3, 10_000, long, &go, refused, never_raised).unwrap_err();
    assert!(
        e.contains("CPU 3") && e.contains("[3] are not in group 0"),
        "{e}"
    );
    // Not raised: no scan.
    let e = hwlat_scan(3, 10_000, long, &go, placed, not_raised).unwrap_err();
    assert!(
        e.contains("TIME_CRITICAL") && e.contains("access denied"),
        "{e}"
    );
}

fn argv(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

#[test]
fn parses_a_duplex_run_under_load() {
    let a = parse(&argv(
        "duplex --driver D1 --report r.json --stop-file stop --progress p.json --frames 32 --seconds 600 --burn-us 100 --stress 4 --panic-at 7 --cycles 3 --activity-channels 101-110,121-124 --audio-cpus 14 --stress-cpus 6-13",
    ))
    .unwrap();
    assert_eq!(
        a,
        Args {
            mode: Mode::Duplex,
            driver: "D1".into(),
            report: "r.json".into(),
            progress: Some("p.json".into()),
            stop_file: "stop".into(),
            frames: 32,
            seconds: 600,
            burn_us: 100,
            stress: 4,
            panic_at: 7,
            cycles: 3,
            audio_cpus: vec![14],
            stress_cpus: vec![6, 7, 8, 9, 10, 11, 12, 13],
            cpu: None,
            threshold_us: 10,
            watched: Watched::parse("101-110,121-124").unwrap(),
        }
    );
}

#[test]
fn probe_needs_no_frames_and_has_defaults() {
    let a = parse(&argv("probe --driver D1 --report r --stop-file s")).unwrap();
    assert_eq!(
        (a.mode, a.frames, a.seconds, a.cycles, a.progress),
        (Mode::Probe, 0, 600, 5, None)
    );
    let r = parse(&argv(
        "reopen --driver D1 --report r --stop-file s --frames 48 --activity-channels all",
    ))
    .unwrap();
    assert_eq!((r.mode, r.watched), (Mode::Reopen, Watched::All));
}

/// Every bad case is otherwise valid (asio-spike.md: reject cases must not
/// be shadowed): its line with a good fill in the hole parses, the bad
/// fill is refused, and the refusal names the flag under test.
#[test]
fn bad_input_is_refused() {
    const DUPLEX: &str =
        "duplex --driver D1 --report r --stop-file s --frames 32 --activity-channels all {}";
    const REOPEN: &str =
        "reopen --driver D1 --report r --stop-file s --frames 32 --activity-channels all {}";
    const HWLAT: &str = "hwlat --report r --stop-file s --cpu 3 {}";
    const NO_FRAMES: &str =
        "duplex --driver D1 --report r --stop-file s --activity-channels all {}";
    const NO_CHANNELS: &str = "duplex --driver D1 --report r --stop-file s --frames 64 {}";
    // (the line with a hole `{}`, the bad fill, a good fill, what the refusal names)
    let cases = [
        (
            "{} --driver D1 --report r --stop-file s",
            "record",
            "probe",
            "mode",
        ),
        (
            "{}",
            "",
            "probe --driver D1 --report r --stop-file s",
            "mode",
        ),
        (
            "probe --report r --stop-file s --driver {}",
            "",
            "D1",
            "--driver",
        ),
        (
            "probe --driver D1 --report r --stop-file s {}",
            "--colour red",
            "",
            "--colour",
        ),
        (
            "probe {} --report r --stop-file s",
            "",
            "--driver D1",
            "--driver",
        ),
        (
            "probe --driver D1 {} --stop-file s",
            "",
            "--report r",
            "--report",
        ),
        (
            "probe --driver D1 --report r {}",
            "",
            "--stop-file s",
            "--stop-file",
        ),
        (NO_FRAMES, "", "--frames 64", "--frames"),
        (NO_FRAMES, "--frames 16", "--frames 48", "--frames"),
        (NO_FRAMES, "--frames 32x", "--frames 32", "--frames"),
        (
            NO_CHANNELS,
            "",
            "--activity-channels all",
            "--activity-channels",
        ),
        (
            "reopen --driver D1 --report r --stop-file s --frames 64 {}",
            "",
            "--activity-channels 101-124",
            "--activity-channels",
        ),
        (
            NO_CHANNELS,
            "--activity-channels 0",
            "--activity-channels 101",
            "--activity-channels",
        ),
        (
            NO_CHANNELS,
            "--activity-channels 5-3",
            "--activity-channels 3-5",
            "--activity-channels",
        ),
        (DUPLEX, "--seconds 0", "--seconds 1", "--seconds"),
        (DUPLEX, "--seconds 36001", "--seconds 36000", "--seconds"),
        (DUPLEX, "--burn-us 301", "--burn-us 300", "--burn-us"),
        (DUPLEX, "--stress 9", "--stress 8", "--stress"),
        (REOPEN, "--cycles 0", "--cycles 1", "--cycles"),
        (REOPEN, "--cycles 21", "--cycles 20", "--cycles"),
        (DUPLEX, "--audio-cpus 1,1", "--audio-cpus 1", "--audio-cpus"),
        (
            DUPLEX,
            "--stress-cpus 70",
            "--stress-cpus 6-13",
            "--stress-cpus",
        ),
        // A stress CPU that is also an audio CPU.
        (
            "duplex --driver D1 --report r --stop-file s --frames 32 --activity-channels all \
             --stress 4 --audio-cpus 14 {}",
            "--stress-cpus 6-14",
            "--stress-cpus 6-13",
            "--audio-cpus",
        ),
        // Busy threads without their own CPUs would run on the process
        // default, the audio CPUs (S1c design note §4.3: housekeeping).
        (
            "duplex --driver D1 --report r --stop-file s --frames 32 --activity-channels all \
             --stress 4 --audio-cpus 14 {}",
            "",
            "--stress-cpus 6-13",
            "--stress-cpus",
        ),
        ("hwlat --report r --stop-file s {}", "", "--cpu 3", "--cpu"),
        (
            "hwlat --report r --stop-file s --cpu {}",
            "64",
            "63",
            "--cpu",
        ),
        (
            HWLAT,
            "--threshold-us 0",
            "--threshold-us 1",
            "--threshold-us",
        ),
        (
            HWLAT,
            "--threshold-us 1001",
            "--threshold-us 1000",
            "--threshold-us",
        ),
    ];
    for (line, bad, good, names) in cases {
        let valid = line.replace("{}", good);
        let parsed = parse(&argv(&valid));
        assert!(parsed.is_ok(), "{valid:?} must parse: {parsed:?}");
        let refused = line.replace("{}", bad);
        match parse(&argv(&refused)) {
            Err(e) => assert!(
                e.contains(names),
                "{refused:?} was refused for {e:?}, not for {names}"
            ),
            Ok(a) => panic!("{refused:?} parsed: {a:?}"),
        }
    }
    assert!(parse(&argv("duplex --driver D1 --report r --stop-file s --frames 64 --seconds 3600 --burn-us 300 --stress 8 --activity-channels all")).is_ok());
    assert!(parse(&argv("duplex --driver D1 --report r --stop-file s --frames 32 --activity-channels all --seconds 36000")).is_ok());
    let h = parse(&argv(
        "hwlat --report r --stop-file s --cpu 14 --seconds 30",
    ))
    .unwrap();
    assert_eq!(
        (
            h.mode,
            h.cpu,
            h.threshold_us,
            h.seconds,
            h.driver.is_empty()
        ),
        (Mode::Hwlat, Some(14), 10, 30, true)
    );
}
