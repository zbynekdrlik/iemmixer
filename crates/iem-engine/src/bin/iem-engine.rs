//! `iem-engine`: the iemmixer audio engine (S3: NullRt and offline render;
//! S6: the ASIO backend, the interlock and the site check). See
//! `iem-engine --help`.

use std::process::ExitCode;

use iem_engine::control::Exit;
use iem_engine::engine::{
    Command, EngineError, USAGE, check_site, interlock, parse_args, render, run,
};
use tracing_subscriber::EnvFilter;

fn code(e: &EngineError) -> u8 {
    match e {
        EngineError::Io(_) => 1,
        EngineError::Site(_) | EngineError::Usage(_) => 2,
        EngineError::Card(_) => 3,
        EngineError::Fault { .. } => 70,
        // EX_TEMPFAIL: the guard tries again without counting a crash.
        EngineError::StateBusy(_) => 75,
    }
}

/// One JSON line on stdout (the guard and `iemmode` read it).
fn print_json(value: &impl serde::Serialize) -> Result<(), EngineError> {
    let line = serde_json::to_string(value).map_err(std::io::Error::other)?;
    println!("{line}");
    Ok(())
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match parse_args(&args) {
        Ok(Command::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Command::Run(cfg)) => run(cfg).map(|exit| match exit {
            Exit::Shutdown { .. } => 0,
            Exit::Card(_) => 3,
            Exit::Fault(_) => 70,
        }),
        Ok(Command::Render(a)) => render(&a).map(|()| 0),
        Ok(Command::Interlock(a)) => {
            interlock(&a).and_then(|(verdict, report)| print_json(&report).map(|()| verdict.code()))
        }
        Ok(Command::CheckSite(site)) => {
            check_site(&site).and_then(|summary| print_json(&summary).map(|()| 0))
        }
        Err(msg) => {
            eprintln!("iem-engine: {msg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(c) => ExitCode::from(c),
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::from(code(&e))
        }
    }
}
