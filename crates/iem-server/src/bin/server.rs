//! `iem-server`: the standalone mixer server (CI E2E and the PC), PIN
//! provisioning and alarms without a running server.
//!
//!   iem-server                      run the server
//!   iem-server pin set-engineer     read a PIN from stdin, store its hash as the engineer PIN
//!   iem-server pin set-member <id>  read a PIN from stdin, store its hash for member <id>
//!   iem-server notify <title> <body>  push one alarm to the engineer's and owner's devices
//!
//! The site config is `$IEMMIXER_CONFIG` (default `iemmixer.toml`); runtime
//! data and secrets live next to it. `IEMMIXER_ENGINE_PIPE` overrides the
//! site's `engine_pipe`; `IEMMIXER_MODE` is `dev` (default) or `live`.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use iem_core::Config;
use iem_server::provision::{self, PinTarget, ProvisionError};
use iem_server::{RunMode, ServerConfig};

const USAGE: &str = "usage: iem-server [pin set-engineer | pin set-member <member-id> | notify <title> <body>]   (a PIN is read from stdin)";

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
        ["notify", title, body] => notify_command(title, body),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
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

/// Exit 0 when a device took the alarm, 3 when none did, 1 on an error.
fn notify_command(title: &str, body: &str) -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("iem-server: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(iem_server::notify::run_cli(&config_path(), title, body)) {
        Ok(0) => {
            eprintln!("iem-server: no device took the alarm");
            ExitCode::from(3)
        }
        Ok(n) => {
            eprintln!("iem-server: alarm sent to {n} device(s)");
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
    runtime.block_on(iem_server::start_server(
        ServerConfig {
            port,
            config,
            config_dir,
            mode,
        },
        None,
    ))
}
