//! The engine (design §4, §5.2): its start and the supervisor pipe. One
//! connection says hello as `supervisor`; a reader thread keeps what the
//! engine sends (`Hello`, `Status`, replies, `DriverReleased` or
//! `DriverParked`; its meters are
//! read by nobody here, #38) so the engine never waits on the guard, and the
//! steps read that inbox. The guard never waits long on the
//! engine either: every send must be taken within [`SEND`], else it fails,
//! the connection counts as closed and the step fails into the plan's error
//! policy (a hung engine at "ide event": `EngineStop`, then `EngineHealth`).
//! The engine ends only by its own `Shutdown`.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::os::windows::io::AsHandle;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use iem_win::process::Handle;
use interprocess::ConnectWaitMode;
use interprocess::local_socket::Stream;
use interprocess::os::windows::named_pipe::local_socket::Stream as PipeStream;
use interprocess::os::windows::named_pipe::{DuplexPipeStream, pipe_mode};
use serde_json::Value;
use tracing::{info, warn};

use super::{WinPc, procs};
use crate::cancel::Cancel;
use crate::effects::engine::{self as proto, Msg, Ready, ReadyWindow, Shutdown, Stopped};
use crate::pc::{EngineSeen, Kid, R, Status, StepError};
use crate::plan::Health;
use crate::site::ENGINE_EXE;
use crate::state;

/// After a start the engine's pipe appears within this.
const CONNECT_AFTER_START: Duration = Duration::from_secs(30);
/// A running engine's pipe answers within this.
const CONNECT: Duration = Duration::from_secs(5);
/// One attempt to connect waits at most this for a free pipe instance: a
/// pipe whose acceptor is stuck fails the attempt (`seen` makes one per
/// look, `supervisor` repeats them until its step's limit).
const CONNECT_ATTEMPT: Duration = Duration::from_millis(200);
/// `Hello` follows the guard's hello at once.
const HELLO: Duration = Duration::from_secs(10);
/// `Status` comes once a second.
const STATUS_GAP: Duration = Duration::from_secs(3);
const REPLY: Duration = Duration::from_secs(5);
/// `iem-engine check-site` loads and compiles a site in well under this.
const CHECK_SITE: Duration = Duration::from_secs(60);
/// `Shutdown` → `DriverReleased` or `DriverParked` (design §5.2, #35), then
/// the process ends.
const RELEASE: Duration = Duration::from_secs(10);
/// The engine takes a sent frame within this (its readers look every
/// 10 ms), or the send fails (`crate::pipe::Bounded`): a hung engine never
/// holds the daemon thread in a write, and "ide event" waits at most this
/// for a send of the step it pre-empts.
const SEND: Duration = Duration::from_secs(1);
const GONE: Duration = Duration::from_secs(5);
const INBOX_POLL: Duration = Duration::from_millis(50);
const MAX_REPLIES: usize = 64;

/// What the reader thread kept.
#[derive(Debug, Default)]
struct Inbox {
    build: Option<String>,
    status: Option<Status>,
    /// Counts the statuses, so a step reads only newer ones.
    status_seq: u64,
    replies: Vec<(u64, Option<String>)>,
    /// The stream's stop (`DriverReleased` or `DriverParked`).
    stopped: Option<Stopped>,
    closed: Option<String>,
}

impl Inbox {
    fn take(&mut self, msg: Msg) {
        match msg {
            Msg::Hello { build } => self.build = Some(build),
            Msg::Status(s) => {
                self.status = Some(s);
                self.status_seq += 1;
            }
            Msg::Reply { id, error } => {
                self.replies.push((id, error));
                if self.replies.len() > MAX_REPLIES {
                    self.replies.remove(0);
                }
            }
            Msg::DriverReleased { reason } => self.stopped = Some(Stopped::Released(reason)),
            Msg::DriverParked { reason } => self.stopped = Some(Stopped::Parked(reason)),
            Msg::Superseded => {
                self.closed = Some("another supervisor took the engine's pipe".to_owned());
            }
            Msg::Other => {}
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
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
                Ok(msg) => lock(inbox).take(msg),
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

/// The engine's pipe `\\.\pipe\<pipe>` (what `GenericNamespaced` names on
/// Windows, as the engine listens), waiting at most [`CONNECT_ATTEMPT`] for a
/// free instance. interprocess's local-socket connect waits for one without
/// a limit (in 2.4 it passes no `ConnectOptions::wait_mode` to a named
/// pipe: `named_pipe/local_socket/stream.rs`), so the pipe is opened with
/// the named-pipe connect and its own wait mode.
fn connect_pipe(pipe: &str) -> io::Result<Stream> {
    let path = OsString::from(format!(r"\\.\pipe\{pipe}"));
    let stream = DuplexPipeStream::<pipe_mode::Bytes>::connect_by_path_with_wait_mode(
        path,
        ConnectWaitMode::Timeout(CONNECT_ATTEMPT),
    )?;
    Ok(Stream::from(PipeStream::from(stream)))
}

impl Supervisor {
    /// One attempt, at most [`CONNECT_ATTEMPT`] on a busy pipe.
    fn connect(pipe: &str) -> Result<Self, String> {
        let stream =
            connect_pipe(pipe).map_err(|e| format!("connecting to the engine's pipe: {e}"))?;
        let stream = Arc::new(stream);
        let inbox = Arc::new(Mutex::new(Inbox::default()));
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

    /// The process serving this connection's pipe instance
    /// (`GetNamedPipeServerProcessId` on our end, S7 HIL v2): the engine
    /// that created the pipe and accepted us.
    fn server_pid(&self) -> io::Result<u32> {
        let Stream::NamedPipe(pipe) = &*self.stream;
        iem_win::pipe::server_pid(pipe.inner().as_handle())
    }

    /// One frame to the engine, which must take it within [`SEND`]
    /// (`crate::pipe::Bounded`, never `&*self.stream`: interprocess's own
    /// writes wait for the engine without a bound). A failed send may have
    /// left part of a frame in the pipe, so the connection counts as closed
    /// from then on: the next step connects again.
    fn send(&self, msg: &Value) -> Result<(), String> {
        let sent = crate::pipe::Bounded::new(&self.stream, SEND).write_all(&proto::frame(msg));
        sent.map_err(|e| {
            let why = format!("the supervisor pipe: {e}");
            let mut inbox = lock(&self.inbox);
            if inbox.closed.is_none() {
                inbox.closed = Some(why.clone());
            }
            why
        })
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

/// Keeps the fault time of the supervisor connection's newest status in
/// `WinPc::last_fault` (S7 HIL v2, `effects::engine::kept_fault`: a faulted
/// status replaces it, none included): at every look, and before the
/// connection is dropped or replaced ([`drop_supervisor`]), since a faulted
/// engine's last `Status` (its faulting callback's time) is in that
/// connection's inbox; so the respawned engine's reply still carries it.
fn keep_fault(pc: &mut WinPc) {
    if let Some(sup) = pc.sup.as_ref() {
        let inbox = lock(&sup.inbox);
        pc.last_fault = proto::kept_fault(pc.last_fault, inbox.status.as_ref());
    }
}

/// Drops the supervisor connection, its fault time kept first
/// ([`keep_fault`]): every place that drops or replaces the connection goes
/// through here.
pub(super) fn drop_supervisor(pc: &mut WinPc) {
    keep_fault(pc);
    pc.sup = None;
}

/// The supervisor connection, made again when the last one closed; the
/// engine's pipe may appear only a while after a start.
fn supervisor<'a>(pc: &'a mut WinPc, limit: Duration, c: &Cancel) -> R<&'a mut Supervisor> {
    if !pc.sup.as_ref().is_some_and(Supervisor::open) {
        drop_supervisor(pc);
        let start = Instant::now();
        loop {
            match Supervisor::connect(&pc.s.pc.engine_pipe) {
                Ok(sup) => {
                    pc.sup = Some(sup);
                    break;
                }
                Err(e) if start.elapsed() >= limit => return Err(StepError::Failed(e)),
                Err(e) => {
                    // An engine of ours that ended never answers: its end
                    // ends the wait at once (#32 F3-r4 4: the step starts
                    // one that ended with exit 75 again).
                    if let Some(code) = pc.kids.ended(Kid::Engine) {
                        return Err(StepError::failed(format!(
                            "the engine ended ({code:?}) before its pipe answered: {e}"
                        )));
                    }
                    c.sleep(procs::POLL)?;
                }
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
    drop_supervisor(pc);
    pc.dacl = None;
    procs::start_kid(pc, Kid::Engine, &mut cmd, false)
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
/// `Shutdown` ends the wait at once (`effects::engine::shutdown`). A stream
/// that stopped parked (`DriverParked`, #35) released nothing: the card is
/// free once the process has ended, which the same wait reads.
pub(super) fn stop(pc: &mut WinPc, c: &Cancel) -> R<()> {
    let pid = procs::running_pid(pc, Kid::Engine)?;
    let handle = Handle::open_waitable(pid).map_err(|e| procs::failed("the engine", e))?;
    let sup = supervisor(pc, CONNECT, c)?;
    lock(&sup.inbox).stopped = None;
    let id = sup.fresh_id();
    sup.send(&proto::request(id, "shutdown"))
        .map_err(StepError::Failed)?;
    let outcome = wait_inbox(sup, RELEASE, c, |i| {
        let reply = take_reply(i, id);
        proto::shutdown(i.stopped.as_ref(), reply)
    })?;
    let stopped = match outcome {
        Some(Shutdown::Stopped(stopped)) => stopped,
        Some(Shutdown::Refused(e)) => {
            return Err(StepError::failed(format!(
                "the engine refused Shutdown: {e}"
            )));
        }
        None => {
            return Err(StepError::failed(format!(
                "no DriverReleased or DriverParked within {} s",
                RELEASE.as_secs()
            )));
        }
    };
    match &stopped {
        Stopped::Released(_) => info!("{}", stopped.note()),
        Stopped::Parked(_) => warn!("{}", stopped.note()),
    }
    // Our end of the supervisor pipe goes now: this engine is going, and
    // the next step connects to the next one.
    drop_supervisor(pc);
    if procs::wait_exit(&handle, GONE, c)?.is_none() {
        return Err(StepError::failed(stopped.not_ended(GONE)));
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

/// The HIL test signal (design §4, §7): `HilTestSignal`, encoded only on
/// `card_tx`, with `listen` also the listen probe (S7, #10); the engine
/// refuses it above its test-signal cap.
pub(super) fn hil_signal(
    pc: &mut WinPc,
    input: &str,
    dbfs: f64,
    ttl_s: f64,
    card_tx: &[u16],
    listen: bool,
) -> R<()> {
    let sup = supervisor(pc, CONNECT, &Cancel::default())?;
    sup.command(
        "hil_test_signal",
        proto::hil_test_signal(input, dbfs, ttl_s, card_tx, listen),
    )
    .map_err(StepError::Failed)
}

/// One supervisor command without fields (`op`), its answer awaited: HIL's
/// forced reopen and the fault injections.
fn bare_request(pc: &mut WinPc, op: &str) -> R<()> {
    let sup = supervisor(pc, CONNECT, &Cancel::default())?;
    sup.request(op).map_err(StepError::Failed)
}

/// A forced driver reopen (HIL, design §7); the engine's reset budget
/// applies.
pub(super) fn force_reopen(pc: &mut WinPc) -> R<()> {
    bare_request(pc, "force_reopen")
}

/// HIL's RT panic (design §7): `InjectFault` over the supervisor pipe. The
/// engine refuses it without its fault-injection flag; with it the RT
/// callback faults and the engine exits 70, which the watch sees.
pub(super) fn inject_fault(pc: &mut WinPc) -> R<()> {
    bare_request(pc, "inject_fault")
}

/// The owner-approved SEH test (design §10 test #4): `InjectSeh` over the
/// supervisor pipe. The engine refuses it without its fault-injection flag;
/// with it the RT callback raises a structured exception, the SEH filter
/// releases the driver or parks, and the engine exits, which the watch sees.
pub(super) fn inject_seh(pc: &mut WinPc) -> R<()> {
    bare_request(pc, "inject_seh")
}

/// The parked-engine test (design §10 test #2, #35): `InjectPark` over the
/// supervisor pipe. The engine refuses it without its fault-injection flag;
/// with it the RT callback raises the SEH test's exception under the
/// backend's test hold: the driver is kept, the SEH filter parks the RT
/// thread, and the engine keeps running with its stream parked (its next
/// `Status`), so the watch sees no exit.
pub(super) fn inject_park(pc: &mut WinPc) -> R<()> {
    bare_request(pc, "inject_park")
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
/// never a wait. `None` until the connection holds the engine's hello and
/// a `Status` (`effects::engine::seen_status`): an engine coming up is
/// absent from the reply, not zeroed. The last fault the guard kept
/// (`keep_fault`) goes with it, also once the engine was respawned.
pub(super) fn seen(pc: &mut WinPc) -> Option<EngineSeen> {
    let pid = pc.kids.pid(Kid::Engine)?;
    if !pc.sup.as_ref().is_some_and(Supervisor::open) {
        drop_supervisor(pc);
        pc.sup = Supervisor::connect(&pc.s.pc.engine_pipe).ok();
    }
    keep_fault(pc);
    let sup = pc.sup.as_ref()?;
    let status = {
        let inbox = lock(&sup.inbox);
        proto::seen_status(inbox.build.as_deref(), inbox.status.as_ref())?
    };
    // Read once per engine process, and again at the next look after a
    // failed read: the DACL (a moment's connection to the pipe) and the
    // pipe's server (a call on our own end of it).
    let (private, server) = match pc.dacl {
        Some((of, private, server)) if of == pid => (Some(private), server),
        _ => (None, None),
    };
    let private = private.or_else(|| match pipe_private(&pc.s.pc.engine_pipe) {
        Ok(private) => Some(private),
        Err(why) => {
            warn!("the engine pipe's DACL: {why}");
            None
        }
    });
    let server = server.or_else(|| match sup.server_pid() {
        Ok(server) => Some(server),
        Err(e) => {
            warn!("the engine pipe's server process: {e}");
            None
        }
    });
    if let Some(private) = private {
        pc.dacl = Some((pid, private, server));
    }
    Some(EngineSeen {
        status,
        pipe_private: private.unwrap_or(false),
        pipe_server_pid: server,
        last_fault_us: pc.last_fault,
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

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use interprocess::local_socket::prelude::*;
    use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Stream};

    use super::Supervisor;

    /// An engine pipe with no free instance (its acceptor stuck): an
    /// attempt to connect as its supervisor fails within a bound instead of
    /// waiting for an instance forever, so neither a guard tick (`seen`)
    /// nor a step holds the daemon thread, and a queued "ide event" behind
    /// it (lane review). On the CI's Windows runner.
    #[test]
    fn a_busy_engine_pipe_fails_the_connect_within_a_bound() {
        let pipe = format!("iemmixer-guard-test-busy-{}", std::process::id());
        let name = || pipe.clone().to_ns_name::<GenericNamespaced>().unwrap();
        let listener = ListenerOptions::new().name(name()).create_sync().unwrap();
        // The listener's one instance is taken and never accepted: the next
        // client finds no free instance (ERROR_PIPE_BUSY).
        let first = Stream::connect(name()).unwrap();
        let (tx, rx) = mpsc::channel();
        let busy = pipe.clone();
        thread::spawn(move || {
            let start = Instant::now();
            let r = Supervisor::connect(&busy).map(|_| ());
            let _ = tx.send((r, start.elapsed()));
        });
        let (r, took) = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the connect to a busy pipe returned within 5 s");
        assert!(r.is_err(), "{r:?}");
        assert!(took < Duration::from_secs(2), "{took:?}");
        drop(first);
        drop(listener);
        // No pipe at all fails at once.
        let start = Instant::now();
        assert!(Supervisor::connect(&format!("{pipe}-none")).is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    /// An engine that stopped reading (hung): a supervisor send to it fails
    /// within a bound instead of holding the daemon thread for good (the
    /// "ide event" plan sends `Shutdown` this way, and its error policy
    /// takes a failed `EngineStop` from there), and the connection counts
    /// as closed, so the next step connects again. On the CI's Windows
    /// runner.
    #[test]
    fn a_send_to_an_engine_that_does_not_read_fails_within_a_bound() {
        let pipe = format!("iemmixer-guard-test-mute-{}", std::process::id());
        let name = pipe.clone().to_ns_name::<GenericNamespaced>().unwrap();
        // The engine's end: its one instance takes the connection and
        // nothing ever reads it.
        let listener = ListenerOptions::new().name(name).create_sync().unwrap();
        // The hello fits the pipe's 512 bytes.
        let sup = Supervisor::connect(&pipe).unwrap();
        assert!(sup.open());
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            // Larger than the pipe: it waits for a read that never comes.
            let big = serde_json::json!({
                "type": "request",
                "id": 1,
                "cmd": {"op": "x".repeat(4096)},
            });
            let start = Instant::now();
            let sent = sup.send(&big);
            let _ = tx.send((sent, start.elapsed(), sup.open()));
        });
        let (sent, took, open) = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a send to an engine that does not read returned within 5 s");
        let why = sent.unwrap_err();
        assert!(why.starts_with("the supervisor pipe: "), "{why}");
        assert!(took < Duration::from_secs(3), "{took:?}");
        assert!(!open, "a failed send leaves the connection open");
        drop(listener);
    }
}
