//! `iem-engine supervise` (Gen 3 phase H, #72; plan
//! `docs/superpowers/plans/2026-10-28-gen3-phase-h-box-gates.md` Task 3): a
//! supervisor client of the engine's control pipe for the Gen 3 box, where
//! no guard runs. On the PC the guard sends the HIL test signal over its
//! supervisor connection (`iem-guard`'s `win::engine::hil_signal`); on the
//! box this does: `test-signal <input> <dbfs> <seconds> [--listen]` sends
//! `HilTestSignal` (every mix TX written as zero while it runs, the sine
//! only on HIL's spare outputs, which the engine opens under its
//! `--test-signal` flag and refuses without it), `stop-test-signal` sends
//! `StopTestSignal`, `status` prints the engine's next `Status`.
//!
//! The protocol is the engine's own (`iem_engine_proto`: the same frames,
//! `ClientMsg` and `EngineMsg` the control loop reads and writes), read
//! through the engine's own [`Framer`]; the guard keeps a copy of the frame
//! only because it stays outside the engine's dependency closure.
//!
//! **One supervisor at a time.** The engine keeps one supervisor beside its
//! controller, and a supervisor's hello replaces the one before it
//! (`Superseded`, then closed; `control/connections.rs`, `Control::hello`).
//! On the box no guard holds that connection, so nothing is displaced: each
//! call is the engine's supervisor for its one command and leaves; two calls
//! at once end the earlier one with `Superseded` (exit 1). On the PC the
//! guard holds it, so a call there would take it from the guard: off Unix
//! the command is refused (exit 2). A supervisor that leaves neither stops
//! nor holds a signal: the burst runs its seconds after the call returned
//! (the engine ends it), `stop-test-signal` ends it sooner.
//!
//! The caps are the guard's, with its words (`daemon::hil::test_signal`):
//! at most −20 dBFS (the engine's own X13 cap), a whole number of seconds
//! from 1 to 60, an input id as the protocol reads one; the tone is the
//! guard's 1 kHz on every spare output the engine opened (its first
//! `Status.hil`, the site's `[guard] hil_tx` in order), as the guard sends
//! it to every `hil_tx`.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use iem_engine_proto::{
    ClientMsg, Cmd, EngineMsg, ErrCode, FrameError, InputId, PROTO, Role, Status, valid_id,
    write_frame,
};
use serde_json::json;

use crate::pipe::Framer;

/// The box's control pipe (`scripts/gen3/iem-engine.service`'s `--pipe`).
pub const PIPE: &str = "/run/iemmixer/engine.sock";
/// The HIL test tone's frequency, the guard's (`effects::engine::HIL_HZ`).
pub const HIL_HZ: f64 = 1000.0;
/// The HIL test signal's ceiling, the guard's (`daemon::HIL_MAX_DBFS`) and
/// the engine's X13 cap (`params::TEST_DBFS`).
pub const HIL_MAX_DBFS: f64 = -20.0;
/// The longest HIL test signal (s), the guard's (`daemon::HIL_MAX_TTL_S`).
pub const HIL_MAX_TTL_S: u32 = 60;
/// The name the hello gives (the engine logs it).
pub const CLIENT: &str = "iem-engine supervise";
/// The one request's id.
const REQUEST: u64 = 1;

/// What one call asks of the engine.
#[derive(Debug, Clone, PartialEq)]
pub enum Ask {
    /// `HilTestSignal` on every spare output, for `seconds`.
    TestSignal {
        input: InputId,
        dbfs: f64,
        seconds: u32,
        listen: bool,
    },
    /// `StopTestSignal`.
    StopTestSignal,
    /// `Ping`, then the engine's next `Status`.
    Status,
}

/// `iem-engine supervise`'s command line.
#[derive(Debug, Clone, PartialEq)]
pub struct Supervise {
    /// `--pipe`: the engine's control socket ([`PIPE`] unless given).
    pub pipe: String,
    pub ask: Ask,
}

fn number(s: &str) -> Result<f64, String> {
    s.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("{s:?} is not a number"))
}

fn whole(s: &str) -> Result<u32, String> {
    s.parse::<u32>()
        .map_err(|_| format!("{s:?} is not a whole number"))
}

fn exactly<'a, const N: usize>(rest: &[&'a str]) -> Result<[&'a str; N], String> {
    let n = N;
    <[&'a str; N]>::try_from(rest)
        .map_err(|_| format!("{n} arguments expected, {} given", rest.len()))
}

/// `test-signal`'s three values, checked as the guard checks them.
fn test_signal(values: [&str; 3], listen: bool) -> Result<Ask, String> {
    let [input, dbfs, seconds] = values;
    if !valid_id(input) {
        return Err(format!(
            "{input:?} is not an input id (1 to 64 of a-z, 0-9, _ . -, from a letter or digit)"
        ));
    }
    let dbfs = number(dbfs)?;
    if dbfs > HIL_MAX_DBFS {
        return Err(format!(
            "{dbfs} dBFS is above the HIL ceiling of {HIL_MAX_DBFS} dBFS"
        ));
    }
    let seconds = whole(seconds)?;
    if seconds == 0 {
        return Err(format!("a TTL of {seconds} s is not a positive time"));
    }
    if seconds > HIL_MAX_TTL_S {
        return Err(format!(
            "a TTL of {seconds} s is above the HIL limit of {HIL_MAX_TTL_S} s"
        ));
    }
    Ok(Ask::TestSignal {
        input: InputId::new(input),
        dbfs,
        seconds,
        listen,
    })
}

/// Parses what follows `supervise`: `[--pipe <socket>]`, then the command
/// and its values. `--listen` only as the last word of `test-signal`, taken
/// off before its three values, so a negative dB value never meets a flag
/// parser (the guard's `iemmode test-signal`).
pub fn parse(words: &[String]) -> Result<Supervise, String> {
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let (pipe, rest) = match words.split_first() {
        Some((&"--pipe", rest)) => {
            let (pipe, rest) = rest
                .split_first()
                .ok_or_else(|| "--pipe needs a value".to_owned())?;
            ((*pipe).to_owned(), rest)
        }
        _ => (PIPE.to_owned(), words.as_slice()),
    };
    let Some((what, values)) = rest.split_first() else {
        return Err("supervise needs test-signal, stop-test-signal or status".to_owned());
    };
    let ask = match *what {
        "test-signal" => {
            let (listen, values) = match values.split_last() {
                Some((&"--listen", head)) => (true, head),
                _ => (false, values),
            };
            test_signal(exactly(values)?, listen)?
        }
        "stop-test-signal" => {
            let [] = exactly(values)?;
            Ask::StopTestSignal
        }
        "status" => {
            let [] = exactly(values)?;
            Ask::Status
        }
        other => return Err(format!("unknown supervise command {other:?}")),
    };
    Ok(Supervise { pipe, ask })
}

/// Why a call failed: fixed words only (an `io::ErrorKind`'s, the engine's
/// error code), never the engine's free text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    /// Off Unix (the PC), where the guard is the engine's supervisor.
    #[error(
        "supervise is for the Gen 3 box: on the PC the guard is the engine's supervisor, and a new supervisor would replace it"
    )]
    NotHere,
    #[error("cannot connect to the engine's pipe ({0})")]
    Connect(String),
    #[error("the engine's pipe failed ({0})")]
    Pipe(String),
    #[error("the engine closed the connection")]
    Closed,
    #[error("another supervisor replaced this connection")]
    Superseded,
    #[error("no Status from the engine within {0:?}")]
    NoStatus(Duration),
    #[error("no answer from the engine within {0:?}")]
    NoReply(Duration),
    /// The engine's error code (`forbidden`: it runs without
    /// `--test-signal`).
    #[error("the engine refused it: {0}")]
    Refused(String),
}

impl Failure {
    /// The exit code: 2 off Unix (as the command line's), else 1.
    pub fn code(&self) -> u8 {
        match self {
            Self::NotHere => 2,
            _ => 1,
        }
    }
}

impl From<FrameError> for Failure {
    fn from(e: FrameError) -> Self {
        match e {
            FrameError::Closed => Self::Closed,
            FrameError::Io(e) => Self::Pipe(e.kind().to_string()),
            FrameError::TooLarge(_) => Self::Pipe("a frame over the limit".to_owned()),
        }
    }
}

/// An error code's protocol word (`forbidden`, `bad_value`, …).
fn word(code: ErrCode) -> String {
    match serde_json::to_value(code) {
        Ok(serde_json::Value::String(w)) => w,
        _ => "unknown".to_owned(),
    }
}

/// What a call that succeeded prints.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The burst runs: the state's revision after it and how many spare
    /// outputs carry it.
    TestSignal {
        rev: u64,
        outputs: usize,
        dbfs: f64,
        seconds: u32,
        listen: bool,
    },
    Stopped {
        rev: u64,
    },
    Status(Box<Status>),
}

impl Outcome {
    /// One JSON line: numbers and fixed words only (P6: no names, no
    /// channel the call itself chose).
    pub fn line(&self) -> String {
        let v = match self {
            Self::TestSignal {
                rev,
                outputs,
                dbfs,
                seconds,
                listen,
            } => json!({"test_signal": {
                "rev": rev,
                "outputs": outputs,
                "dbfs": dbfs,
                "seconds": seconds,
                "listen": listen,
            }}),
            Self::Stopped { rev } => json!({"stop_test_signal": {"rev": rev}}),
            Self::Status(status) => json!({ "status": status }),
        };
        v.to_string()
    }
}

/// How long a call waits for a `Status` and for its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waits {
    pub status: Duration,
    pub reply: Duration,
}

/// The guard's waits (`win::engine::STATUS_GAP`, `REPLY`): the engine sends
/// a `Status` once a second.
pub const WAITS: Waits = Waits {
    status: Duration::from_secs(3),
    reply: Duration::from_secs(5),
};

fn send<W: Write>(w: &mut W, msg: &ClientMsg) -> Result<(), Failure> {
    write_frame(w, msg).map_err(Failure::from)
}

/// The engine's next message before `until`; `None` once it passed, looked
/// at before every message, so the engine's steady meter frames never
/// stretch a wait. A message this build cannot read (a newer engine's,
/// §2.4) is passed over.
fn next_msg<S: Read>(
    s: &mut S,
    framer: &mut Framer,
    until: Instant,
) -> Result<Option<EngineMsg>, Failure> {
    loop {
        if until.saturating_duration_since(Instant::now()).is_zero() {
            return Ok(None);
        }
        match framer.next_frame()? {
            Some(bytes) => {
                if let Ok(msg) = serde_json::from_slice::<EngineMsg>(&bytes) {
                    return Ok(Some(msg));
                }
            }
            None => {
                framer.fill(&mut *s)?;
            }
        }
    }
}

/// Reads until `pick` takes a message, within `wait`; `missed` after it.
fn wait_for<S: Read, T>(
    s: &mut S,
    framer: &mut Framer,
    wait: Duration,
    missed: Failure,
    mut pick: impl FnMut(EngineMsg) -> Option<T>,
) -> Result<T, Failure> {
    let until = Instant::now() + wait;
    loop {
        match next_msg(s, framer, until)? {
            None => return Err(missed),
            Some(EngineMsg::Superseded) => return Err(Failure::Superseded),
            Some(msg) => {
                if let Some(t) = pick(msg) {
                    return Ok(t);
                }
            }
        }
    }
}

fn next_status<S: Read>(s: &mut S, framer: &mut Framer, wait: Duration) -> Result<Status, Failure> {
    wait_for(s, framer, wait, Failure::NoStatus(wait), |m| match m {
        EngineMsg::Status(status) => Some(status),
        _ => None,
    })
}

/// Sends `cmd` and waits for its answer: the revision after it, or the
/// engine's refusal.
fn request<S: Read + Write>(
    s: &mut S,
    framer: &mut Framer,
    cmd: Cmd,
    wait: Duration,
) -> Result<u64, Failure> {
    send(
        s,
        &ClientMsg::Request {
            id: REQUEST,
            origin: None,
            cmd,
        },
    )?;
    let reply = wait_for(s, framer, wait, Failure::NoReply(wait), |m| match m {
        EngineMsg::Reply(r) if r.id == REQUEST => Some(r),
        _ => None,
    })?;
    match reply.error {
        None => Ok(reply.rev),
        Some(e) => Err(Failure::Refused(word(e.code))),
    }
}

/// One call on a connected stream: the hello as the supervisor, then `ask`.
pub fn exchange<S: Read + Write>(s: &mut S, ask: &Ask, waits: Waits) -> Result<Outcome, Failure> {
    let mut framer = Framer::new();
    send(
        s,
        &ClientMsg::Hello {
            proto: PROTO,
            role: Role::Supervisor,
            client: CLIENT.to_owned(),
        },
    )?;
    match ask {
        Ask::TestSignal {
            input,
            dbfs,
            seconds,
            listen,
        } => {
            // The spare outputs the engine opened (none without its
            // test-signal flag, which then refuses the signal itself).
            let status = next_status(s, &mut framer, waits.status)?;
            let card_tx: Vec<u16> = status.hil.iter().map(|h| h.tx).collect();
            let outputs = card_tx.len();
            let cmd = Cmd::HilTestSignal {
                input: input.clone(),
                hz: HIL_HZ,
                dbfs: *dbfs,
                ttl_s: f64::from(*seconds),
                card_tx,
                listen: *listen,
            };
            let rev = request(s, &mut framer, cmd, waits.reply)?;
            Ok(Outcome::TestSignal {
                rev,
                outputs,
                dbfs: *dbfs,
                seconds: *seconds,
                listen: *listen,
            })
        }
        Ask::StopTestSignal => {
            let rev = request(s, &mut framer, Cmd::StopTestSignal, waits.reply)?;
            Ok(Outcome::Stopped { rev })
        }
        Ask::Status => {
            request(s, &mut framer, Cmd::Ping, waits.reply)?;
            let status = next_status(s, &mut framer, waits.status)?;
            Ok(Outcome::Status(Box::new(status)))
        }
    }
}

/// Connects to the engine's control socket and makes the call with the
/// guard's waits: reads wait at most the engine's own `pipe::POLL` at a
/// time, writes at most its `pipe::SEND_TIMEOUT`. Off Unix: [`Failure::NotHere`].
pub fn run(a: &Supervise) -> Result<Outcome, Failure> {
    #[cfg(unix)]
    {
        let fail = |e: std::io::Error| Failure::Connect(e.kind().to_string());
        let mut s = std::os::unix::net::UnixStream::connect(&a.pipe).map_err(fail)?;
        s.set_read_timeout(Some(crate::pipe::POLL)).map_err(fail)?;
        s.set_write_timeout(Some(crate::pipe::SEND_TIMEOUT))
            .map_err(fail)?;
        exchange(&mut s, &a.ask, WAITS)
    }
    #[cfg(not(unix))]
    {
        let _ = a;
        Err(Failure::NotHere)
    }
}

#[cfg(test)]
mod tests;
