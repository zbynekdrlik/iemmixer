//! `iem-soakclient`: the soak harness's binary (S7 design note §4). Its
//! decisions are the library's (`parse_args`, `credential`, `net::run`);
//! this is the argv, environment, CPU Set and file glue. Exit codes: 0 the
//! whole run with its final summary written, 1 a run that ended early (its
//! reason code on stderr) or whose final summary could not be written
//! (`summary-unwritable`), 2 a usage error (both credentials or neither
//! too). Stderr never carries a site value or a secret (P6): usage messages
//! name flags, not their values, and a run's end is a code. It ends no
//! process: its sockets end with a WebSocket Close.
//!
//! `iem-soakclient token …` (S7 plan Task 23) is the PC's token for the
//! live run ([`mint`]): 0 with the token in its `--out` file and only
//! `token-written` on stdout, 1 with its code on stderr (`secret-unreadable`,
//! `token-unwritable`, `clock-unreadable`), 2 on a usage error. The token is
//! never printed.

#![forbid(unsafe_code)]

use std::cell::Cell;
use std::process::ExitCode;
use std::time::SystemTime;

use iem_soakclient::mint;
use iem_soakclient::net::{Limits, run};
use iem_soakclient::{PIN_ENV, Reason, Summary, USAGE, credential, parse_args, write_summary};

fn main() -> ExitCode {
    let argv: Option<Vec<String>> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.into_string().ok())
        .collect();
    if let Some((command, rest)) = argv.as_deref().and_then(<[String]>::split_first)
        && command == "token"
    {
        return token(rest);
    }
    let parsed = argv
        .ok_or_else(|| "arguments must be UTF-8".to_owned())
        .and_then(|argv| parse_args(&argv));
    let args = match parsed {
        Ok(args) => args,
        Err(e) => return usage(&e),
    };
    // A value that is not UTF-8 is no PIN; `pin_from` refuses it unechoed.
    // Set at all, it is a credential: beside the secret file it is both.
    let pin = std::env::var_os(PIN_ENV).map(|v| v.into_string().unwrap_or_default());
    let credential = match credential(args.jwt_secret_file.as_deref(), pin) {
        Ok(credential) => credential,
        Err(e) => return usage(&e),
    };
    // A summary that cannot be written never ends the run (a reader may hold
    // the file on Windows): the next write tries again. The final one must
    // be written, or the run is no evidence (exit 1).
    let written = Cell::new(false);
    let mut save = |summary: &Summary| {
        let ok = write_summary(&args.out, summary).is_ok();
        if !ok {
            eprintln!("iem-soakclient: summary-unwritable");
        }
        written.set(ok);
    };
    let summary = match place(&args.cpu_sets) {
        Ok(()) => run(&args, &credential, &Limits::default(), &mut save),
        Err(reason) => {
            let summary = Summary {
                error: Some(reason),
                ..Summary::default()
            };
            save(&summary);
            summary
        }
    };
    if let Some(reason) = summary.error {
        eprintln!("iem-soakclient: {}", reason.code());
    }
    println!("{}", serde_json::to_string(&summary).unwrap_or_default());
    if summary.complete && written.get() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// `iem-soakclient token`: the token goes to its file only ([`mint::run`]);
/// stdout gets [`mint::WRITTEN`], stderr a usage message or a code.
fn token(argv: &[String]) -> ExitCode {
    let args = match mint::parse_args(argv) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("iem-soakclient token: {e}\n\n{}", mint::USAGE);
            return ExitCode::from(2);
        }
    };
    match mint::unix_seconds(SystemTime::now()).and_then(|now| mint::run(&args, now)) {
        Ok(()) => {
            println!("{}", mint::WRITTEN);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("iem-soakclient: {}", e.code());
            ExitCode::FAILURE
        }
    }
}

fn usage(error: &str) -> ExitCode {
    eprintln!("iem-soakclient: {error}\n\n{USAGE}");
    ExitCode::from(2)
}

/// Confines this process to the housekeeping CPU Sets (S1c profile, P10),
/// as the engine places itself on its own.
#[cfg(windows)]
fn place(ids: &[u32]) -> Result<(), Reason> {
    if ids.is_empty() {
        return Ok(());
    }
    iem_win::power::set_cpu_sets(ids).map_err(|_| Reason::CpuSets)
}

/// `parse_args` refuses `--cpu-sets` off Windows.
#[cfg(not(windows))]
fn place(_ids: &[u32]) -> Result<(), Reason> {
    Ok(())
}
