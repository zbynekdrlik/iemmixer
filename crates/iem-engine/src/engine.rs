//! Engine start-up and the offline renderer (design note §1, §3.7): load the
//! site and the state, start the backend (NullRt, or on Windows the ASIO
//! card of the site's `[card]` table, S6), open the pipes, run the control
//! loop; or render a WAV file through the processor with `Offline`; or, S6,
//! check a site (`check-site`). Nothing listens to the stage before a switch
//! (#38: only the owner's signal decides).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use iem_audio_io::hist::HistSnapshot;
use iem_audio_io::owner::StopOutcome;
use iem_audio_io::{InputSignal, NullRt, NullRtConfig, Offline, StreamStats, wav};
use iem_engine_proto::media::stream;
use iem_engine_proto::{Alarm, AlarmCode, MixState, write_media};
use interprocess::local_socket::Listener;
use interprocess::local_socket::traits::Listener as _;
use rtrb::{Consumer, Producer};
use serde::Serialize;
use tracing::{error, info, warn};

use crate::SAMPLE_RATE;
use crate::control::{Control, CtlMsg, Driver, Exit, Parts, Settings};
use crate::core::{Core, Flags};
use crate::media::{Frame, TalkbackFeed, TapFramer};
use crate::persist::{Source, StateLock, Store, decode};
use crate::pipe::{Conn, Framer, control_name, listen, media_name, read_loop};
use crate::rt::{FADE_IN_MS, Options, Processor, RtHandles};
use crate::site::{self, SiteError, load, parse, parse_card, parse_hil_tx};
use crate::topology::{Topology, compile};

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
    /// The card is refused (exit 3, S6 design note §4): a holder of the
    /// driver module (I3), no such driver, a refused format or channel
    /// map, the preference window, a measured period other than 32.
    #[error("card refused: {0}")]
    Card(String),
    /// Another process held the state directory past [`STATE_WAIT`]
    /// (exit 75, #32 minor-4): the guard starts the engine again without
    /// counting a crash.
    #[error("{0}")]
    StateBusy(String),
}

/// The audio backend of `run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Paced real time with synthetic inputs (CI, E2E, soak).
    NullRt,
    /// The ASIO card of the site's `[card]` table (Windows).
    Asio,
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
    pub backend: Backend,
    /// `--hold`: silent until the supervisor's `Arm` (S6).
    pub hold: bool,
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
            backend: Backend::NullRt,
            hold: false,
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
    CheckSite(PathBuf),
    Help,
}

pub const USAGE: &str = "\
usage:
  iem-engine run --site <site.toml> --state-dir <dir> --pipe <name>
                 [--backend nullrt|asio] [--hold] [--block <frames>] [--sine <hz>]
                 [--test-signal] [--fault-injection]
  iem-engine render --site <site.toml> [--state <state.json>] --in <in.wav> --out <out.wav>
                 [--block <frames>]
  iem-engine check-site --site <site.toml>

run: the engine at 96 kHz on the paced NullRt backend (--block, --sine), or
with --backend asio (Windows) on the card of the site's [card] table at its
32 samples; --hold keeps every output silent until the supervisor's Arm.
--pipe is a socket path on Unix and a pipe name on Windows (the media pipe
is <name>.media).
render: the input WAV must be 96 kHz with one channel per RX channel of the
site; the output has one channel per TX channel.
check-site: validates the site (I4; [card] and [guard] hil_tx too) and
prints its topology hash and counts.
exit codes: 0 shut down, 1 i/o error, 2 usage or site error, 3 card
refused, 70 RT fault, 75 state directory busy";

fn value<'a>(it: &mut impl Iterator<Item = &'a String>, flag: &str) -> Result<&'a String, String> {
    it.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn block(v: &str) -> Result<usize, String> {
    match v.parse::<usize>() {
        Ok(n) if (1..=MAX_BLOCK).contains(&n) => Ok(n),
        _ => Err(format!("--block must be 1…{MAX_BLOCK}, not {v:?}")),
    }
}

fn backend(v: &str) -> Result<Backend, String> {
    match v {
        "nullrt" => Ok(Backend::NullRt),
        "asio" => Ok(Backend::Asio),
        _ => Err(format!("--backend is nullrt or asio, not {v:?}")),
    }
}

/// Parses the command line (without the program name).
pub fn parse_args(args: &[String]) -> Result<Command, String> {
    let mut it = args.iter();
    let sub = match it.next().map(String::as_str) {
        None | Some("-h" | "--help" | "help") => return Ok(Command::Help),
        Some(s @ ("run" | "render" | "check-site")) => s,
        Some(other) => return Err(format!("unknown command {other:?}")),
    };
    let mut paths: [Option<PathBuf>; 5] = Default::default();
    let mut pipe = None;
    let mut size = None;
    let mut flags = Flags::default();
    let mut signal = None;
    let mut chosen = Backend::NullRt;
    let mut hold = false;
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
            "--block" => size = Some(block(value(&mut it, flag)?)?),
            "--sine" => {
                let v = value(&mut it, flag)?;
                let hz: f64 = v
                    .parse()
                    .ok()
                    .filter(|hz: &f64| hz.is_finite() && *hz > 0.0 && *hz < 48_000.0)
                    .ok_or_else(|| format!("--sine needs a frequency below 48 kHz, not {v:?}"))?;
                signal = Some(InputSignal::Sine { hz, amp: 0.1 });
            }
            "--test-signal" => flags.test_signal = true,
            "--fault-injection" => flags.fault_injection = true,
            "--backend" => chosen = backend(value(&mut it, flag)?)?,
            "--hold" => hold = true,
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    let [site, state_dir, state, input, output] = paths;
    let need = |p: Option<PathBuf>, flag: &str| p.ok_or_else(|| format!("{sub} needs {flag}"));
    match sub {
        "run" => {
            if chosen == Backend::Asio && (size.is_some() || signal.is_some()) {
                return Err(
                    "--block and --sine are for the nullrt backend: the card runs at its [card] frames"
                        .to_owned(),
                );
            }
            let mut cfg = RunConfig::new(
                need(site, "--site")?,
                need(state_dir, "--state-dir")?,
                pipe.ok_or_else(|| "run needs --pipe".to_owned())?,
            );
            cfg.block = size.unwrap_or(32);
            cfg.flags = flags;
            cfg.signal = signal.unwrap_or(InputSignal::Silence);
            cfg.backend = chosen;
            cfg.hold = hold;
            Ok(Command::Run(cfg))
        }
        "render" => Ok(Command::Render(RenderArgs {
            site: need(site, "--site")?,
            state,
            input: need(input, "--in")?,
            output: need(output, "--out")?,
            block: size.unwrap_or(32),
        })),
        _ => Ok(Command::CheckSite(need(site, "--site")?)),
    }
}

/// The ASIO backend locks its memory after this long of streaming (S1c
/// hand-off): the working set has settled by then.
pub const LOCK_AFTER: Duration = Duration::from_secs(5);
/// How far it raises the minimum working set first (MiB).
pub const LOCK_EXTRA_MB: usize = 64;

/// Whether the memory lock is due: once, `LOCK_AFTER` after the stream
/// started.
pub fn lock_due(started: Instant, now: Instant, done: bool) -> bool {
    !done && now.saturating_duration_since(started) >= LOCK_AFTER
}

struct NullRtDriver(NullRt<Processor>);

impl Driver for NullRtDriver {
    fn stats(&self) -> StreamStats {
        self.0.stats()
    }

    /// NullRt holds no card: its stop always releases.
    fn stop(self: Box<Self>) -> StopOutcome {
        if self.0.stop().is_none() {
            warn!("the NullRt thread ended outside its guarded callback");
        }
        StopOutcome::Released
    }

    fn histograms(&self) -> Option<HistSnapshot> {
        Some(self.0.histograms())
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

/// Drains the listen taps, then the listen probe's taps (S7: streams 2 and
/// 3), into 48 kHz frames for the current media client; a client that takes
/// nothing for `pipe::SEND_TIMEOUT` is dropped (`Conn::writer`), so `run`,
/// which joins this thread, still returns. A probe framer keeps a burst's
/// partial frame and filter history, which open the next burst's first frame
/// (the same sine at the same level).
fn media_pump(
    mut taps: [Consumer<f32>; 2],
    mut probes: [Consumer<f32>; 2],
    conns: mpsc::Receiver<Conn>,
    stop: Arc<AtomicBool>,
) {
    let mut framers = [
        TapFramer::new(stream::ENGINEER_LISTEN),
        TapFramer::new(stream::MEMBER_LISTEN),
        TapFramer::new(stream::ENGINEER_PROBE),
        TapFramer::new(stream::MEMBER_PROBE),
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
        let rings = taps.iter_mut().chain(probes.iter_mut());
        for (tap, framer) in rings.zip(framers.iter_mut()) {
            let (got, _) = tap.pop_partial_slice(&mut buf);
            let n = got.len();
            framer.feed(buf.get(..n).unwrap_or_default(), &mut frames);
        }
        let mut failed = false;
        if let Some(c) = current.as_ref().filter(|c| !c.is_closed()) {
            for (h, samples) in &frames {
                if write_media(&mut c.writer(), h, samples).is_err() {
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
    let text = site::read(&cfg.site)?;
    let topo = Arc::new(compile(&parse(&text)?)?);
    let card = match cfg.backend {
        Backend::NullRt => None,
        Backend::Asio => Some(parse_card(&text)?.ok_or(SiteError::NoCardTable)?),
    };
    #[cfg(not(windows))]
    if card.is_some() {
        return Err(EngineError::Usage(ASIO_ON_WINDOWS.to_owned()));
    }
    let hil = run_hil(cfg.flags, &text, &topo)?;
    // Crash dialogs off, the RT panic hook and the SEH filter, priority and
    // power throttling, before the card opens (S6 design note §3).
    #[cfg(windows)]
    if let Some(card) = &card {
        crate::asio::prepare(card);
    }
    info!(
        "site {}: {} inputs, {} groups, {} mixes, topology {}",
        cfg.site.display(),
        topo.inputs.len(),
        topo.groups.len(),
        topo.mixes.len(),
        topo.hash
    );
    if !hil.is_empty() {
        info!("HIL's spare card outputs {hil:?} after the topology's TX");
    }
    let store = Store::open(&cfg.state_dir)?;
    // Held until `run` returns: one engine per state directory (#32 P5).
    let _state_lock = lock_state(&store)?;
    let loaded = store.load(&topo);
    for (path, why) in &loaded.rejected {
        warn!("state file {} skipped: {why}", path.display());
    }
    if let Some(note) = dropped_note(&loaded.dropped) {
        warn!("{note}");
    }
    let mut alarms = Vec::new();
    match loaded.source {
        Source::Current => info!("state rev {} loaded", loaded.persisted.rev),
        // The newest state: a crash cut its save off between the renames.
        Source::Interrupted => warn!(
            "state rev {} loaded from save.tmp: a crash cut its save off before current.json",
            loaded.persisted.rev
        ),
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
    // #32: why the state loaded may be older than one the directory holds.
    alarms.extend(loaded.alarms.iter().map(|detail| Alarm {
        code: AlarmCode::StateFallback,
        detail: detail.clone(),
    }));
    // #32: the directory holds what was loaded before the engine runs.
    let recovery = store.recover(&loaded);
    if let Some(to) = &recovery.quarantined {
        warn!(
            "the damaged current.json is moved aside to {}",
            to.display()
        );
    }
    if recovery.finished {
        info!("the interrupted save is finished: save.tmp is current.json");
    }
    for why in &recovery.warnings {
        warn!("{why}");
    }
    alarms.extend(recovery.failed.into_iter().map(|detail| Alarm {
        code: AlarmCode::SaveFailed,
        detail,
    }));
    for a in &alarms {
        warn!("alarm {:?}: {}", a.code, a.detail);
    }
    let counters: Vec<u64> = topo
        .mixes
        .iter()
        .map(|b| loaded.persisted.counters.get(&b.id).copied().unwrap_or(0))
        .collect();
    let core = Core::new(
        Arc::clone(&topo),
        &loaded.persisted.state,
        loaded.persisted.rev,
        cfg.flags,
    )
    .with_hil(hil.clone());
    // The D5(b) loopback return (S6 test 5): under `--test-signal` the engine
    // also opens the spare card inputs the HIL spare outputs loop back to, so
    // it can measure the round-trip. One return per HIL output.
    let hil_rx = hil.len();
    let (processor, handles) = Processor::with_hil(
        Arc::clone(&topo),
        &loaded.persisted.state,
        &counters,
        Options {
            fade_in_ms: FADE_IN_MS,
            hold: cfg.hold,
        },
        hil.len(),
        hil_rx,
    );
    let outputs = processor.outputs();
    let RtHandles {
        cmds,
        meters,
        taps,
        probes,
        talkback,
        status,
    } = handles;
    let control_listener = listen(control_name(&cfg.pipe)?)?;
    let media_listener = listen(media_name(&cfg.pipe)?)?;
    let (driver, block): (Box<dyn Driver>, usize) = match card {
        None => {
            let nullrt = NullRt::start(
                NullRtConfig {
                    sample_rate: SAMPLE_RATE,
                    block: cfg.block,
                    inputs: processor.input_channels(),
                    outputs,
                    signal: cfg.signal,
                },
                processor,
            )?;
            info!(
                "NullRt running: 96 kHz, block {}, pipe {}",
                cfg.block, cfg.pipe
            );
            let driver: Box<dyn Driver> = Box::new(NullRtDriver(nullrt));
            (driver, cfg.block)
        }
        #[cfg(windows)]
        Some(card) => crate::asio::start(&card, &topo, &hil, processor)?,
        #[cfg(not(windows))]
        Some(_) => return Err(EngineError::Usage(ASIO_ON_WINDOWS.to_owned())),
    };
    if cfg.hold {
        info!("held: every output is silent until the supervisor's Arm");
    }
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
            move || media_pump(taps, probes, media_rx, stop)
        })?,
    ];
    let control = Control::new(Parts {
        core,
        store,
        cmds,
        meters,
        status,
        talkback_dropped: dropped,
        driver,
        counters,
        alarms,
        settings: Settings {
            solo_grace: cfg.solo_grace,
            block: u32::try_from(block).unwrap_or(u32::MAX),
            hold: cfg.hold,
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

/// How long `run` waits for its state directory while another process
/// holds it (#32 minor-4): an engine that just ended may hold its lock a
/// moment after its exit (a lock's release can lag the process end).
/// With the load's read pauses (2 s at most) the engine still listens well
/// within the guard's READY_S (10 s).
pub const STATE_WAIT: Duration = Duration::from_secs(3);

/// Takes the state directory (`Store::lock_within` [`STATE_WAIT`]); still
/// held by another process then, it is `EngineError::StateBusy` (exit 75).
fn lock_state(store: &Store) -> Result<StateLock, EngineError> {
    store.lock_within(STATE_WAIT).map_err(|e| {
        if e.kind() == std::io::ErrorKind::WouldBlock {
            EngineError::StateBusy(format!("{e} (waited {STATE_WAIT:?})"))
        } else {
            EngineError::Io(e)
        }
    })
}

#[cfg(not(windows))]
const ASIO_ON_WINDOWS: &str = "the asio backend runs on Windows only";

/// What `check-site` prints (I4, F30).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SiteSummary {
    pub topology: String,
    pub inputs: usize,
    pub groups: usize,
    pub mixes: usize,
    /// Whether the site has a (valid) `[card]` table.
    pub card: bool,
}

/// HIL's spare outputs for `run` (S6): the site's `[guard] hil_tx`, checked
/// by `Topology::hil_outputs`, with the test-signal flag only. Without it
/// no HIL signal can start, so a live engine never reads the key and opens
/// no card output outside the topology.
fn run_hil(flags: Flags, text: &str, topo: &Topology) -> Result<Vec<u16>, SiteError> {
    if !flags.test_signal {
        return Ok(Vec::new());
    }
    topo.hil_outputs(&parse_hil_tx(text)?)
}

/// Loads and compiles a site, checks its `[card]` table and HIL's spare
/// outputs (`[guard] hil_tx`: card channels from 1 that no mix
/// uses and no input is, `Topology::hil_outputs`; the card's own output and
/// input counts, the latter for the loopback returns, are checked when the
/// stream opens).
pub fn check_site(path: &Path) -> Result<SiteSummary, EngineError> {
    let text = site::read(path)?;
    let topo = compile(&parse(&text)?)?;
    let card = parse_card(&text)?;
    // The HIL signal goes only to spare outputs, never to a band member
    // (the owner's decision on #9 of 2026-09-28).
    topo.hil_outputs(&parse_hil_tx(&text)?)?;
    Ok(SiteSummary {
        topology: topo.hash,
        inputs: topo.inputs.len(),
        groups: topo.groups.len(),
        mixes: topo.mixes.len(),
        card: card.is_some(),
    })
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
    let topo = Arc::new(compile(&load(&a.site)?)?);
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
    if audio.channels() != topo.rx.len() {
        return Err(EngineError::Usage(format!(
            "{} has {} channels; the site has {} RX channels",
            a.input.display(),
            audio.channels(),
            topo.rx.len()
        )));
    }
    if !(1..=MAX_BLOCK).contains(&a.block) {
        return Err(EngineError::Usage(format!("block must be 1…{MAX_BLOCK}")));
    }
    let (mut p, _handles) = Processor::new(
        Arc::clone(&topo),
        &state,
        &[],
        Options {
            fade_in_ms: 0.0,
            hold: false,
        },
    );
    let run = Offline { block: a.block }.run(&mut p, &audio, topo.tx.len());
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
mod tests;
