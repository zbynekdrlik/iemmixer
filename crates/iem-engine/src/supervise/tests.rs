use std::io;
use std::path::PathBuf;

use iem_engine_proto::{ErrorBody, Hello, HilOut, Meters, Reply, read_frame};
use serde_json::Value;

use super::*;

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

fn ask(s: &str) -> Result<Supervise, String> {
    parse(&words(s))
}

fn err(s: &str) -> String {
    ask(s).unwrap_err()
}

fn burst(input: &str, dbfs: f64, seconds: u32, listen: bool) -> Ask {
    Ask::TestSignal {
        input: InputId::new(input),
        dbfs,
        seconds,
        listen,
    }
}

#[test]
fn the_commands_parse_with_the_box_pipe_unless_one_is_given() {
    assert_eq!(
        ask("test-signal mic1 -30 30"),
        Ok(Supervise {
            pipe: PIPE.to_owned(),
            ask: burst("mic1", -30.0, 30, false),
        })
    );
    assert_eq!(
        ask("--pipe /tmp/e.sock test-signal hand_1.b-2 -20.5 1 --listen"),
        Ok(Supervise {
            pipe: "/tmp/e.sock".to_owned(),
            ask: burst("hand_1.b-2", -20.5, 1, true),
        })
    );
    assert_eq!(
        ask("stop-test-signal"),
        Ok(Supervise {
            pipe: PIPE.to_owned(),
            ask: Ask::StopTestSignal,
        })
    );
    assert_eq!(
        ask("--pipe p status"),
        Ok(Supervise {
            pipe: "p".to_owned(),
            ask: Ask::Status,
        })
    );
}

/// The guard's caps (`daemon::hil::test_signal`) with its words: the
/// ceiling and the longest TTL are allowed, a step past either is not.
#[test]
fn the_caps_are_the_guards() {
    for (line, dbfs, seconds) in [
        ("test-signal mic1 -20 60", -20.0, 60),
        ("test-signal mic1 -120 1", -120.0, 1),
    ] {
        assert_eq!(
            ask(line).map(|a| a.ask),
            Ok(burst("mic1", dbfs, seconds, false))
        );
    }
    assert_eq!(
        err("test-signal mic1 -19.5 30"),
        "-19.5 dBFS is above the HIL ceiling of -20 dBFS"
    );
    assert_eq!(
        err("test-signal mic1 0 30"),
        "0 dBFS is above the HIL ceiling of -20 dBFS"
    );
    assert_eq!(
        err("test-signal mic1 -30 61"),
        "a TTL of 61 s is above the HIL limit of 60 s"
    );
    assert_eq!(
        err("test-signal mic1 -30 0"),
        "a TTL of 0 s is not a positive time"
    );
    assert_eq!(err("test-signal mic1 loud 30"), "\"loud\" is not a number");
    assert_eq!(err("test-signal mic1 NaN 30"), "\"NaN\" is not a number");
    assert_eq!(err("test-signal mic1 -inf 30"), "\"-inf\" is not a number");
    assert_eq!(
        err("test-signal mic1 -30 2.5"),
        "\"2.5\" is not a whole number"
    );
    assert_eq!(
        err("test-signal mic1 -30 -1"),
        "\"-1\" is not a whole number"
    );
    // The guard's ceiling is the engine's own X13 cap.
    assert_eq!(HIL_MAX_DBFS, crate::params::TEST_DBFS.1);
}

/// An input id as the protocol reads one (`valid_id`).
#[test]
fn the_input_is_an_id() {
    for bad in ["Mic1", "-mic", "mic 1", "mic/1"] {
        let line = vec![
            "test-signal".to_owned(),
            bad.to_owned(),
            "-30".to_owned(),
            "5".to_owned(),
        ];
        let e = parse(&line).unwrap_err();
        assert!(e.starts_with(&format!("{bad:?} is not an input id")), "{e}");
    }
    let long = "m".repeat(65);
    assert!(
        err(&format!("test-signal {long} -30 5")).contains("is not an input id"),
        "65 bytes"
    );
    let most = "m".repeat(64);
    assert!(
        ask(&format!("test-signal {most} -30 5")).is_ok(),
        "64 bytes"
    );
}

/// `--listen` only as `test-signal`'s last word, as the guard's
/// `iemmode test-signal` takes it; anywhere else it is a fourth value.
#[test]
fn the_command_line_is_exact() {
    for (line, want) in [
        (
            "test-signal --listen mic1 -20 30",
            "3 arguments expected, 4 given",
        ),
        (
            "test-signal mic1 -20 --listen 30",
            "3 arguments expected, 4 given",
        ),
        (
            "test-signal mic1 -20 30 --loud",
            "3 arguments expected, 4 given",
        ),
        (
            "test-signal mic1 -20 30 --listen --listen",
            "3 arguments expected, 4 given",
        ),
        (
            "test-signal mic1 -20 --listen",
            "3 arguments expected, 2 given",
        ),
        ("test-signal mic1", "3 arguments expected, 1 given"),
        ("stop-test-signal now", "0 arguments expected, 1 given"),
        ("status --listen", "0 arguments expected, 1 given"),
        (
            "",
            "supervise needs test-signal, stop-test-signal or status",
        ),
        (
            "--pipe p",
            "supervise needs test-signal, stop-test-signal or status",
        ),
        ("--pipe", "--pipe needs a value"),
        ("arm", "unknown supervise command \"arm\""),
        ("status --pipe p", "0 arguments expected, 2 given"),
    ] {
        assert_eq!(err(line), want, "{line:?}");
    }
}

/// `iem-engine supervise` reaches here through the engine's parser.
#[test]
fn the_engine_parser_hands_supervise_its_words() {
    use crate::engine::{Command, parse_args};
    assert_eq!(
        parse_args(&words("supervise --pipe p test-signal mic1 -30 5 --listen")),
        Ok(Command::Supervise(Supervise {
            pipe: "p".to_owned(),
            ask: burst("mic1", -30.0, 5, true),
        }))
    );
    assert_eq!(
        parse_args(&words("supervise test-signal mic1 -10 5")),
        Err("-10 dBFS is above the HIL ceiling of -20 dBFS".to_owned())
    );
    assert_eq!(
        parse_args(&words("supervise")),
        Err("supervise needs test-signal, stop-test-signal or status".to_owned())
    );
}

/// The box unit's `--pipe` is the default (`scripts/gen3/iem-engine.service`).
#[test]
fn the_default_pipe_is_the_box_units() {
    let unit =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/gen3/iem-engine.service");
    let unit = std::fs::read_to_string(unit).unwrap();
    assert!(unit.contains(&format!(" --pipe {PIPE} ")), "{unit}");
}

#[test]
fn a_failure_exits_1_and_off_unix_2() {
    assert_eq!(Failure::NotHere.code(), 2);
    for f in [
        Failure::Connect("entity not found".into()),
        Failure::Pipe("broken pipe".into()),
        Failure::Closed,
        Failure::Superseded,
        Failure::NoStatus(Duration::from_secs(3)),
        Failure::NoReply(Duration::from_secs(5)),
        Failure::Refused("forbidden".into()),
    ] {
        assert_eq!(f.code(), 1, "{f}");
    }
    assert_eq!(
        Failure::Refused("forbidden".into()).to_string(),
        "the engine refused it: forbidden"
    );
    assert_eq!(
        Failure::NoReply(Duration::from_secs(5)).to_string(),
        "no answer from the engine within 5s"
    );
}

/// The line is the outcome's numbers and fixed words.
#[test]
fn each_outcome_is_one_json_line() {
    let read = |o: Outcome| -> Value {
        let line = o.line();
        assert!(!line.contains('\n'), "{line}");
        serde_json::from_str(&line).unwrap()
    };
    assert_eq!(
        read(Outcome::TestSignal {
            rev: 7,
            outputs: 2,
            dbfs: -30.5,
            seconds: 30,
            listen: true,
        }),
        json!({"test_signal": {"rev": 7, "outputs": 2, "dbfs": -30.5, "seconds": 30, "listen": true}})
    );
    assert_eq!(
        read(Outcome::Stopped { rev: 8 }),
        json!({"stop_test_signal": {"rev": 8}})
    );
    let status = Status {
        callbacks: 9,
        hil: vec![HilOut { tx: 94, peak: 0.5 }],
        loopback_samples: 32,
        ..Status::default()
    };
    let v = read(Outcome::Status(Box::new(status.clone())));
    assert_eq!(
        serde_json::from_value::<Status>(v["status"].clone()).unwrap(),
        status
    );
    assert_eq!(v["status"]["callbacks"], 9);
}

/// The engine's side of one call as a script: the frames it sends, then the
/// end of the stream (`ends`) or silence (a read that times out); what the
/// client writes is kept.
struct Script {
    input: Vec<u8>,
    at: usize,
    ends: bool,
    written: Vec<u8>,
    /// Writes fail with this (a closed peer).
    refuse_writes: Option<io::ErrorKind>,
}

impl Script {
    fn of(msgs: &[EngineMsg], ends: bool) -> Self {
        let mut input = Vec::new();
        for m in msgs {
            write_frame(&mut input, m).unwrap();
        }
        Self {
            input,
            at: 0,
            ends,
            written: Vec::new(),
            refuse_writes: None,
        }
    }

    /// Raw bytes after the frames.
    fn then_raw(mut self, bytes: &[u8]) -> Self {
        self.input.extend_from_slice(bytes);
        self
    }

    /// What the client wrote, message by message.
    fn sent(&self) -> Vec<ClientMsg> {
        let mut r = self.written.as_slice();
        let mut out = Vec::new();
        let mut buf = Vec::new();
        while read_frame(&mut r, &mut buf).is_ok() {
            out.push(serde_json::from_slice(&buf).unwrap());
        }
        out
    }
}

impl Read for Script {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let rest = &self.input[self.at..];
        if rest.is_empty() {
            if self.ends {
                return Ok(0);
            }
            // A socket's receive timeout.
            std::thread::sleep(Duration::from_millis(1));
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let n = rest.len().min(out.len());
        out[..n].copy_from_slice(&rest[..n]);
        self.at += n;
        Ok(n)
    }
}

impl Write for Script {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(kind) = self.refuse_writes {
            return Err(kind.into());
        }
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

const SHORT: Waits = Waits {
    status: Duration::from_millis(100),
    reply: Duration::from_millis(100),
};

fn hello() -> EngineMsg {
    EngineMsg::Hello(Hello {
        proto: PROTO,
        engine_build: "test".into(),
        topology_hash: "h".into(),
        state_rev: 3,
        sample_rate: 96_000,
        block: 32,
        role: Role::Supervisor,
    })
}

fn status(callbacks: u64, spare: &[u16]) -> EngineMsg {
    EngineMsg::Status(Status {
        callbacks,
        hil: spare.iter().map(|&tx| HilOut { tx, peak: 0.0 }).collect(),
        ..Status::default()
    })
}

fn reply(id: u64, rev: u64, error: Option<ErrCode>) -> EngineMsg {
    EngineMsg::Reply(Reply {
        id,
        rev,
        error: error.map(|code| ErrorBody {
            code,
            msg: "the engine's own words".into(),
        }),
    })
}

fn supervisor_hello() -> ClientMsg {
    ClientMsg::Hello {
        proto: PROTO,
        role: Role::Supervisor,
        client: CLIENT.to_owned(),
    }
}

fn request_of(cmd: Cmd) -> ClientMsg {
    ClientMsg::Request {
        id: 1,
        origin: None,
        cmd,
    }
}

/// The burst goes to every spare output the engine's first `Status` names,
/// in its order, at the asked level and length, the guard's 1 kHz.
#[test]
fn the_burst_goes_to_every_spare_output_the_status_names() {
    for listen in [false, true] {
        let mut s = Script::of(&[hello(), status(1, &[94, 95]), reply(1, 7, None)], false);
        let got = exchange(&mut s, &burst("mic1", -30.0, 30, listen), SHORT);
        assert_eq!(
            got,
            Ok(Outcome::TestSignal {
                rev: 7,
                outputs: 2,
                dbfs: -30.0,
                seconds: 30,
                listen,
            })
        );
        assert_eq!(
            s.sent(),
            vec![
                supervisor_hello(),
                request_of(Cmd::HilTestSignal {
                    input: InputId::new("mic1"),
                    hz: 1000.0,
                    dbfs: -30.0,
                    ttl_s: 30.0,
                    card_tx: vec![94, 95],
                    listen,
                }),
            ],
            "listen {listen}"
        );
    }
}

/// The engine decides: without its test-signal flag its `Status` names no
/// spare output, the burst names none, and the engine's refusal is the
/// call's (its code only, never its text).
#[test]
fn a_refusal_names_the_engines_code() {
    let mut s = Script::of(
        &[
            hello(),
            status(1, &[]),
            reply(1, 3, Some(ErrCode::Forbidden)),
        ],
        false,
    );
    let got = exchange(&mut s, &burst("mic1", -30.0, 5, false), SHORT);
    assert_eq!(got, Err(Failure::Refused("forbidden".into())));
    let Some(ClientMsg::Request {
        cmd: Cmd::HilTestSignal { card_tx, .. },
        ..
    }) = s.sent().pop()
    else {
        panic!("{:?}", s.sent())
    };
    assert!(card_tx.is_empty());
    let mut s = Script::of(&[reply(1, 3, Some(ErrCode::BadValue))], false);
    assert_eq!(
        exchange(&mut s, &Ask::StopTestSignal, SHORT),
        Err(Failure::Refused("bad_value".into()))
    );
}

/// `stop-test-signal` sends `StopTestSignal` at once; `status` sends `Ping`
/// and prints the first `Status` after its answer.
#[test]
fn stop_sends_one_command_and_status_reads_the_next_status() {
    let mut s = Script::of(&[hello(), reply(1, 4, None)], false);
    assert_eq!(
        exchange(&mut s, &Ask::StopTestSignal, SHORT),
        Ok(Outcome::Stopped { rev: 4 })
    );
    assert_eq!(
        s.sent(),
        vec![supervisor_hello(), request_of(Cmd::StopTestSignal)]
    );
    let mut s = Script::of(
        &[
            hello(),
            status(1, &[94]),
            reply(1, 4, None),
            status(2, &[94]),
        ],
        false,
    );
    let Ok(Outcome::Status(got)) = exchange(&mut s, &Ask::Status, SHORT) else {
        panic!()
    };
    assert_eq!(got.callbacks, 2, "the Status after the answer");
    assert_eq!(s.sent(), vec![supervisor_hello(), request_of(Cmd::Ping)]);
}

/// Only the answer to its own request counts; a message this build cannot
/// read is passed over (additive protocol).
#[test]
fn only_its_own_answer_counts_and_unknown_messages_are_passed_over() {
    let unknown = br#"{"type":"something_newer","x":1}"#;
    let mut framed = (unknown.len() as u32).to_le_bytes().to_vec();
    framed.extend_from_slice(unknown);
    let mut s = Script::of(&[hello()], false).then_raw(&framed);
    let mut tail = Vec::new();
    write_frame(&mut tail, &reply(1, 6, None)).unwrap();
    s = s.then_raw(&tail);
    assert_eq!(
        exchange(&mut s, &Ask::StopTestSignal, SHORT),
        Ok(Outcome::Stopped { rev: 6 })
    );
    let mut s = Script::of(&[hello(), reply(0, 6, None), reply(2, 6, None)], false);
    assert_eq!(
        exchange(&mut s, &Ask::StopTestSignal, SHORT),
        Err(Failure::NoReply(SHORT.reply))
    );
}

/// Every way a call ends without its answer, each bounded.
#[test]
fn a_call_ends_on_silence_an_end_a_new_supervisor_or_a_broken_pipe() {
    // Silence: no Status, then no answer, within their waits.
    let mut s = Script::of(&[hello()], false);
    let t0 = Instant::now();
    assert_eq!(
        exchange(&mut s, &burst("mic1", -30.0, 5, false), SHORT),
        Err(Failure::NoStatus(SHORT.status))
    );
    assert!(t0.elapsed() >= SHORT.status, "{:?}", t0.elapsed());
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
    let mut s = Script::of(&[hello(), status(1, &[94])], false);
    assert_eq!(
        exchange(&mut s, &Ask::Status, SHORT),
        Err(Failure::NoReply(SHORT.reply))
    );
    let mut s = Script::of(&[hello(), reply(1, 1, None), status(1, &[94])], false);
    assert!(exchange(&mut s, &Ask::Status, SHORT).is_ok());
    let mut s = Script::of(&[hello(), reply(1, 1, None)], false);
    assert_eq!(
        exchange(&mut s, &Ask::Status, SHORT),
        Err(Failure::NoStatus(SHORT.status))
    );
    // The engine ended the connection.
    let mut s = Script::of(&[hello()], true);
    assert_eq!(
        exchange(&mut s, &burst("mic1", -30.0, 5, false), SHORT),
        Err(Failure::Closed)
    );
    // A supervisor that said hello later replaced this one.
    let mut s = Script::of(&[hello(), EngineMsg::Superseded], true);
    assert_eq!(
        exchange(&mut s, &Ask::StopTestSignal, SHORT),
        Err(Failure::Superseded)
    );
    // A frame the engine never sends.
    let mut s = Script::of(&[hello()], false).then_raw(&u32::MAX.to_le_bytes());
    assert_eq!(
        exchange(&mut s, &Ask::StopTestSignal, SHORT),
        Err(Failure::Pipe("a frame over the limit".into()))
    );
    // The hello cannot be written.
    let mut s = Script::of(&[hello(), reply(1, 1, None)], false);
    s.refuse_writes = Some(io::ErrorKind::BrokenPipe);
    assert_eq!(
        exchange(&mut s, &Ask::StopTestSignal, SHORT),
        Err(Failure::Pipe("broken pipe".into()))
    );
}

/// An engine that sends meter frames without end and never answers.
struct Chatter {
    frame: Vec<u8>,
    at: usize,
}

impl Read for Chatter {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        for b in out.iter_mut() {
            *b = self.frame[self.at];
            self.at = (self.at + 1) % self.frame.len();
        }
        Ok(out.len())
    }
}

impl Write for Chatter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The engine's steady meter frames never stretch a wait: the deadline is
/// looked at before every message.
#[test]
fn an_engine_that_only_chatters_ends_the_wait_in_time() {
    let mut frame = Vec::new();
    write_frame(&mut frame, &EngineMsg::Meters(Meters::default())).unwrap();
    let mut s = Chatter { frame, at: 0 };
    let t0 = Instant::now();
    assert_eq!(
        exchange(&mut s, &Ask::StopTestSignal, SHORT),
        Err(Failure::NoReply(SHORT.reply))
    );
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
}

/// No socket at the path: exit 1, the error's kind only.
#[cfg(unix)]
#[test]
fn a_pipe_nobody_serves_cannot_be_connected() {
    let dir = tempfile::tempdir().unwrap();
    let got = run(&Supervise {
        pipe: dir.path().join("none.sock").to_string_lossy().into_owned(),
        ask: Ask::Status,
    });
    assert_eq!(got, Err(Failure::Connect("entity not found".into())));
}

/// Off Unix the guard is the engine's supervisor (the PC): refused as a
/// usage error, nothing connected.
#[cfg(not(unix))]
#[test]
fn off_unix_supervise_is_refused() {
    let got = run(&Supervise {
        pipe: "iem-test-none".into(),
        ask: Ask::Status,
    });
    assert_eq!(got, Err(Failure::NotHere));
    assert_eq!(Failure::NotHere.code(), 2);
}
