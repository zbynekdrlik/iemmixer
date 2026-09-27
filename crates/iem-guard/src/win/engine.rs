//! The engine (design §4, §5.2): its start, the interlock run, and the
//! supervisor pipe. One connection says hello as `supervisor`; a reader
//! thread keeps what the engine sends (`Hello`, `Topology`, `Status`,
//! `Meters`, replies, `DriverReleased`) so the engine never waits on the
//! guard, and the steps read that inbox. The engine ends only by its own
//! `Shutdown`.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use iem_win::process::Handle;
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, Stream};
use serde_json::Value;
use tracing::{info, warn};

use super::{WinPc, procs};
use crate::cancel::Cancel;
use crate::effects::engine::{self as proto, Msg, Quiet, Ready, ReadyWindow, Shutdown, StagePeaks};
use crate::pc::{EngineSeen, Kid, R, Status, StepError};
use crate::plan::Health;
use crate::site::ENGINE_EXE;
use crate::state;

/// After a start the engine's pipe appears within this.
const CONNECT_AFTER_START: Duration = Duration::from_secs(30);
/// A running engine's pipe answers within this.
const CONNECT: Duration = Duration::from_secs(5);
/// `Hello` and `Topology` follow the hello at once.
const HELLO: Duration = Duration::from_secs(10);
/// `Status` comes once a second.
const STATUS_GAP: Duration = Duration::from_secs(3);
const REPLY: Duration = Duration::from_secs(5);
/// `iem-engine check-site` loads and compiles a site in well under this.
const CHECK_SITE: Duration = Duration::from_secs(60);
/// `Shutdown` → `DriverReleased` (design §5.2), then the process ends.
const RELEASE: Duration = Duration::from_secs(10);
const GONE: Duration = Duration::from_secs(5);
const INBOX_POLL: Duration = Duration::from_millis(50);
/// The interlock's stop file under the guard directory (`--stop-file`).
const INTERLOCK_STOP: &str = "interlock.stop";
const MAX_REPLIES: usize = 64;

/// What the reader thread kept.
#[derive(Debug)]
struct Inbox {
    stage_ids: Vec<String>,
    build: Option<String>,
    topology: bool,
    /// The stage inputs' positions in `Meters.inputs`.
    stage: Vec<usize>,
    /// Stage inputs the topology lacks.
    unknown: Vec<String>,
    status: Option<Status>,
    /// Counts the statuses, so a step reads only newer ones.
    status_seq: u64,
    replies: Vec<(u64, Option<String>)>,
    released: Option<String>,
    peaks: StagePeaks,
    quiet: Quiet,
    closed: Option<String>,
}

impl Inbox {
    fn new(stage_ids: Vec<String>, now: Instant) -> Self {
        Self {
            stage_ids,
            build: None,
            topology: false,
            stage: Vec::new(),
            unknown: Vec::new(),
            status: None,
            status_seq: 0,
            replies: Vec::new(),
            released: None,
            peaks: StagePeaks::default(),
            quiet: Quiet::new(now),
            closed: None,
        }
    }

    fn take(&mut self, msg: Msg, now: Instant) {
        match msg {
            Msg::Hello { build } => self.build = Some(build),
            Msg::Topology { inputs } => {
                let (stage, unknown) = proto::stage_indices(&inputs, &self.stage_ids);
                self.peaks.reset(stage.len());
                self.stage = stage;
                self.unknown = unknown;
                self.topology = true;
            }
            Msg::Status(s) => {
                self.status = Some(s);
                self.status_seq += 1;
            }
            Msg::Meters { inputs } => {
                self.peaks.observe(&inputs, &self.stage);
                self.quiet
                    .observe(proto::stage_max(&inputs, &self.stage), now);
            }
            Msg::Reply { id, error } => {
                self.replies.push((id, error));
                if self.replies.len() > MAX_REPLIES {
                    self.replies.remove(0);
                }
            }
            Msg::DriverReleased { reason } => self.released = Some(reason),
            Msg::Superseded => {
                self.closed = Some("another supervisor took the engine's pipe".to_owned());
            }
            Msg::Other => {}
        }
    }

    /// The stage inputs are known: the topology named every one of them.
    fn stage_known(&self) -> Result<(), String> {
        if !self.topology {
            return Err("the engine sent no topology".to_owned());
        }
        if !self.unknown.is_empty() {
            return Err(format!(
                "stage inputs missing from the engine's topology: {}",
                self.unknown.join(", ")
            ));
        }
        if self.stage.is_empty() {
            return Err("no stage input in the engine's topology".to_owned());
        }
        Ok(())
    }
}

fn lock(inbox: &Mutex<Inbox>) -> MutexGuard<'_, Inbox> {
    inbox.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The reply to `id`, taken out of the inbox: `Some(None)` accepted,
/// `Some(Some(error))` refused, `None` not yet.
fn take_reply(inbox: &mut Inbox, id: u64) -> Option<Option<String>> {
    let at = inbox.replies.iter().position(|(r, _)| *r == id)?;
    Some(inbox.replies.remove(at).1)
}

fn read_loop(stream: &Stream, inbox: &Mutex<Inbox>) {
    let mut r = stream;
    let why = loop {
        match proto::read_frame(&mut r) {
            Ok(Some(body)) => match proto::parse(&body) {
                Ok(msg) => lock(inbox).take(msg, Instant::now()),
                Err(e) => warn!("{e}"),
            },
            Ok(None) => break "the engine closed the supervisor pipe".to_owned(),
            Err(e) => break format!("the supervisor pipe: {e}"),
        }
    };
    info!("supervisor reader ends: {why}");
    let mut inbox = lock(inbox);
    if inbox.closed.is_none() {
        inbox.closed = Some(why);
    }
}

pub(super) struct Supervisor {
    stream: Arc<Stream>,
    inbox: Arc<Mutex<Inbox>>,
    next_id: u64,
}

impl Supervisor {
    fn connect(pipe: &str, stage_ids: Vec<String>) -> Result<Self, String> {
        let name = pipe
            .to_owned()
            .to_ns_name::<GenericNamespaced>()
            .map_err(|e| format!("the engine's pipe name {pipe:?}: {e}"))?;
        let stream =
            Stream::connect(name).map_err(|e| format!("connecting to the engine's pipe: {e}"))?;
        let stream = Arc::new(stream);
        let inbox = Arc::new(Mutex::new(Inbox::new(stage_ids, Instant::now())));
        let (reader, shared) = (Arc::clone(&stream), Arc::clone(&inbox));
        let _reader = thread::Builder::new()
            .name("iemmixer-guard-supervisor".to_owned())
            .spawn(move || read_loop(&reader, &shared))
            .map_err(|e| format!("the supervisor pipe's reader: {e}"))?;
        let sup = Self {
            stream,
            inbox,
            next_id: 1,
        };
        sup.send(&proto::hello())?;
        info!("connected to the engine as its supervisor");
        Ok(sup)
    }

    fn open(&self) -> bool {
        lock(&self.inbox).closed.is_none()
    }

    fn send(&self, msg: &Value) -> Result<(), String> {
        (&*self.stream)
            .write_all(&proto::frame(msg))
            .map_err(|e| format!("the supervisor pipe: {e}"))
    }

    fn fresh_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Sends `op` and waits up to [`REPLY`] for its answer.
    fn request(&mut self, op: &str) -> Result<(), String> {
        let id = self.fresh_id();
        self.exchange(op, id, &proto::request(id, op))
    }

    /// Sends a whole command and waits up to [`REPLY`] for its answer.
    fn command(&mut self, what: &str, cmd: Value) -> Result<(), String> {
        let id = self.fresh_id();
        self.exchange(what, id, &proto::request_cmd(id, cmd))
    }

    fn exchange(&mut self, op: &str, id: u64, msg: &Value) -> Result<(), String> {
        self.send(msg)?;
        let start = Instant::now();
        loop {
            {
                let mut inbox = lock(&self.inbox);
                if let Some(answer) = take_reply(&mut inbox, id) {
                    return match answer {
                        None => Ok(()),
                        Some(e) => Err(format!("{op}: {e}")),
                    };
                }
                if let Some(why) = &inbox.closed {
                    return Err(format!("{op}: {why}"));
                }
            }
            if start.elapsed() >= REPLY {
                return Err(format!("{op}: no answer within {} s", REPLY.as_secs()));
            }
            thread::sleep(INBOX_POLL);
        }
    }
}

/// Waits up to `limit` for `f` to find something in the inbox; a closed
/// pipe ends the wait with its reason, "ide event" with `Preempted`.
fn wait_inbox<T>(
    sup: &Supervisor,
    limit: Duration,
    c: &Cancel,
    mut f: impl FnMut(&mut Inbox) -> Option<T>,
) -> R<Option<T>> {
    let start = Instant::now();
    loop {
        {
            let mut inbox = lock(&sup.inbox);
            if let Some(found) = f(&mut inbox) {
                return Ok(Some(found));
            }
            if let Some(why) = &inbox.closed {
                return Err(StepError::Failed(why.clone()));
            }
        }
        if start.elapsed() >= limit {
            return Ok(None);
        }
        c.sleep(INBOX_POLL)?;
    }
}

/// A status newer than the `seen`-th, with its number.
fn newer_status(inbox: &Inbox, seen: u64) -> Option<(u64, Status)> {
    if inbox.status_seq > seen {
        inbox.status.clone().map(|s| (inbox.status_seq, s))
    } else {
        None
    }
}

/// The supervisor connection, made again when the last one closed; the
/// engine's pipe may appear only a while after a start.
fn supervisor<'a>(pc: &'a mut WinPc, limit: Duration, c: &Cancel) -> R<&'a mut Supervisor> {
    if !pc.sup.as_ref().is_some_and(Supervisor::open) {
        pc.sup = None;
        let start = Instant::now();
        loop {
            match Supervisor::connect(&pc.s.pc.engine_pipe, pc.s.stage_inputs.clone()) {
                Ok(sup) => {
                    pc.sup = Some(sup);
                    break;
                }
                Err(e) if start.elapsed() >= limit => return Err(StepError::Failed(e)),
                Err(_) => c.sleep(procs::POLL)?,
            }
        }
    }
    pc.sup
        .as_mut()
        .ok_or_else(|| StepError::failed("no supervisor connection"))
}

/// The engine from the bundle, with `pc.toml`'s arguments, `--pipe` from
/// `engine_pipe` (the supervisor's and the server's pipe), `--hold` on
/// request and the HIL flags inside a HIL job (`Settings::engine_argv`).
pub(super) fn start(pc: &mut WinPc, hold: bool, hil: bool) -> R<u32> {
    if !procs::list(pc).engine.is_empty() {
        return Err(StepError::failed("an engine already runs"));
    }
    let dir = pc.bundle_dir()?;
    let args =
        pc.s.engine_argv(&dir, hold, hil)
            .map_err(StepError::Failed)?;
    let mut cmd = Command::new(dir.join(ENGINE_EXE));
    cmd.args(args).current_dir(dir);
    pc.sup = None;
    pc.dacl = None;
    procs::start_kid(pc, Kid::Engine, &mut cmd, false)
}

/// `iem-engine interlock` (design §4): a wait, so "ide event" creates its
/// stop file, which it sees within 0.1 s: it releases the card and exits 6
/// (never Ctrl-Break: its default handler would end it with the card open).
/// It opens the card, so it runs outside the guard's job like the engine.
pub(super) fn interlock(pc: &WinPc, seconds: u32, c: &Cancel) -> R<(bool, String)> {
    let dir = pc.bundle_dir()?;
    let guard = pc.s.guard_dir();
    fs::create_dir_all(&guard).map_err(|e| procs::failed("the guard directory", e))?;
    let stop = guard.join(INTERLOCK_STOP);
    match fs::remove_file(&stop) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(procs::failed("the interlock's old stop file", e)),
    }
    let mut cmd = Command::new(dir.join(ENGINE_EXE));
    cmd.arg("interlock")
        .arg("--site")
        .arg(&pc.s.pc.site)
        .arg("--seconds")
        .arg(seconds.to_string())
        .arg("--stop-file")
        .arg(&stop)
        .current_dir(dir);
    let limit = Duration::from_secs(u64::from(seconds) + 30);
    let out = procs::run_outside_job("iem-engine interlock", &mut cmd, limit, c, &stop)?;
    proto::interlock_result(out.code, &out.stdout).map_err(StepError::Failed)
}

pub(super) fn ready(pc: &mut WinPc, secs: u32, c: &Cancel) -> R<Status> {
    let sha = pc
        .bundle
        .clone()
        .ok_or_else(|| StepError::failed("no bundle is active"))?;
    let sup = supervisor(pc, CONNECT_AFTER_START, c)?;
    let build = wait_inbox(sup, HELLO, c, |i| i.build.clone())?
        .ok_or_else(|| StepError::failed("the engine said no hello"))?;
    if !proto::build_matches(&build, &sha) {
        return Err(StepError::failed(format!(
            "the engine is build {build}, not the bundle {sha}"
        )));
    }
    let mut window = ReadyWindow::new(secs);
    let limit = Duration::from_secs(u64::from(secs) * 3 + 30);
    let start = Instant::now();
    let mut seen = 0;
    loop {
        let next = wait_inbox(sup, STATUS_GAP, c, |i| newer_status(i, seen))?;
        let Some((seq, mut status)) = next else {
            return Err(StepError::failed(format!(
                "the engine sent no status for {} s",
                STATUS_GAP.as_secs()
            )));
        };
        seen = seq;
        status.build.clone_from(&build);
        match window.observe(&status, Instant::now()) {
            Ready::Wait => {}
            Ready::Done => {
                info!(
                    "the engine is ready: {} frames, {} callbacks, {} missed",
                    status.frames, status.callbacks, status.missed
                );
                return Ok(status);
            }
            Ready::Failed(why) => return Err(StepError::Failed(why)),
        }
        if start.elapsed() >= limit {
            return Err(StepError::failed(format!(
                "the engine was not ready within {} s",
                limit.as_secs()
            )));
        }
    }
}

pub(super) fn arm(pc: &mut WinPc) -> R<()> {
    let sup = supervisor(pc, CONNECT, &Cancel::default())?;
    sup.request("arm").map_err(StepError::Failed)
}

/// `Shutdown`, `DriverReleased` ≤ 10 s, the process gone ≤ 5 s; a refused
/// `Shutdown` ends the wait at once (`effects::engine::shutdown`).
pub(super) fn stop(pc: &mut WinPc, c: &Cancel) -> R<()> {
    let pid = procs::running_pid(pc, Kid::Engine)?;
    let handle = Handle::open_waitable(pid).map_err(|e| procs::failed("the engine", e))?;
    let sup = supervisor(pc, CONNECT, c)?;
    lock(&sup.inbox).released = None;
    let id = sup.fresh_id();
    sup.send(&proto::request(id, "shutdown"))
        .map_err(StepError::Failed)?;
    let outcome = wait_inbox(sup, RELEASE, c, |i| {
        let reply = take_reply(i, id);
        proto::shutdown(i.released.as_deref(), reply)
    })?;
    let reason = match outcome {
        Some(Shutdown::Released(reason)) => reason,
        Some(Shutdown::Refused(e)) => {
            return Err(StepError::failed(format!(
                "the engine refused Shutdown: {e}"
            )));
        }
        None => {
            return Err(StepError::failed(format!(
                "no DriverReleased within {} s",
                RELEASE.as_secs()
            )));
        }
    };
    info!("the engine released the driver: {reason}");
    // Our end of the supervisor pipe goes now: a client still holding the
    // stream keeps the pipe's name from the next engine (engine.md, Pipes).
    pc.sup = None;
    if procs::wait_exit(&handle, GONE, c)?.is_none() {
        return Err(StepError::failed(format!(
            "the engine released the driver but did not end within {} s",
            GONE.as_secs()
        )));
    }
    pc.dacl = None;
    pc.kids.forget(Kid::Engine);
    Ok(())
}

/// Two statuses about 1 s apart (design §5.2 "back to event" step 2).
pub(super) fn health(pc: &mut WinPc) -> R<Health> {
    let c = Cancel::default();
    let sup = supervisor(pc, CONNECT, &c)?;
    let (seq, first) = wait_inbox(sup, STATUS_GAP, &c, |i| newer_status(i, 0))?
        .ok_or_else(|| StepError::failed("the engine sent no status"))?;
    let (_, second) = wait_inbox(sup, STATUS_GAP, &c, |i| newer_status(i, seq))?
        .ok_or_else(|| StepError::failed("the engine sent no second status"))?;
    Ok(proto::health(&first, &second))
}

/// The loudest peak of each stage input over `seconds`, from the engine's
/// meters (no card reopen).
pub(super) fn stage_peaks(pc: &mut WinPc, seconds: u32, c: &Cancel) -> R<Vec<f64>> {
    let sup = supervisor(pc, CONNECT, c)?;
    wait_inbox(sup, HELLO, c, |i| i.topology.then_some(()))?;
    {
        let mut inbox = lock(&sup.inbox);
        inbox.stage_known().map_err(StepError::Failed)?;
        let inputs = inbox.stage.len();
        inbox.peaks.reset(inputs);
    }
    c.sleep(Duration::from_secs(u64::from(seconds)))?;
    let inbox = lock(&sup.inbox);
    if let Some(why) = &inbox.closed {
        return Err(StepError::Failed(why.clone()));
    }
    if inbox.peaks.frames() == 0 {
        return Err(StepError::failed(format!("no meter frame in {seconds} s")));
    }
    Ok(inbox.peaks.loudest().to_vec())
}

/// How long the stage inputs have been below the band-activity level, as
/// far as this connection saw (a new one starts from zero).
pub(super) fn quiet_for(pc: &mut WinPc) -> R<Duration> {
    let c = Cancel::default();
    let sup = supervisor(pc, CONNECT, &c)?;
    wait_inbox(sup, HELLO, &c, |i| i.topology.then_some(()))?;
    let inbox = lock(&sup.inbox);
    inbox.stage_known().map_err(StepError::Failed)?;
    Ok(inbox.quiet.quiet_for(Instant::now()))
}

/// The HIL test signal (design §4, §7): `HilTestSignal`, encoded only on
/// `card_tx`; the engine refuses it above its test-signal cap.
pub(super) fn hil_signal(
    pc: &mut WinPc,
    input: &str,
    dbfs: f64,
    ttl_s: f64,
    card_tx: &[u16],
) -> R<()> {
    let sup = supervisor(pc, CONNECT, &Cancel::default())?;
    sup.command(
        "hil_test_signal",
        proto::hil_test_signal(input, dbfs, ttl_s, card_tx),
    )
    .map_err(StepError::Failed)
}

/// A forced driver reopen (HIL, design §7); the engine's reset budget
/// applies.
pub(super) fn force_reopen(pc: &mut WinPc) -> R<()> {
    let sup = supervisor(pc, CONNECT, &Cancel::default())?;
    sup.request("force_reopen").map_err(StepError::Failed)
}

/// HIL's RT panic (design §7): `InjectFault` over the supervisor pipe. The
/// engine refuses it without its fault-injection flag; with it the RT
/// callback faults and the engine exits 70, which the watch sees.
pub(super) fn inject_fault(pc: &mut WinPc) -> R<()> {
    let sup = supervisor(pc, CONNECT, &Cancel::default())?;
    sup.request("inject_fault").map_err(StepError::Failed)
}

/// Whether the engine's control pipe admits only this user and SYSTEM, read
/// back through its DACL (HIL, design §7). Reading connects to the pipe for
/// a moment, so it is read once per engine process.
fn pipe_private(pipe: &str) -> Result<bool, String> {
    let user = iem_win::token::current_user_sid().map_err(|e| format!("the user's SID: {e}"))?;
    let user = iem_win::token::sddl_sid(&user).map_err(|e| format!("the user's SDDL name: {e}"))?;
    let sddl = iem_win::token::pipe_sddl(pipe).map_err(|e| format!("the pipe's DACL: {e}"))?;
    Ok(iem_win::token::sddl_is_private(&sddl, &user))
}

/// What the supervisor connection holds of our running engine (the guard's
/// `Reply.engine`): one attempt to connect when there is no connection,
/// never a wait.
pub(super) fn seen(pc: &mut WinPc) -> Option<EngineSeen> {
    let pid = pc.kids.pid(Kid::Engine)?;
    if !pc.sup.as_ref().is_some_and(Supervisor::open) {
        pc.sup = Supervisor::connect(&pc.s.pc.engine_pipe, pc.s.stage_inputs.clone()).ok();
    }
    let sup = pc.sup.as_ref()?;
    // Read once per engine process, and again at the next look after a
    // failed read.
    let cached = pc.dacl;
    let private = match cached {
        Some((of, private)) if of == pid => private,
        _ => match pipe_private(&pc.s.pc.engine_pipe) {
            Ok(private) => {
                pc.dacl = Some((pid, private));
                private
            }
            Err(why) => {
                warn!("the engine pipe's DACL: {why}");
                false
            }
        },
    };
    let inbox = lock(&sup.inbox);
    let mut status = inbox.status.clone().unwrap_or_default();
    status.build = inbox.build.clone().unwrap_or_default();
    Some(EngineSeen {
        status,
        pipe_private: private,
    })
}

/// `iem-engine check-site --site <file>` from the bundle: its report. A
/// read-only wait: "ide event" ends it at once (Ctrl-Break to its own
/// process group; the command ends by itself).
fn check_site(pc: &WinPc, site: &Path, c: &Cancel) -> R<String> {
    let dir = pc.bundle_dir()?;
    let mut cmd = Command::new(dir.join(ENGINE_EXE));
    cmd.arg("check-site")
        .arg("--site")
        .arg(site)
        .current_dir(&dir);
    let out = procs::run(
        "iem-engine check-site",
        &mut cmd,
        CHECK_SITE,
        c,
        procs::OnCancel::Break,
    )?;
    proto::check_site_result(out.code, &out.stdout, &out.stderr).map_err(StepError::Failed)
}

/// F30 (design §7): the new site must keep the guard's own tables (the
/// guard reads them at every start: a site it cannot load would leave only
/// a reboot as the way back to REAPER) and pass `check-site`; then it
/// replaces the site file (atomically). The guard's own settings are read
/// again at its next start. The checks are waits "ide event" ends.
pub(super) fn install_site(pc: &WinPc, path: &str, c: &Cancel) -> R<String> {
    let new = Path::new(path);
    let text = fs::read_to_string(new).map_err(|e| procs::failed(path, e))?;
    pc.s.check_new_site(&text)
        .map_err(|why| StepError::Failed(format!("the guard's tables: {why}")))?;
    let now = check_site(pc, new, c)?;
    let site = pc.s.pc.site.clone();
    let before = match check_site(pc, &site, c) {
        Ok(report) => report,
        Err(StepError::Preempted) => return Err(StepError::Preempted),
        Err(e) => format!("unreadable ({e})"),
    };
    state::write_atomic(&site, text.as_bytes())
        .map_err(|e| procs::failed(&site.display().to_string(), e))?;
    info!("the site {} replaced {}", new.display(), site.display());
    Ok(format!("site before: {before}; site now: {now}"))
}
