//! S1a ASIO spike on the real card (design note
//! `docs/superpowers/specs/2026-09-27-s1a-asio-spike-design.md`). It runs
//! only on the IEM PC in dev time, started by the Interactive task from a
//! request file (`scripts/asio-spike/`). Outputs stay silent; the card's
//! rate, clock and buffer are never changed from here.
//!
//! Exit codes: 0 done or stopped, 1 other error, 2 usage, 3 driver missing,
//! 4 refused (rate, buffer, format), 5 band activity, 6 fault caught
//! (`--panic-at`), 7 the driver changed the sample rate, 8 a callback did not
//! leave the stream within the stop wait (R6: reported, the driver is left
//! alone, nothing is killed).
//!
//! The report keeps every segment, reset and reopen cycle as it completes,
//! so a run that fails half-way still reports what it measured.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use iem_audio_io::telemetry::ActivityGuard;
use serde_json::Value;

const USAGE: &str = "usage: asio_spike probe|duplex|reopen --driver <name> --report <file> --stop-file <file> \
[--progress <file>] [--frames 32|48|64] [--seconds S] [--burn-us U] [--stress T] [--panic-at K] [--cycles C]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
enum Mode {
    /// Read-only driver facts; no buffers.
    Probe,
    /// Duplex with silent outputs for `seconds`, optionally under load.
    Duplex,
    /// `cycles` × (start, 5 s, stop, release, open), timing every phase.
    Reopen,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
struct Args {
    mode: Mode,
    driver: String,
    report: PathBuf,
    progress: Option<PathBuf>,
    stop_file: PathBuf,
    frames: i32,
    seconds: u64,
    burn_us: u32,
    stress: u32,
    panic_at: u64,
    cycles: u32,
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let mut it = argv.iter();
    let mode = match it.next().map(String::as_str) {
        Some("probe") => Mode::Probe,
        Some("duplex") => Mode::Duplex,
        Some("reopen") => Mode::Reopen,
        other => return Err(format!("unknown mode {other:?}")),
    };
    let mut a = Args {
        mode,
        driver: String::new(),
        report: PathBuf::new(),
        progress: None,
        stop_file: PathBuf::new(),
        frames: 0,
        seconds: 600,
        burn_us: 0,
        stress: 0,
        panic_at: 0,
        cycles: 5,
    };
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        let num = |max: u64| match value.parse::<u64>() {
            Ok(n) if n <= max => Ok(n),
            _ => Err(format!(
                "{flag}: expected a number up to {max}, got {value:?}"
            )),
        };
        match flag.as_str() {
            "--driver" => a.driver.clone_from(value),
            "--report" => a.report = value.into(),
            "--progress" => a.progress = Some(value.into()),
            "--stop-file" => a.stop_file = value.into(),
            "--frames" => a.frames = i32::try_from(num(4096)?).unwrap_or(0),
            "--seconds" => a.seconds = num(3600)?,
            "--burn-us" => a.burn_us = u32::try_from(num(300)?).unwrap_or(0),
            "--stress" => a.stress = u32::try_from(num(8)?).unwrap_or(0),
            "--panic-at" => a.panic_at = num(u64::MAX)?,
            "--cycles" => a.cycles = u32::try_from(num(20)?).unwrap_or(0),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if a.driver.is_empty() || a.report.as_os_str().is_empty() || a.stop_file.as_os_str().is_empty()
    {
        return Err("--driver, --report and --stop-file are required".to_owned());
    }
    if a.mode != Mode::Probe && ![32, 48, 64].contains(&a.frames) {
        return Err("--frames must be 32, 48 or 64".to_owned());
    }
    if a.seconds == 0 || a.cycles == 0 {
        return Err("--seconds and --cycles must be positive".to_owned());
    }
    Ok(a)
}

#[cfg_attr(not(windows), allow(dead_code))]
const ACTIVITY_SECONDS: u32 = 3;
#[cfg_attr(not(windows), allow(dead_code))]
const ONE_SECOND: Duration = Duration::from_secs(1);

/// Why a run ends before its time (checked on every poll of duplex and reopen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
enum End {
    Stopped,
    RateChanged,
    BandActivity,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl End {
    fn outcome(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::RateChanged => "rate-changed",
            Self::BandActivity => "band-activity",
        }
    }
}

/// The exit code of a run's outcome (module header).
#[cfg_attr(not(windows), allow(dead_code))]
fn code_of(outcome: &str) -> u8 {
    match outcome {
        "band-activity" => 5,
        "fault-caught" => 6,
        "rate-changed" => 7,
        "stop-hung" => 8,
        _ => 0,
    }
}

/// The run guards duplex and reopen share (design note §3): the stop file,
/// a rate change, and once a second the input peak for band activity.
#[cfg_attr(not(windows), allow(dead_code))]
struct Watch {
    guard: ActivityGuard,
    next_second: Instant,
    loudest: f64,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Watch {
    fn new(now: Instant) -> Self {
        Self {
            guard: ActivityGuard::new(ACTIVITY_SECONDS),
            next_second: now + ONE_SECOND,
            loudest: 0.0,
        }
    }

    /// The loudest one-second input peak seen (linear).
    fn loudest(&self) -> f64 {
        self.loudest
    }

    /// One poll. `peak` empties the stream's peak, so it is read at most
    /// once a second; after a pause (a reopen) the next read is a second
    /// later, never a burst of catch-up reads.
    fn poll(
        &mut self,
        now: Instant,
        stop_file: bool,
        rate_changed: bool,
        peak: impl FnOnce() -> f64,
    ) -> Option<End> {
        if stop_file {
            return Some(End::Stopped);
        }
        if rate_changed {
            return Some(End::RateChanged);
        }
        if now < self.next_second {
            return None;
        }
        self.next_second = now + ONE_SECOND;
        let p = peak();
        self.loudest = self.loudest.max(p);
        self.guard.observe(p).then_some(End::BandActivity)
    }
}

/// Appends `item` to the report's list `key`: measurements are kept as they
/// complete.
#[cfg_attr(not(windows), allow(dead_code))]
fn push(report: &mut Value, key: &str, item: Value) {
    if let Some(obj) = report.as_object_mut()
        && let Some(list) = obj
            .entry(key)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
    {
        list.push(item);
    }
}

/// Busy threads at normal priority standing in for the server and the
/// stream. They stop and are joined on drop, so every path ends them.
#[cfg_attr(not(windows), allow(dead_code))]
struct Stress {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Stress {
    fn start(n: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let threads = (0..n)
            .map(|_| {
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        std::hint::spin_loop();
                    }
                })
            })
            .collect();
        Self { stop, threads }
    }
}

impl Drop for Stress {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match parse(&argv) {
        Ok(args) => platform(&args),
        Err(e) => {
            eprintln!("asio_spike: {e}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(not(windows))]
fn platform(_: &Args) -> ExitCode {
    eprintln!("asio_spike: Windows only (the card is on the IEM PC)");
    ExitCode::from(2)
}

#[cfg(windows)]
fn platform(args: &Args) -> ExitCode {
    spike::main(args)
}

#[cfg(windows)]
mod spike {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use iem_audio_io::asio::{
        self, AsioError, DriverInfo, Host, Running, StopTimings, StreamConfig,
    };
    use iem_audio_io::format::SampleFormat;
    use iem_audio_io::telemetry::{Snapshot, dbfs};
    use serde_json::{Value, json};

    use super::{Args, ExitCode, Mode, Stress, Watch, code_of, push};

    const FIRST_CALLBACK_WAIT: Duration = Duration::from_secs(2);
    const AFTER_FAULT: Duration = Duration::from_secs(2);
    const REOPEN_RUN: Duration = Duration::from_secs(5);

    pub fn main(a: &Args) -> ExitCode {
        let mut report = json!({
            "tool": "asio_spike",
            "version": env!("CARGO_PKG_VERSION"),
            "build_sha": option_env!("GITHUB_SHA"),
            "mode": format!("{:?}", a.mode).to_lowercase(),
            "frames": a.frames, "seconds": a.seconds, "burn_us": a.burn_us,
            "stress": a.stress, "panic_at": a.panic_at, "cycles": a.cycles,
        });
        let code = match run(a, &mut report) {
            Ok(code) => code,
            Err(e) => {
                let (outcome, code) = match e {
                    AsioError::NoDrivers(_) | AsioError::NotFound { .. } => ("no-driver", 3),
                    AsioError::Refused(_) => ("refused", 4),
                    _ => ("error", 1),
                };
                report["outcome"] = json!(outcome);
                report["error"] = json!(e.to_string());
                code
            }
        };
        match write_json(&a.report, &report) {
            Ok(()) => ExitCode::from(code),
            Err(e) => {
                eprintln!("asio_spike: writing {}: {e}", a.report.display());
                ExitCode::from(1)
            }
        }
    }

    fn run(a: &Args, report: &mut Value) -> Result<u8, AsioError> {
        // Pre-empted before the start: the card is never opened.
        if a.stop_file.exists() {
            report["outcome"] = json!("stopped");
            return Ok(0);
        }
        let host = Host::open(&a.driver)?;
        let info = host.info()?;
        report["driver"] = info_json(&info);
        match a.mode {
            Mode::Probe => {
                report["outcome"] = json!("done");
                Ok(0)
            }
            Mode::Duplex => duplex(a, host, info, report),
            Mode::Reopen => reopen(a, host, info, report),
        }
    }

    /// Releases the driver and creates it again on this thread, recording
    /// both times (or the failed open) under `entry`.
    fn recreate(a: &Args, host: Host, entry: &mut Value) -> Result<(Host, DriverInfo), AsioError> {
        let t = Instant::now();
        drop(host);
        entry["release_us"] = json!(us(t.elapsed()));
        let t = Instant::now();
        let opened = Host::open(&a.driver).and_then(|h| h.info().map(|i| (h, i)));
        match &opened {
            Ok(_) => entry["open_us"] = json!(us(t.elapsed())),
            Err(e) => entry["open_error"] = json!(e.to_string()),
        }
        opened
    }

    fn duplex(
        a: &Args,
        mut host: Host,
        mut info: DriverInfo,
        report: &mut Value,
    ) -> Result<u8, AsioError> {
        let cfg = StreamConfig {
            frames: a.frames,
            burn_us: a.burn_us,
            panic_at: a.panic_at,
        };
        let _stress = Stress::start(a.stress);
        let deadline = Instant::now() + Duration::from_secs(a.seconds);
        let mut watch = Watch::new(Instant::now());
        let mut fault: Option<(Instant, u64)> = None;
        let mut outcome = "done";
        report["segments"] = json!([]);
        report["resets_handled"] = json!([]);
        loop {
            let running = host.start(&info, cfg)?;
            let first = wait_first_callback(&running)?;
            let latency = host.latencies()?;
            let t0 = Instant::now();
            let mut next_progress = t0 + Duration::from_secs(5);
            let reopen = loop {
                asio::pump_messages();
                std::thread::sleep(Duration::from_millis(10));
                let now = Instant::now();
                if let Some(end) =
                    watch.poll(now, a.stop_file.exists(), running.rate_changed(), || {
                        running.take_input_peak()
                    })
                {
                    outcome = end.outcome();
                    break false;
                }
                if now >= deadline {
                    break false;
                }
                if running.take_reopen() {
                    break true;
                }
                if running.faulted() {
                    match fault {
                        None => fault = Some((now, running.callbacks())),
                        Some((at, _)) if now.duration_since(at) >= AFTER_FAULT => {
                            outcome = "fault-caught";
                            break false;
                        }
                        Some(_) => {}
                    }
                }
                if now >= next_progress {
                    next_progress += Duration::from_secs(5);
                    if let (Some(path), Some(s)) = (&a.progress, running.snapshot()) {
                        let _ = write_json(path, &progress_json(t0.elapsed(), &s, watch.loudest()));
                    }
                }
            };
            let seconds = t0.elapsed().as_secs_f64();
            let (snap, stop) = running.finish();
            if let (Some((_, at)), Some(s)) = (fault, &snap) {
                report["callbacks_after_fault"] = json!(s.callbacks.saturating_sub(at));
            }
            push(
                report,
                "segments",
                json!({
                    "latency_in": latency.0, "latency_out": latency.1,
                    "create_buffers_us": us(first.0.create_buffers), "start_us": us(first.0.start),
                    "first_callback_us": us(first.1), "seconds": seconds,
                    "telemetry": snap.as_ref().map(telemetry_json), "stop": stop_json(stop),
                }),
            );
            report["loudest_input_dbfs"] = json!(dbfs(watch.loudest()));
            if stop.hung {
                // R6: a callback is still inside the driver; never call it again.
                outcome = "stop-hung";
                std::mem::forget(host);
                break;
            }
            if !reopen {
                break;
            }
            // The driver asked for a reset: release it and create it again on this thread.
            let mut entry = json!({});
            let recreated = recreate(a, host, &mut entry);
            push(report, "resets_handled", entry);
            (host, info) = recreated?;
        }
        report["outcome"] = json!(outcome);
        Ok(code_of(outcome))
    }

    fn reopen(
        a: &Args,
        mut host: Host,
        mut info: DriverInfo,
        report: &mut Value,
    ) -> Result<u8, AsioError> {
        let cfg = StreamConfig {
            frames: a.frames,
            burn_us: 0,
            panic_at: 0,
        };
        let mut watch = Watch::new(Instant::now());
        let mut outcome = "done";
        report["cycles"] = json!([]);
        for cycle in 0..a.cycles {
            let running = host.start(&info, cfg)?;
            let (timings, first) = wait_first_callback(&running)?;
            let t = Instant::now();
            let mut reset_requested = false;
            let mut end = None;
            while t.elapsed() < REOPEN_RUN {
                asio::pump_messages();
                std::thread::sleep(Duration::from_millis(10));
                // A reset request needs no action here: every cycle recreates the driver.
                reset_requested |= running.take_reopen();
                end = watch.poll(
                    Instant::now(),
                    a.stop_file.exists(),
                    running.rate_changed(),
                    || running.take_input_peak(),
                );
                if end.is_some() {
                    break;
                }
            }
            let (snap, stop) = running.finish();
            let mut entry = json!({
                "cycle": cycle,
                "create_buffers_us": us(timings.create_buffers), "start_us": us(timings.start),
                "first_callback_us": us(first), "stop": stop_json(stop),
                "reset_requested": reset_requested,
                "callbacks": snap.as_ref().map_or(0, |s| s.callbacks),
                "missed": snap.as_ref().map_or(0, |s| s.missed),
            });
            report["loudest_input_dbfs"] = json!(dbfs(watch.loudest()));
            if stop.hung {
                push(report, "cycles", entry);
                outcome = "stop-hung";
                std::mem::forget(host);
                break;
            }
            if let Some(end) = end {
                push(report, "cycles", entry);
                outcome = end.outcome();
                break;
            }
            let recreated = recreate(a, host, &mut entry);
            push(report, "cycles", entry);
            (host, info) = recreated?;
        }
        report["outcome"] = json!(outcome);
        Ok(code_of(outcome))
    }

    /// Pumps messages until the first callback; the start timings and the wait.
    fn wait_first_callback(
        running: &Running<'_>,
    ) -> Result<(asio::StartTimings, Duration), AsioError> {
        let t = Instant::now();
        while t.elapsed() < FIRST_CALLBACK_WAIT {
            asio::pump_messages();
            if running.callbacks() > 0 {
                return Ok((running.timings, t.elapsed()));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Err(AsioError::Call(
            "start",
            format!("no callback within {FIRST_CALLBACK_WAIT:?}"),
        ))
    }

    fn us(d: Duration) -> f64 {
        d.as_secs_f64() * 1e6
    }

    fn info_json(i: &DriverInfo) -> Value {
        let mut types: BTreeMap<String, usize> = BTreeMap::new();
        for &t in &i.sample_types {
            let name = SampleFormat::from_asio(t)
                .map_or_else(|| format!("unsupported:{t}"), |f| f.name().to_owned());
            *types.entry(name).or_default() += 1;
        }
        json!({
            "name": i.name, "version": i.version, "inputs": i.inputs, "outputs": i.outputs,
            "buffer": { "min": i.buffer_min, "max": i.buffer_max, "preferred": i.buffer_preferred, "granularity": i.buffer_granularity },
            "rate": i.rate, "can_96k": i.can_96k,
            "latency_at_preferred": { "in": i.latency_in, "out": i.latency_out },
            "clocks": i.clocks.iter().map(|c| json!({ "index": c.index, "name": c.name, "current": c.current })).collect::<Vec<_>>(),
            "sample_types": types,
        })
    }

    fn telemetry_json(s: &Snapshot) -> Value {
        let q = |v: [f64; 4]| json!({ "p50": v[0], "p99": v[1], "p999": v[2], "max": v[3] });
        json!({
            "period_us": s.period_ns as f64 / 1e3,
            "callbacks": s.callbacks, "late": s.late, "missed": s.missed,
            "overruns": s.overruns, "position_gaps": s.position_gaps,
            "first_callback_us": s.first_callback_ns as f64 / 1e3,
            "messages": {
                "resets": s.resets, "resyncs": s.resyncs, "latency_changes": s.latency_changes,
                "buffer_size_changes": s.buffer_size_changes, "overloads": s.overloads, "rate_changes": s.rate_changes,
            },
            "interval_us": q(s.interval.summary_us()),
            "duration_us": q(s.duration.summary_us()),
            "drift_ppm": s.drift_ppm,
        })
    }

    fn progress_json(elapsed: Duration, s: &Snapshot, loudest: f64) -> Value {
        json!({
            "elapsed_s": elapsed.as_secs(), "callbacks": s.callbacks, "late": s.late, "missed": s.missed,
            "overruns": s.overruns, "position_gaps": s.position_gaps, "resets": s.resets,
            "loudest_input_dbfs": dbfs(loudest),
        })
    }

    fn stop_json(t: StopTimings) -> Value {
        json!({ "stop_us": us(t.stop), "dispose_us": us(t.dispose), "stop_ok": t.stop_ok, "dispose_ok": t.dispose_ok, "hung": t.hung })
    }

    /// Writes `value` next to `path` and renames it into place.
    fn write_json(path: &Path, value: &Value) -> std::io::Result<()> {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn outcomes_have_their_exit_codes() {
        assert_eq!(
            [
                "done",
                "stopped",
                "band-activity",
                "fault-caught",
                "rate-changed",
                "stop-hung"
            ]
            .map(code_of),
            [0, 0, 5, 6, 7, 8]
        );
        assert_eq!(
            [End::Stopped, End::RateChanged, End::BandActivity].map(End::outcome),
            ["stopped", "rate-changed", "band-activity"]
        );
    }

    #[test]
    fn the_watch_stops_on_the_stop_file_a_rate_change_and_band_activity() {
        let t0 = Instant::now();
        let s = Duration::from_secs(1);
        let mut w = Watch::new(t0);
        assert_eq!(w.poll(t0, true, true, || 0.01), Some(End::Stopped));
        assert_eq!(w.poll(t0, false, true, || 0.01), Some(End::RateChanged));
        // The peak is read once a second; three loud seconds in a row are band activity.
        let reads = Cell::new(0);
        let peak = || {
            reads.set(reads.get() + 1);
            0.01
        };
        assert_eq!(w.poll(t0 + s / 2, false, false, peak), None);
        assert_eq!(reads.get(), 0);
        assert_eq!(w.poll(t0 + s, false, false, peak), None);
        assert_eq!(w.poll(t0 + s, false, false, peak), None);
        assert_eq!(reads.get(), 1);
        assert_eq!(w.poll(t0 + 2 * s, false, false, peak), None);
        assert_eq!(
            w.poll(t0 + 3 * s, false, false, peak),
            Some(End::BandActivity)
        );
        assert_eq!((reads.get(), w.loudest()), (3, 0.01));
    }

    #[test]
    fn a_quiet_second_resets_the_band_guard_and_a_late_poll_reads_once() {
        let t0 = Instant::now();
        let s = Duration::from_secs(1);
        let mut w = Watch::new(t0);
        assert_eq!(w.poll(t0 + s, false, false, || 0.5), None);
        assert_eq!(w.poll(t0 + 2 * s, false, false, || 0.0), None);
        assert_eq!(w.poll(t0 + 3 * s, false, false, || 0.5), None);
        // A pause of 10 s (a reopen): one read, the next one a second later.
        let reads = Cell::new(0);
        let peak = || {
            reads.set(reads.get() + 1);
            0.5
        };
        assert_eq!(w.poll(t0 + 13 * s, false, false, peak), None);
        assert_eq!(w.poll(t0 + 13 * s, false, false, peak), None);
        assert_eq!(reads.get(), 1);
        assert_eq!(
            w.poll(t0 + 14 * s, false, false, peak),
            Some(End::BandActivity)
        );
        assert_eq!(w.loudest(), 0.5);
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
        let s = Stress::start(2);
        let flag = Arc::clone(&s.stop);
        assert_eq!(s.threads.len(), 2);
        drop(s);
        assert!(flag.load(Ordering::Relaxed));
    }

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn parses_a_duplex_run_under_load() {
        let a = parse(&argv(
            "duplex --driver D1 --report r.json --stop-file stop --progress p.json --frames 32 --seconds 600 --burn-us 100 --stress 4 --panic-at 7 --cycles 3",
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
        assert_eq!(
            parse(&argv(
                "reopen --driver D1 --report r --stop-file s --frames 48"
            ))
            .unwrap()
            .mode,
            Mode::Reopen
        );
    }

    #[test]
    fn bad_input_is_refused() {
        for bad in [
            "",
            "record --driver D1 --report r --stop-file s",
            "probe --driver",
            "probe --driver D1 --report r --stop-file s --colour red",
            "probe --report r --stop-file s",
            "probe --driver D1 --stop-file s",
            "probe --driver D1 --report r",
            "duplex --driver D1 --report r --stop-file s",
            "duplex --driver D1 --report r --stop-file s --frames 16",
            "duplex --driver D1 --report r --stop-file s --frames 32x",
            "duplex --driver D1 --report r --stop-file s --frames 32 --seconds 0",
            "duplex --driver D1 --report r --stop-file s --frames 32 --seconds 3601",
            "duplex --driver D1 --report r --stop-file s --frames 32 --burn-us 301",
            "duplex --driver D1 --report r --stop-file s --frames 32 --stress 9",
            "reopen --driver D1 --report r --stop-file s --frames 32 --cycles 0",
            "reopen --driver D1 --report r --stop-file s --frames 32 --cycles 21",
        ] {
            assert!(parse(&argv(bad)).is_err(), "{bad:?}");
        }
        assert!(parse(&argv("duplex --driver D1 --report r --stop-file s --frames 64 --seconds 3600 --burn-us 300 --stress 8")).is_ok());
    }
}
