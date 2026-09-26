//! Engine start-up and the offline renderer (design note §1, §3.7): load the
//! site and the state, start the backend, open the pipes, run the control
//! loop; or render a WAV file through the processor with `Offline`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use iem_audio_io::{InputSignal, NullRt, NullRtConfig, Offline, StreamStats, wav};
use iem_engine_proto::media::stream;
use iem_engine_proto::{Alarm, AlarmCode, MixState, write_media};
use interprocess::local_socket::Listener;
use interprocess::local_socket::traits::Listener as _;
use rtrb::{Consumer, Producer};
use tracing::{error, info, warn};

use crate::SAMPLE_RATE;
use crate::control::{Control, CtlMsg, Driver, Exit, Parts, Settings};
use crate::core::{Core, Flags};
use crate::graph::compile;
use crate::media::{Frame, TalkbackFeed, TapFramer};
use crate::persist::{Source, Store, decode};
use crate::pipe::{Conn, Framer, control_name, listen, media_name, read_loop};
use crate::rt::{Options, Processor, RtHandles};
use crate::site::{SiteError, load};

/// Largest block the engine accepts.
pub const MAX_BLOCK: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Site(#[from] SiteError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Usage(String),
    #[error("the render faulted at frame {frame}: {message}")]
    Fault { frame: u64, message: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunConfig {
    pub site: PathBuf,
    pub state_dir: PathBuf,
    pub pipe: String,
    pub block: usize,
    pub flags: Flags,
    pub signal: InputSignal,
    /// X2: solos clear this long after the controller left.
    pub solo_grace: Duration,
}

impl RunConfig {
    pub fn new(site: PathBuf, state_dir: PathBuf, pipe: String) -> Self {
        Self {
            site,
            state_dir,
            pipe,
            block: 32,
            flags: Flags::default(),
            signal: InputSignal::Silence,
            solo_grace: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderArgs {
    pub site: PathBuf,
    pub state: Option<PathBuf>,
    pub input: PathBuf,
    pub output: PathBuf,
    pub block: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Run(RunConfig),
    Render(RenderArgs),
    Help,
}

pub const USAGE: &str = "\
usage:
  iem-engine run --site <site.toml> --state-dir <dir> --pipe <name>
                 [--block <frames>] [--sine <hz>] [--test-signal] [--fault-injection]
  iem-engine render --site <site.toml> [--state <state.json>] --in <in.wav> --out <out.wav>
                 [--block <frames>]

run: the engine on the paced NullRt backend (96 kHz); --pipe is a socket path
on Unix and a pipe name on Windows (the media pipe is <name>.media).
render: the input WAV must be 96 kHz with one channel per RX channel of the
site; the output has one channel per TX channel.
exit codes: 0 shut down, 1 i/o error, 2 usage or site error, 70 RT fault";

fn value<'a>(it: &mut impl Iterator<Item = &'a String>, flag: &str) -> Result<&'a String, String> {
    it.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn block(v: &str) -> Result<usize, String> {
    match v.parse::<usize>() {
        Ok(n) if (1..=MAX_BLOCK).contains(&n) => Ok(n),
        _ => Err(format!("--block must be 1…{MAX_BLOCK}, not {v:?}")),
    }
}

/// Parses the command line (without the program name).
pub fn parse_args(args: &[String]) -> Result<Command, String> {
    let mut it = args.iter();
    let sub = match it.next().map(String::as_str) {
        None | Some("-h" | "--help" | "help") => return Ok(Command::Help),
        Some(s @ ("run" | "render")) => s,
        Some(other) => return Err(format!("unknown command {other:?}")),
    };
    let mut paths: [Option<PathBuf>; 5] = Default::default();
    let mut pipe = None;
    let mut size = 32;
    let mut flags = Flags::default();
    let mut signal = InputSignal::Silence;
    while let Some(flag) = it.next() {
        let slot = match flag.as_str() {
            "--site" => Some(0),
            "--state-dir" => Some(1),
            "--state" => Some(2),
            "--in" => Some(3),
            "--out" => Some(4),
            _ => None,
        };
        if let Some(k) = slot {
            let v = value(&mut it, flag)?;
            if let Some(p) = paths.get_mut(k) {
                *p = Some(PathBuf::from(v));
            }
            continue;
        }
        match flag.as_str() {
            "--pipe" => pipe = Some(value(&mut it, flag)?.clone()),
            "--block" => size = block(value(&mut it, flag)?)?,
            "--sine" => {
                let v = value(&mut it, flag)?;
                let hz: f64 = v
                    .parse()
                    .ok()
                    .filter(|hz: &f64| hz.is_finite() && *hz > 0.0 && *hz < 48_000.0)
                    .ok_or_else(|| format!("--sine needs a frequency below 48 kHz, not {v:?}"))?;
                signal = InputSignal::Sine { hz, amp: 0.1 };
            }
            "--test-signal" => flags.test_signal = true,
            "--fault-injection" => flags.fault_injection = true,
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    let [site, state_dir, state, input, output] = paths;
    let need = |p: Option<PathBuf>, flag: &str| p.ok_or_else(|| format!("{sub} needs {flag}"));
    if sub == "run" {
        let mut cfg = RunConfig::new(
            need(site, "--site")?,
            need(state_dir, "--state-dir")?,
            pipe.ok_or_else(|| "run needs --pipe".to_owned())?,
        );
        cfg.block = size;
        cfg.flags = flags;
        cfg.signal = signal;
        Ok(Command::Run(cfg))
    } else {
        Ok(Command::Render(RenderArgs {
            site: need(site, "--site")?,
            state,
            input: need(input, "--in")?,
            output: need(output, "--out")?,
            block: size,
        }))
    }
}

struct NullRtDriver(NullRt<Processor>);

impl Driver for NullRtDriver {
    fn stats(&self) -> StreamStats {
        self.0.stats()
    }

    fn stop(self: Box<Self>) {
        if self.0.stop().is_none() {
            warn!("the NullRt thread ended outside its guarded callback");
        }
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name(name.into()).spawn(f)
}

/// How long an acceptor waits after `accept` found nothing (10 ms) or failed
/// (100 ms, logged).
fn backoff(e: &std::io::Error, what: &str) -> Duration {
    if e.kind() == std::io::ErrorKind::WouldBlock {
        Duration::from_millis(10)
    } else {
        warn!("{what} accept failed: {e}");
        Duration::from_millis(100)
    }
}

fn accept_control(listener: Listener, tx: mpsc::Sender<CtlMsg>, stop: Arc<AtomicBool>) {
    let mut next = 1u64;
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok(stream) => {
                let conn = Conn::new(stream);
                let (id, reader, frames) = (next, conn.clone(), tx.clone());
                next += 1;
                if tx.send(CtlMsg::Connected { id, conn }).is_err() {
                    return;
                }
                let started = spawn(&format!("iem-ctl-{id}"), move || {
                    let why = read_loop(&reader, Framer::next_frame, |bytes| {
                        frames.send(CtlMsg::Frame { id, bytes }).is_ok()
                    });
                    let _ = frames.send(CtlMsg::Closed {
                        id,
                        why: why.to_string(),
                    });
                });
                if let Err(e) = started {
                    error!("cannot start a reader thread: {e}");
                }
            }
            Err(e) => std::thread::sleep(backoff(&e, "control")),
        }
    }
}

type Talk = Arc<Mutex<(TalkbackFeed, Producer<f32>)>>;

fn accept_media(
    listener: Listener,
    conns: mpsc::Sender<Conn>,
    talk: Talk,
    dropped: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok(stream) => {
                let conn = Conn::new(stream);
                let (reader, talk, dropped) =
                    (conn.clone(), Arc::clone(&talk), Arc::clone(&dropped));
                if conns.send(conn).is_err() {
                    return;
                }
                info!("media connection opened");
                let started = spawn("iem-media-in", move || {
                    let why = read_loop(&reader, Framer::next_media, |(h, samples)| {
                        if h.stream == stream::TALKBACK
                            && h.channels == 1
                            && let Ok(mut g) = talk.lock()
                        {
                            let (feed, ring) = &mut *g;
                            dropped.fetch_add(feed.feed(&samples, ring) as u64, Ordering::Relaxed);
                        }
                        true
                    });
                    info!("media connection closed: {why}");
                });
                if let Err(e) = started {
                    error!("cannot start a media reader thread: {e}");
                }
            }
            Err(e) => std::thread::sleep(backoff(&e, "media")),
        }
    }
}

/// Drains the listen taps into 48 kHz frames for the current media client.
fn media_pump(mut taps: [Consumer<f32>; 2], conns: mpsc::Receiver<Conn>, stop: Arc<AtomicBool>) {
    let mut framers = [
        TapFramer::new(stream::ENGINEER_LISTEN),
        TapFramer::new(stream::MEMBER_LISTEN),
    ];
    let mut current: Option<Conn> = None;
    let mut buf = vec![0.0f32; crate::rt::TAP_RING];
    let mut frames: Vec<Frame> = Vec::new();
    while !stop.load(Ordering::Acquire) {
        while let Ok(c) = conns.try_recv() {
            if let Some(old) = current.replace(c) {
                old.close();
            }
        }
        // 5 ms of a tap is 960 values; the buffer holds a whole tap ring.
        for (tap, framer) in taps.iter_mut().zip(framers.iter_mut()) {
            let (got, _) = tap.pop_partial_slice(&mut buf);
            let n = got.len();
            framer.feed(buf.get(..n).unwrap_or_default(), &mut frames);
        }
        let mut failed = false;
        if let Some(c) = current.as_ref().filter(|c| !c.is_closed()) {
            for (h, samples) in &frames {
                if write_media(&mut &*c.stream, h, samples).is_err() {
                    failed = true;
                    break;
                }
            }
        } else {
            current = None;
        }
        frames.clear();
        if failed && let Some(c) = current.take() {
            info!("media client gone");
            c.close();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The warning for state entries the topology no longer has, if any.
fn dropped_note(dropped: &[String]) -> Option<String> {
    (!dropped.is_empty()).then(|| {
        format!(
            "state entries the topology no longer has: {}",
            dropped.join(", ")
        )
    })
}

/// Runs the engine until `Shutdown` or a fault (blocking).
pub fn run(cfg: RunConfig) -> Result<Exit, EngineError> {
    if !(1..=MAX_BLOCK).contains(&cfg.block) {
        return Err(EngineError::Usage(format!("block must be 1…{MAX_BLOCK}")));
    }
    let graph = Arc::new(compile(&load(&cfg.site)?)?);
    info!(
        "site {}: {} inputs, {} buses, {} sends, topology {}",
        cfg.site.display(),
        graph.inputs.len(),
        graph.buses.len(),
        graph.sends.len(),
        graph.hash
    );
    let store = Store::open(&cfg.state_dir)?;
    let loaded = store.load(&graph);
    for (path, why) in &loaded.rejected {
        warn!("state file {} skipped: {why}", path.display());
    }
    if let Some(note) = dropped_note(&loaded.dropped) {
        warn!("{note}");
    }
    let mut alarms = Vec::new();
    match loaded.source {
        Source::Current => info!("state rev {} loaded", loaded.persisted.rev),
        Source::Generation(_) | Source::Baseline => alarms.push(Alarm {
            code: AlarmCode::StateFallback,
            detail: format!(
                "state rev {} from {:?}",
                loaded.persisted.rev, loaded.source
            ),
        }),
        Source::Defaults => alarms.push(Alarm {
            code: AlarmCode::StateLost,
            detail: "no usable state: every output is muted".into(),
        }),
    }
    for a in &alarms {
        warn!("alarm {:?}: {}", a.code, a.detail);
    }
    let counters: Vec<u64> = graph
        .buses
        .iter()
        .map(|b| loaded.persisted.counters.get(&b.id).copied().unwrap_or(0))
        .collect();
    let core = Core::new(
        Arc::clone(&graph),
        &loaded.persisted.state,
        loaded.persisted.rev,
        cfg.flags,
    );
    let (processor, handles) = Processor::new(
        Arc::clone(&graph),
        &loaded.persisted.state,
        &counters,
        Options::default(),
    );
    let RtHandles {
        cmds,
        meters,
        taps,
        talkback,
        status,
    } = handles;
    let control_listener = listen(control_name(&cfg.pipe)?)?;
    let media_listener = listen(media_name(&cfg.pipe)?)?;
    let driver = NullRt::start(
        NullRtConfig {
            sample_rate: SAMPLE_RATE,
            block: cfg.block,
            inputs: graph.rx.len(),
            outputs: graph.tx.len(),
            signal: cfg.signal,
        },
        processor,
    )?;
    info!(
        "NullRt running: 96 kHz, block {}, pipe {}",
        cfg.block, cfg.pipe
    );
    let stop = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicU64::new(0));
    let talk: Talk = Arc::new(Mutex::new((TalkbackFeed::new(), talkback)));
    let (tx, rx) = mpsc::channel();
    let (media_tx, media_rx) = mpsc::channel();
    let threads = vec![
        spawn("iem-accept", {
            let stop = Arc::clone(&stop);
            move || accept_control(control_listener, tx, stop)
        })?,
        spawn("iem-media-accept", {
            let (stop, dropped) = (Arc::clone(&stop), Arc::clone(&dropped));
            move || accept_media(media_listener, media_tx, talk, dropped, stop)
        })?,
        spawn("iem-media", {
            let stop = Arc::clone(&stop);
            move || media_pump(taps, media_rx, stop)
        })?,
    ];
    let control = Control::new(Parts {
        core,
        store,
        cmds,
        meters,
        status,
        talkback_dropped: dropped,
        driver: Box::new(NullRtDriver(driver)),
        counters,
        alarms,
        settings: Settings {
            solo_grace: cfg.solo_grace,
            block: u32::try_from(cfg.block).unwrap_or(u32::MAX),
        },
    });
    let exit = control.run(&rx);
    stop.store(true, Ordering::Release);
    for t in threads {
        let _ = t.join();
    }
    info!("engine exit: {exit:?}");
    Ok(exit)
}

/// Reads a state file (the persisted format, or a plain `MixState` JSON).
fn read_state(path: &Path) -> Result<MixState, EngineError> {
    let bytes = std::fs::read(path)?;
    decode(&bytes)
        .map(|p| p.state)
        .or_else(|why| {
            serde_json::from_slice::<MixState>(&bytes)
                .map_err(|e| format!("{}: {why}; as a plain mix state: {e}", path.display()))
        })
        .map_err(EngineError::Usage)
}

/// Renders a 96 kHz WAV (one channel per RX channel) to a WAV with one
/// channel per TX channel, deterministically (§3.5 parity harness).
pub fn render(a: &RenderArgs) -> Result<(), EngineError> {
    let graph = Arc::new(compile(&load(&a.site)?)?);
    let state = match &a.state {
        Some(p) => read_state(p)?,
        None => MixState::default(),
    };
    let (rate, audio) = wav::read_file(&a.input)?;
    if rate != SAMPLE_RATE {
        return Err(EngineError::Usage(format!(
            "{} is {rate} Hz; the engine runs at {SAMPLE_RATE} Hz only (I2)",
            a.input.display()
        )));
    }
    if audio.channels() != graph.rx.len() {
        return Err(EngineError::Usage(format!(
            "{} has {} channels; the site has {} RX channels",
            a.input.display(),
            audio.channels(),
            graph.rx.len()
        )));
    }
    if !(1..=MAX_BLOCK).contains(&a.block) {
        return Err(EngineError::Usage(format!("block must be 1…{MAX_BLOCK}")));
    }
    let (mut p, _handles) =
        Processor::new(Arc::clone(&graph), &state, &[], Options { fade_in_ms: 0.0 });
    let run = Offline { block: a.block }.run(&mut p, &audio, graph.tx.len());
    if let Some(f) = run.fault {
        return Err(EngineError::Fault {
            frame: f.frame,
            message: f.message,
        });
    }
    wav::write_file(&a.output, SAMPLE_RATE, &run.output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn run_arguments_parse() {
        let cmd = parse_args(&args(
            "run --site s.toml --state-dir d --pipe p --block 64 --sine 440 --test-signal --fault-injection",
        ))
        .unwrap();
        let Command::Run(cfg) = cmd else {
            panic!("{cmd:?}")
        };
        assert_eq!(cfg.site, PathBuf::from("s.toml"));
        assert_eq!(cfg.state_dir, PathBuf::from("d"));
        assert_eq!(cfg.pipe, "p");
        assert_eq!(cfg.block, 64);
        assert_eq!(
            cfg.signal,
            InputSignal::Sine {
                hz: 440.0,
                amp: 0.1
            }
        );
        assert!(cfg.flags.test_signal && cfg.flags.fault_injection);
        assert_eq!(cfg.solo_grace, Duration::from_secs(10));
        let plain = parse_args(&args("run --site s --state-dir d --pipe p")).unwrap();
        let Command::Run(cfg) = plain else {
            panic!("{plain:?}")
        };
        assert_eq!(
            (cfg.block, cfg.signal, cfg.flags),
            (32, InputSignal::Silence, Flags::default())
        );
    }

    #[test]
    fn render_arguments_parse() {
        let cmd = parse_args(&args(
            "render --site s --in a.wav --out b.wav --state x.json --block 97",
        ))
        .unwrap();
        assert_eq!(
            cmd,
            Command::Render(RenderArgs {
                site: "s".into(),
                state: Some("x.json".into()),
                input: "a.wav".into(),
                output: "b.wav".into(),
                block: 97
            })
        );
        let Command::Render(r) = parse_args(&args("render --site s --in a --out b")).unwrap()
        else {
            panic!()
        };
        assert_eq!((r.state, r.block), (None, 32));
    }

    #[test]
    fn bad_arguments_are_explained() {
        assert_eq!(parse_args(&[]).unwrap(), Command::Help);
        assert_eq!(parse_args(&args("--help")).unwrap(), Command::Help);
        for (line, want) in [
            ("start", "unknown command"),
            ("run --site s --state-dir d", "--pipe"),
            ("run --state-dir d --pipe p", "--site"),
            ("run --site s --pipe p", "--state-dir"),
            ("render --site s --out b", "--in"),
            ("render --site s --in a", "--out"),
            ("render --in a --out b", "--site"),
            ("run --site", "needs a value"),
            ("run --site s --state-dir d --pipe p --block 0", "--block"),
            (
                "run --site s --state-dir d --pipe p --block 5000",
                "--block",
            ),
            ("run --site s --state-dir d --pipe p --block x", "--block"),
            ("run --site s --state-dir d --pipe p --sine 0", "--sine"),
            ("run --site s --state-dir d --pipe p --sine 50000", "--sine"),
            ("run --site s --state-dir d --pipe p --sine 48000", "--sine"),
            ("run --site s --state-dir d --pipe p --sine nan", "--sine"),
            (
                "run --site s --state-dir d --pipe p --loud",
                "unknown option",
            ),
        ] {
            let e = parse_args(&args(line)).unwrap_err();
            assert!(e.contains(want), "{line}: {e}");
        }
        assert_eq!(block("4096"), Ok(4096));
        assert_eq!(block("1"), Ok(1));
    }

    #[test]
    fn acceptors_back_off_briefly_when_idle_and_longer_on_errors() {
        let idle = std::io::Error::from(std::io::ErrorKind::WouldBlock);
        assert_eq!(backoff(&idle, "t"), Duration::from_millis(10));
        let broken = std::io::Error::other("broken");
        assert_eq!(backoff(&broken, "t"), Duration::from_millis(100));
    }

    #[test]
    fn dropped_state_entries_are_named_once() {
        assert_eq!(dropped_note(&[]), None);
        assert_eq!(
            dropped_note(&["input ghost".into(), "bus old".into()]).as_deref(),
            Some("state entries the topology no longer has: input ghost, bus old")
        );
    }

    /// `run` in a thread, bounded: a refused start must return at once.
    fn run_bounded(cfg: RunConfig) -> Result<Exit, EngineError> {
        let handle = std::thread::spawn(move || run(cfg));
        let start = std::time::Instant::now();
        while !handle.is_finished() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "run did not refuse"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        handle.join().unwrap()
    }

    #[test]
    fn run_refuses_a_bad_block_and_a_bad_site() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = RunConfig::new(
            crate::test_support::test_site_path(),
            dir.path().join("state"),
            dir.path().join("p.sock").to_string_lossy().into_owned(),
        );
        cfg.block = 0;
        assert!(matches!(
            run_bounded(cfg.clone()),
            Err(EngineError::Usage(_))
        ));
        cfg.block = MAX_BLOCK + 1;
        assert!(matches!(
            run_bounded(cfg.clone()),
            Err(EngineError::Usage(_))
        ));
        cfg.block = 32;
        cfg.site = dir.path().join("missing.toml");
        assert!(matches!(
            run_bounded(cfg),
            Err(EngineError::Site(SiteError::Io(_)))
        ));
    }

    #[test]
    fn render_checks_rate_channels_and_block() {
        let dir = tempfile::tempdir().unwrap();
        let site = crate::test_support::test_site_path();
        let input = dir.path().join("in.wav");
        let output = dir.path().join("out.wav");
        let args = |block: usize| RenderArgs {
            site: site.clone(),
            state: None,
            input: input.clone(),
            output: output.clone(),
            block,
        };
        wav::write_file(&input, 48_000, &iem_audio_io::Planar::new(32, 10)).unwrap();
        let e = render(&args(32)).unwrap_err().to_string();
        assert!(e.contains("48000 Hz"), "{e}");
        wav::write_file(&input, 96_000, &iem_audio_io::Planar::new(4, 10)).unwrap();
        let e = render(&args(32)).unwrap_err().to_string();
        assert!(e.contains("4 channels"), "{e}");
        wav::write_file(&input, 96_000, &iem_audio_io::Planar::new(32, 100)).unwrap();
        assert!(matches!(render(&args(0)), Err(EngineError::Usage(_))));
        render(&args(32)).unwrap();
        let (rate, out) = wav::read_file(&output).unwrap();
        assert_eq!((rate, out.channels(), out.frames()), (96_000, 23, 100));
        // A plain mix-state JSON is accepted as --state; garbage is explained.
        let state = dir.path().join("state.json");
        std::fs::write(&state, "{}").unwrap();
        render(&RenderArgs {
            state: Some(state.clone()),
            ..args(32)
        })
        .unwrap();
        std::fs::write(&state, "[1]").unwrap();
        let e = render(&RenderArgs {
            state: Some(state),
            ..args(32)
        })
        .unwrap_err()
        .to_string();
        assert!(e.contains("plain mix state"), "{e}");
    }
}
