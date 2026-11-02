// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! `iem-dante-dev`'s own part (#72): the box's health ([`health`], the
//! `health` subcommand), which moved here from dstack at its v0.2.0
//! (ROZHODNUTÉ on #72, 30.10.2026), and `ctl` at our socket ([`SOCKET`]).
//! Every other command line is the stack's (`daemon::cli::main`);
//! [`parse_args`] and [`parse_ctl`] tell them apart.

pub mod health;

use std::path::PathBuf;

/// The device's control socket on the box: the units' `--control`, the
/// `RuntimeDirectory=iem-dante` they make, and where `ctl` and `health`
/// look. dstack's own default moved away at v0.5.6 (`ctl::SOCKET`).
pub const SOCKET: &str = "/run/iem-dante/control.sock";

/// What `health`'s command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `health [--socket <path>] [--server-port <n>]`: the box's health.
    Health(health::Options),
    /// `health … --help` (or `-h`): the usage.
    Help,
}

/// The command line (without the program's name): `None` when its first
/// word is not `health` (the stack's command line, `daemon::cli::main`),
/// else `health`'s own arguments, `--socket <path>` and `--server-port
/// <n>` (1 to 65535), each at most once; `--help` or `-h` among them is
/// the usage.
pub fn parse_args(args: &[String]) -> Option<Result<Command, String>> {
    match args.split_first() {
        Some((first, rest)) if first == "health" => Some(parse_health(rest)),
        _ => None,
    }
}

// Ported from dstack fdfc321 crates/dante-stack/src/daemon.rs (#72):
// `parse_health`.
fn parse_health(args: &[String]) -> Result<Command, String> {
    let mut socket = None;
    let mut port = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let slot = match arg.as_str() {
            "--help" | "-h" => return Ok(Command::Help),
            "--socket" => &mut socket,
            "--server-port" => &mut port,
            other => return Err(format!("health: unknown argument {other:?}")),
        };
        let Some(value) = it.next() else {
            return Err(format!("{arg} needs a value"));
        };
        if slot.replace(value).is_some() {
            return Err(format!("{arg} given twice"));
        }
    }
    let socket = match socket {
        Some(path) if path.is_empty() => return Err("--socket needs a path".into()),
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(SOCKET),
    };
    let server_port = match port {
        None => health::SERVER_PORT,
        Some(text) => match text.parse::<u16>() {
            Ok(n) if n > 0 => n,
            _ => {
                return Err(format!(
                    "--server-port {text:?} is not a port from 1 to 65535"
                ));
            }
        },
    };
    Ok(Command::Health(health::Options {
        socket,
        server_port,
    }))
}

/// `health`'s usage, for the binary called `name`.
pub fn health_usage(name: &str) -> String {
    format!(
        "usage: {name} health [--socket <path>] [--server-port <n>]

health: the box's health, one JSON line of numbers and fixed words: the
state (healthy, degraded, failed, stopped), its reasons, the systemd states
of iem-engine, iem-server and iem-dante, the device's clock and the
engine's pump from two reads of the control socket (default
{socket}) a second apart, and whether the server
answers /api/version on 127.0.0.1 (default port {port}) with this build.
Exit 0 healthy, 1 degraded or failed, 3 stopped (all three inactive), 2 for
the command line.",
        socket = SOCKET,
        port = health::SERVER_PORT,
    )
}

/// The request of a `ctl` command line that names no socket, which goes to
/// [`SOCKET`] (the stack's `ctl` would look at its own default); `None` for
/// anything else: `--socket`, `--help` and a missing request stay the
/// stack's.
pub fn parse_ctl(args: &[String]) -> Option<String> {
    match args.split_first() {
        Some((first, rest)) if first == "ctl" => match rest.first().map(String::as_str) {
            None | Some("--socket" | "--help" | "-h") => None,
            Some(_) => Some(rest.join(" ")),
        },
        _ => None,
    }
}

/// One request to the control socket `path`, its answer line (the stack's
/// `ctl` client, which it does not export).
#[cfg(target_os = "linux")]
fn ctl_exchange(path: &std::path::Path, request: &str) -> std::io::Result<String> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    let wait = Some(std::time::Duration::from_secs(5));
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(wait)?;
    stream.set_write_timeout(wait)?;
    stream.write_all(format!("{request}\n").as_bytes())?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer)?;
    Ok(answer.trim_end().to_owned())
}

/// `ctl <request>`: the answer on stdout; exit 0 when the device did it, 1
/// for an error answer or no socket, as the stack's `ctl`.
#[cfg(target_os = "linux")]
pub fn run_ctl(name: &str, socket: &std::path::Path, request: &str) -> std::process::ExitCode {
    use std::io::Write;
    use std::process::ExitCode;
    let answer = match ctl_exchange(socket, request) {
        Ok(answer) => answer,
        Err(e) => {
            eprintln!("{name} ctl: the control socket {}: {e}", socket.display());
            return ExitCode::from(1);
        }
    };
    if let Err(e) = writeln!(std::io::stdout(), "{answer}") {
        eprintln!("{name} ctl: stdout: {e}");
        return ExitCode::from(1);
    }
    if iem_dante::daemon::ctl_succeeded(&answer) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Runs `health`'s parsed command line for the binary `build` (its name
/// prefixes the error lines, its `sha` is the build the server must name):
/// the usage (exit 0), a usage error (exit 2), or the check's one line on
/// stdout with its state's exit code (1 when stdout is closed).
#[cfg(target_os = "linux")]
pub fn run_health(
    build: iem_dante::daemon::Build,
    command: Result<Command, String>,
) -> std::process::ExitCode {
    use std::io::Write;
    use std::process::ExitCode;
    let opts = match command {
        Ok(Command::Health(opts)) => opts,
        Ok(Command::Help) => {
            println!("{}", health_usage(build.name));
            return ExitCode::SUCCESS;
        }
        Err(msg) => {
            eprintln!("{}: {msg}\n\n{}", build.name, health_usage(build.name));
            return ExitCode::from(2);
        }
    };
    // Ported from dstack fdfc321 crates/dante-stack/src/daemon/cli.rs
    // (#72): `health_line`.
    let verdict = health::check(&opts, build.name, build.sha);
    if let Err(e) = writeln!(std::io::stdout(), "{}", verdict.line()) {
        eprintln!("{} health: stdout: {e}", build.name);
        return ExitCode::from(1);
    }
    ExitCode::from(verdict.state.exit_code())
}

#[cfg(test)]
mod tests;
