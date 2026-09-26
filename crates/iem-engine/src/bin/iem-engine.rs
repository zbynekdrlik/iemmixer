//! `iem-engine`: the iemmixer audio engine (S3: NullRt and offline render;
//! the ASIO backend follows in S6). See `iem-engine --help`.

use std::process::ExitCode;

use iem_engine::control::Exit;
use iem_engine::engine::{Command, EngineError, USAGE, parse_args, render, run};
use tracing_subscriber::EnvFilter;

fn code(e: &EngineError) -> u8 {
    match e {
        EngineError::Io(_) => 1,
        EngineError::Site(_) | EngineError::Usage(_) => 2,
        EngineError::Fault { .. } => 70,
    }
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
            Exit::Shutdown => 0,
            Exit::Fault(_) => 70,
        }),
        Ok(Command::Render(a)) => render(&a).map(|()| 0),
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
