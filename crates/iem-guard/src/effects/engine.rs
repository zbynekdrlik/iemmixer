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
/// encodes only the card outputs `card_tx` while the TTL runs.
pub fn hil_test_signal(input: &str, dbfs: f64, ttl_s: f64, card_tx: &[u16]) -> Value {
    json!({
        "op": "hil_test_signal",
        "input": input,
        "hz": HIL_HZ,
        "dbfs": dbfs,
        "ttl_s": ttl_s,
        "card_tx": card_tx,
    })
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

/// `Status.interval_hist` / `process_hist` (S7): `[[bucket, count], …]`. A
/// histogram with any entry that is not a pair of integers in range reads as
/// none (the soak verdict then names it), never as part of one.
fn hist(v: &Value, key: &str) -> Vec<(u32, u64)> {
    let Some(list) = v.get(key).and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .map(|e| {
            let pair = e.as_array().filter(|p| p.len() == 2)?;
            let bucket = u32::try_from(pair.first()?.as_u64()?).ok()?;
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
mod tests {
    use super::*;

    fn status(frames: u32, callbacks: u64, missed: u64) -> Status {
        Status {
            build: String::new(),
            frames,
            callbacks,
            missed,
            resets: 0,
            faulted: false,
            parked: false,
            hil: Vec::new(),
            loopback_samples: 0,
            late: 0,
            overruns: 0,
            process_max_us: 0.0,
            hist_top_us: 0,
            interval_hist: Vec::new(),
            process_hist: Vec::new(),
        }
    }

    fn released() -> Stopped {
        Stopped::Released("shutdown".into())
    }

    fn parked() -> Stopped {
        Stopped::Parked("shutdown".into())
    }

    #[test]
    fn a_shutdown_waits_for_the_release() {
        assert_eq!(shutdown(None, None), None);
        assert_eq!(shutdown(None, Some(None)), None);
        assert_eq!(
            shutdown(Some(&released()), None),
            Some(Shutdown::Stopped(released()))
        );
        assert_eq!(
            shutdown(Some(&released()), Some(None)),
            Some(Shutdown::Stopped(released()))
        );
        // The release wins over a late refusal.
        assert_eq!(
            shutdown(Some(&released()), Some(Some("forbidden".into()))),
            Some(Shutdown::Stopped(released()))
        );
    }

    /// A stream that stopped parked (#35) ends the wait as a release does:
    /// `EngineStop` then waits for the engine's process to end, after which
    /// the card is free, exactly as before the engine said so.
    #[test]
    fn a_parked_stop_ends_the_wait_like_a_release() {
        assert_eq!(
            shutdown(Some(&parked()), Some(None)),
            Some(Shutdown::Stopped(parked()))
        );
        assert_eq!(
            shutdown(Some(&parked()), Some(Some("forbidden".into()))),
            Some(Shutdown::Stopped(parked()))
        );
    }

    /// The log line and the step's failure say what happened (#35): a
    /// parked stream released nothing, so neither says "released".
    #[test]
    fn a_parked_stop_is_never_called_a_release() {
        let gone = Duration::from_secs(5);
        assert_eq!(
            released().note(),
            "the engine released the driver: shutdown"
        );
        assert_eq!(
            released().not_ended(gone),
            "the engine released the driver but did not end within 5 s"
        );
        assert_eq!(
            Stopped::Parked("fault".into()).note(),
            "the engine's stream stayed parked (fault): nothing was released; \
             the card is free once the engine has ended"
        );
        assert_eq!(
            parked().not_ended(gone),
            "the engine's stream stayed parked and the engine did not end within 5 s: \
             the card may still be held"
        );
    }

    /// A refused `Shutdown` ends the wait at once: "ide event" goes on to
    /// the engine's health instead of waiting out the 10 s release.
    #[test]
    fn a_refused_shutdown_ends_the_wait_at_once() {
        assert_eq!(
            shutdown(None, Some(Some("forbidden: not the supervisor".into()))),
            Some(Shutdown::Refused("forbidden: not the supervisor".into()))
        );
    }

    #[test]
    fn frames_carry_a_little_endian_length() {
        let f = frame(&json!({"a": 1}));
        assert_eq!(f, b"\x07\x00\x00\x00{\"a\":1}");
        let mut r: &[u8] = &f;
        assert_eq!(read_frame(&mut r).unwrap(), Some(b"{\"a\":1}".to_vec()));
        assert_eq!(read_frame(&mut r).unwrap(), None);
    }

    #[test]
    fn the_guard_says_hello_as_supervisor_and_asks_by_op() {
        assert_eq!(
            hello(),
            json!({"type": "hello", "proto": 1, "role": "supervisor", "client": "iemmixer-guard"})
        );
        assert_eq!(
            request(7, "arm"),
            json!({"type": "request", "id": 7, "cmd": {"op": "arm"}})
        );
        assert_eq!(
            request_cmd(8, json!({"op": "x", "n": 1})),
            json!({"type": "request", "id": 8, "cmd": {"op": "x", "n": 1}})
        );
    }

    #[test]
    fn check_site_passes_only_with_exit_0() {
        let out = "loading\n{\"topology\": \"ab\", \"inputs\": 32}\n";
        assert_eq!(
            check_site_result(Some(0), out, ""),
            Ok(r#"{"topology": "ab", "inputs": 32}"#.to_owned())
        );
        assert_eq!(
            check_site_result(Some(0), "no report", ""),
            Ok(String::new())
        );
        assert_eq!(
            check_site_result(Some(2), out, "  unknown input mic9\n"),
            Err("check-site ended with Some(2): unknown input mic9".to_owned())
        );
        assert_eq!(
            check_site_result(None, "", ""),
            Err("check-site ended with None: ".to_owned())
        );
    }

    #[test]
    fn the_hil_test_signal_names_its_card_outputs() {
        assert_eq!(
            hil_test_signal("mic1", -24.5, 30.0, &[94, 95]),
            json!({
                "op": "hil_test_signal",
                "input": "mic1",
                "hz": 1000.0,
                "dbfs": -24.5,
                "ttl_s": 30.0,
                "card_tx": [94, 95],
            })
        );
        assert_eq!(HIL_HZ, 1000.0);
    }

    #[test]
    fn a_frame_cut_short_or_too_large_is_an_error() {
        let mut cut: &[u8] = b"\x05\x00\x00\x00{\"a\"";
        assert_eq!(
            read_frame(&mut cut).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        let mut head_cut: &[u8] = b"\x05\x00";
        assert_eq!(
            read_frame(&mut head_cut).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        let big = u32::try_from(MAX_FRAME + 1).unwrap().to_le_bytes();
        let mut r: &[u8] = &big;
        assert_eq!(
            read_frame(&mut r).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        // The largest frame is read.
        let mut max = u32::try_from(MAX_FRAME).unwrap().to_le_bytes().to_vec();
        max.extend(std::iter::repeat_n(b' ', MAX_FRAME));
        let mut r: &[u8] = &max;
        assert_eq!(
            read_frame(&mut r).unwrap().map(|b| b.len()),
            Some(MAX_FRAME)
        );
        let mut empty: &[u8] = b"\x00\x00\x00\x00";
        assert_eq!(read_frame(&mut empty).unwrap(), Some(Vec::new()));
    }

    #[test]
    fn an_interrupted_read_is_retried() {
        struct Once(bool, &'static [u8]);
        impl Read for Once {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if !self.0 {
                    self.0 = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                self.1.read(buf)
            }
        }
        let mut r = Once(false, b"\x02\x00\x00\x00{}");
        assert_eq!(read_frame(&mut r).unwrap(), Some(b"{}".to_vec()));
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
        assert_eq!(
            read_frame(&mut Broken).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn read_frame_returns_a_persistent_error_instead_of_looping() {
        // A bounded, deterministic catch for the match-guard -> `true` mutant of
        // `Err(e) if e.kind() == io::ErrorKind::Interrupted` (effects/engine.rs:76).
        // The real code retries only an Interrupted read and returns any other
        // error at once; the mutant treats every error as Interrupted and loops
        // forever on a reader that keeps failing. Run it on a thread and require
        // it to end within a bound: the original returns the error in
        // microseconds; the mutant never returns, so this fails its assertion in
        // 3 s (a clean FAIL) instead of hanging until nextest's slow-timeout
        // (#23, and it runs first under `priority = 100`).
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let kind = read_frame(&mut Broken)
                .map(|o| o.map(|b| b.len()))
                .map_err(|e| e.kind());
            let _ = done_tx.send(kind);
        });
        match done_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(r) => assert_eq!(r, Err(io::ErrorKind::BrokenPipe)),
            Err(_) => panic!("read_frame looped on a persistent non-Interrupted error"),
        }
    }

    fn p(v: Value) -> Msg {
        parse(v.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn engine_messages_are_read_field_by_field() {
        assert_eq!(
            p(
                json!({"type": "hello", "proto": 1, "engine_build": "2.0.0+abc", "role": "supervisor"})
            ),
            Msg::Hello {
                build: "2.0.0+abc".into()
            }
        );
        // The topology and the meters are the engine's to the server: the
        // guard reads no stage (#38).
        assert_eq!(
            p(
                json!({"type": "topology", "hash": "h", "inputs": [{"id": "mic1", "channels": 1}, {"id": "mic2"}]})
            ),
            Msg::Other
        );
        assert_eq!(
            p(
                json!({"type": "status", "callbacks": 3000, "frames": 32, "missed": 2, "resets": 1, "faulted": true, "parked": true, "late": 1})
            ),
            Msg::Status(Status {
                build: String::new(),
                frames: 32,
                callbacks: 3000,
                missed: 2,
                resets: 1,
                faulted: true,
                parked: true,
                hil: Vec::new(),
                loopback_samples: 0,
                late: 1,
                overruns: 0,
                process_max_us: 0.0,
                hist_top_us: 0,
                interval_hist: Vec::new(),
                process_hist: Vec::new(),
            })
        );
        // S7: the soak's figures and both histograms (design note §3).
        assert_eq!(
            p(json!({
                "type": "status", "callbacks": 360_000, "frames": 32, "late": 5, "overruns": 1,
                "process_max_us": 61.5, "hist_top_us": 667,
                "interval_hist": [[333, 359_990], [400, 9]], "process_hist": [[60, 360_000]]
            })),
            Msg::Status(Status {
                frames: 32,
                callbacks: 360_000,
                late: 5,
                overruns: 1,
                process_max_us: 61.5,
                hist_top_us: 667,
                interval_hist: vec![(333, 359_990), (400, 9)],
                process_hist: vec![(60, 360_000)],
                ..Status::default()
            })
        );
        // HIL's spare outputs with their peaks (design §7); a partial or
        // out-of-range entry reads 0, anything but a list none.
        let hil = |outs: serde_json::Value| match p(json!({"type": "status", "hil": outs})) {
            Msg::Status(s) => s.hil,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            hil(json!([{"tx": 94, "peak": 0.0316}, {"tx": 95, "peak": 0.0}])),
            vec![
                HilOut {
                    tx: 94,
                    peak: 0.0316
                },
                HilOut { tx: 95, peak: 0.0 }
            ]
        );
        assert_eq!(
            hil(json!([{"tx": 94}, {"peak": 0.5}, {"tx": 70_000, "peak": "x"}])),
            vec![
                HilOut { tx: 94, peak: 0.0 },
                HilOut { tx: 0, peak: 0.5 },
                HilOut { tx: 0, peak: 0.0 }
            ]
        );
        assert!(hil(json!("x")).is_empty());
        assert!(hil(json!([])).is_empty());
        // An engine before S6: no measured period.
        assert_eq!(
            p(json!({"type": "status", "callbacks": 3000, "faulted": false})),
            Msg::Status(status(0, 3000, 0))
        );
        assert_eq!(
            p(
                json!({"type": "status", "callbacks": 1, "frames": 5_000_000_000_u64, "hist_top_us": 5_000_000_000_u64})
            ),
            Msg::Status(status(0, 1, 0))
        );
        assert_eq!(
            p(json!({"type": "meters", "seq": 4, "inputs": [[0.5, 0.25], [0.0, 0.75], [], "x"]})),
            Msg::Other
        );
        assert_eq!(
            p(json!({"type": "reply", "id": 9, "rev": 3, "error": null})),
            Msg::Reply { id: 9, error: None }
        );
        assert_eq!(
            p(json!({"type": "reply", "id": 9, "rev": 3})),
            Msg::Reply { id: 9, error: None }
        );
        assert_eq!(
            p(json!({"type": "reply", "id": 10, "error": {"code": "forbidden", "msg": "no"}})),
            Msg::Reply {
                id: 10,
                error: Some("no".into())
            }
        );
        assert_eq!(
            p(json!({"type": "reply", "id": 11, "error": {"code": "forbidden"}})),
            Msg::Reply {
                id: 11,
                error: Some("refused".into())
            }
        );
        assert_eq!(
            p(json!({"type": "driver_released", "reason": "shutdown"})),
            Msg::DriverReleased {
                reason: "shutdown".into()
            }
        );
        assert_eq!(
            p(json!({"type": "driver_parked", "reason": "fault"})),
            Msg::DriverParked {
                reason: "fault".into()
            }
        );
        assert_eq!(p(json!({"type": "superseded"})), Msg::Superseded);
        assert_eq!(p(json!({"type": "state", "rev": 1})), Msg::Other);
        assert_eq!(p(json!({"no": "type"})), Msg::Other);
        assert!(
            parse(b"{not json")
                .unwrap_err()
                .starts_with("bad engine message: ")
        );
    }

    /// A histogram is read whole or not at all (S7): one entry that is not a
    /// pair of integers in range makes it none, never part of one (the soak
    /// verdict then names it); an absent one is none (an older engine).
    #[test]
    fn a_histogram_with_a_bad_entry_reads_as_none() {
        let read = |h: Value| match p(
            json!({"type": "status", "interval_hist": h, "process_hist": [[7, 1]]}),
        ) {
            Msg::Status(s) => (s.interval_hist, s.process_hist),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            read(json!([[0, 7], [4_294_967_295_u64, u64::MAX]])),
            (vec![(0, 7), (u32::MAX, u64::MAX)], vec![(7, 1)])
        );
        for bad in [
            json!([[1]]),
            json!([[1, 2, 3]]),
            json!([[-1, 2]]),
            json!([[1, -2]]),
            json!([[4_294_967_296_u64, 1]]),
            json!([["a", 1]]),
            json!([[1, "b"]]),
            json!([[1.5, 2]]),
            json!([7]),
            json!({"a": 1}),
            json!("x"),
            json!(null),
            json!([[333, 2], [1], [400, 9]]),
        ] {
            assert_eq!(read(bad.clone()), (Vec::new(), vec![(7, 1)]), "{bad}");
        }
        match p(json!({"type": "status", "callbacks": 4})) {
            Msg::Status(s) => assert!(s.interval_hist.is_empty() && s.process_hist.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_reply_names_the_engine_by_its_commit() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(commit_of(&format!("2.0.0-dev.9+{sha}")), sha);
        assert_eq!(commit_of("2.0.0+a+b"), "b");
        assert_eq!(commit_of("local"), "local");
        assert_eq!(commit_of(""), "");
        let spare = vec![
            HilOut {
                tx: 94,
                peak: 0.0316,
            },
            HilOut { tx: 95, peak: 0.0 },
        ];
        let seen = EngineSeen {
            status: Status {
                build: format!("2.0.0-dev.9+{sha}"),
                frames: 32,
                callbacks: 360_000,
                missed: 1,
                resets: 2,
                faulted: true,
                parked: true,
                hil: spare.clone(),
                loopback_samples: 129,
                late: 5,
                overruns: 1,
                process_max_us: 61.5,
                hist_top_us: 667,
                interval_hist: vec![(333, 359_990), (400, 9)],
                process_hist: vec![(60, 360_000)],
            },
            pipe_private: true,
        };
        assert_eq!(
            engine_status(&seen, 3, Some(70), Some(4242)),
            EngineStatus {
                build: sha.to_owned(),
                frames: 32,
                callbacks: 360_000,
                missed: 1,
                resets: 2,
                parked: true,
                faulted: true,
                pipe_private: true,
                spawns: 3,
                last_exit: Some(70),
                hil: spare,
                loopback_samples: 129,
                loopback_ms: 129.0 * 1000.0 / 96_000.0,
                pid: Some(4242),
                late: 5,
                overruns: 1,
                process_max_us: 61.5,
                hist_top_us: 667,
                interval_hist: vec![(333, 359_990), (400, 9)],
                process_hist: vec![(60, 360_000)],
            }
        );
        let quiet = EngineSeen {
            pipe_private: false,
            ..EngineSeen::default()
        };
        assert_eq!(
            engine_status(&quiet, 0, None, None),
            EngineStatus {
                build: String::new(),
                ..EngineStatus::default()
            }
        );
    }

    /// The engine's pipe exists before its card opens: until the connection
    /// holds the hello's build and a `Status`, the guard shows no engine
    /// (`Reply.engine` absent), never an empty build with zero counters that
    /// HIL v1 would read as a failed check (lane review).
    #[test]
    fn an_engine_is_seen_only_with_its_hello_and_a_status() {
        let build = "2.0.0-dev.9+0123456789abcdef0123456789abcdef01234567";
        let status = Status {
            frames: 32,
            callbacks: 9_000,
            missed: 1,
            resets: 2,
            ..Status::default()
        };
        assert_eq!(seen_status(None, None), None, "a bare connection");
        assert_eq!(seen_status(Some(build), None), None, "hello, no status yet");
        assert_eq!(seen_status(None, Some(&status)), None, "no hello");
        assert_eq!(
            seen_status(Some(build), Some(&status)),
            Some(Status {
                build: build.to_owned(),
                ..status
            })
        );
    }

    #[test]
    fn the_build_must_name_the_bundle() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert!(build_matches(&format!("2.0.0-dev.9+{sha}"), sha));
        assert!(build_matches(&format!("2.0.0+{}", sha.to_uppercase()), sha));
        assert!(!build_matches("2.0.0+local", sha));
        assert!(!build_matches(sha, sha));
        assert!(!build_matches(&format!("2.0.0+{sha}x"), sha));
        assert!(!build_matches("2.0.0+", ""));
        assert!(!build_matches("", ""));
    }

    #[test]
    fn the_window_opens_on_the_first_measured_callbacks_and_closes_after_its_length() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut w = ReadyWindow::new(10);
        assert_eq!(w.observe(&status(0, 0, 0), at(0)), Ready::Wait);
        assert_eq!(w.observe(&status(0, 100, 0), at(1)), Ready::Wait);
        assert_eq!(w.observe(&status(32, 3000, 1), at(2)), Ready::Wait);
        assert_eq!(w.observe(&status(32, 6000, 1), at(5)), Ready::Wait);
        let just_before = at(12) - Duration::from_nanos(1);
        assert_eq!(w.observe(&status(32, 9000, 1), just_before), Ready::Wait);
        assert_eq!(w.observe(&status(32, 12000, 1), at(12)), Ready::Done);
    }

    #[test]
    fn one_warm_up_miss_restarts_the_window_once() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut w = ReadyWindow::new(10);
        assert_eq!(w.observe(&status(32, 1000, 0), at(0)), Ready::Wait);
        assert_eq!(w.observe(&status(32, 2000, 1), at(1)), Ready::Wait);
        // The window starts again at 1 s.
        assert_eq!(w.observe(&status(32, 3000, 1), at(10)), Ready::Wait);
        assert_eq!(w.observe(&status(32, 4000, 1), at(11)), Ready::Done);
        let mut w = ReadyWindow::new(10);
        assert_eq!(w.observe(&status(32, 1000, 0), at(0)), Ready::Wait);
        assert_eq!(w.observe(&status(32, 2000, 1), at(1)), Ready::Wait);
        assert_eq!(
            w.observe(&status(32, 3000, 3), at(2)),
            Ready::Failed("2 periods missed after the warm-up restart".into())
        );
    }

    #[test]
    fn a_wrong_period_a_stall_a_fault_or_a_park_fails_at_once() {
        let t0 = Instant::now();
        let mut w = ReadyWindow::new(10);
        assert_eq!(
            w.observe(&status(64, 1000, 0), t0),
            Ready::Failed("measured period 64 frames, not 32".into())
        );
        let mut w = ReadyWindow::new(10);
        assert_eq!(w.observe(&status(32, 1000, 0), t0), Ready::Wait);
        assert_eq!(
            w.observe(&status(32, 1000, 0), t0 + Duration::from_secs(1)),
            Ready::Failed("callbacks stopped at 1000".into())
        );
        let mut w = ReadyWindow::new(10);
        assert_eq!(w.observe(&status(32, 1000, 0), t0), Ready::Wait);
        assert_eq!(
            w.observe(&status(32, 999, 0), t0 + Duration::from_secs(1)),
            Ready::Failed("callbacks stopped at 999".into())
        );
        let faulted = Status {
            faulted: true,
            ..status(32, 1000, 0)
        };
        assert_eq!(
            ReadyWindow::new(10).observe(&faulted, t0),
            Ready::Failed("the engine faulted".into())
        );
        let parked = Status {
            parked: true,
            ..status(0, 0, 0)
        };
        assert_eq!(
            ReadyWindow::new(10).observe(&parked, t0),
            Ready::Failed("the engine parked its stream".into())
        );
        // A zero-length window is done on the status after the first.
        let mut w = ReadyWindow::new(0);
        assert_eq!(w.observe(&status(32, 1, 0), t0), Ready::Wait);
        assert_eq!(w.observe(&status(32, 2, 0), t0), Ready::Done);
    }

    #[test]
    fn health_needs_advancing_callbacks_without_fault_or_park() {
        let a = status(32, 1000, 0);
        assert_eq!(health(&a, &status(32, 1001, 0)), Health::Healthy);
        assert_eq!(health(&a, &status(32, 1000, 0)), Health::Dead);
        assert_eq!(health(&a, &status(32, 999, 0)), Health::Dead);
        let faulted = Status {
            faulted: true,
            ..status(32, 2000, 0)
        };
        assert_eq!(health(&a, &faulted), Health::Dead);
        let parked = Status {
            parked: true,
            ..status(32, 2000, 0)
        };
        assert_eq!(health(&a, &parked), Health::Parked);
        assert_eq!(health(&parked, &status(32, 3000, 0)), Health::Parked);
        // A faulted first status does not matter once the second advances.
        let first_faulted = Status {
            faulted: true,
            ..a.clone()
        };
        assert_eq!(
            health(&first_faulted, &status(32, 2000, 0)),
            Health::Healthy
        );
    }
}
