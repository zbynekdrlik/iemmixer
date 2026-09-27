//! `iemmode`: the guard's only client (S6 design note §5.1). One request to
//! the guard over its pipe (the guard's task is started when nothing
//! answers), one JSON reply on stdout with every kept alarm; exit 0 ok,
//! 1 refused or failed, 2 usage, 4 guard unreachable. `event --direct` runs
//! the event plan in this process when no guard runs (`iempc event` uses it
//! on exit 4). The parsing and the decisions are `iem_guard::cli`.

use std::process::ExitCode;

use iem_guard::cli::{self, Cli};
use iem_guard::pipe;
use iem_guard::proto::{self, Request};
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    // stdout carries the reply; the log goes to stderr.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse(&args) {
        Ok(Cli::Ask(req)) => ask(&req),
        Ok(Cli::Direct { dry_run }) => direct(dry_run),
        Err(why) => {
            eprintln!("iemmode: {why}\n{}", cli::IEMMODE_USAGE);
            ExitCode::from(cli::EXIT_USAGE)
        }
    }
}

fn ask(req: &Request) -> ExitCode {
    let result = cli::call_starting(
        || pipe::call(proto::NAME, req),
        start_guard,
        cli::START_WAIT,
        cli::START_POLL,
    );
    match result {
        Ok(reply) => {
            println!("{}", cli::reply_json(&reply));
            ExitCode::from(cli::exit_code(&reply))
        }
        Err(why) => {
            println!("{}", cli::unreachable_json(&why));
            ExitCode::from(cli::EXIT_UNREACHABLE)
        }
    }
}

#[cfg(windows)]
fn start_guard() -> Result<(), String> {
    iem_guard::win::start_guard_task()
}

#[cfg(not(windows))]
fn start_guard() -> Result<(), String> {
    Err("the guard's task exists on the PC only".to_owned())
}

/// Without a guard: the guard's mutex (free only when no guard runs), the
/// state, and the same event plan with the PC's effects in this process.
#[cfg(windows)]
fn direct(dry_run: bool) -> ExitCode {
    use iem_guard::daemon::{self, Clock, Guard, SiteConf};
    use iem_guard::win::WinPc;
    use iem_win::sync::GlobalMutex;

    let refuse = |why: String| {
        println!("{}", serde_json::json!({"ok": false, "detail": why}));
        ExitCode::from(cli::EXIT_REFUSED)
    };
    let lock = match GlobalMutex::try_take(proto::NAME) {
        Ok(lock) => lock,
        Err(e) => return refuse(format!("the guard's mutex: {e}")),
    };
    let mut pc = match WinPc::load() {
        Ok(pc) => pc,
        Err(e) => return refuse(format!("the guard's settings: {e}")),
    };
    let root = pc.settings().pc.root.clone();
    let site = SiteConf::from_site(&pc.settings().guard);
    let mut g = Guard::open(&root, site, Clock::System);
    let reply = daemon::direct_event(&mut pc, &mut g, lock, dry_run);
    println!("{}", cli::reply_json(&reply));
    ExitCode::from(cli::exit_code(&reply))
}

#[cfg(not(windows))]
fn direct(_dry_run: bool) -> ExitCode {
    println!(
        "{}",
        serde_json::json!({"ok": false, "detail": "event --direct runs on the PC only"})
    );
    ExitCode::from(cli::EXIT_REFUSED)
}
