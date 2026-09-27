//! Processes (S6 design note §5.1, §5.5): the once-a-second list, our
//! children (started outside the guard's job on consoles of their own,
//! adopted after a guard restart, watched through handles), stops by
//! request (Ctrl-Break on the child's own console, the tray's Quit) and
//! bounded helper commands. Nothing here ends a process.

use std::fmt::Display;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use iem_win::console;
use iem_win::process::{self, Handle};
use iem_win::spawn;
use tracing::{info, warn};

use super::{WinPc, web};
use crate::cancel::Cancel;
use crate::effects::app::one_pid;
use crate::effects::{argv, web as decide};
use crate::pc::{Audience, Kid, Procs, R, StepError, adoptable};
use crate::plan::Mode;
use crate::site::{SERVER_EXE, TRAY_EXE};
use crate::state::{Child as Record, Children};

/// How often a waiting step looks again (well inside the 1 s pre-emption).
pub(super) const POLL: Duration = Duration::from_millis(250);

pub(super) fn failed(what: &str, e: impl Display) -> StepError {
    StepError::Failed(format!("{what}: {e}"))
}

/// Milliseconds since the Unix epoch.
pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// `%SystemRoot%\System32\<name>`: Windows' own tools by their full path.
pub(super) fn system_exe(name: &str) -> PathBuf {
    std::env::var_os("SystemRoot")
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
        .join("System32")
        .join(name)
}

/// The process list (one snapshot); an unreadable list is empty (logged).
pub(super) fn list(pc: &WinPc) -> Procs {
    match process::list() {
        Ok(all) => Procs::from_list(&all, &pc.images),
        Err(e) => {
            warn!("the process list could not be read: {e}");
            Procs::default()
        }
    }
}

/// Looks at `check` every [`POLL`] until it holds (true) or `limit` has
/// passed (false); "ide event" ends the wait.
pub(super) fn poll(limit: Duration, c: &Cancel, mut check: impl FnMut() -> R<bool>) -> R<bool> {
    let start = Instant::now();
    loop {
        if check()? {
            return Ok(true);
        }
        if start.elapsed() >= limit {
            return Ok(false);
        }
        c.sleep(POLL)?;
    }
}

/// Waits up to `limit` for the process behind `handle` to end, in slices of
/// [`Cancel::SLICE`]: its exit code, or `None` while it still runs.
pub(super) fn wait_exit(handle: &Handle, limit: Duration, c: &Cancel) -> R<Option<u32>> {
    let start = Instant::now();
    loop {
        if let Some(code) = handle
            .wait(Cancel::SLICE)
            .map_err(|e| failed("waiting for a process", e))?
        {
            return Ok(Some(code));
        }
        if c.preempted() {
            return Err(StepError::Preempted);
        }
        if start.elapsed() >= limit {
            return Ok(None);
        }
    }
}

#[derive(Debug)]
struct Tracked {
    record: Record,
    handle: Handle,
}

/// Our children, each watched through a handle opened when it started or
/// was adopted, so a recycled pid never answers for it.
#[derive(Debug, Default)]
pub(super) struct Kids {
    engine: Option<Tracked>,
    server: Option<Tracked>,
    tray: Option<Tracked>,
    runner: Option<Tracked>,
}

impl Kids {
    fn slot(&mut self, kid: Kid) -> &mut Option<Tracked> {
        match kid {
            Kid::Engine => &mut self.engine,
            Kid::Server => &mut self.server,
            Kid::Tray => &mut self.tray,
            Kid::Runner => &mut self.runner,
        }
    }

    fn get(&self, kid: Kid) -> Option<&Tracked> {
        match kid {
            Kid::Engine => self.engine.as_ref(),
            Kid::Server => self.server.as_ref(),
            Kid::Tray => self.tray.as_ref(),
            Kid::Runner => self.runner.as_ref(),
        }
    }

    pub(super) fn pid(&self, kid: Kid) -> Option<u32> {
        self.get(kid).map(|t| t.record.pid)
    }

    /// Stops watching a child that ended on our request.
    pub(super) fn forget(&mut self, kid: Kid) {
        *self.slot(kid) = None;
    }

    /// Watches a child just started: a handle, its start time and image.
    fn track(&mut self, kid: Kid, child: Child) -> R<u32> {
        let pid = child.id();
        let handle = Handle::open_waitable(pid).map_err(|e| failed(kid.id(), e))?;
        let start_time = process::start_time(pid).map_err(|e| failed(kid.id(), e))?;
        let image = process::image_path(pid).map_err(|e| failed(kid.id(), e))?;
        *self.slot(kid) = Some(Tracked {
            record: Record {
                pid,
                start_time,
                image,
            },
            handle,
        });
        info!("started the {} (pid {pid})", kid.id());
        Ok(pid)
    }

    /// Adopts a child a previous guard started. The handle comes first, so
    /// the pid cannot be recycled while its image and start time are read.
    pub(super) fn adopt(&mut self, kid: Kid, saved: &Record) -> bool {
        let Ok(handle) = Handle::open_waitable(saved.pid) else {
            return false;
        };
        let same = match (
            process::image_path(saved.pid),
            process::start_time(saved.pid),
        ) {
            (Ok(image), Ok(start)) => adoptable(saved, &image, start),
            _ => false,
        };
        if same {
            *self.slot(kid) = Some(Tracked {
                record: saved.clone(),
                handle,
            });
            info!("adopted the {} (pid {})", kid.id(), saved.pid);
        }
        same
    }

    pub(super) fn records(&self) -> Children {
        let record = |kid: Kid| self.get(kid).map(|t| t.record.clone());
        Children {
            engine: record(Kid::Engine),
            server: record(Kid::Server),
            tray: record(Kid::Tray),
            runner: record(Kid::Runner),
        }
    }

    /// Our children that ended since the last look, with their exit codes;
    /// they are no longer watched.
    pub(super) fn reap(&mut self) -> Vec<(Kid, Option<i32>)> {
        let mut out = Vec::new();
        for kid in Kid::ALL {
            let slot = self.slot(kid);
            let ended = match slot.as_ref().map(|t| t.handle.wait(Duration::ZERO)) {
                Some(Ok(Some(code))) => Some(Some(i32::from_ne_bytes(code.to_ne_bytes()))),
                Some(Err(e)) => {
                    warn!("watching the {} failed: {e}", kid.id());
                    None
                }
                Some(Ok(None)) | None => None,
            };
            if let Some(code) = ended {
                *slot = None;
                out.push((kid, code));
            }
        }
        out
    }
}

/// The pid of `kid`: the watched child's, else the one the list shows.
pub(super) fn running_pid(pc: &WinPc, kid: Kid) -> R<u32> {
    match pc.kids.pid(kid) {
        Some(pid) => Ok(pid),
        None => one_pid(list(pc).of(kid), kid.id()).map_err(StepError::Failed),
    }
}

/// Starts `kid` outside the guard's job on a console of its own (design
/// §5.1, §5.5), its output appended to `logs\<kid>.log`, and watches it.
pub(super) fn start_kid(pc: &mut WinPc, kid: Kid, cmd: &mut Command, new_group: bool) -> R<u32> {
    let dir = pc.s.logs_dir();
    fs::create_dir_all(&dir).map_err(|e| failed("the log directory", e))?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{}.log", kid.id())))
        .map_err(|e| failed("the log file", e))?;
    let err = log.try_clone().map_err(|e| failed("the log file", e))?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err));
    let child = spawn::spawn_detached(cmd, new_group)
        .map_err(|e| failed(&format!("starting the {}", kid.id()), e))?;
    pc.kids.track(kid, child)
}

/// Starts a program of the band's system directly (`[guard] start_direct`,
/// design §5.1), outside the guard's job, in its own directory. The guard
/// does not watch it: the handover checks do.
pub(super) fn start_detached(exe: &Path) -> R<()> {
    let mut cmd = Command::new(exe);
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = spawn::spawn_detached(&mut cmd, false)
        .map_err(|e| failed(&format!("starting {}", exe.display()), e))?;
    info!("started {} (pid {})", exe.display(), child.id());
    Ok(())
}

/// Asks `kid` to stop with Ctrl-Break on its own console (design §5.5) and
/// waits up to `limit` for it to end.
fn stop_by_break(pc: &mut WinPc, kid: Kid, limit: Duration, c: &Cancel) -> R<()> {
    let pid = running_pid(pc, kid)?;
    let handle = Handle::open_waitable(pid).map_err(|e| failed(kid.id(), e))?;
    let gone = handle
        .wait(Duration::ZERO)
        .map_err(|e| failed(kid.id(), e))?
        .is_some();
    if !gone {
        console::ctrl_break(pid)
            .map_err(|e| failed(&format!("Ctrl-Break to the {}", kid.id()), e))?;
        if wait_exit(&handle, limit, c)?.is_none() {
            return Err(StepError::failed(format!(
                "the {} did not end within {} s of Ctrl-Break",
                kid.id(),
                limit.as_secs()
            )));
        }
    }
    info!("the {} ended", kid.id());
    pc.kids.forget(kid);
    Ok(())
}

pub(super) fn server_start(pc: &mut WinPc, mode: Mode) -> R<u32> {
    let run_mode = decide::server_mode(mode).map_err(StepError::Failed)?;
    let config = pc.s.pc.server_config.clone();
    let text = fs::read_to_string(&config).map_err(|e| failed(&config.display().to_string(), e))?;
    decide::pins_frozen(&text).map_err(StepError::Failed)?;
    let dir = pc.bundle_dir()?;
    let mut cmd = Command::new(dir.join(SERVER_EXE));
    cmd.env("IEMMIXER_CONFIG", &config)
        .env("IEMMIXER_MODE", run_mode)
        .env("IEMMIXER_ENGINE_PIPE", &pc.s.pc.engine_pipe);
    if let Some(config_dir) = config.parent() {
        cmd.current_dir(config_dir);
    }
    start_kid(pc, Kid::Server, &mut cmd, true)
}

/// Ctrl-Break, gone ≤ 10 s, then ports 80/443 free.
pub(super) fn server_stop(pc: &mut WinPc, c: &Cancel) -> R<()> {
    stop_by_break(pc, Kid::Server, Duration::from_secs(10), c)?;
    let (http, https) = web::ports()?;
    if http.is_some() || https.is_some() {
        return Err(StepError::failed(format!(
            "ports 80/443 are still held (pids {http:?}, {https:?})"
        )));
    }
    Ok(())
}

pub(super) fn tray_start(pc: &mut WinPc) -> R<()> {
    let dir = pc.bundle_dir()?;
    let mut cmd = Command::new(dir.join(TRAY_EXE));
    cmd.current_dir(dir);
    start_kid(pc, Kid::Tray, &mut cmd, false).map(|_| ())
}

/// `Quit` over the guard pipe, then gone ≤ 10 s.
pub(super) fn tray_stop(pc: &mut WinPc, c: &Cancel) -> R<()> {
    let pid = running_pid(pc, Kid::Tray)?;
    let handle = Handle::open_waitable(pid).map_err(|e| failed("the tray", e))?;
    let Some(quit) = pc.tray_quit.as_mut() else {
        return Err(StepError::failed(
            "no route to the tray's Quit (the guard pipe)",
        ));
    };
    quit().map_err(|e| StepError::failed(format!("asking the tray to quit: {e}")))?;
    if wait_exit(&handle, Duration::from_secs(10), c)?.is_none() {
        return Err(StepError::failed("the tray did not quit within 10 s"));
    }
    pc.kids.forget(Kid::Tray);
    Ok(())
}

pub(super) fn runner_start(pc: &mut WinPc) -> R<()> {
    let base = pc.bundle_dir().unwrap_or_else(|_| pc.s.pc.root.clone());
    let command = argv::expand(&pc.s.pc.runner, &pc.s.vars(&base)).map_err(StepError::Failed)?;
    let Some((exe, args)) = command.split_first() else {
        return Err(StepError::failed("pc.toml: runner is empty"));
    };
    let mut cmd = Command::new(exe);
    cmd.args(args).current_dir(&pc.s.pc.runner_dir);
    start_kid(pc, Kid::Runner, &mut cmd, true).map(|_| ())
}

/// Ctrl-Break (the daemon stops only an idle runner), gone ≤ 30 s.
pub(super) fn runner_stop(pc: &mut WinPc, c: &Cancel) -> R<()> {
    stop_by_break(pc, Kid::Runner, Duration::from_secs(30), c)
}

pub(super) fn notify(pc: &WinPc, audience: Audience, title: &str, body: &str) -> R<()> {
    let mut cmd = Command::new(pc.bundle_dir()?.join(SERVER_EXE));
    cmd.args(["notify", "--to", audience.arg(), title, body])
        .env("IEMMIXER_CONFIG", &pc.s.pc.server_config);
    let out = run(
        "iem-server notify",
        &mut cmd,
        Duration::from_secs(30),
        &Cancel::default(),
        OnCancel::Finish,
    )?;
    decide::notify_result(out.code, &out.stderr).map_err(StepError::Failed)
}

/// What a helper command printed and how it ended.
#[derive(Debug)]
pub(super) struct Output {
    pub(super) code: Option<i32>,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

/// What a bounded run does on "ide event".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OnCancel<'a> {
    /// A mutation (a data command, a task start, a notice): it finishes
    /// first, the token is not looked at.
    Finish,
    /// A wait (an HTTPS check): Ctrl-Break to the command's own process
    /// group, then `Preempted` at once; the command ends by itself.
    Break,
    /// A wait that holds the card (`iem-engine interlock`): the guard creates
    /// this stop file (`--stop-file`), which the interlock sees within 0.1 s;
    /// it releases the card and exits 6. The guard waits up to
    /// [`STOP_FILE_WAIT`] for that, then `Preempted`.
    StopFile(&'a Path),
}

/// How long a pre-empted interlock gets to release the card and end.
const STOP_FILE_WAIT: Duration = Duration::from_millis(900);

fn piped(cmd: &mut Command) {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
}

/// Runs a helper command without a window, inside the guard's job
/// ([`spawn::helper_flags`]: a job that refuses breakaway never refuses
/// a helper), and reads its output, for at most `limit`. Nothing is ended:
/// after the limit, or a pre-emption of a waiting run, the command is left
/// to finish by itself.
pub(super) fn run(
    what: &str,
    cmd: &mut Command,
    limit: Duration,
    c: &Cancel,
    on_cancel: OnCancel<'_>,
) -> R<Output> {
    piped(cmd);
    let child = cmd
        .creation_flags(spawn::helper_flags(on_cancel == OnCancel::Break))
        .spawn()
        .map_err(|e| failed(what, e))?;
    finish(what, child, limit, c, on_cancel)
}

/// [`run`] for `iem-engine interlock`, which opens the card: it starts
/// outside the guard's job like the engine (design §5.1, I9), so the end of
/// the guard's task never ends a holder of the card. A wait: on "ide event"
/// the guard creates `stop` (its `--stop-file`).
pub(super) fn run_outside_job(
    what: &str,
    cmd: &mut Command,
    limit: Duration,
    c: &Cancel,
    stop: &Path,
) -> R<Output> {
    piped(cmd);
    let child = spawn::spawn_detached(cmd, true).map_err(|e| failed(what, e))?;
    finish(what, child, limit, c, OnCancel::StopFile(stop))
}

fn drain<S: Read + Send + 'static>(stream: Option<S>) -> Option<JoinHandle<String>> {
    stream.map(|mut s| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Err(e) = s.read_to_end(&mut bytes) {
                warn!("reading a command's output: {e}");
            }
            String::from_utf8_lossy(&bytes).into_owned()
        })
    })
}

fn joined(reader: Option<JoinHandle<String>>) -> String {
    reader.and_then(|r| r.join().ok()).unwrap_or_default()
}

fn finish(
    what: &str,
    mut child: Child,
    limit: Duration,
    c: &Cancel,
    on_cancel: OnCancel<'_>,
) -> R<Output> {
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|e| failed(what, e))? {
            return Ok(Output {
                code: status.code(),
                stdout: joined(out),
                stderr: joined(err),
            });
        }
        let preempted = on_cancel != OnCancel::Finish && c.preempted();
        if preempted || start.elapsed() >= limit {
            match on_cancel {
                OnCancel::Finish => {}
                OnCancel::Break => {
                    if let Err(e) = console::ctrl_break(child.id()) {
                        warn!("Ctrl-Break to {what}: {e}");
                    }
                }
                OnCancel::StopFile(stop) => {
                    if let Err(e) = fs::write(stop, b"stop") {
                        warn!("the stop file of {what} ({}): {e}", stop.display());
                    }
                    let asked = Instant::now();
                    while asked.elapsed() < STOP_FILE_WAIT {
                        if child.try_wait().map_err(|e| failed(what, e))?.is_some() {
                            info!("{what} stopped at its stop file");
                            break;
                        }
                        thread::sleep(Cancel::SLICE);
                    }
                }
            }
            return Err(if preempted {
                StepError::Preempted
            } else {
                StepError::failed(format!(
                    "{what} did not finish within {} s",
                    limit.as_secs()
                ))
            });
        }
        thread::sleep(Cancel::SLICE);
    }
}
