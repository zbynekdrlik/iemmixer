//! The engine's supervisor pipe (S6 design note §4, §5.2): what the guard
//! sends, what it reads, and the decisions on it. Frames are the engine's
//! (a little-endian `u32` length, then JSON); messages are read field by
//! field, so any engine of protocol 1 whose `Status` carries the S6 fields
//! (`frames`, `missed`, `parked`) serves, and an older one is simply not
//! ready (its measured period reads 0).

use std::io::{self, Read};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::pc::{EngineSeen, Status};
use crate::plan::Health;
use crate::proto::{EngineStatus, HilOut};
use crate::site::FRAMES;

/// The engine protocol the guard speaks.
pub const PROTO: u64 = 1;
/// The engine's largest frame body (`iem_engine_proto::MAX_FRAME`).
pub const MAX_FRAME: usize = 1 << 20;

/// One frame of `msg`.
pub fn frame(msg: &Value) -> Vec<u8> {
    let body = msg.to_string().into_bytes();
    let len = u32::try_from(body.len()).unwrap_or(u32::MAX);
    let mut out = len.to_le_bytes().to_vec();
    out.extend_from_slice(&body);
    out
}

/// The hello of the guard's one supervisor connection.
pub fn hello() -> Value {
    json!({"type": "hello", "proto": PROTO, "role": "supervisor", "client": "iemmixer-guard"})
}

/// A request without arguments (`arm`, `shutdown`, `inject_fault`).
pub fn request(id: u64, op: &str) -> Value {
    request_cmd(id, json!({"op": op}))
}

/// A request carrying a whole command (`hil_test_signal`).
pub fn request_cmd(id: u64, cmd: Value) -> Value {
    json!({"type": "request", "id": id, "cmd": cmd})
}

/// The HIL test tone's frequency (design §7).
pub const HIL_HZ: f64 = 1000.0;

/// `HilTestSignal` (design §4): the engine renders every mix as usual but
/// encodes only the card outputs `card_tx` while the TTL runs. With `listen`
/// (the listen probe, S7 #10) the engine also emits the sine on its probe
/// streams 2 and 3, for the server's `&hil=1` listeners; the key is written
/// only when true, so a plain signal is what an older engine reads.
pub fn hil_test_signal(input: &str, dbfs: f64, ttl_s: f64, card_tx: &[u16], listen: bool) -> Value {
    let mut cmd = json!({
        "op": "hil_test_signal",
        "input": input,
        "hz": HIL_HZ,
        "dbfs": dbfs,
        "ttl_s": ttl_s,
        "card_tx": card_tx,
    });
    if listen {
        cmd["listen"] = Value::Bool(true);
    }
    cmd
}

/// Reads one frame's body; `Ok(None)` when the stream ends cleanly before a
/// frame, an error when it ends inside one or announces more than
/// [`MAX_FRAME`].
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut head = [0u8; 4];
    let (first, rest) = head.split_at_mut(1);
    loop {
        match r.read(first) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    r.read_exact(rest)?;
    let len = u32::from_le_bytes(head) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("an engine frame of {len} bytes"),
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(Some(body))
}

/// What the guard reads from the engine. Its meters and topology are no
/// business of the guard's (#38: nothing reads the stage): they are `Other`.
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Hello {
        build: String,
    },
    /// `build` is empty: it comes from the hello.
    Status(Status),
    Reply {
        id: u64,
        error: Option<String>,
    },
    DriverReleased {
        reason: String,
    },
    /// The engine's stream stopped parked (#35): nothing was released.
    DriverParked {
        reason: String,
    },
    Superseded,
    Other,
}

fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn number(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// `Status.hil`: each spare output's card channel and peak; an entry
/// without them reads 0.
fn hil_outs(v: &Value) -> Vec<HilOut> {
    v.get("hil")
        .and_then(Value::as_array)
        .map(|outs| {
            outs.iter()
                .map(|o| HilOut {
                    tx: u16::try_from(number(o, "tx")).unwrap_or(0),
                    peak: o.get("peak").and_then(Value::as_f64).unwrap_or(0.0),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The engine's largest histogram bucket (S7): the range is capped at 1 ms
/// in 1 µs buckets (`iem_audio_io::hist::MAX_RANGE_NS`), so the overflow
/// bucket is at most 1000.
pub const HIST_TOP_MAX: u32 = 1000;
/// The most buckets an engine histogram has (0 to [`HIST_TOP_MAX`]): what a
/// guard reply carries at most (`proto::tests::the_largest_reply_fits_a_frame`).
pub const HIST_LEN_MAX: usize = 1001;

/// `Status.interval_hist` / `process_hist` (S7): `[[bucket, count], …]`. A
/// histogram with more than [`HIST_LEN_MAX`] entries, or any entry that is
/// not a pair of integers with the bucket up to [`HIST_TOP_MAX`], reads as
/// none (the soak verdict then names it), never as part of one.
fn hist(v: &Value, key: &str) -> Vec<(u32, u64)> {
    let Some(list) = v
        .get(key)
        .and_then(Value::as_array)
        .filter(|l| l.len() <= HIST_LEN_MAX)
    else {
        return Vec::new();
    };
    list.iter()
        .map(|e| {
            let pair = e.as_array().filter(|p| p.len() == 2)?;
            let bucket = u32::try_from(pair.first()?.as_u64()?)
                .ok()
                .filter(|&b| b <= HIST_TOP_MAX)?;
            Some((bucket, pair.get(1)?.as_u64()?))
        })
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default()
}

/// Parses one engine message.
pub fn parse(body: &[u8]) -> Result<Msg, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("bad engine message: {e}"))?;
    Ok(
        match v.get("type").and_then(Value::as_str).unwrap_or_default() {
            "hello" => Msg::Hello {
                build: text(&v, "engine_build"),
            },
            "status" => Msg::Status(Status {
                build: String::new(),
                frames: u32::try_from(number(&v, "frames")).unwrap_or(0),
                callbacks: number(&v, "callbacks"),
                missed: number(&v, "missed"),
                resets: number(&v, "resets"),
                faulted: flag(&v, "faulted"),
                parked: flag(&v, "parked"),
                hil: hil_outs(&v),
                loopback_samples: number(&v, "loopback_samples"),
                late: number(&v, "late"),
                overruns: number(&v, "overruns"),
                process_max_us: v
                    .get("process_max_us")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
                hist_top_us: u32::try_from(number(&v, "hist_top_us")).unwrap_or(0),
                interval_hist: hist(&v, "interval_hist"),
                process_hist: hist(&v, "process_hist"),
            }),
            "reply" => Msg::Reply {
                id: number(&v, "id"),
                error: match v.get("error") {
                    None | Some(Value::Null) => None,
                    Some(e) => Some(
                        e.get("msg")
                            .and_then(Value::as_str)
                            .unwrap_or("refused")
                            .to_owned(),
                    ),
                },
            },
            "driver_released" => Msg::DriverReleased {
                reason: text(&v, "reason"),
            },
            "driver_parked" => Msg::DriverParked {
                reason: text(&v, "reason"),
            },
            "superseded" => Msg::Superseded,
            _ => Msg::Other,
        },
    )
}

/// The commit of `Hello.engine_build` (`<version>+<commit>`); a build
/// without a `+` is its own text.
pub fn commit_of(build: &str) -> &str {
    build.rsplit_once('+').map_or(build, |(_, commit)| commit)
}

/// The running engine's status as the guard shows it (`Pc::engine_seen`):
/// the newest `Status` of the supervisor connection with the hello's build.
/// `None` until the connection holds both: the engine's pipe exists before
/// its card opens and its first `Status` follows about 1 s later, and a
/// reply then shows no engine rather than an empty build and zero counters.
pub fn seen_status(build: Option<&str>, status: Option<&Status>) -> Option<Status> {
    let (build, status) = (build?, status?);
    Some(Status {
        build: build.to_owned(),
        ..status.clone()
    })
}

/// The guard's `Reply.engine` (design §7, what HIL v1 reads through
/// `iemmode status`): the engine as the supervisor connection saw it, its
/// build as the bare commit (the bundle's SHA), with the engine starts of
/// this guard, the exit code of the engine before the running one and the
/// running one's pid (`GuardState.pids`; S7, the soak's "one pid").
pub fn engine_status(
    seen: &EngineSeen,
    spawns: u64,
    last_exit: Option<i32>,
    pid: Option<u32>,
) -> EngineStatus {
    let s = &seen.status;
    EngineStatus {
        build: commit_of(&s.build).to_owned(),
        frames: s.frames,
        callbacks: s.callbacks,
        missed: s.missed,
        resets: s.resets,
        parked: s.parked,
        faulted: s.faulted,
        pipe_private: seen.pipe_private,
        spawns,
        last_exit,
        hil: s.hil.clone(),
        loopback_samples: s.loopback_samples,
        loopback_ms: s.loopback_samples as f64 * 1000.0 / 96_000.0,
        pid,
        late: s.late,
        overruns: s.overruns,
        process_max_us: s.process_max_us,
        hist_top_us: s.hist_top_us,
        interval_hist: s.interval_hist.clone(),
        process_hist: s.process_hist.clone(),
    }
}

/// `Hello.engine_build` (`<version>+<commit>`) names the bundle's commit.
pub fn build_matches(build: &str, sha: &str) -> bool {
    !sha.is_empty()
        && build
            .rsplit_once('+')
            .is_some_and(|(_, commit)| commit.eq_ignore_ascii_case(sha))
}

/// Where readiness stands after one status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ready {
    Wait,
    Done,
    Failed(String),
}

/// The warm-up after `--hold` (design §5.2 step 7): the measured period is
/// 32, callbacks advance, and no period is missed for the whole window; the
/// first miss restarts the window once, a second one fails.
#[derive(Debug, Clone)]
pub struct ReadyWindow {
    window: Duration,
    /// When the window began and the missed count then.
    start: Option<(Instant, u64)>,
    restarted: bool,
    callbacks: u64,
}

impl ReadyWindow {
    pub fn new(secs: u32) -> Self {
        Self {
            window: Duration::from_secs(u64::from(secs)),
            start: None,
            restarted: false,
            callbacks: 0,
        }
    }

    /// One status, read at `now`. Statuses come once a second; each must
    /// be newer than the last.
    pub fn observe(&mut self, s: &Status, now: Instant) -> Ready {
        if s.faulted {
            return Ready::Failed("the engine faulted".into());
        }
        if s.parked {
            return Ready::Failed("the engine parked its stream".into());
        }
        // Not streaming yet, or the period not measured yet.
        if s.callbacks == 0 || s.frames == 0 {
            return Ready::Wait;
        }
        if s.frames != FRAMES {
            return Ready::Failed(format!("measured period {} frames, not {FRAMES}", s.frames));
        }
        if s.callbacks <= self.callbacks {
            return Ready::Failed(format!("callbacks stopped at {}", s.callbacks));
        }
        self.callbacks = s.callbacks;
        match self.start {
            None => {
                self.start = Some((now, s.missed));
                Ready::Wait
            }
            Some((_, missed)) if s.missed > missed => {
                if self.restarted {
                    return Ready::Failed(format!(
                        "{} periods missed after the warm-up restart",
                        s.missed - missed
                    ));
                }
                self.restarted = true;
                self.start = Some((now, s.missed));
                Ready::Wait
            }
            Some((at, _)) if now.saturating_duration_since(at) >= self.window => Ready::Done,
            Some(_) => Ready::Wait,
        }
    }
}

/// The engine's health from two statuses about 1 s apart (design §5.2
/// "back to event" step 2).
pub fn health(first: &Status, second: &Status) -> Health {
    if first.parked || second.parked {
        Health::Parked
    } else if !second.faulted && second.callbacks > first.callbacks {
        Health::Healthy
    } else {
        Health::Dead
    }
}

/// How the engine's stream stopped: its last word before the engine ends,
/// with its reason (`shutdown`, `fault`, `card refused`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    /// `DriverReleased`: the card is free.
    Released(String),
    /// `DriverParked` (#35): the stream stayed parked (a callback stuck in
    /// it, or the parked-engine test's hold), nothing was released; the
    /// card is free once the engine's process has ended.
    Parked(String),
}

impl Stopped {
    /// What the guard logs when the word comes.
    pub fn note(&self) -> String {
        match self {
            Self::Released(reason) => format!("the engine released the driver: {reason}"),
            Self::Parked(reason) => format!(
                "the engine's stream stayed parked ({reason}): nothing was released; \
                 the card is free once the engine has ended"
            ),
        }
    }

    /// `EngineStop`'s failure when the engine did not end within `gone`
    /// after the word.
    pub fn not_ended(&self, gone: Duration) -> String {
        let secs = gone.as_secs();
        match self {
            Self::Released(_) => {
                format!("the engine released the driver but did not end within {secs} s")
            }
            Self::Parked(_) => format!(
                "the engine's stream stayed parked and the engine did not end within {secs} s: \
                 the card may still be held"
            ),
        }
    }
}

/// Where a `Shutdown` request stands (design §5.2 "back to event" step 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shutdown {
    /// The stream stopped (`DriverReleased` or `DriverParked`): the step
    /// waits for the engine's process to end either way.
    Stopped(Stopped),
    /// The engine refused the request (its reply carried an error).
    Refused(String),
}

/// `stopped`: the engine's word on its stream (`DriverReleased` or
/// `DriverParked`); `reply`: the request's reply, `None` not yet,
/// `Some(None)` accepted, `Some(Some(error))` refused. The stream's stop
/// ends the wait, and so does a refusal; an accepted request waits on for
/// the stop (`None`).
pub fn shutdown(stopped: Option<&Stopped>, reply: Option<Option<String>>) -> Option<Shutdown> {
    match (stopped, reply) {
        (Some(stopped), _) => Some(Shutdown::Stopped(stopped.clone())),
        (None, Some(Some(error))) => Some(Shutdown::Refused(error)),
        (None, Some(None) | None) => None,
    }
}

/// `iem-engine check-site` (design §4, F30): exit 0 and its report (the
/// last JSON line); anything else refuses the site.
pub fn check_site_result(code: Option<i32>, stdout: &str, stderr: &str) -> Result<String, String> {
    let report = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with('{'))
        .unwrap_or_default();
    match code {
        Some(0) => Ok(report.to_owned()),
        other => Err(format!(
            "check-site ended with {other:?}: {}",
            super::tail(stderr, 300)
        )),
    }
}

#[cfg(test)]
mod tests;
