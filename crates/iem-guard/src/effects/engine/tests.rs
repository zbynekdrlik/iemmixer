//! The engine's supervisor pipe (`effects::engine`): its tests.

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
        last_reopen_us: 0,
        fault_callback_us: 0.0,
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
        hil_test_signal("mic1", -24.5, 30.0, &[94, 95], false),
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

/// The listen probe (S7, #10): the engine's `HilTestSignal.listen`, written
/// only when true, so a plain signal stays what an older engine reads.
#[test]
fn the_listen_probe_adds_listen_to_the_hil_signal() {
    assert_eq!(
        hil_test_signal("mic1", -20.0, 30.0, &[94, 95], true),
        json!({
            "op": "hil_test_signal",
            "input": "mic1",
            "hz": 1000.0,
            "dbfs": -20.0,
            "ttl_s": 30.0,
            "card_tx": [94, 95],
            "listen": true,
        })
    );
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
        p(json!({"type": "hello", "proto": 1, "engine_build": "2.0.0+abc", "role": "supervisor"})),
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
            last_reopen_us: 0,
            fault_callback_us: 0.0,
        })
    );
    // S7 HIL v2 (#10): the last reopen's time and the faulting callback's
    // time; an older engine's read 0.
    assert_eq!(
        p(
            json!({"type": "status", "callbacks": 7, "faulted": true, "last_reopen_us": 104_000, "fault_callback_us": 412.5})
        ),
        Msg::Status(Status {
            callbacks: 7,
            faulted: true,
            last_reopen_us: 104_000,
            fault_callback_us: 412.5,
            ..Status::default()
        })
    );
    assert_eq!(
        p(
            json!({"type": "status", "callbacks": 7, "last_reopen_us": -1, "fault_callback_us": "x"})
        ),
        Msg::Status(status(0, 7, 0))
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
/// pair of integers in range (a bucket up to the engine's largest overflow
/// bucket, 1000) or more entries than the engine has buckets (1001) make it
/// none, never part of one (the soak verdict then names it), so the guard's
/// reply stays within its frame; an absent one is none (an older engine).
#[test]
fn a_histogram_with_a_bad_entry_reads_as_none() {
    let read =
        |h: Value| match p(json!({"type": "status", "interval_hist": h, "process_hist": [[7, 1]]}))
        {
            Msg::Status(s) => (s.interval_hist, s.process_hist),
            other => panic!("{other:?}"),
        };
    assert_eq!(
        read(json!([[0, 7], [1000, u64::MAX]])),
        (vec![(0, 7), (1000, u64::MAX)], vec![(7, 1)])
    );
    assert_eq!((HIST_TOP_MAX, HIST_LEN_MAX), (1000, 1001));
    assert_eq!(
        read(json!(vec![[1000_u64, 1]; 1001])),
        (vec![(1000, 1); 1001], vec![(7, 1)])
    );
    for bad in [
        json!(vec![[0_u64, 1]; 1002]),
        json!([[1001, 1]]),
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
            last_reopen_us: 104_000,
            fault_callback_us: 412.5,
        },
        pipe_private: true,
        pipe_server_pid: Some(4242),
        last_fault_us: Some(398.25),
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
            // S7 HIL v2 (#10): the pipe's server, the last reopen, and the
            // last fault the guard kept (not the running engine's status).
            pipe_server_pid: Some(4242),
            last_reopen_us: 104_000,
            last_fault_us: Some(398.25),
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

/// HIL v2's fault time (S7, #10): the faulting callback's time of a
/// faulted `Status`, which the guard keeps across the respawn; none from a
/// status that is not faulted, or whose time is 0 (an older engine, a fault
/// the backend raised itself), negative or not finite.
#[test]
fn the_last_fault_is_a_faulted_status_callback_time() {
    let at = |faulted: bool, us: f64| {
        fault_time(&Status {
            faulted,
            fault_callback_us: us,
            ..Status::default()
        })
    };
    assert_eq!(at(true, 412.5), Some(412.5));
    assert_eq!(at(true, 0.001), Some(0.001));
    assert_eq!(at(false, 412.5), None);
    assert_eq!(at(true, 0.0), None);
    assert_eq!(at(true, -1.0), None);
    assert_eq!(at(true, f64::INFINITY), None);
    assert_eq!(at(true, f64::NAN), None);
    assert_eq!(at(false, 0.0), None);
}

/// The fault time the guard keeps (S7 HIL v2, lane review): a newer faulted
/// status replaces it, also with none when that fault has no callback time
/// (the reopen budget, a structured exception, an older engine), so an
/// earlier fault's time never stands for a later fault; a status that is not
/// faulted, or none, keeps it (the respawned engine's).
#[test]
fn the_kept_fault_is_the_newest_faulted_status() {
    let faulted = |us: f64| Status {
        faulted: true,
        fault_callback_us: us,
        ..Status::default()
    };
    let healthy = Status {
        callbacks: 9,
        fault_callback_us: 7.0,
        ..Status::default()
    };
    assert_eq!(kept_fault(None, Some(&faulted(412.5))), Some(412.5));
    assert_eq!(kept_fault(Some(398.25), Some(&faulted(412.5))), Some(412.5));
    assert_eq!(kept_fault(Some(398.25), Some(&faulted(0.0))), None);
    assert_eq!(kept_fault(Some(398.25), Some(&faulted(f64::NAN))), None);
    assert_eq!(kept_fault(Some(398.25), Some(&healthy)), Some(398.25));
    assert_eq!(kept_fault(None, Some(&healthy)), None);
    assert_eq!(kept_fault(Some(398.25), None), Some(398.25));
    assert_eq!(kept_fault(None, None), None);
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
