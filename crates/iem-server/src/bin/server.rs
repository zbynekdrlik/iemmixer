//! `iem-server`: the standalone mixer server (CI E2E and the PC), PIN
//! provisioning and alarms without a running server.
//!
//!   iem-server                      run the server
//!   iem-server pin set-engineer     read a PIN from stdin, store its hash as the engineer PIN
//!   iem-server pin set-member <id>  read a PIN from stdin, store its hash for member <id>
//!   iem-server notify --to alarm <title> <body>
//!                                   push a technical alarm to the alarm recipients only
//!   iem-server notify --to band-activity <title> <body>
//!                                   push the band-activity notice to the engineer's devices only
//!   iem-server notify --count alarm print the number of alarm recipients
//!   iem-server alarm-link [--ttl-h N]  print the owner's one-time alarm link (default 24 h)
//!
//! The site config is `$IEMMIXER_CONFIG` (default `iemmixer.toml`); runtime
//! data and secrets live next to it. `IEMMIXER_ENGINE_PIPE` overrides the
//! site's `engine_pipe`; `IEMMIXER_MODE` is `dev` (default) or `live`.
//! SIGTERM or SIGINT (Windows: Ctrl-Break or Ctrl-C) stop the server
//! gracefully: open requests get up to 5 s, then it exits 0.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context as _;
use iem_core::Config;
use iem_server::notify::Audience;
use iem_server::provision::{self, PinTarget, ProvisionError};
use iem_server::{RunMode, ServerConfig};

const USAGE: &str = "usage: iem-server [pin set-engineer | pin set-member <member-id> | notify --to alarm|band-activity <title> <body> | notify --count alarm | alarm-link [--ttl-h <hours>]]   (a PIN is read from stdin)";

fn config_path() -> PathBuf {
    PathBuf::from(std::env::var("IEMMIXER_CONFIG").unwrap_or_else(|_| "iemmixer.toml".to_string()))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] => match run_server() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("iem-server: {e:#}");
                ExitCode::FAILURE
            }
        },
        ["pin", "set-engineer"] => pin_command(PinTarget::Engineer),
        ["pin", "set-member", member] => pin_command(PinTarget::Member((*member).to_string())),
        ["notify", "--to", to, title, body] => match Audience::parse(to) {
            Some(audience) => notify_command(audience, title, body),
            None => usage(),
        },
        ["notify", "--count", "alarm"] => count_command(),
        ["alarm-link", rest @ ..] => alarm_link_command(rest),
        _ => usage(),
    }
}

/// Bad arguments: the usage line, exit 2.
fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn pin_command(target: PinTarget) -> ExitCode {
    let stdin = std::io::stdin();
    match provision::run(&config_path(), &target, stdin.lock()) {
        Ok(()) => {
            eprintln!("iem-server: stored the {} PIN hash", target.label());
            ExitCode::SUCCESS
        }
        Err(e @ ProvisionError::Invalid(_)) => {
            eprintln!("iem-server: {e}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("iem-server: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Exit 0 when a device took the notice, 3 when none did (no recipient of
/// that audience, or every push failed), 1 on an error.
fn notify_command(to: Audience, title: &str, body: &str) -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("iem-server: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(iem_server::notify::run_cli(&config_path(), to, title, body)) {
        Ok(0) => {
            eprintln!("iem-server: no device took the notice ({to:?})");
            ExitCode::from(3)
        }
        Ok(n) => {
            eprintln!("iem-server: notice sent to {n} device(s) ({to:?})");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("iem-server: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Prints the number of alarm recipients on stdout (exit 0); 1 on an error.
fn count_command() -> ExitCode {
    match iem_server::notify::count_cli(&config_path()) {
        Ok(n) => {
            println!("{n}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("iem-server: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Prints the link on stdout (exit 0); 2 for bad arguments, 1 on an error.
fn alarm_link_command(args: &[&str]) -> ExitCode {
    let ttl_h = match iem_server::alarm_link::parse_ttl(args) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("iem-server: {e}");
            return ExitCode::from(2);
        }
    };
    match iem_server::alarm_link::run_cli(&config_path(), ttl_h) {
        Ok(url) => {
            println!("{url}");
            eprintln!("iem-server: the link works once within {ttl_h} h");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("iem-server: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_server() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("iem_server=info".parse()?),
        )
        .init();
    tracing::info!("Starting the iemmixer server v{}", iem_core::VERSION);
    let path = config_path();
    let mut config =
        Config::load(&path).with_context(|| format!("loading site config {}", path.display()))?;
    if let Ok(pipe) = std::env::var("IEMMIXER_ENGINE_PIPE") {
        config.engine_pipe = pipe;
    }
    let mode = RunMode::parse(std::env::var("IEMMIXER_MODE").ok().as_deref());
    tracing::info!(
        path = %path.display(),
        members = config.members.len(),
        inputs = config.inputs.len(),
        engine_pipe = %config.engine_pipe,
        ?mode,
        "site config loaded"
    );
    let port = match std::env::var("PORT") {
        Ok(p) => p
            .parse()
            .with_context(|| format!("PORT={p} is not a port number"))?,
        Err(_) => config.port,
    };
    let config_dir = provision::config_dir_of(&path);
    let runtime = tokio::runtime::Runtime::new().context("creating the tokio runtime")?;
    let served = runtime.block_on(async {
        let stop = shutdown_signal().context("registering the stop signals")?;
        iem_server::start_server_until(
            ServerConfig {
                port,
                config,
                config_dir,
                mode,
            },
            None,
            stop,
        )
        .await
    });
    // The engine client, the backup daemon and the tunnel watchdog are tasks
    // of this runtime: shutting it down ends them and closes the engine pipes.
    runtime.shutdown_timeout(Duration::from_secs(1));
    served?;
    tracing::info!("iem-server stopped");
    Ok(())
}

/// The graceful stop request (S6): SIGTERM or SIGINT. The handlers are
/// installed at the call, so a stop that arrives before the server listens
/// is not lost; the future resolves on the first one.
#[cfg(unix)]
fn shutdown_signal() -> std::io::Result<impl std::future::Future<Output = ()> + Send + 'static> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = term.recv() => tracing::info!("SIGTERM: stopping"),
            _ = int.recv() => tracing::info!("SIGINT: stopping"),
        }
    })
}

/// The graceful stop request (S6): Ctrl-Break (the guard delivers it to the
/// server's own console) or Ctrl-C, installed at the call.
#[cfg(windows)]
fn shutdown_signal() -> std::io::Result<impl std::future::Future<Output = ()> + Send + 'static> {
    use tokio::signal::windows::{ctrl_break, ctrl_c};
    let mut brk = ctrl_break()?;
    let mut c = ctrl_c()?;
    Ok(async move {
        tokio::select! {
            _ = brk.recv() => tracing::info!("Ctrl-Break: stopping"),
            _ = c.recv() => tracing::info!("Ctrl-C: stopping"),
        }
    })
}
