//! `iemmixer-guard`: the guard daemon (S6 design note §5.1).
//!
//! - `run`: the daemon. It holds `Global\iemmixer-guard` (one guard across
//!   sessions; a new guard waits up to 10 s for one that hands over), serves
//!   the guard pipe, watches the session's end, applies the reboot rule and
//!   adopts the children, then handles requests and the once-a-second watch.
//! - `install <zip>`: without a running guard (the mutex), installs a bundle;
//!   the very first one is also activated into `bin\`.
//! - `install <zip> --verify-only`: CI's check of a bundle zip, nothing kept.
//! - `activate <sha>`: without a running guard (the mutex), from a bundle's
//!   own exe, activates an installed bundle in an idle event (a guard too
//!   old to activate in event, #9 2026-09-28); it starts no guard.
//!
//! The decisions are `iem_guard::{daemon, install, cli}`.

use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use iem_guard::cli::{self, GuardCli};
use iem_guard::install;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse_guard(&args) {
        Ok(GuardCli::Run) => run(),
        Ok(GuardCli::Install {
            zip,
            verify_only: true,
        }) => verify_only(&zip),
        Ok(GuardCli::Install {
            zip,
            verify_only: false,
        }) => install_offline(&zip),
        Ok(GuardCli::Activate { sha }) => activate_offline(&sha),
        Err(why) => {
            eprintln!("iemmixer-guard: {why}\n{}", cli::GUARD_USAGE);
            ExitCode::from(cli::EXIT_USAGE)
        }
    }
}

/// Unpacks into a fresh temp directory, verifies, removes that directory.
fn verify_only(zip: &str) -> ExitCode {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let scratch =
        std::env::temp_dir().join(format!("iemmixer-verify-{}-{nanos}", std::process::id()));
    match install::verify_only(Path::new(zip), &scratch) {
        Ok(m) => {
            println!(
                "verified bundle {} (branch {}, version {}, run {})",
                m.sha, m.branch, m.version, m.run
            );
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("iemmixer-guard install --verify-only: {why}");
            ExitCode::from(cli::EXIT_REFUSED)
        }
    }
}

#[cfg(windows)]
fn local_app_data() -> Result<std::path::PathBuf, String> {
    std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "LOCALAPPDATA is not set".to_owned())
}

#[cfg(windows)]
fn install_offline(zip: &str) -> ExitCode {
    use iem_guard::daemon::{self, Clock, Guard, SiteConf};
    use iem_guard::proto;
    use iem_win::sync::GlobalMutex;

    let _lock = match GlobalMutex::try_take(proto::NAME) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            eprintln!("iemmixer-guard install: a guard runs; use iemmode install");
            return ExitCode::from(cli::EXIT_REFUSED);
        }
        Err(e) => {
            eprintln!("iemmixer-guard install: the guard's mutex: {e}");
            return ExitCode::from(cli::EXIT_REFUSED);
        }
    };
    let root = match local_app_data() {
        Ok(base) => install::root_dir(&base),
        Err(why) => {
            eprintln!("iemmixer-guard install: {why}");
            return ExitCode::from(cli::EXIT_REFUSED);
        }
    };
    let mut g = Guard::open(&root, SiteConf::default(), Clock::System);
    let (ok, detail) = daemon::install_offline(&mut g, Path::new(zip));
    if ok {
        println!("{detail}");
        ExitCode::SUCCESS
    } else {
        eprintln!("iemmixer-guard install: {detail}");
        ExitCode::from(cli::EXIT_REFUSED)
    }
}

#[cfg(not(windows))]
fn install_offline(_zip: &str) -> ExitCode {
    eprintln!("iemmixer-guard install: installs on the PC only (--verify-only runs anywhere)");
    ExitCode::from(cli::EXIT_REFUSED)
}

/// `activate <sha>` while no guard runs, from a bundle's own exe (#9
/// 2026-09-28): the guard's mutex for the whole run (a guard that holds it
/// refuses it: `iemmode activate`), the saved state, the process list
/// through `WinPc`, then `daemon::activate_offline`; one JSON line. It
/// never starts a guard: the next `iemmode` call starts the guard's task.
#[cfg(windows)]
fn activate_offline(sha: &str) -> ExitCode {
    use iem_guard::daemon::{self, Clock, Guard, SiteConf};
    use iem_guard::proto;
    use iem_guard::win::WinPc;
    use iem_win::sync::GlobalMutex;

    let refuse = |why: String| {
        println!("{}", serde_json::json!({"ok": false, "detail": why}));
        ExitCode::from(cli::EXIT_REFUSED)
    };
    // Nothing is read or written while a guard runs.
    let lock = match GlobalMutex::try_take(proto::NAME) {
        Ok(Some(lock)) => lock,
        Ok(None) => return refuse("a guard runs; use iemmode activate".to_owned()),
        Err(e) => return refuse(format!("the guard's mutex: {e}")),
    };
    let mut pc = match WinPc::load() {
        Ok(pc) => pc,
        Err(e) => return refuse(format!("the guard's settings: {e}")),
    };
    // The guard's log (no guard writes it while the mutex is held): the
    // exclude task's answer and this activation stay readable on the PC.
    init_logging(&pc.settings().logs_dir());
    tracing::info!(
        "iemmixer-guard {} ({}) activates {sha} without a guard",
        env!("CARGO_PKG_VERSION"),
        proto::GUARD_BUILD
    );
    let root = pc.settings().pc.root.clone();
    let site = SiteConf::from_site(&pc.settings().guard);
    let mut g = Guard::open(&root, site, Clock::System);
    let reply = daemon::activate_offline(&mut pc, &mut g, Some(lock), sha);
    tracing::info!("activate {sha} without a guard: {}", reply.detail);
    println!("{}", cli::reply_json(&reply));
    ExitCode::from(cli::exit_code(&reply))
}

#[cfg(not(windows))]
fn activate_offline(_sha: &str) -> ExitCode {
    println!(
        "{}",
        serde_json::json!({"ok": false, "detail": "iemmixer-guard activate runs on the PC only"})
    );
    ExitCode::from(cli::EXIT_REFUSED)
}

/// The guard's log: `<root>\logs\guard.log` (the task has no console).
#[cfg(windows)]
fn init_logging(dir: &Path) {
    use std::fs::{self, OpenOptions};
    use std::sync::Mutex;

    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let file = fs::create_dir_all(dir).and_then(|()| {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("guard.log"))
    });
    match file {
        Ok(f) => tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(Mutex::new(f))
            .with_env_filter(filter)
            .init(),
        Err(e) => {
            tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_env_filter(filter)
                .init();
            tracing::warn!("the guard's log file in {}: {e}", dir.display());
        }
    }
}

/// `Global\iemmixer-guard`, waiting up to 10 s for a guard that hands over.
#[cfg(windows)]
fn take_mutex() -> std::io::Result<Option<iem_win::sync::GlobalMutex>> {
    use std::time::{Duration, Instant};

    use iem_guard::proto;
    use iem_win::sync::GlobalMutex;

    let start = Instant::now();
    loop {
        if let Some(lock) = GlobalMutex::try_take(proto::NAME)? {
            return Ok(Some(lock));
        }
        if start.elapsed() >= Duration::from_secs(10) {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(windows)]
fn run() -> ExitCode {
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use iem_guard::daemon::{self, Clock, Guard, SiteConf};
    use iem_guard::win::{self, WinPc};
    use iem_guard::{pipe, proto};
    use tracing::{info, warn};

    let lock = match take_mutex() {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            eprintln!("iemmixer-guard: already running");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("iemmixer-guard: the guard's mutex: {e}");
            return ExitCode::from(cli::EXIT_REFUSED);
        }
    };
    let mut pc = match WinPc::load() {
        Ok(pc) => pc,
        Err(e) => {
            eprintln!("iemmixer-guard: the guard's settings: {e}");
            return ExitCode::from(cli::EXIT_REFUSED);
        }
    };
    let settings = pc.settings().clone();
    init_logging(&settings.logs_dir());
    info!("iemmixer-guard {} starts", env!("CARGO_PKG_VERSION"));
    for failed in install::clean_old_bins(&install::bin_dir(&settings.pc.root)) {
        warn!("an old exe stays: {failed}");
    }
    let mut g = Guard::open(
        &settings.pc.root,
        SiteConf::from_site(&settings.guard),
        Clock::System,
    );
    let tray = Arc::clone(&g.shared);
    pc.set_tray_quit(Box::new(move || tray.tray_quit()));
    // The pipe first: while the start runs its event plan, `iemmode`
    // already sees it (and "ide event" waits for it). After a hand-over the
    // old guard's instances live on until its process has ended: the first
    // instance waits.
    let listener = match pipe::retry(
        "the guard pipe's first instance",
        pipe::LISTEN_WAIT,
        pipe::LISTEN_EVERY,
        || pipe::listen(proto::NAME),
    ) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("iemmixer-guard: the guard pipe: {e}");
            warn!("the guard pipe: {e}");
            return ExitCode::from(cli::EXIT_REFUSED);
        }
    };
    let (jobs, requests) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let acceptor = match pipe::serve(listener, Arc::clone(&g.shared), jobs, Arc::clone(&stop)) {
        Ok(thread) => thread,
        Err(e) => {
            warn!("the guard pipe's thread: {e}");
            return ExitCode::from(cli::EXIT_REFUSED);
        }
    };
    let waiter = Arc::clone(&g.shared);
    if let Err(e) = win::watch_session_end(Arc::clone(&g.session_ending), move || {
        if !waiter.await_session_done(Duration::from_secs(20)) {
            warn!("the session ends before the guard stopped its children");
        }
    }) {
        warn!("the session window: {e}");
    }
    let boot = match iem_win::process::boot_time() {
        Ok(t) => t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        Err(e) => {
            warn!("the boot time: {e}");
            0
        }
    };
    daemon::start(&mut pc, &mut g, boot);
    daemon::serve_requests(&mut pc, &mut g, &requests);
    // The last reply (quit, the activation that hands over) reaches its
    // client before this process and its pipe end.
    if !g.shared.await_replies(pipe::LAST_REPLY) {
        warn!("the last reply was not written before the guard stops");
    }
    stop.store(true, Ordering::SeqCst);
    // The listener and its waiting instance end with the acceptor (one
    // poll, 50 ms) before the mutex is released: a new guard then waits
    // only for this process's open connections.
    if acceptor.join().is_err() {
        warn!("the guard pipe's thread panicked");
    }
    let handover = g.handover.clone();
    // The new guard waits for the mutex: release it first.
    drop(lock);
    if let Some(exe) = handover {
        let mut cmd = Command::new(&exe);
        cmd.arg("run");
        // Placed by this guard's job like its children (design §5.1): a job
        // that would end it when it closes refuses it, and the next
        // `iemmode` call starts the guard's task instead.
        match iem_win::spawn::spawn_detached(&mut cmd, false) {
            Ok(child) => info!("handed over to {} (pid {})", exe.display(), child.id()),
            Err(e) => warn!("starting {}: {e}", exe.display()),
        }
    }
    info!("iemmixer-guard stops; its children keep running");
    ExitCode::SUCCESS
}

#[cfg(not(windows))]
fn run() -> ExitCode {
    eprintln!("iemmixer-guard run: the daemon runs on the PC only");
    ExitCode::from(cli::EXIT_REFUSED)
}
