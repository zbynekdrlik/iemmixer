//! The engine's supervisor pipe (S6 design note §4, §5.2): what the guard
//! sends, what it reads, and the decisions on it. Frames are the engine's
//! (a little-endian `u32` length, then JSON); messages are read field by
//! field, so any engine of protocol 1 whose `Status` carries the S6 fields
//! (`frames`, `missed`, `parked`) serves, and an older one is simply not
//! ready (its measured period reads 0).

use std::io::{self, Read};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::handover::FLOOR_DB;
use crate::pc::{EngineSeen, Status};
use crate::plan::Health;
use crate::proto::EngineStatus;
use crate::site::FRAMES;

/// The engine protocol the guard speaks.
pub const PROTO: u64 = 1;
/// The engine's largest frame body (`iem_engine_proto::MAX_FRAME`).
pub const MAX_FRAME: usize = 1 << 20;
/// Band activity (program spec §4.2): a stage peak above this is playing.
pub const ACTIVE_DB: f64 = -50.0;

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

/// What the guard reads from the engine.
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Hello {
        build: String,
    },
    /// The input ids in topology order (the order of `Meters.inputs`).
    Topology {
        inputs: Vec<String>,
    },
    /// `build` is empty: it comes from the hello.
    Status(Status),
    /// Each input's peak, the louder of its two channels (linear).
    Meters {
        inputs: Vec<f64>,
    },
    Reply {
        id: u64,
        error: Option<String>,
    },
    DriverReleased {
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

fn louder(pair: &Value) -> f64 {
    pair.as_array()
        .map(|ch| ch.iter().filter_map(Value::as_f64).fold(0.0, f64::max))
        .unwrap_or(0.0)
}

/// Parses one engine message.
pub fn parse(body: &[u8]) -> Result<Msg, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("bad engine message: {e}"))?;
    let inputs = v.get("inputs").and_then(Value::as_array);
    Ok(
        match v.get("type").and_then(Value::as_str).unwrap_or_default() {
            "hello" => Msg::Hello {
                build: text(&v, "engine_build"),
            },
            "topology" => Msg::Topology {
                inputs: inputs
                    .map(|a| a.iter().map(|i| text(i, "id")).collect())
                    .unwrap_or_default(),
            },
            "status" => Msg::Status(Status {
                build: String::new(),
                frames: u32::try_from(number(&v, "frames")).unwrap_or(0),
                callbacks: number(&v, "callbacks"),
                missed: number(&v, "missed"),
                resets: number(&v, "resets"),
                faulted: flag(&v, "faulted"),
                parked: flag(&v, "parked"),
            }),
            "meters" => Msg::Meters {
                inputs: inputs
                    .map(|a| a.iter().map(louder).collect())
                    .unwrap_or_default(),
            },
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
pub fn seen_status(build: Option<&str>, status: Option<&Status>) -> Option<Status> {
    let mut seen = status.cloned().unwrap_or_default();
    seen.build = build.unwrap_or_default().to_owned();
    Some(seen)
}

/// The guard's `Reply.engine` (design §7, what HIL v1 reads through
/// `iemmode status`): the engine as the supervisor connection saw it, its
/// build as the bare commit (the bundle's SHA), with the engine starts of
/// this guard and the exit code of the engine before the running one.
pub fn engine_status(seen: &EngineSeen, spawns: u64, last_exit: Option<i32>) -> EngineStatus {
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

/// Where a `Shutdown` request stands (design §5.2 "back to event" step 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shutdown {
    /// `DriverReleased` came, with its reason.
    Released(String),
    /// The engine refused the request (its reply carried an error).
    Refused(String),
}

/// `released`: the reason of a `DriverReleased`; `reply`: the request's
/// reply, `None` not yet, `Some(None)` accepted, `Some(Some(error))`
/// refused. The release ends the wait, and so does a refusal; an accepted
/// request waits on for the release (`None`).
pub fn shutdown(released: Option<&str>, reply: Option<Option<String>>) -> Option<Shutdown> {
    match (released, reply) {
        (Some(reason), _) => Some(Shutdown::Released(reason.to_owned())),
        (None, Some(Some(error))) => Some(Shutdown::Refused(error)),
        (None, Some(None) | None) => None,
    }
}

/// The stage inputs' positions in the topology's input order, and the ids
/// the topology lacks.
pub fn stage_indices(topology: &[String], stage: &[String]) -> (Vec<usize>, Vec<String>) {
    let mut found = Vec::new();
    let mut unknown = Vec::new();
    for id in stage {
        match topology.iter().position(|t| t == id) {
            Some(i) => found.push(i),
            None => unknown.push(id.clone()),
        }
    }
    (found, unknown)
}

/// A linear peak in dBFS, floored at −150 dBFS (silence, and anything that
/// is not a number).
pub fn peak_db(lin: f64) -> f64 {
    (20.0 * lin.log10()).max(FLOOR_DB)
}

/// The loudest stage peak of one meter frame (dBFS).
pub fn stage_max(inputs: &[f64], stage: &[usize]) -> f64 {
    stage
        .iter()
        .filter_map(|i| inputs.get(*i))
        .map(|l| peak_db(*l))
        .fold(FLOOR_DB, f64::max)
}

/// Per stage input, the loudest peak (dBFS) since the last reset.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StagePeaks {
    loudest: Vec<f64>,
    frames: u64,
}

impl StagePeaks {
    pub fn reset(&mut self, inputs: usize) {
        self.loudest = vec![FLOOR_DB; inputs];
        self.frames = 0;
    }

    pub fn observe(&mut self, inputs: &[f64], stage: &[usize]) {
        for (l, i) in self.loudest.iter_mut().zip(stage) {
            if let Some(v) = inputs.get(*i) {
                *l = l.max(peak_db(*v));
            }
        }
        self.frames += 1;
    }

    pub fn loudest(&self) -> &[f64] {
        &self.loudest
    }

    /// Meter frames seen since the last reset.
    pub fn frames(&self) -> u64 {
        self.frames
    }
}

/// How long the stage has been quiet, from the meter frames the guard saw
/// (a HIL job needs 5 min, design §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quiet {
    since: Instant,
}

impl Quiet {
    /// Quiet from `now` on: nothing earlier was seen.
    pub fn new(now: Instant) -> Self {
        Self { since: now }
    }

    /// One frame's loudest stage peak (dBFS) at `now`.
    pub fn observe(&mut self, loudest_db: f64, now: Instant) {
        if loudest_db > ACTIVE_DB {
            self.since = now;
        }
    }

    pub fn quiet_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.since)
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

/// `iem-engine interlock` (design §4): exit 0 quiet, 5 activity; anything
/// else is a failed check (6: its stop file ended it). Its report is the
/// last JSON line it printed.
pub fn interlock_result(code: Option<i32>, stdout: &str) -> Result<(bool, String), String> {
    let report = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with('{'))
        .unwrap_or_default()
        .to_owned();
    match code {
        Some(0) => Ok((true, report)),
        Some(5) => Ok((false, report)),
        Some(3) => Err(format!("the card refused the interlock: {report}")),
        Some(6) => Err(format!(
            "the interlock was stopped by its stop file: {report}"
        )),
        other => Err(format!("the interlock ended with {other:?}: {report}")),
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
        }
    }

    #[test]
    fn a_shutdown_waits_for_the_release() {
        assert_eq!(shutdown(None, None), None);
        assert_eq!(shutdown(None, Some(None)), None);
        assert_eq!(
            shutdown(Some("shutdown"), None),
            Some(Shutdown::Released("shutdown".into()))
        );
        assert_eq!(
            shutdown(Some("shutdown"), Some(None)),
            Some(Shutdown::Released("shutdown".into()))
        );
        // The release wins over a late refusal.
        assert_eq!(
            shutdown(Some("shutdown"), Some(Some("forbidden".into()))),
            Some(Shutdown::Released("shutdown".into()))
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
            hil_test_signal("mic1", -24.5, 30.0, &[71, 72]),
            json!({
                "op": "hil_test_signal",
                "input": "mic1",
                "hz": 1000.0,
                "dbfs": -24.5,
                "ttl_s": 30.0,
                "card_tx": [71, 72],
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
        assert_eq!(
            p(
                json!({"type": "topology", "hash": "h", "inputs": [{"id": "mic1", "channels": 1}, {"id": "mic2"}]})
            ),
            Msg::Topology {
                inputs: vec!["mic1".into(), "mic2".into()]
            }
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
            })
        );
        // An engine before S6: no measured period.
        assert_eq!(
            p(json!({"type": "status", "callbacks": 3000, "faulted": false})),
            Msg::Status(status(0, 3000, 0))
        );
        assert_eq!(
            p(json!({"type": "status", "callbacks": 1, "frames": 5_000_000_000_u64})),
            Msg::Status(status(0, 1, 0))
        );
        assert_eq!(
            p(json!({"type": "meters", "seq": 4, "inputs": [[0.5, 0.25], [0.0, 0.75], [], "x"]})),
            Msg::Meters {
                inputs: vec![0.5, 0.75, 0.0, 0.0]
            }
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
        assert_eq!(p(json!({"type": "superseded"})), Msg::Superseded);
        assert_eq!(p(json!({"type": "state", "rev": 1})), Msg::Other);
        assert_eq!(p(json!({"no": "type"})), Msg::Other);
        assert_eq!(
            p(json!({"type": "topology"})),
            Msg::Topology { inputs: vec![] }
        );
        assert_eq!(p(json!({"type": "meters"})), Msg::Meters { inputs: vec![] });
        assert!(
            parse(b"{not json")
                .unwrap_err()
                .starts_with("bad engine message: ")
        );
    }

    #[test]
    fn the_reply_names_the_engine_by_its_commit() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(commit_of(&format!("2.0.0-dev.9+{sha}")), sha);
        assert_eq!(commit_of("2.0.0+a+b"), "b");
        assert_eq!(commit_of("local"), "local");
        assert_eq!(commit_of(""), "");
        let seen = EngineSeen {
            status: Status {
                build: format!("2.0.0-dev.9+{sha}"),
                frames: 32,
                callbacks: 360_000,
                missed: 1,
                resets: 2,
                faulted: true,
                parked: true,
            },
            pipe_private: true,
        };
        assert_eq!(
            engine_status(&seen, 3, Some(70)),
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
            }
        );
        let quiet = EngineSeen {
            pipe_private: false,
            ..EngineSeen::default()
        };
        assert_eq!(
            engine_status(&quiet, 0, None),
            EngineStatus {
                build: String::new(),
                ..EngineStatus::default()
            }
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

    #[test]
    fn stage_inputs_are_found_in_topology_order() {
        let topo: Vec<String> = ["prog", "mic1", "mic2", "hand1"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let stage: Vec<String> = ["hand1", "mic1", "keys"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        assert_eq!(
            stage_indices(&topo, &stage),
            (vec![3, 1], vec!["keys".to_owned()])
        );
        assert_eq!(stage_indices(&topo, &[]), (vec![], vec![]));
    }

    #[test]
    fn peaks_are_decibels_floored_at_silence() {
        assert_eq!(peak_db(1.0), 0.0);
        assert!((peak_db(0.1) + 20.0).abs() < 1e-9);
        assert!((peak_db(0.01) + 40.0).abs() < 1e-9);
        assert!((peak_db(2.0) - 6.020_599_913_279_624).abs() < 1e-9);
        assert_eq!(peak_db(0.0), FLOOR_DB);
        assert_eq!(peak_db(1e-9), FLOOR_DB);
        assert_eq!(peak_db(-0.5), FLOOR_DB);
        assert_eq!(peak_db(f64::NAN), FLOOR_DB);
    }

    #[test]
    fn the_loudest_stage_input_counts() {
        let frame = [1.0, 0.1, 0.01, 0.5];
        assert!((stage_max(&frame, &[1, 2]) + 20.0).abs() < 1e-9);
        assert_eq!(stage_max(&frame, &[0]), 0.0);
        assert_eq!(stage_max(&frame, &[]), FLOOR_DB);
        assert_eq!(stage_max(&frame, &[9]), FLOOR_DB);
    }

    #[test]
    fn stage_peaks_keep_each_inputs_loudest_since_the_reset() {
        let mut s = StagePeaks::default();
        assert_eq!((s.loudest(), s.frames()), (&[][..], 0));
        s.reset(2);
        assert_eq!(s.loudest(), [FLOOR_DB, FLOOR_DB]);
        s.observe(&[0.01, 0.1, 1.0], &[2, 0]);
        s.observe(&[0.1, 0.1, 0.001], &[2, 0]);
        assert_eq!(s.frames(), 2);
        let l = s.loudest();
        assert_eq!(l.first(), Some(&0.0));
        assert!((l.get(1).unwrap() + 20.0).abs() < 1e-9);
        // An input missing from a frame keeps its value.
        s.observe(&[], &[2, 0]);
        assert_eq!(s.frames(), 3);
        assert_eq!(s.loudest().first(), Some(&0.0));
        s.reset(1);
        assert_eq!((s.loudest(), s.frames()), (&[FLOOR_DB][..], 0));
    }

    #[test]
    fn quiet_time_restarts_on_any_stage_peak_above_the_activity_level() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut q = Quiet::new(t0);
        assert_eq!(q.quiet_for(at(0)), Duration::ZERO);
        q.observe(-60.0, at(1000));
        q.observe(ACTIVE_DB, at(2000));
        assert_eq!(q.quiet_for(at(3000)), Duration::from_secs(3));
        q.observe(-49.9, at(4000));
        assert_eq!(q.quiet_for(at(4500)), Duration::from_millis(500));
        // A clock read before the last peak is no quiet at all.
        assert_eq!(q.quiet_for(at(3000)), Duration::ZERO);
    }

    #[test]
    fn the_interlock_reports_quiet_activity_or_failure() {
        let out = "starting\n{\"quiet\": true, \"loudest\": []}\n";
        assert_eq!(
            interlock_result(Some(0), out),
            Ok((true, "{\"quiet\": true, \"loudest\": []}".into()))
        );
        let loud = "{\"quiet\": false}\n  {\"quiet\": false, \"loudest\": [[101, -20.0]]}  \nbye\n";
        assert_eq!(
            interlock_result(Some(5), loud),
            Ok((
                false,
                "{\"quiet\": false, \"loudest\": [[101, -20.0]]}".into()
            ))
        );
        assert_eq!(
            interlock_result(Some(3), ""),
            Err("the card refused the interlock: ".into())
        );
        assert_eq!(
            interlock_result(Some(6), "{\"stopped\": true}"),
            Err("the interlock was stopped by its stop file: {\"stopped\": true}".into())
        );
        assert_eq!(
            interlock_result(Some(1), "{}"),
            Err("the interlock ended with Some(1): {}".into())
        );
        assert_eq!(
            interlock_result(None, ""),
            Err("the interlock ended with None: ".into())
        );
    }
}
