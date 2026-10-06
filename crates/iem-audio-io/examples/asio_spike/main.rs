//! S1a ASIO spike on the real card (design note
//! `docs/superpowers/specs/2026-09-27-s1a-asio-spike-design.md`). It runs
//! only on the IEM PC in dev time, started by the Interactive task from a
//! request file (`scripts/asio-spike/`). Outputs stay silent; the card's
//! rate, clock and buffer are never changed from here.
//!
//! The band guard listens only to the inputs given by `--activity-channels`
//! (the site's stage inputs, card numbers from 1; `all` is the explicit
//! fallback); the report and the progress file list the five loudest inputs.
//!
//! Exit codes: 0 done or stopped, 1 other error, 2 usage, 3 driver missing,
//! 4 refused (rate, buffer, format, activity channels), 5 band activity, 6 fault caught
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
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use iem_audio_io::cpuset;
use iem_audio_io::spike_run::{self, Applied};
use iem_audio_io::telemetry::{ActivityGuard, GapScan, GapSummary, Loudest, Watched, dbfs};
use serde_json::{Value, json};

const USAGE: &str =
    "usage: asio_spike probe|duplex|reopen|hwlat --report <file> --stop-file <file> \
[--driver <name>] [--progress <file>] [--frames 32|48|64] \
[--activity-channels all|<list, e.g. 101-110,121-124>] [--seconds S] [--burn-us U] [--stress T] \
[--panic-at K] [--cycles C] [--audio-cpus LIST] [--stress-cpus LIST] [--cpu N] [--threshold-us U]
(duplex and reopen need --frames and --activity-channels; hwlat needs --cpu and --threshold-us; \
--stress with --audio-cpus needs --stress-cpus, which never overlaps --audio-cpus)";

/// The longest run: an 8 h soak with margin (S1c design note §8 W4).
const MAX_SECONDS: u64 = 36_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
enum Mode {
    /// Read-only driver facts; no buffers.
    Probe,
    /// Duplex with silent outputs for `seconds`, optionally under load.
    Duplex,
    /// `cycles` × (start, 5 s, stop, release, open), timing every phase.
    Reopen,
    /// One TIME_CRITICAL thread on `--cpu` reads the clock in a loop and
    /// records its gaps (S1c design note §4.1); the card is never opened.
    Hwlat,
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
    audio_cpus: Vec<u8>,
    stress_cpus: Vec<u8>,
    cpu: Option<u8>,
    threshold_us: u64,
    /// The inputs the band guard listens to.
    watched: Watched,
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let mut it = argv.iter();
    let mode = match it.next().map(String::as_str) {
        Some("probe") => Mode::Probe,
        Some("duplex") => Mode::Duplex,
        Some("reopen") => Mode::Reopen,
        Some("hwlat") => Mode::Hwlat,
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
        audio_cpus: Vec::new(),
        stress_cpus: Vec::new(),
        cpu: None,
        threshold_us: 10,
        watched: Watched::All,
    };
    let mut watched = None;
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        let num = |max: u64| match value.parse::<u64>() {
            Ok(n) if n <= max => Ok(n),
            _ => Err(format!(
                "{flag}: expected a number up to {max}, got {value:?}"
            )),
        };
        // A list parser's refusal, prefixed with the flag it came from.
        let named = |e: String| format!("{flag}: {e}");
        match flag.as_str() {
            "--driver" => a.driver.clone_from(value),
            "--report" => a.report = value.into(),
            "--progress" => a.progress = Some(value.into()),
            "--stop-file" => a.stop_file = value.into(),
            "--frames" => a.frames = i32::try_from(num(4096)?).unwrap_or(0),
            "--seconds" => a.seconds = num(MAX_SECONDS)?,
            "--burn-us" => a.burn_us = u32::try_from(num(300)?).unwrap_or(0),
            "--stress" => a.stress = u32::try_from(num(8)?).unwrap_or(0),
            "--panic-at" => a.panic_at = num(u64::MAX)?,
            "--cycles" => a.cycles = u32::try_from(num(20)?).unwrap_or(0),
            "--activity-channels" => watched = Some(Watched::parse(value).map_err(named)?),
            "--audio-cpus" => a.audio_cpus = cpuset::parse_lps(value).map_err(named)?,
            "--stress-cpus" => a.stress_cpus = cpuset::parse_lps(value).map_err(named)?,
            "--cpu" => a.cpu = Some(u8::try_from(num(63)?).unwrap_or(0)),
            "--threshold-us" => a.threshold_us = num(1000)?,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if a.report.as_os_str().is_empty() || a.stop_file.as_os_str().is_empty() {
        return Err("--report and --stop-file are required".to_owned());
    }
    if a.mode != Mode::Hwlat && a.driver.is_empty() {
        return Err("--driver is required (every mode but hwlat)".to_owned());
    }
    if matches!(a.mode, Mode::Duplex | Mode::Reopen) && ![32, 48, 64].contains(&a.frames) {
        return Err("--frames must be 32, 48 or 64".to_owned());
    }
    if a.mode == Mode::Hwlat && (a.cpu.is_none() || a.threshold_us == 0) {
        return Err("hwlat needs --cpu 0..63 and --threshold-us 1..1000".to_owned());
    }
    if a.seconds == 0 || a.cycles == 0 {
        return Err("--seconds and --cycles must be positive".to_owned());
    }
    spike_run::check_stress_cpus(a.stress, &a.audio_cpus, &a.stress_cpus)?;
    match watched {
        Some(w) => a.watched = w,
        // All inputs only when asked for: a site's program inputs may carry
        // signal while the band is silent (hwlat opens no card).
        None if matches!(a.mode, Mode::Duplex | Mode::Reopen) => {
            return Err("--activity-channels is required (the stage inputs, or all)".to_owned());
        }
        None => {}
    }
    Ok(a)
}

#[cfg_attr(not(windows), allow(dead_code))]
const ACTIVITY_SECONDS: u32 = 3;
#[cfg_attr(not(windows), allow(dead_code))]
const ONE_SECOND: Duration = Duration::from_secs(1);
/// How many of the loudest inputs the report lists.
#[cfg_attr(not(windows), allow(dead_code))]
const HOT_INPUTS: usize = 5;

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

/// A placement's report value (`spike_run::Applied`): null when nothing was
/// placed, the processors with the CPU Set IDs applied or with the error.
#[cfg_attr(not(windows), allow(dead_code))]
fn applied_json(lps: &[u8], applied: &Applied) -> Value {
    match applied {
        Applied::Nothing => Value::Null,
        Applied::Ids(ids) => json!({ "lps": lps, "ids": ids }),
        Applied::Failed(e) => json!({ "lps": lps, "error": e }),
    }
}

/// The run guards duplex and reopen share (design note §3): the stop file,
/// a rate change, and once a second the watched inputs' peak for band
/// activity. Every input's loudest second is kept for the report.
#[cfg_attr(not(windows), allow(dead_code))]
struct Watch {
    guard: ActivityGuard,
    watched: Watched,
    next_second: Instant,
    loudest: Loudest,
    loudest_watched: f64,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Watch {
    fn new(now: Instant, watched: Watched) -> Self {
        Self {
            guard: ActivityGuard::new(ACTIVITY_SECONDS),
            watched,
            next_second: now + ONE_SECOND,
            loudest: Loudest::default(),
            loudest_watched: 0.0,
        }
    }

    /// One poll. `peaks` empties the stream's per-input peaks, so it is
    /// read at most once a second; after a pause (a reopen) the next read is
    /// a second later, never a burst of catch-up reads.
    fn poll(
        &mut self,
        now: Instant,
        stop_file: bool,
        rate_changed: bool,
        peaks: impl FnOnce() -> Vec<f64>,
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
        let p = peaks();
        self.loudest.observe(&p);
        let watched = self.watched.peak(&p);
        self.loudest_watched = self.loudest_watched.max(watched);
        self.guard.observe(watched).then_some(End::BandActivity)
    }

    /// The input levels so far into the report or the progress file: the
    /// watched inputs, the loudest of all and of the watched ones, and the
    /// five loudest inputs (card number from 1, index from 0, dBFS).
    fn record_levels(&self, out: &mut Value) {
        out["activity_channels"] = self
            .watched
            .numbers()
            .map_or_else(|| json!("all"), |n| json!(n));
        out["loudest_input_dbfs"] = json!(dbfs(self.loudest.max()));
        out["loudest_watched_dbfs"] = json!(dbfs(self.loudest_watched));
        out["loudest_inputs"] = self
            .loudest
            .top(HOT_INPUTS)
            .into_iter()
            .map(
                |(index, peak)| json!({ "channel": index + 1, "index": index, "dbfs": dbfs(peak) }),
            )
            .collect();
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

/// Places the calling thread on the given processors: the CPU Set IDs
/// applied, or why not (`os::set_thread_cpus` on the PC).
#[cfg_attr(not(windows), allow(dead_code))]
type Place = fn(&[u8]) -> Result<Vec<u32>, String>;

/// Busy threads at normal priority standing in for the server and the
/// stream (S1c design note §4.3: on the housekeeping CPUs). They stop and
/// are joined on drop, so every path ends them.
#[cfg_attr(not(windows), allow(dead_code))]
struct Stress {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Stress {
    /// Starts `n` busy threads; each first places itself on `cpus` through
    /// `place` (`spike_run::place_on`: without `cpus` they run on the process
    /// default) and sends the result back. Returns what was applied
    /// (`spike_run::stress_placement`), or why a thread could not be placed:
    /// the run fails then, and every thread is stopped and joined.
    fn start(n: u32, cpus: &[u8], place: Place) -> Result<(Self, Applied), String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let threads = (0..n)
            .map(|_| {
                let stop = Arc::clone(&stop);
                let cpus = cpus.to_vec();
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let placed = spike_run::place_on(&cpus, place);
                    let ok = placed.is_ok();
                    // `start` receives every thread's result before it drops
                    // the receiver, so this send cannot fail.
                    let _ = tx.send(placed);
                    drop(tx);
                    while spike_run::keeps_busy(ok, stop.load(Ordering::Relaxed)) {
                        std::hint::spin_loop();
                    }
                })
            })
            .collect();
        drop(tx);
        // From here every return ends the threads (drop).
        let stress = Self { stop, threads };
        match spike_run::stress_placement(cpus, (0..n).map(|_| rx.recv().ok())) {
            Applied::Failed(e) => Err(e),
            applied => Ok((stress, applied)),
        }
    }
}

/// Raises the calling thread to TIME_CRITICAL, or says why not
/// (`os::set_thread_time_critical` on the PC).
#[cfg_attr(not(windows), allow(dead_code))]
type Raise = fn() -> Result<(), String>;

/// The hwlat scanner's thread (S1c design note §4.1): it scans only once
/// `spike_run::hwlat_ready` placed it on `cpu` and raised it to
/// TIME_CRITICAL, else it returns why. Then it reads the clock in a tight
/// loop until `spike_run::scan_ends` (`end` or `stop`): every gap of at
/// least `threshold_ns` is a stall of that processor. Returns the gaps and
/// the CPU Set IDs applied.
#[cfg_attr(not(windows), allow(dead_code))]
fn hwlat_scan(
    cpu: u8,
    threshold_ns: u64,
    end: Duration,
    stop: &AtomicBool,
    place: Place,
    raise: Raise,
) -> Result<(GapSummary, Vec<u32>), String> {
    let ids = spike_run::hwlat_ready(cpu, place, raise)?;
    let mut scan = GapScan::new(threshold_ns);
    let t0 = Instant::now();
    let mut prev = 0_u64;
    loop {
        let now = t0.elapsed();
        let ns = u64::try_from(now.as_nanos()).unwrap_or(u64::MAX);
        scan.observe(prev, ns);
        prev = ns;
        if spike_run::scan_ends(now, end, stop.load(Ordering::Relaxed)) {
            break;
        }
    }
    Ok((scan.summary(), ids))
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use iem_audio_io::asio::{
        self, AsioError, DriverInfo, Host, Running, StopTimings, StreamConfig,
    };
    use iem_audio_io::format::SampleFormat;
    use iem_audio_io::glitch_report::{keep_glitches, write_markers};
    use iem_audio_io::os;
    use iem_audio_io::spike_run::{self, Applied, exit_code};
    use iem_audio_io::telemetry::{Glitch, Snapshot};
    use serde_json::{Value, json};

    use super::{Args, ExitCode, Mode, Stress, Watch, applied_json, hwlat_scan, push};

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
            "audio_cpus": a.audio_cpus, "stress_cpus": a.stress_cpus,
            "cpu": a.cpu, "threshold_us": a.threshold_us,
        });
        let (process, failed) = process_setup(a);
        report["process"] = process;
        let code = if let Some((outcome, why)) = failed {
            report["outcome"] = json!(outcome);
            report["error"] = json!(why);
            exit_code(outcome)
        } else {
            match run(a, &mut report) {
                Ok(code) => code,
                Err(e) => {
                    let outcome = match e {
                        AsioError::NoDrivers(_) | AsioError::NotFound { .. } => "no-driver",
                        AsioError::Refused(_) => "refused",
                        _ => "error",
                    };
                    report["outcome"] = json!(outcome);
                    report["error"] = json!(e.to_string());
                    exit_code(outcome)
                }
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
        if a.mode == Mode::Hwlat {
            return Ok(hwlat(a, report));
        }
        // Pre-empted before the start: the card is never opened.
        if a.stop_file.exists() {
            report["outcome"] = json!("stopped");
            return Ok(exit_code("stopped"));
        }
        let host = Host::open(&a.driver)?;
        let info = host.info()?;
        report["driver"] = info_json(&info);
        if let Err(e) = a.watched.check(usize::try_from(info.inputs).unwrap_or(0)) {
            report["outcome"] = json!("refused");
            report["error"] = json!(e);
            return Ok(exit_code("refused"));
        }
        match a.mode {
            Mode::Probe => {
                report["outcome"] = json!("done");
                Ok(exit_code("done"))
            }
            Mode::Duplex => duplex(a, host, info, report),
            Mode::Reopen => reopen(a, host, info, report),
            Mode::Hwlat => Ok(hwlat(a, report)),
        }
    }

    /// In-process levers (S1c design note §6.2 L5): power throttling off and
    /// the audio CPU Set as the process default (every thread without its own
    /// selection, the driver's included, runs there; no priority changes).
    /// A lever that could not be applied fails the run
    /// (`spike_run::setup_failure`: a failed audio CPU Set refuses it,
    /// throttling left on is an error), since the measurement would not be
    /// of the process the report names: the outcome and why.
    fn process_setup(a: &Args) -> (Value, Option<(&'static str, String)>) {
        let throttling = os::disable_power_throttling().map_err(|e| e.to_string());
        let topology = match os::system_cpu_sets() {
            Ok(sets) => json!(sets
                .iter()
                .map(|c| json!({ "id": c.id, "group": c.group, "lp": c.lp, "core": c.core, "realtime": c.realtime }))
                .collect::<Vec<_>>()),
            Err(e) => json!({ "error": e.to_string() }),
        };
        let audio = spike_run::applied(
            &a.audio_cpus,
            spike_run::place_on(&a.audio_cpus, |lps| {
                os::set_process_cpus(lps).map_err(|e| e.to_string())
            }),
        );
        let failed = spike_run::setup_failure(&throttling, &audio);
        let throttling = throttling.map_or_else(|e| json!(e), |()| json!("off"));
        // The stress threads' CPU Set: what duplex applies (null until then).
        (
            json!({ "power_throttling": throttling, "audio_cpus": applied_json(&a.audio_cpus, &audio), "stress_cpus": null, "topology": topology }),
            failed,
        )
    }

    /// Puts the calling thread on `cpus`: the CPU Set IDs applied.
    fn place_thread(cpus: &[u8]) -> Result<Vec<u32>, String> {
        os::set_thread_cpus(cpus).map_err(|e| e.to_string())
    }

    /// Makes the calling thread TIME_CRITICAL (the hwlat scanner only).
    fn raise_thread() -> Result<(), String> {
        os::set_thread_time_critical().map_err(|e| e.to_string())
    }

    /// hwlat (S1c design note §4.1): one thread at TIME_CRITICAL on `--cpu`
    /// reads the clock in a tight loop (`hwlat_scan`); every gap of at least
    /// the threshold is a stall of that processor. A scanner that cannot be
    /// placed or raised never scans: outcome "error", exit 1. The card is
    /// never opened.
    fn hwlat(a: &Args, report: &mut Value) -> u8 {
        if a.stop_file.exists() {
            report["outcome"] = json!("stopped");
            return exit_code("stopped");
        }
        let Some(cpu) = a.cpu else {
            // The parser requires --cpu for hwlat.
            report["error"] = json!("hwlat without --cpu");
            report["outcome"] = json!("error");
            return exit_code("error");
        };
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let threshold_ns = a.threshold_us.saturating_mul(1_000);
        let end = Duration::from_secs(a.seconds);
        let scanner = std::thread::spawn(move || {
            hwlat_scan(cpu, threshold_ns, end, &flag, place_thread, raise_thread)
        });
        let mut outcome = "done";
        while !scanner.is_finished() {
            std::thread::sleep(Duration::from_millis(100));
            if a.stop_file.exists() && !stop.load(Ordering::Relaxed) {
                stop.store(true, Ordering::Relaxed);
                outcome = "stopped";
            }
        }
        match scanner.join() {
            Ok(Ok((s, ids))) => {
                let q =
                    |v: [f64; 4]| json!({ "p50": v[0], "p99": v[1], "p999": v[2], "max": v[3] });
                report["hwlat"] = json!({
                    "cpu": cpu, "threshold_us": a.threshold_us, "placed": ids, "priority": "time-critical",
                    "reads": s.reads, "over": s.over, "gaps_us": q(s.gaps.summary_us()),
                    "largest": s.largest.iter().map(|&(at, gap)| json!({ "at_us": at as f64 / 1e3, "gap_us": gap as f64 / 1e3 })).collect::<Vec<_>>(),
                });
                report["outcome"] = json!(outcome);
                exit_code(outcome)
            }
            Ok(Err(e)) => {
                report["hwlat"] = json!({ "cpu": cpu, "threshold_us": a.threshold_us, "error": e });
                report["error"] = json!(e);
                report["outcome"] = json!("error");
                exit_code("error")
            }
            Err(_) => {
                report["error"] = json!("the hwlat scanner thread panicked");
                report["outcome"] = json!("error");
                exit_code("error")
            }
        }
    }

    /// Drains the stream's new glitches: one trace marker each, then into the
    /// segment's list (capped). Returns how many did not fit the list.
    fn take_glitches(
        running: &Running<'_>,
        fresh: &mut Vec<Glitch>,
        kept: &mut Vec<Glitch>,
        markers: Option<&os::Markers>,
        qpc: Option<(i64, i64)>,
    ) -> usize {
        fresh.clear();
        running.drain_glitches(fresh);
        if let (Some(m), Some((base, freq))) = (markers, qpc) {
            write_markers(
                fresh,
                base,
                freq,
                || os::qpc().map_or(0, |q| q.0),
                |text| m.write(text),
            );
        }
        keep_glitches(kept, fresh)
    }

    fn glitches_json(list: &[Glitch]) -> Value {
        json!(
            list.iter()
                .map(|g| json!({ "kind": g.kind.name(), "at_ns": g.at_ns, "value": g.value }))
                .collect::<Vec<_>>()
        )
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
        // The busy threads run on the CPUs the report names, or the run fails.
        let _stress = match Stress::start(a.stress, &a.stress_cpus, place_thread) {
            Ok((stress, applied)) => {
                report["process"]["stress_cpus"] = applied_json(&a.stress_cpus, &applied);
                stress
            }
            Err(e) => {
                report["process"]["stress_cpus"] =
                    applied_json(&a.stress_cpus, &Applied::Failed(e.clone()));
                report["error"] = json!(e);
                report["outcome"] = json!("error");
                return Ok(exit_code("error"));
            }
        };
        let markers = os::Markers::register().ok();
        let deadline = Instant::now() + Duration::from_secs(a.seconds);
        let mut watch = Watch::new(Instant::now(), a.watched.clone());
        let mut fault: Option<(Instant, u64)> = None;
        let mut outcome = "done";
        report["segments"] = json!([]);
        report["resets_handled"] = json!([]);
        loop {
            let running = host.start(&info, cfg)?;
            let first = wait_first_callback(&running)?;
            let latency = host.latencies()?;
            let qpc = running.qpc_base();
            let mut glitches: Vec<Glitch> = Vec::new();
            let mut fresh: Vec<Glitch> = Vec::with_capacity(1_024);
            let mut unreported = 0_usize;
            let t0 = Instant::now();
            let mut next_progress = t0 + Duration::from_secs(5);
            let reopen = loop {
                asio::pump_messages();
                std::thread::sleep(Duration::from_millis(10));
                unreported +=
                    take_glitches(&running, &mut fresh, &mut glitches, markers.as_ref(), qpc);
                let now = Instant::now();
                if let Some(end) =
                    watch.poll(now, a.stop_file.exists(), running.rate_changed(), || {
                        running.take_input_peaks()
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
                        let _ = write_json(path, &progress_json(t0.elapsed(), &s, &watch));
                    }
                }
            };
            let seconds = t0.elapsed().as_secs_f64();
            unreported += take_glitches(&running, &mut fresh, &mut glitches, markers.as_ref(), qpc);
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
                    "glitches": glitches_json(&glitches), "glitches_unreported": unreported,
                    "qpc": qpc.map(|(base, freq)| json!({ "base": base, "freq": freq })),
                }),
            );
            watch.record_levels(report);
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
        Ok(exit_code(outcome))
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
        let mut watch = Watch::new(Instant::now(), a.watched.clone());
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
                    || running.take_input_peaks(),
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
            watch.record_levels(report);
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
        Ok(exit_code(outcome))
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
            "callback_cpus": s.callback_cpus.iter().map(|&(lp, n)| (lp.to_string(), json!(n))).collect::<serde_json::Map<_, _>>(),
            "cpu_other": s.cpu_other, "callback_thread": s.callback_thread,
            "thread_switches": s.thread_switches, "glitches_dropped": s.glitches_dropped,
        })
    }

    fn progress_json(elapsed: Duration, s: &Snapshot, watch: &Watch) -> Value {
        let mut p = json!({
            "elapsed_s": elapsed.as_secs(), "callbacks": s.callbacks, "late": s.late, "missed": s.missed,
            "overruns": s.overruns, "position_gaps": s.position_gaps, "resets": s.resets,
            "callback_thread": s.callback_thread,
        });
        watch.record_levels(&mut p);
        p
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
mod tests;
