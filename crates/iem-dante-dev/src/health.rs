// Ported from dante-stack fdfc321 crates/dante-stack/src/health.rs (#72).
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The box's health (Gen 3 phase D, plan
//! `docs/superpowers/plans/2026-10-26-gen3-phase-d-the-box.md` Task 6,
//! choice 4): `iem-dante-dev health [--socket <path>] [--server-port <n>]`
//! prints one JSON line of numbers and fixed words and exits 0 healthy, 1
//! degraded or failed, 3 stopped (2 for the command line). A command, not a
//! port: it is in the bundle, versioned with it; the dev box reads it over
//! ssh and deploy reads it back after a start; the server, its routes and
//! the engine protocol stay unchanged.
//!
//! It reads the three units' systemd states ([`parse_units`] of `systemctl
//! show`), the device's control socket twice a second apart (dante-stack's
//! `health::read_status` and `parse_status`: the clock and the engine's
//! pump; its `device_reasons` judges the device between the reads) and the
//! server's `/api/version` on 127.0.0.1 ([`server_answer`]: its build
//! against this binary's), and [`verdict`] judges them (pure). The I/O is
//! the Linux glue (`check`, `ask_server`). The box health lives here, not
//! in dante-stack, since its v0.2.0 (#72, 30.10.2026).
//!
//! The line: `{"health": {state, reasons, units: [{unit, active, result,
//! restarts}], clock: {ready, holdover, holdovers, re_anchors, freq_ppb},
//! pump: {media, blocks, late, missed, rebootstraps, max_process_us},
//! server: {answers, build}}}`. `unit` is `engine`, `server` or `device`;
//! `active` and `result` are systemd's own words, an unknown one `other`;
//! `clock` and `pump` are the second read's (the first's when only it
//! came; `null` without one, `pump` also without an engine); `build` is
//! `same`, `other` or `null` without an answer. dantesync is the box's
//! clock (owner, 2026-10-26): `clock` carries what the status has of it.
//! No name, address or build hash is ever printed.

use iem_dante::health::{Clock, DeviceReason, Pump, Status, device_reasons};
use serde::{Deserialize, Serialize};

/// The stack's units, as `systemctl show` names them: the engine (it hosts
/// the device), the server, the standalone device.
pub const UNITS: [&str; 3] = [
    "iem-engine.service",
    "iem-server.service",
    "iem-dante.service",
];
/// The properties [`parse_units`] reads (`systemctl show --property=`).
pub const PROPERTIES: &str = "Id,ActiveState,Result,NRestarts,ActiveEnterTimestampMonotonic";
/// The server's port on loopback (`iem-server.service`'s `PORT`; 80 since
/// phase E, the band's tunnel's origin).
pub const SERVER_PORT: u16 = 80;
/// How long the host unit may be active with a clock that is not ready
/// before that fails the box, µs: dantesync's first usable reply comes
/// within seconds of a start.
pub const CLOCK_GRACE_US: u64 = 30_000_000;

/// `health`'s options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// The device's control socket (dante-stack's `ctl::SOCKET` unless
    /// `--socket`).
    pub socket: std::path::PathBuf,
    /// The server's port on 127.0.0.1 ([`SERVER_PORT`] unless
    /// `--server-port`).
    pub server_port: u16,
}

/// A unit's `ActiveState`: systemd's words, an unknown one [`Active::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Active {
    Active,
    Reloading,
    Inactive,
    Failed,
    Activating,
    Deactivating,
    Maintenance,
    Refreshing,
    #[default]
    Other,
}

impl Active {
    pub fn from_word(word: &str) -> Self {
        match word {
            "active" => Active::Active,
            "reloading" => Active::Reloading,
            "inactive" => Active::Inactive,
            "failed" => Active::Failed,
            "activating" => Active::Activating,
            "deactivating" => Active::Deactivating,
            "maintenance" => Active::Maintenance,
            "refreshing" => Active::Refreshing,
            _ => Active::Other,
        }
    }
}

/// A service's `Result`: systemd's words, an unknown one
/// [`UnitResult::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnitResult {
    Success,
    Resources,
    Protocol,
    Timeout,
    ExitCode,
    Signal,
    CoreDump,
    Watchdog,
    StartLimitHit,
    OomKill,
    ExecCondition,
    #[default]
    Other,
}

impl UnitResult {
    pub fn from_word(word: &str) -> Self {
        match word {
            "success" => UnitResult::Success,
            "resources" => UnitResult::Resources,
            "protocol" => UnitResult::Protocol,
            "timeout" => UnitResult::Timeout,
            "exit-code" => UnitResult::ExitCode,
            "signal" => UnitResult::Signal,
            "core-dump" => UnitResult::CoreDump,
            "watchdog" => UnitResult::Watchdog,
            "start-limit-hit" => UnitResult::StartLimitHit,
            "oom-kill" => UnitResult::OomKill,
            "exec-condition" => UnitResult::ExecCondition,
            _ => UnitResult::Other,
        }
    }
}

/// One unit as systemd shows it. A unit `systemctl show` did not list
/// reads `other`, `other`, 0, 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Unit {
    pub active: Active,
    pub result: UnitResult,
    /// `NRestarts`: systemd's automatic restarts since the last start by
    /// hand (`reset-failed` clears them).
    pub restarts: u32,
    /// `ActiveEnterTimestampMonotonic`: when it last became active,
    /// `CLOCK_MONOTONIC` µs (0 never).
    pub active_since_us: u64,
}

/// The three units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Units {
    pub engine: Unit,
    pub server: Unit,
    pub device: Unit,
}

/// The output of `systemctl show --property=`[`PROPERTIES`] of the
/// [`UNITS`]: one block of `Key=Value` lines a unit (in any order), blocks
/// apart by an empty line, each named by its `Id`. Anything else is passed
/// over; a number that does not read is 0.
pub fn parse_units(text: &str) -> Units {
    let mut units = Units::default();
    for block in text.split("\n\n") {
        let mut unit = Unit::default();
        let mut id = None;
        for line in block.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "Id" => id = Some(value),
                "ActiveState" => unit.active = Active::from_word(value),
                "Result" => unit.result = UnitResult::from_word(value),
                "NRestarts" => unit.restarts = value.parse().unwrap_or(0),
                "ActiveEnterTimestampMonotonic" => {
                    unit.active_since_us = value.parse().unwrap_or(0)
                }
                _ => {}
            }
        }
        let slot = match id {
            Some("iem-engine.service") => &mut units.engine,
            Some("iem-server.service") => &mut units.server,
            Some("iem-dante.service") => &mut units.device,
            _ => continue,
        };
        *slot = unit;
    }
    units
}

/// The server's answer to `GET /api/version`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerAnswer {
    /// No answer within the wait, or not a 200 with a version.
    Silent,
    /// Its `git_hash` (never printed; [`Build`] says how it compares).
    Answered { git_hash: String },
}

#[derive(Deserialize)]
struct VersionReply {
    git_hash: String,
}

/// A whole HTTP reply to `GET /api/version`: a 200 whose body has a
/// `git_hash` string is an answer; anything else is [`ServerAnswer::Silent`].
pub fn server_answer(reply: &[u8]) -> ServerAnswer {
    let text = String::from_utf8_lossy(reply);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return ServerAnswer::Silent;
    };
    let mut status = head.lines().next().unwrap_or("").split_ascii_whitespace();
    let (Some(version), Some("200")) = (status.next(), status.next()) else {
        return ServerAnswer::Silent;
    };
    if !version.starts_with("HTTP/1.") {
        return ServerAnswer::Silent;
    }
    match serde_json::from_str::<VersionReply>(body) {
        Ok(v) => ServerAnswer::Answered {
            git_hash: v.git_hash,
        },
        Err(_) => ServerAnswer::Silent,
    }
}

/// The box's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// The engine and the server active, or the standalone device alone,
    /// with no reason.
    Healthy,
    /// Running, with a reason to look (none that [`State::Failed`] has).
    Degraded,
    /// Not doing its job: a unit failed or down, no status, the server
    /// silent, the pump stalled, or the clock not ready past
    /// [`CLOCK_GRACE_US`].
    Failed,
    /// All three units inactive (none failed).
    Stopped,
}

impl State {
    /// `health`'s exit code: 0 healthy, 1 degraded or failed, 3 stopped.
    pub fn exit_code(self) -> u8 {
        match self {
            State::Healthy => 0,
            State::Degraded | State::Failed => 1,
            State::Stopped => 3,
        }
    }
}

/// Why the box is not healthy; a verdict lists each at most once, in this
/// order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// A unit is `failed`.
    UnitFailed,
    /// A unit the running stack needs is not active (the engine and the
    /// server, or the standalone device), and not failed.
    UnitDown,
    /// A unit has restarted by itself (`NRestarts` > 0).
    Restarted,
    /// The host unit is active and a read of its control socket gave no
    /// status.
    NoStatus,
    /// The host unit is active and the clock is not ready (fails the box
    /// once the host has been active over [`CLOCK_GRACE_US`]).
    ClockNotReady,
    /// dantesync was in holdover at either read, or went into it between
    /// them.
    Holdover,
    /// The clock is ready and the engine's pump paces itself.
    PumpFree,
    /// The pump's blocks did not rise between the reads (or a read has no
    /// pump while the engine runs).
    PumpStalled,
    /// The pump re-bootstrapped between the reads.
    PumpRebootstrap,
    /// The server is active and did not answer.
    ServerDown,
    /// The server's build is not this binary's.
    BuildMismatch,
}

impl From<DeviceReason> for Reason {
    fn from(reason: DeviceReason) -> Self {
        match reason {
            DeviceReason::NoStatus => Reason::NoStatus,
            DeviceReason::ClockNotReady => Reason::ClockNotReady,
            DeviceReason::Holdover => Reason::Holdover,
            DeviceReason::PumpFree => Reason::PumpFree,
            DeviceReason::PumpStalled => Reason::PumpStalled,
            DeviceReason::PumpRebootstrap => Reason::PumpRebootstrap,
        }
    }
}

/// How the server's build compares with this binary's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Build {
    Same,
    Other,
}

/// What [`verdict`] says, with what the line shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub state: State,
    pub reasons: Vec<Reason>,
    pub units: Units,
    pub clock: Option<Clock>,
    pub pump: Option<Pump>,
    /// The server answered.
    pub answers: bool,
    /// `None` without an answer.
    pub build: Option<Build>,
}

/// What runs: the engine (it hosts the device) with the server, or the
/// standalone device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Engine,
    Device,
}

/// The engine's shape once the engine or the server is anything but
/// inactive; else the device's once it is; else nothing runs.
fn shape(u: &Units) -> Option<Shape> {
    if u.engine.active != Active::Inactive || u.server.active != Active::Inactive {
        Some(Shape::Engine)
    } else if u.device.active != Active::Inactive {
        Some(Shape::Device)
    } else {
        None
    }
}

/// The server's `git_hash` names this build: a non-empty prefix of it.
fn same_build(git_hash: &str, own_build: &str) -> bool {
    !git_hash.is_empty() && own_build.starts_with(git_hash)
}

/// Whether `reason` fails the box (else it degrades it); a clock not
/// ready fails it only past the grace (`clock_grace` false).
fn fails(reason: Reason, clock_grace: bool) -> bool {
    match reason {
        Reason::UnitFailed
        | Reason::UnitDown
        | Reason::NoStatus
        | Reason::PumpStalled
        | Reason::ServerDown => true,
        Reason::ClockNotReady => !clock_grace,
        Reason::Restarted
        | Reason::Holdover
        | Reason::PumpFree
        | Reason::PumpRebootstrap
        | Reason::BuildMismatch => false,
    }
}

/// The box's verdict from its units, two status reads a second apart
/// (`first`, `second`; `None` for a read without a status), the server's
/// answer, the build the server must name (this binary's `GITHUB_SHA`) and
/// `CLOCK_MONOTONIC` in µs taken with the units. Pure.
///
/// - Stopped: all three units inactive; no reason.
/// - The engine's shape (the engine or the server not inactive) needs the
///   engine and the server active, the device's shape (the device alone)
///   the device: else `unit_down` (`unit_failed` for a failed one, any of
///   the three).
/// - The host (the engine, or the device) active: both reads, else
///   `no_status`; the second's clock ready, else `clock_not_ready`; a
///   holdover; with the engine, its pump between the reads (dante-stack's
///   `device_reasons`, in its order).
/// - With the engine and the server active: the server's answer and its
///   build.
/// - Failed when a reason fails the box ([`State::Failed`]), degraded for
///   any other reason, else healthy.
pub fn verdict(
    units: &Units,
    first: Option<&Status>,
    second: Option<&Status>,
    server: &ServerAnswer,
    own_build: &str,
    now_mono_us: u64,
) -> Verdict {
    let build = match server {
        ServerAnswer::Silent => None,
        ServerAnswer::Answered { git_hash } => Some(if same_build(git_hash, own_build) {
            Build::Same
        } else {
            Build::Other
        }),
    };
    let shown = second.or(first);
    let mut v = Verdict {
        state: State::Stopped,
        reasons: Vec::new(),
        units: *units,
        clock: shown.map(|s| s.clock),
        pump: shown.and_then(|s| s.engine),
        answers: build.is_some(),
        build,
    };
    let Some(shape) = shape(units) else {
        return v;
    };
    let all = [&units.engine, &units.server, &units.device];
    let (host, needed): (&Unit, &[&Unit]) = match shape {
        Shape::Engine => (&units.engine, &[&units.engine, &units.server]),
        Shape::Device => (&units.device, &[&units.device]),
    };
    let reasons = &mut v.reasons;
    if all.iter().any(|u| u.active == Active::Failed) {
        reasons.push(Reason::UnitFailed);
    }
    if needed
        .iter()
        .any(|u| u.active != Active::Active && u.active != Active::Failed)
    {
        reasons.push(Reason::UnitDown);
    }
    if all.iter().any(|u| u.restarts > 0) {
        reasons.push(Reason::Restarted);
    }
    let mut clock_grace = false;
    if host.active == Active::Active {
        for reason in device_reasons(first, second, shape == Shape::Engine) {
            let reason = Reason::from(reason);
            if reason == Reason::ClockNotReady {
                clock_grace = now_mono_us.saturating_sub(host.active_since_us) <= CLOCK_GRACE_US;
            }
            reasons.push(reason);
        }
    }
    if shape == Shape::Engine && units.server.active == Active::Active {
        match build {
            None => reasons.push(Reason::ServerDown),
            Some(Build::Other) => reasons.push(Reason::BuildMismatch),
            Some(Build::Same) => {}
        }
    }
    v.state = if v.reasons.iter().any(|&r| fails(r, clock_grace)) {
        State::Failed
    } else if v.reasons.is_empty() {
        State::Healthy
    } else {
        State::Degraded
    };
    v
}

#[derive(Serialize)]
struct UnitLine {
    unit: &'static str,
    active: Active,
    result: UnitResult,
    restarts: u32,
}

#[derive(Serialize)]
struct ServerLine {
    answers: u8,
    build: Option<Build>,
}

#[derive(Serialize)]
struct Body<'a> {
    state: State,
    reasons: &'a [Reason],
    units: [UnitLine; 3],
    clock: Option<Clock>,
    pump: Option<Pump>,
    server: ServerLine,
}

#[derive(Serialize)]
struct Line<'a> {
    health: Body<'a>,
}

impl Verdict {
    /// The line `health` prints (the module names its fields).
    pub fn line(&self) -> String {
        let unit = |unit, u: &Unit| UnitLine {
            unit,
            active: u.active,
            result: u.result,
            restarts: u.restarts,
        };
        let line = Line {
            health: Body {
                state: self.state,
                reasons: &self.reasons,
                units: [
                    unit("engine", &self.units.engine),
                    unit("server", &self.units.server),
                    unit("device", &self.units.device),
                ],
                clock: self.clock,
                pump: self.pump,
                server: ServerLine {
                    answers: u8::from(self.answers),
                    build: self.build,
                },
            },
        };
        // Numbers, fixed words and nulls only: nothing here can fail to
        // serialize.
        serde_json::to_string(&line).expect("the health line serializes")
    }
}

#[cfg(target_os = "linux")]
pub use glue::{ask_server, check};

/// The Linux glue: the `systemctl` call, the control socket's reads
/// (dante-stack's `read_status`) and the server's version over loopback.
#[cfg(target_os = "linux")]
mod glue {
    use std::io::{self, ErrorKind, Read, Write};
    use std::net::{Ipv4Addr, SocketAddr, TcpStream};
    use std::process::Command;
    use std::time::{Duration, Instant};

    use iem_dante::health::read_status;

    use super::{
        Options, PROPERTIES, ServerAnswer, UNITS, Verdict, parse_units, server_answer, verdict,
    };

    /// The control socket's reads, a second apart.
    pub const READ_APART: Duration = Duration::from_secs(1);
    /// How long one control-socket read may take.
    pub const STATUS_WAIT: Duration = Duration::from_secs(2);
    /// How long the server's version may take, connect to last byte.
    pub const SERVER_WAIT: Duration = Duration::from_secs(2);
    /// The longest HTTP reply read, bytes (dante-stack keeps its own for
    /// the status line).
    pub const MAX_READ: u64 = 1 << 20;
    /// The request (HTTP/1.0: the server answers with a length or closes).
    pub const VERSION_REQUEST: &[u8] =
        b"GET /api/version HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";

    /// The whole check: the units with the monotonic time, the two reads a
    /// second apart, the server; then [`verdict`] against `own_build`
    /// (this binary's `GITHUB_SHA`); `name` prefixes the error lines.
    pub fn check(opts: &Options, name: &str, own_build: &str) -> Verdict {
        let units = parse_units(&systemctl_show(name));
        let now_us = u64::try_from(iem_dante::clock::monotonic_ns() / 1_000).unwrap_or(0);
        let first = read_status(&opts.socket, STATUS_WAIT);
        std::thread::sleep(READ_APART);
        let second = read_status(&opts.socket, STATUS_WAIT);
        let server = ask_server(
            SocketAddr::from((Ipv4Addr::LOCALHOST, opts.server_port)),
            SERVER_WAIT,
        );
        verdict(
            &units,
            first.as_ref(),
            second.as_ref(),
            &server,
            own_build,
            now_us,
        )
    }

    /// `systemctl show` of the three units' [`PROPERTIES`]; empty when
    /// systemctl cannot run (every unit then reads `other`).
    fn systemctl_show(name: &str) -> String {
        match Command::new("systemctl")
            .arg("show")
            .arg(format!("--property={PROPERTIES}"))
            .args(UNITS)
            .output()
        {
            Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
            Err(e) => {
                eprintln!("{name} health: systemctl did not run: {e}");
                String::new()
            }
        }
    }

    /// `GET /api/version` to `addr`, the whole reply within `wait` from the
    /// connect on; [`ServerAnswer::Silent`] for no connection, no whole
    /// reply in time, or a reply that is not a version.
    pub fn ask_server(addr: SocketAddr, wait: Duration) -> ServerAnswer {
        let deadline = Instant::now() + wait;
        let ask = || -> io::Result<Vec<u8>> {
            let mut stream = TcpStream::connect_timeout(&addr, wait)?;
            stream.set_write_timeout(Some(wait))?;
            stream.write_all(VERSION_REQUEST)?;
            let mut reply = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(io::Error::new(ErrorKind::TimedOut, "no whole reply"));
                }
                stream.set_read_timeout(Some(left))?;
                let n = stream.read(&mut buf)?;
                if n == 0 {
                    return Ok(reply);
                }
                reply.extend_from_slice(&buf[..n]);
                if reply.len() as u64 > MAX_READ {
                    return Err(io::Error::new(ErrorKind::InvalidData, "a reply too long"));
                }
            }
        };
        ask().map_or(ServerAnswer::Silent, |reply| server_answer(&reply))
    }
}

#[cfg(test)]
mod tests;
