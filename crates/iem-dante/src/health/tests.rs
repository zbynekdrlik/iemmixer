use super::*;

use std::sync::Arc;

/// This binary's build in the tests: 40 synthetic hex digits.
const OWN: &str = "0123456789abcdef0123456789abcdef01234567";
/// `CLOCK_MONOTONIC` at the check, µs: 100 s after boot.
const NOW: u64 = 100_000_000;
/// When the running units became active: 90 s before [`NOW`].
const SINCE: u64 = 10_000_000;

fn up() -> Unit {
    Unit {
        active: Active::Active,
        result: UnitResult::Success,
        restarts: 0,
        active_since_us: SINCE,
    }
}

fn down() -> Unit {
    Unit {
        active: Active::Inactive,
        result: UnitResult::Success,
        restarts: 0,
        active_since_us: 0,
    }
}

fn with(active: Active) -> Unit {
    Unit { active, ..up() }
}

fn engine_up() -> Units {
    Units {
        engine: up(),
        server: up(),
        device: down(),
    }
}

fn device_up() -> Units {
    Units {
        engine: down(),
        server: down(),
        device: up(),
    }
}

fn stopped() -> Units {
    Units {
        engine: down(),
        server: down(),
        device: down(),
    }
}

/// A ready clock, out of holdover, with past holdovers and a re-anchor
/// (equal in both reads: nothing new).
fn clock() -> Clock {
    Clock {
        ready: 1,
        holdover: 0,
        holdovers: 2,
        re_anchors: 1,
        freq_ppb: -750,
    }
}

/// A pump on the media clock with a past re-bootstrap.
fn pump(blocks: u64) -> Pump {
    Pump {
        media: 1,
        blocks,
        late: 3,
        missed: 4,
        rebootstraps: 1,
        max_process_us: 217,
    }
}

/// Two reads of the engine-hosted device a second apart: 3 000 blocks.
fn engine_reads() -> (Status, Status) {
    (
        Status {
            clock: clock(),
            engine: Some(pump(30_000)),
        },
        Status {
            clock: clock(),
            engine: Some(pump(33_000)),
        },
    )
}

/// Two reads of the standalone device: no engine.
fn device_reads() -> (Status, Status) {
    let s = Status {
        clock: clock(),
        engine: None,
    };
    (s, s)
}

fn same() -> ServerAnswer {
    ServerAnswer::Answered {
        git_hash: OWN[..7].to_string(),
    }
}

fn judge(units: &Units, reads: &(Status, Status), server: &ServerAnswer) -> Verdict {
    verdict(units, Some(&reads.0), Some(&reads.1), server, OWN, NOW)
}

fn judged(v: &Verdict) -> (State, Vec<Reason>) {
    (v.state, v.reasons.clone())
}

#[test]
fn the_engine_and_the_server_can_be_healthy() {
    let reads = engine_reads();
    let v = judge(&engine_up(), &reads, &same());
    assert_eq!(judged(&v), (State::Healthy, vec![]));
    assert_eq!(v.state.exit_code(), 0);
    // What the line shows: the second read's clock and pump, the answer.
    assert_eq!(v.clock, Some(reads.1.clock));
    assert_eq!(v.pump, Some(pump(33_000)));
    assert!(v.answers);
    assert_eq!(v.build, Some(Build::Same));
    assert_eq!(v.units, engine_up());
    // The whole build is a prefix of itself.
    let whole = ServerAnswer::Answered {
        git_hash: OWN.to_string(),
    };
    assert_eq!(
        judged(&judge(&engine_up(), &reads, &whole)),
        (State::Healthy, vec![])
    );
}

#[test]
fn the_standalone_device_alone_can_be_healthy() {
    let reads = device_reads();
    let v = judge(&device_up(), &reads, &ServerAnswer::Silent);
    assert_eq!(judged(&v), (State::Healthy, vec![]));
    assert_eq!(v.state.exit_code(), 0);
    assert_eq!(v.clock, Some(clock()));
    assert_eq!(v.pump, None);
    assert!(!v.answers);
    assert_eq!(v.build, None);
    // The server is not the device's: its answer is shown, never judged.
    let other = ServerAnswer::Answered {
        git_hash: "fedcba9".into(),
    };
    let v = judge(&device_up(), &reads, &other);
    assert_eq!(judged(&v), (State::Healthy, vec![]));
    assert_eq!((v.answers, v.build), (true, Some(Build::Other)));
    // Only with a ready clock.
    let mut unready = reads;
    unready.1.clock.ready = 0;
    assert_eq!(
        judge(&device_up(), &unready, &ServerAnswer::Silent).reasons,
        vec![Reason::ClockNotReady]
    );
}

#[test]
fn stopped_exits_3() {
    let v = verdict(&stopped(), None, None, &ServerAnswer::Silent, OWN, NOW);
    assert_eq!(judged(&v), (State::Stopped, vec![]));
    assert_eq!(v.state.exit_code(), 3);
    assert_eq!(
        (v.clock, v.pump, v.answers, v.build),
        (None, None, false, None)
    );
    // Restarts before the stop and a failure's result do not make it more
    // than stopped: all three are inactive.
    let mut units = stopped();
    units.engine.restarts = 2;
    units.engine.result = UnitResult::ExitCode;
    let v = verdict(&units, None, None, &ServerAnswer::Silent, OWN, NOW);
    assert_eq!(judged(&v), (State::Stopped, vec![]));
}

#[test]
fn the_other_states_exit_1() {
    assert_eq!(State::Degraded.exit_code(), 1);
    assert_eq!(State::Failed.exit_code(), 1);
}

#[test]
fn a_failed_unit_is_unit_failed_and_fails_the_box() {
    // The server failed: unit_failed, not unit_down, and no server reason.
    let mut units = engine_up();
    units.server = Unit {
        result: UnitResult::StartLimitHit,
        ..with(Active::Failed)
    };
    let v = judge(&units, &engine_reads(), &ServerAnswer::Silent);
    assert_eq!(judged(&v), (State::Failed, vec![Reason::UnitFailed]));
    assert_eq!(v.state.exit_code(), 1);
    // The standalone device failed while the engine runs.
    let mut units = engine_up();
    units.device = with(Active::Failed);
    assert_eq!(
        judged(&judge(&units, &engine_reads(), &same())),
        (State::Failed, vec![Reason::UnitFailed])
    );
    // The device failed, nothing else ran: no read is judged.
    let mut units = stopped();
    units.device = with(Active::Failed);
    let v = verdict(&units, None, None, &ServerAnswer::Silent, OWN, NOW);
    assert_eq!(judged(&v), (State::Failed, vec![Reason::UnitFailed]));
}

#[test]
fn a_needed_unit_not_active_is_unit_down() {
    // The engine runs, the server is stopped: no server reason of its own.
    let mut units = engine_up();
    units.server = down();
    assert_eq!(
        judged(&judge(&units, &engine_reads(), &ServerAnswer::Silent)),
        (State::Failed, vec![Reason::UnitDown])
    );
    // The server alone: the engine's shape, the engine down, no read.
    let mut units = engine_up();
    units.engine = down();
    let v = verdict(&units, None, None, &same(), OWN, NOW);
    assert_eq!(judged(&v), (State::Failed, vec![Reason::UnitDown]));
    // The engine waiting to restart (activating, a restart counted).
    let mut units = engine_up();
    units.engine = Unit {
        restarts: 1,
        ..with(Active::Activating)
    };
    let v = verdict(&units, None, None, &same(), OWN, NOW);
    assert_eq!(
        judged(&v),
        (State::Failed, vec![Reason::UnitDown, Reason::Restarted])
    );
    // The standalone device stopping.
    let mut units = stopped();
    units.device = with(Active::Deactivating);
    let v = verdict(&units, None, None, &ServerAnswer::Silent, OWN, NOW);
    assert_eq!(judged(&v), (State::Failed, vec![Reason::UnitDown]));
    // A word systemd may add later is not active either.
    let mut units = device_up();
    units.device = with(Active::Other);
    let v = judge(&units, &device_reads(), &ServerAnswer::Silent);
    assert_eq!(judged(&v), (State::Failed, vec![Reason::UnitDown]));
}

#[test]
fn restarts_are_restarted_and_degrade_the_box() {
    let mut units = engine_up();
    units.server.restarts = 1;
    let v = judge(&units, &engine_reads(), &same());
    assert_eq!(judged(&v), (State::Degraded, vec![Reason::Restarted]));
    assert_eq!(v.state.exit_code(), 1);
    let mut units = device_up();
    units.device.restarts = 3;
    assert_eq!(
        judged(&judge(&units, &device_reads(), &ServerAnswer::Silent)),
        (State::Degraded, vec![Reason::Restarted])
    );
}

#[test]
fn an_active_host_without_a_status_is_no_status() {
    let (a, b) = engine_reads();
    for (first, second) in [(None, Some(&b)), (Some(&a), None), (None, None)] {
        let v = verdict(&engine_up(), first, second, &same(), OWN, NOW);
        assert_eq!(judged(&v), (State::Failed, vec![Reason::NoStatus]));
        // The line shows the read that came.
        assert_eq!(v.pump, second.or(first).and_then(|s| s.engine));
    }
    let v = verdict(&engine_up(), Some(&a), None, &same(), OWN, NOW);
    assert_eq!(v.pump, Some(pump(30_000)));
    let (a, _) = device_reads();
    let v = verdict(&device_up(), Some(&a), None, &same(), OWN, NOW);
    assert_eq!(judged(&v), (State::Failed, vec![Reason::NoStatus]));
}

#[test]
fn a_clock_not_ready_degrades_for_30_s_then_fails() {
    // The engine paces itself while the clock is not ready.
    let mut reads = engine_reads();
    reads.1.clock.ready = 0;
    reads.1.engine = Some(Pump {
        media: 0,
        ..pump(33_000)
    });
    let at = |since: u64| {
        let mut units = engine_up();
        units.engine.active_since_us = since;
        judged(&verdict(
            &units,
            Some(&reads.0),
            Some(&reads.1),
            &same(),
            OWN,
            NOW,
        ))
    };
    let not_ready = vec![Reason::ClockNotReady];
    assert_eq!(
        at(NOW - CLOCK_GRACE_US),
        (State::Degraded, not_ready.clone())
    );
    assert_eq!(
        at(NOW - CLOCK_GRACE_US - 1),
        (State::Failed, not_ready.clone())
    );
    assert_eq!(at(NOW), (State::Degraded, not_ready.clone()));
    // A start time after now (never on one boot): still within the grace.
    assert_eq!(at(NOW + 1), (State::Degraded, not_ready.clone()));
    // The grace is the host's: the server's start does not count.
    let mut units = engine_up();
    units.server.active_since_us = NOW;
    let v = verdict(&units, Some(&reads.0), Some(&reads.1), &same(), OWN, NOW);
    assert_eq!(judged(&v), (State::Failed, not_ready.clone()));
    // The standalone device likewise.
    let mut reads = device_reads();
    reads.1.clock.ready = 0;
    let mut units = device_up();
    units.device.active_since_us = NOW - CLOCK_GRACE_US - 1;
    let v = judge(&units, &reads, &ServerAnswer::Silent);
    assert_eq!(judged(&v), (State::Failed, not_ready.clone()));
    // Judged on the second read: a clock that just became ready is ready.
    let mut reads = engine_reads();
    reads.0.clock.ready = 0;
    assert_eq!(
        judged(&judge(&engine_up(), &reads, &same())),
        (State::Healthy, vec![])
    );
}

#[test]
fn a_holdover_at_either_read_or_between_them_is_holdover() {
    let holdover = (State::Degraded, vec![Reason::Holdover]);
    let mut reads = engine_reads();
    reads.0.clock.holdover = 1;
    assert_eq!(judged(&judge(&engine_up(), &reads, &same())), holdover);
    let mut reads = engine_reads();
    reads.1.clock.holdover = 1;
    reads.1.clock.holdovers = 3;
    assert_eq!(judged(&judge(&engine_up(), &reads, &same())), holdover);
    // In and out again between the reads.
    let mut reads = engine_reads();
    reads.1.clock.holdovers = 3;
    assert_eq!(judged(&judge(&engine_up(), &reads, &same())), holdover);
    let mut reads = device_reads();
    reads.1.clock.holdover = 1;
    assert_eq!(
        judged(&judge(&device_up(), &reads, &ServerAnswer::Silent)),
        holdover
    );
}

#[test]
fn a_ready_clock_with_a_pump_pacing_itself_is_pump_free() {
    let mut reads = engine_reads();
    reads.1.engine = Some(Pump {
        media: 0,
        ..pump(33_000)
    });
    assert_eq!(
        judged(&judge(&engine_up(), &reads, &same())),
        (State::Degraded, vec![Reason::PumpFree])
    );
    // Judged on the second read: a pump that just went onto the clock.
    let mut reads = engine_reads();
    reads.0.engine = Some(Pump {
        media: 0,
        ..pump(30_000)
    });
    assert_eq!(
        judged(&judge(&engine_up(), &reads, &same())),
        (State::Healthy, vec![])
    );
}

#[test]
fn blocks_that_did_not_rise_are_pump_stalled() {
    let stalled = (State::Failed, vec![Reason::PumpStalled]);
    let mut reads = engine_reads();
    reads.1.engine = Some(pump(30_000));
    assert_eq!(judged(&judge(&engine_up(), &reads, &same())), stalled);
    // Fewer (the engine started again between the reads).
    let mut reads = engine_reads();
    reads.1.engine = Some(pump(12));
    assert_eq!(judged(&judge(&engine_up(), &reads, &same())), stalled);
    // A read without the pump while the engine runs.
    for first in [false, true] {
        let mut reads = engine_reads();
        if first {
            reads.0.engine = None;
        } else {
            reads.1.engine = None;
        }
        assert_eq!(judged(&judge(&engine_up(), &reads, &same())), stalled);
    }
}

#[test]
fn a_rebootstrap_between_the_reads_is_pump_rebootstrap() {
    let mut reads = engine_reads();
    reads.1.engine = Some(Pump {
        rebootstraps: 2,
        ..pump(33_000)
    });
    assert_eq!(
        judged(&judge(&engine_up(), &reads, &same())),
        (State::Degraded, vec![Reason::PumpRebootstrap])
    );
}

#[test]
fn an_active_server_that_does_not_answer_is_server_down() {
    let v = judge(&engine_up(), &engine_reads(), &ServerAnswer::Silent);
    assert_eq!(judged(&v), (State::Failed, vec![Reason::ServerDown]));
    assert_eq!((v.answers, v.build), (false, None));
}

#[test]
fn another_build_is_build_mismatch() {
    let mismatch = (State::Degraded, vec![Reason::BuildMismatch]);
    for hash in [
        "fedcba9",
        "",
        "unknown",
        "0123456789abcdef0123456789abcdef012345678",
    ] {
        let other = ServerAnswer::Answered {
            git_hash: hash.into(),
        };
        let v = judge(&engine_up(), &engine_reads(), &other);
        assert_eq!(judged(&v), mismatch, "{hash:?}");
        assert_eq!((v.answers, v.build), (true, Some(Build::Other)));
    }
    // A local build ("local") never matches a server's hash.
    let v = verdict(
        &engine_up(),
        Some(&engine_reads().0),
        Some(&engine_reads().1),
        &same(),
        "local",
        NOW,
    );
    assert_eq!(judged(&v), mismatch);
}

#[test]
fn every_reason_is_listed_once_in_order() {
    let mut units = engine_up();
    units.device = with(Active::Failed);
    units.engine.restarts = 4;
    units.engine.active_since_us = 0;
    let mut reads = engine_reads();
    reads.0.clock.holdover = 1;
    reads.1.clock.ready = 0;
    reads.1.engine = Some(Pump {
        media: 0,
        rebootstraps: 9,
        ..pump(30_000)
    });
    let other = ServerAnswer::Answered {
        git_hash: "fedcba9".into(),
    };
    let v = judge(&units, &reads, &other);
    assert_eq!(
        judged(&v),
        (
            State::Failed,
            vec![
                Reason::UnitFailed,
                Reason::Restarted,
                Reason::ClockNotReady,
                Reason::Holdover,
                Reason::PumpStalled,
                Reason::PumpRebootstrap,
                Reason::BuildMismatch,
            ]
        )
    );
}

/// The fixed words of the line: its keys, the states, the reasons, the
/// units, systemd's words, the builds.
const WORDS: &[&str] = &[
    "health",
    "state",
    "reasons",
    "units",
    "unit",
    "active",
    "result",
    "restarts",
    "clock",
    "ready",
    "holdover",
    "holdovers",
    "re_anchors",
    "freq_ppb",
    "pump",
    "media",
    "blocks",
    "late",
    "missed",
    "rebootstraps",
    "max_process_us",
    "server",
    "answers",
    "build",
    "healthy",
    "degraded",
    "failed",
    "stopped",
    "unit_failed",
    "unit_down",
    "restarted",
    "no_status",
    "clock_not_ready",
    "pump_free",
    "pump_stalled",
    "pump_rebootstrap",
    "server_down",
    "build_mismatch",
    "engine",
    "device",
    "reloading",
    "inactive",
    "activating",
    "deactivating",
    "maintenance",
    "refreshing",
    "other",
    "success",
    "resources",
    "protocol",
    "timeout",
    "exit-code",
    "signal",
    "core-dump",
    "watchdog",
    "start-limit-hit",
    "oom-kill",
    "exec-condition",
    "same",
];

/// Every string of `v` (keys included); panics on a value that is not a
/// number, null, an object, an array or a string.
fn strings(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Number(_) | serde_json::Value::Null => {}
        serde_json::Value::Array(items) => items.iter().for_each(|i| strings(i, out)),
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                out.push(k.clone());
                strings(v, out);
            }
        }
        serde_json::Value::Bool(b) => panic!("a bool {b} in the line"),
    }
}

#[test]
fn the_line_is_numbers_and_fixed_words() {
    // Every reason, unknown systemd words, a build that differs.
    let mut units = engine_up();
    units.engine.restarts = 4;
    units.engine.result = UnitResult::Other;
    units.device = Unit {
        result: UnitResult::CoreDump,
        ..with(Active::Failed)
    };
    let mut reads = engine_reads();
    reads.1.clock.ready = 0;
    reads.1.clock.holdover = 1;
    let v = judge(&units, &reads, &ServerAnswer::Silent);
    let other = judge(
        &engine_up(),
        &engine_reads(),
        &ServerAnswer::Answered {
            git_hash: "fedcba9".into(),
        },
    );
    let stopped = verdict(&stopped(), None, None, &ServerAnswer::Silent, OWN, NOW);
    // Every state, reason and build is a word of the set.
    let words = [
        serde_json::to_value([
            State::Healthy,
            State::Degraded,
            State::Failed,
            State::Stopped,
        ])
        .unwrap(),
        serde_json::to_value([
            Reason::UnitFailed,
            Reason::UnitDown,
            Reason::Restarted,
            Reason::NoStatus,
            Reason::ClockNotReady,
            Reason::Holdover,
            Reason::PumpFree,
            Reason::PumpStalled,
            Reason::PumpRebootstrap,
            Reason::ServerDown,
            Reason::BuildMismatch,
        ])
        .unwrap(),
        serde_json::to_value([Build::Same, Build::Other]).unwrap(),
    ];
    for w in &words {
        let mut out = Vec::new();
        strings(w, &mut out);
        for word in out {
            assert!(WORDS.contains(&word.as_str()), "{word:?}");
        }
    }
    for v in [&v, &other, &stopped] {
        let line = v.line();
        assert!(!line.contains('\n'), "{line}");
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        let mut words = Vec::new();
        strings(&parsed, &mut words);
        for word in words {
            assert!(WORDS.contains(&word.as_str()), "{word:?} in {line}");
        }
    }
    // The whole line, pinned once.
    assert_eq!(
        judge(&engine_up(), &engine_reads(), &same()).line(),
        "{\"health\":{\"state\":\"healthy\",\"reasons\":[],\"units\":[\
         {\"unit\":\"engine\",\"active\":\"active\",\"result\":\"success\",\"restarts\":0},\
         {\"unit\":\"server\",\"active\":\"active\",\"result\":\"success\",\"restarts\":0},\
         {\"unit\":\"device\",\"active\":\"inactive\",\"result\":\"success\",\"restarts\":0}],\
         \"clock\":{\"ready\":1,\"holdover\":0,\"holdovers\":2,\"re_anchors\":1,\"freq_ppb\":-750},\
         \"pump\":{\"media\":1,\"blocks\":33000,\"late\":3,\"missed\":4,\"rebootstraps\":1,\
         \"max_process_us\":217},\"server\":{\"answers\":1,\"build\":\"same\"}}}"
    );
    assert_eq!(
        stopped.line(),
        "{\"health\":{\"state\":\"stopped\",\"reasons\":[],\"units\":[\
         {\"unit\":\"engine\",\"active\":\"inactive\",\"result\":\"success\",\"restarts\":0},\
         {\"unit\":\"server\",\"active\":\"inactive\",\"result\":\"success\",\"restarts\":0},\
         {\"unit\":\"device\",\"active\":\"inactive\",\"result\":\"success\",\"restarts\":0}],\
         \"clock\":null,\"pump\":null,\"server\":{\"answers\":0,\"build\":null}}}"
    );
    assert!(
        v.line().contains(
            "\"reasons\":[\"unit_failed\",\"restarted\",\"clock_not_ready\",\"holdover\",\
             \"server_down\"],\"units\":[{\"unit\":\"engine\",\"active\":\"active\",\
             \"result\":\"other\",\"restarts\":4},"
        ),
        "{}",
        v.line()
    );
    assert!(
        v.line()
            .contains("{\"unit\":\"device\",\"active\":\"failed\",\"result\":\"core-dump\""),
        "{}",
        v.line()
    );
    assert!(
        other
            .line()
            .ends_with("\"server\":{\"answers\":1,\"build\":\"other\"}}}"),
        "{}",
        other.line()
    );
}

#[test]
fn unknown_systemd_words_read_as_other() {
    for word in ["", "Active", "running", "dead", "active ", "auto-restart"] {
        assert_eq!(Active::from_word(word), Active::Other, "{word:?}");
    }
    for word in ["", "Success", "exit_code", "start-limit", "success "] {
        assert_eq!(UnitResult::from_word(word), UnitResult::Other, "{word:?}");
    }
    // Every word of ours reads back as itself.
    let actives = [
        Active::Active,
        Active::Reloading,
        Active::Inactive,
        Active::Failed,
        Active::Activating,
        Active::Deactivating,
        Active::Maintenance,
        Active::Refreshing,
    ];
    for a in actives {
        let word = serde_json::to_value(a).unwrap();
        assert_eq!(Active::from_word(word.as_str().unwrap()), a, "{word}");
    }
    let results = [
        UnitResult::Success,
        UnitResult::Resources,
        UnitResult::Protocol,
        UnitResult::Timeout,
        UnitResult::ExitCode,
        UnitResult::Signal,
        UnitResult::CoreDump,
        UnitResult::Watchdog,
        UnitResult::StartLimitHit,
        UnitResult::OomKill,
        UnitResult::ExecCondition,
    ];
    for r in results {
        let word = serde_json::to_value(r).unwrap();
        assert_eq!(UnitResult::from_word(word.as_str().unwrap()), r, "{word}");
    }
    assert_eq!(serde_json::to_value(Active::Other).unwrap(), "other");
    assert_eq!(serde_json::to_value(UnitResult::Other).unwrap(), "other");
    // Through systemctl's output.
    let units =
        parse_units("Id=iem-engine.service\nActiveState=hibernating\nResult=bogus\nNRestarts=0\n");
    assert_eq!(
        (units.engine.active, units.engine.result),
        (Active::Other, UnitResult::Other)
    );
}

#[test]
fn systemctl_show_s_blocks_read_by_their_id() {
    // As `systemctl show` prints them: a block a unit, any property order.
    let text = "\
Result=exit-code
NRestarts=2
Id=iem-server.service
ActiveState=activating
ActiveEnterTimestampMonotonic=0

Id=iem-engine.service
Result=success
NRestarts=0
ActiveState=active
ActiveEnterTimestampMonotonic=4113207719

NRestarts=0
Result=start-limit-hit
ActiveEnterTimestampMonotonic=123
ActiveState=failed
Id=iem-dante.service
";
    let units = parse_units(text);
    assert_eq!(
        units,
        Units {
            engine: Unit {
                active: Active::Active,
                result: UnitResult::Success,
                restarts: 0,
                active_since_us: 4_113_207_719,
            },
            server: Unit {
                active: Active::Activating,
                result: UnitResult::ExitCode,
                restarts: 2,
                active_since_us: 0,
            },
            device: Unit {
                active: Active::Failed,
                result: UnitResult::StartLimitHit,
                restarts: 0,
                active_since_us: 123,
            },
        }
    );
    // A unit systemctl did not list, a block of another unit or without an
    // id, a number that does not read, a line without `=`.
    let text = "\
Id=iem-engine.service
ActiveState=inactive
Result=success
NRestarts=many
ActiveEnterTimestampMonotonic=-1
something without a key

Id=other.service
ActiveState=active

ActiveState=active
NRestarts=7
";
    let units = parse_units(text);
    assert_eq!(
        units.engine,
        Unit {
            active: Active::Inactive,
            result: UnitResult::Success,
            restarts: 0,
            active_since_us: 0,
        }
    );
    assert_eq!(units.server, Unit::default());
    assert_eq!(units.device, Unit::default());
    assert_eq!(
        Unit::default(),
        Unit {
            active: Active::Other,
            result: UnitResult::Other,
            restarts: 0,
            active_since_us: 0,
        }
    );
    // Nothing at all (systemctl did not run): every unit `other`.
    assert_eq!(parse_units(""), Units::default());
}

#[test]
fn the_device_s_own_status_line_reads() {
    use crate::clock::{Event, ManualClock};
    use crate::daemon::media::tests::{T, Unready, device_at, media_on, media_with_engine};
    use crate::stream::{PumpStats, Timing};
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    // A fresh device: no clock, no engine.
    let media = media_on(&device, Arc::new(Unready));
    assert_eq!(
        parse_status(&crate::ctl::status(&media)),
        Some(Status {
            clock: Clock {
                ready: 0,
                holdover: 0,
                holdovers: 0,
                re_anchors: 0,
                freq_ppb: -750,
            },
            engine: None,
        })
    );
    // Ready, in holdover after one re-anchor, an engine attached.
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_with_engine(&device, Arc::new(ManualClock::new(T)), &[0]);
    media.clock_stats.record(Event::Ready { k_ns: 1 });
    media
        .clock_stats
        .record(Event::ReAnchored { delta_ns: 30_000 });
    media.clock_stats.record(Event::Holdover);
    media
        .freq
        .store(1_250, std::sync::atomic::Ordering::Relaxed);
    let timing = Timing {
        block: 32,
        playout: 96,
        margin: 96,
    };
    media.attach_engine(Arc::new(PumpStats::new(timing, 96_000)));
    let status = parse_status(&crate::ctl::status(&media)).unwrap();
    assert_eq!(
        status,
        Status {
            clock: Clock {
                ready: 1,
                holdover: 1,
                holdovers: 1,
                re_anchors: 1,
                freq_ppb: 1_250,
            },
            engine: Some(Pump {
                media: 0,
                blocks: 0,
                late: 0,
                missed: 0,
                rebootstraps: 0,
                max_process_us: 0,
            }),
        }
    );
    // Anything else is no status.
    for line in [
        "error: unknown request: status, source or check",
        "",
        "{}",
        "{\"clock\":{\"ready\":1}}",
        "ok",
    ] {
        assert_eq!(parse_status(line), None, "{line:?}");
    }
}

#[test]
fn the_server_s_version_reply_reads() {
    let body = "{\"version\":\"2.0.0-dev.45\",\"git_hash\":\"0123456\",\"branch\":\"dev\"}";
    let reply =
        |head: &str, body: &str| format!("{head}\r\ncontent-type: application/json\r\n\r\n{body}");
    let answered = ServerAnswer::Answered {
        git_hash: "0123456".into(),
    };
    for head in ["HTTP/1.1 200 OK", "HTTP/1.0 200 OK", "HTTP/1.1 200"] {
        assert_eq!(
            server_answer(reply(head, body).as_bytes()),
            answered,
            "{head}"
        );
    }
    for (head, body) in [
        ("HTTP/1.1 404 Not Found", body),
        ("HTTP/1.1 500 Internal Server Error", body),
        ("HTTP/2 200 OK", body),
        ("XTTP/1.1 200 OK", body),
        ("HTTP/1.1", body),
        ("", body),
        ("HTTP/1.1 200 OK", "{\"version\":\"2.0.0-dev.45\"}"),
        ("HTTP/1.1 200 OK", "{\"git_hash\":7}"),
        ("HTTP/1.1 200 OK", "not json"),
        ("HTTP/1.1 200 OK", ""),
    ] {
        assert_eq!(
            server_answer(reply(head, body).as_bytes()),
            ServerAnswer::Silent,
            "{head} {body}"
        );
    }
    // A reply cut before its body; nothing at all.
    assert_eq!(
        server_answer(b"HTTP/1.1 200 OK\r\ncontent-length: 70\r\n"),
        ServerAnswer::Silent
    );
    assert_eq!(server_answer(b""), ServerAnswer::Silent);
}

#[cfg(target_os = "linux")]
mod glue {
    use super::*;

    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{Ipv4Addr, SocketAddr, TcpListener};
    use std::os::unix::net::UnixListener;
    use std::time::{Duration, Instant};

    use crate::daemon::media::tests::BOUND;

    const STATUS_LINE: &str = "{\"clock\":{\"ready\":1,\"epoch\":0,\"polls\":9,\"re_anchors\":1,\
        \"holdovers\":2,\"holdover\":0,\"freq_ppb\":-750},\"requests\":[],\"sent\":[],\
        \"uncounted\":0,\"tx\":[],\"rx\":[],\"rx_poll_errors\":0,\"channels\":[],\
        \"sources\":[],\"checks\":[]}";

    /// A control socket in `dir` that answers one connection: it reads the
    /// request line and sends `answer`; returns the request it read.
    fn one_answer(
        dir: &std::path::Path,
        answer: &'static str,
    ) -> (std::path::PathBuf, std::thread::JoinHandle<String>) {
        let path = dir.join("control.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let served = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(BOUND)).unwrap();
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            reader.get_mut().write_all(answer.as_bytes()).unwrap();
            request
        });
        (path, served)
    }

    #[test]
    fn a_status_request_reads_the_status_line() {
        let dir = tempfile::tempdir().unwrap();
        let answer: &'static str = Box::leak(format!("{STATUS_LINE}\n").into_boxed_str());
        let (path, served) = one_answer(dir.path(), answer);
        let got = read_status(&path, BOUND);
        assert!(got.is_some());
        assert_eq!(got, parse_status(STATUS_LINE));
        assert_eq!(served.join().unwrap(), "status\n");
    }

    #[test]
    fn no_socket_or_an_error_answer_is_no_status() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_status(&dir.path().join("none.sock"), BOUND), None);
        let (path, served) = one_answer(dir.path(), "error: the request is not UTF-8\n");
        assert_eq!(read_status(&path, BOUND), None);
        served.join().unwrap();
    }

    #[test]
    fn a_device_that_never_answers_is_no_status_within_the_wait() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let held = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            // Holds the connection, answers nothing, until the client went.
            let mut buf = [0u8; 64];
            stream.set_read_timeout(Some(BOUND)).unwrap();
            let mut stream = stream;
            while matches!(stream.read(&mut buf), Ok(n) if n > 0) {}
        });
        let start = Instant::now();
        assert_eq!(read_status(&path, Duration::from_millis(200)), None);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
        held.join().unwrap();
    }

    /// A server on 127.0.0.1 that reads one request and sends `reply`, or
    /// holds the connection `hold` and sends nothing; returns its address
    /// and the request it read.
    fn one_reply(
        reply: Option<&'static [u8]>,
        hold: Duration,
    ) -> (SocketAddr, std::thread::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let served = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(BOUND)).unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 512];
            while !request.ends_with(b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0, "the request ended early");
                request.extend_from_slice(&buf[..n]);
            }
            match reply {
                Some(bytes) => {
                    // In two pieces: the reply is read to its end.
                    let (a, b) = bytes.split_at(bytes.len() / 2);
                    stream.write_all(a).unwrap();
                    stream.flush().unwrap();
                    std::thread::sleep(Duration::from_millis(20));
                    stream.write_all(b).unwrap();
                }
                None => std::thread::sleep(hold),
            }
            request
        });
        (addr, served)
    }

    #[test]
    fn the_server_is_asked_its_version_over_http_1_0() {
        let reply: &'static [u8] = b"HTTP/1.0 200 OK\r\ncontent-type: application/json\r\n\
            content-length: 24\r\n\r\n{\"git_hash\":\"0123456\"}  ";
        let (addr, served) = one_reply(Some(reply), Duration::ZERO);
        assert_eq!(
            ask_server(addr, BOUND),
            ServerAnswer::Answered {
                git_hash: "0123456".into()
            }
        );
        assert_eq!(
            served.join().unwrap(),
            b"GET /api/version HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        );
    }

    #[test]
    fn a_reply_is_read_up_to_1_mib() {
        use crate::health::glue::MAX_READ;
        let head: &[u8] =
            b"HTTP/1.0 200 OK\r\ncontent-type: application/json\r\n\r\n{\"git_hash\":\"0123456\"}";
        // The body padded with spaces to `len` bytes in all.
        let whole = |len: u64| -> &'static [u8] {
            let mut reply = head.to_vec();
            reply.resize(usize::try_from(len).unwrap(), b' ');
            Box::leak(reply.into_boxed_slice())
        };
        let (addr, served) = one_reply(Some(whole(MAX_READ)), Duration::ZERO);
        assert_eq!(
            ask_server(addr, BOUND),
            ServerAnswer::Answered {
                git_hash: "0123456".into()
            }
        );
        served.join().unwrap();
        let (addr, served) = one_reply(Some(whole(MAX_READ + 1)), Duration::ZERO);
        assert_eq!(ask_server(addr, BOUND), ServerAnswer::Silent);
        served.join().unwrap();
    }

    #[test]
    fn a_closed_port_or_a_silent_server_is_silent_within_the_wait() {
        // A port nobody listens on (bound, then closed).
        let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap();
        assert_eq!(ask_server(closed, BOUND), ServerAnswer::Silent);
        // A server that takes the request and never answers.
        let (addr, served) = one_reply(None, Duration::from_millis(1_500));
        let start = Instant::now();
        assert_eq!(
            ask_server(addr, Duration::from_millis(300)),
            ServerAnswer::Silent
        );
        let took = start.elapsed();
        assert!(
            took >= Duration::from_millis(250) && took < Duration::from_millis(1_400),
            "{took:?}"
        );
        served.join().unwrap();
    }
}
