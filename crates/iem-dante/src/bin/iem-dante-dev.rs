// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! `iem-dante-dev`: our own Dante device on the Gen 3 box (#72 phase A,
//! plan Task 5), run by systemd as `iem-dante.service`
//! (`scripts/gen3/iem-dante.service`). See `iem-dante-dev --help`.
//!
//! Glue only, Linux only: the device itself is `iem_dante::host` (phase C,
//! plan Task 2: the config and state files, the interface and its files,
//! the sockets, the mDNS daemon on the one interface, the goodbyes), which
//! the engine hosts the same way; here it is prepared with no engine
//! channel and started with SIGTERM and SIGINT ending it
//! (`host::Signals::Stop`), and the binary waits for its end. Every
//! decision is in `iem_dante::daemon` and `iem_dante::host` (tested and
//! mutated there). Exit codes (`host::exit_code`): 0 after a
//! signal and the goodbyes; 1 when the address is not there (yet: an
//! interface still coming up at boot), the interface's MAC cannot be read,
//! serving failed or the goodbyes were not confirmed, which the unit
//! restarts; 2 for the command line, the config, the state file (also one
//! whose folder cannot be written, found at the start) or an interface
//! without a usable MAC, which it does not (`RestartPreventExitStatus=2`).
//!
//! `iem-dante-dev clock --seconds <n>` (phase B, plan Task 2) serves
//! nothing: it polls dantesync's time server on the box n times, a second
//! apart, with the media clock's own poller (`iem_dante::clock::Poller`),
//! and prints each poll as one JSON line, numbers only
//! (`iem_dante::daemon::ClockLines`). Exit 0 after the n lines, 1 when it
//! has no socket or stdout closes, 2 for the command line.
//!
//! Phase B (plan Task 7): the device also runs the media clock's poller of
//! dantesync, flow control on UDP 4455, the TX and RX threads and our RX's
//! manager (`iem_dante::daemon::media`), and its control socket
//! (`--control`, default `/run/iem-dante/control.sock`, the unit's
//! `RuntimeDirectory`; `iem_dante::ctl`). Its stop ends every RX flow at
//! its transmitter before the goodbyes. `iem-dante-dev ctl [--socket
//! <path>] <request>` sends one request to that socket and prints the
//! answer line: exit 0 for `ok` or the status, 1 for an error answer or a
//! socket it cannot reach, 2 for the command line.

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("iem-dante-dev runs on Linux only (Gen 3, #72)");
    std::process::ExitCode::from(2)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::process::ExitCode;
    use std::sync::atomic::AtomicI32;
    use std::time::Duration;

    use iem_dante::clock::{DANTESYNC, MonoClock, POLL, Poller};
    use iem_dante::daemon::{ClockLines, Command, USAGE, ctl_succeeded, parse_args, version_line};
    use iem_dante::host::{self, Host, Signals};
    use tokio::time::MissedTickBehavior;
    use tracing::error;
    use tracing_subscriber::EnvFilter;

    /// How long `ctl` waits for the device's answer.
    const CTL_WAIT: Duration = Duration::from_secs(5);

    pub fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let opts = match parse_args(&args) {
            Ok(Command::Version) => {
                println!("{}", version_line());
                return ExitCode::SUCCESS;
            }
            Ok(Command::Help) => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            Ok(Command::Clock { seconds }) => return clock(seconds),
            Ok(Command::Ctl { socket, request }) => return ctl_request(&socket, &request),
            Ok(Command::Run(opts)) => opts,
            Err(msg) => {
                eprintln!("iem-dante-dev: {msg}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        };
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .init();
        // The standalone device: no engine channel, the signals end it.
        let ran = host::prepare(&opts, &[])
            .and_then(|prepared| prepared.start(Signals::Stop))
            .and_then(Host::wait);
        match ran {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                error!("{e}");
                ExitCode::from(host::exit_code(&e))
            }
        }
    }

    /// `clock --seconds <n>`: n polls of dantesync's time server, a second
    /// apart (the first at once), each one line on stdout; no log.
    fn clock(seconds: u32) -> ExitCode {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("iem-dante-dev clock: no runtime: {e}");
                return ExitCode::from(1);
            }
        };
        rt.block_on(async {
            let mut poller = match Poller::connect(DANTESYNC).await {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("iem-dante-dev clock: no socket to dantesync's time server: {e}");
                    return ExitCode::from(1);
                }
            };
            let clock = MonoClock::new();
            let freq = AtomicI32::new(0);
            let mut lines = ClockLines::new();
            let mut tick = tokio::time::interval(POLL);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            for t_s in 0..seconds {
                tick.tick().await;
                let polled = poller.once(&clock, &freq).await;
                let line = lines.line(t_s, &polled);
                if let Err(e) = writeln!(std::io::stdout(), "{line}") {
                    eprintln!("iem-dante-dev clock: stdout: {e}");
                    return ExitCode::from(1);
                }
            }
            ExitCode::SUCCESS
        })
    }

    /// `ctl`: one request line to the control socket at `path`, its answer
    /// line on stdout (exit 0 for `ok` or the status, 1 otherwise).
    fn ctl_request(path: &Path, request: &str) -> ExitCode {
        let exchange = || -> std::io::Result<String> {
            let mut stream = UnixStream::connect(path)?;
            stream.set_read_timeout(Some(CTL_WAIT))?;
            stream.set_write_timeout(Some(CTL_WAIT))?;
            stream.write_all(format!("{request}\n").as_bytes())?;
            let mut answer = String::new();
            BufReader::new(stream).read_line(&mut answer)?;
            Ok(answer)
        };
        let answer = match exchange() {
            Ok(answer) => answer,
            Err(e) => {
                eprintln!(
                    "iem-dante-dev ctl: the control socket {}: {e}",
                    path.display()
                );
                return ExitCode::from(1);
            }
        };
        let line = answer.trim_end();
        if let Err(e) = writeln!(std::io::stdout(), "{line}") {
            eprintln!("iem-dante-dev ctl: stdout: {e}");
            return ExitCode::from(1);
        }
        if ctl_succeeded(line) {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        }
    }
}
