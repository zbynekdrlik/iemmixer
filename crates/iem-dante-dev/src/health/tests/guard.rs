// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The latency guard on the verdict (#122): its numbers and reasons, the
//! status lines it judges, the raw status read.

use super::*;

use latency::GuardRead;

/// The tests' bound on a socket wait.
#[cfg(target_os = "linux")]
const BOUND: std::time::Duration = std::time::Duration::from_secs(5);

/// One read health shows, none before it.
pub(super) fn shown(line: &str) -> latency::Reads<'_> {
    latency::Reads {
        shown: Some(line),
        before: None,
    }
}

/// The box's status line (the shape of `health::latency`'s tests): D 56, a
/// stage rack's flow at delay 16 on RX 1, a wireless one at 48 on RX 2, the
/// members' in-ear TX flow at 250 µs (24 samples).
pub(super) const STATUS: &str = r#"{"clock":{"ready":1,"holdover":0,"holdovers":2,"re_anchors":1,"freq_ppb":-750},"tx":[{"flow":1,"channels":2,"tx_channels":[1,2],"receiver_latency_us":250}],"rx":[{"flow":4,"channels":1,"rx_channels":[1],"delay":16},{"flow":5,"channels":1,"rx_channels":[2],"delay":48}],"engine":{"media":1,"offset":56,"blocks":1,"late":0,"missed":0,"rebootstraps":0,"max_process_us":80}}"#;

/// mic1 on RX 1 at 1 ms (its path 96), the wireless input on RX 2 at
/// `wireless` (its path 128).
pub(super) fn box_guard(wireless: u64) -> latency::Guard {
    latency::Guard {
        rate: 96_000,
        in_ear_tx: vec![1, 2],
        inputs: vec![
            latency::Input {
                rx: vec![1],
                budget: 96,
            },
            latency::Input {
                rx: vec![2],
                budget: wireless,
            },
        ],
    }
}

#[test]
fn every_path_at_its_budget_stays_healthy() {
    let v = judge(&engine_up(), &engine_reads(), &same())
        .with_latency(&GuardRead::Read(box_guard(128)), shown(STATUS));
    assert_eq!(judged(&v), (State::Healthy, vec![]));
    assert_eq!(
        v.latency.map(|l| (l.known, l.over, l.worst_samples)),
        Some((2, 0, Some(128)))
    );
    assert_eq!(v.state.exit_code(), 0);
}

#[test]
fn a_path_above_its_budget_degrades_a_healthy_box() {
    let v = judge(&engine_up(), &engine_reads(), &same())
        .with_latency(&GuardRead::Read(box_guard(127)), shown(STATUS));
    assert_eq!(
        judged(&v),
        (State::Degraded, vec![Reason::LatencyOverBudget])
    );
    assert_eq!(v.state.exit_code(), 1);
    // After the box's own reasons, never instead of them; a failed box stays failed.
    let v = judge(&engine_up(), &engine_reads(), &ServerAnswer::Silent)
        .with_latency(&GuardRead::Read(box_guard(127)), shown(STATUS));
    assert_eq!(
        judged(&v),
        (
            State::Failed,
            vec![Reason::ServerDown, Reason::LatencyOverBudget]
        )
    );
    let mut units = engine_up();
    units.engine.restarts = 1;
    let v = judge(&units, &engine_reads(), &same())
        .with_latency(&GuardRead::Read(box_guard(127)), shown(STATUS));
    assert_eq!(
        judged(&v),
        (
            State::Degraded,
            vec![Reason::Restarted, Reason::LatencyOverBudget]
        )
    );
}

#[test]
fn an_unread_in_ear_latency_or_guard_degrades_the_box() {
    let unread = STATUS.replace("\"receiver_latency_us\":250", "\"receiver_latency_us\":0");
    let v = judge(&engine_up(), &engine_reads(), &same())
        .with_latency(&GuardRead::Read(box_guard(128)), shown(&unread));
    assert_eq!(judged(&v), (State::Degraded, vec![Reason::LatencyUnread]));
    assert_eq!(v.latency.map(|l| l.unread_tx), Some(1));
    let v = judge(&engine_up(), &engine_reads(), &same())
        .with_latency(&GuardRead::Unreadable, shown(STATUS));
    assert_eq!(judged(&v), (State::Degraded, vec![Reason::LatencyUnread]));
}

#[test]
fn without_the_guard_s_file_nothing_is_judged() {
    let v = judge(&engine_up(), &engine_reads(), &same())
        .with_latency(&GuardRead::Absent, shown(STATUS));
    assert_eq!(judged(&v), (State::Healthy, vec![]));
    assert_eq!(v.latency, None);
}

#[test]
fn the_guard_judges_only_a_running_engine() {
    // The standalone device (no D) and a stopped stack: the numbers shown,
    // no reason; the device's own state kept.
    for (units, reads, state) in [
        (device_up(), device_reads(), State::Healthy),
        (stopped(), engine_reads(), State::Stopped),
    ] {
        for guard in [GuardRead::Read(box_guard(1)), GuardRead::Unreadable] {
            let v = judge(&units, &reads, &same()).with_latency(&guard, shown(STATUS));
            assert_eq!(judged(&v), (state, vec![]), "{units:?} {guard:?}");
            assert!(v.latency.is_some());
        }
    }
    // An engine that is not active (activating): its own unit_down, nothing more.
    let mut units = engine_up();
    units.engine.active = Active::Activating;
    let v = judge(&units, &engine_reads(), &same())
        .with_latency(&GuardRead::Read(box_guard(1)), shown(STATUS));
    assert!(
        !v.reasons.contains(&Reason::LatencyOverBudget),
        "{:?}",
        v.reasons
    );
}

#[test]
fn the_guard_judges_the_read_health_shows_with_the_one_before() {
    let good = r#"{"clock":{"ready":1,"holdover":0,"holdovers":0,"re_anchors":0,"freq_ppb":0}}"#;
    let other = r#"{"clock":{"ready":0,"holdover":0,"holdovers":0,"re_anchors":0,"freq_ppb":0}}"#;
    let pair = |shown, before| latency::Reads { shown, before };
    assert_eq!(
        reads(Some(good), Some(other)),
        pair(Some(other), Some(good))
    );
    assert_eq!(reads(Some(good), None), pair(Some(good), None));
    assert_eq!(reads(Some(good), Some("error: x")), pair(Some(good), None));
    assert_eq!(
        reads(Some("error: x"), Some(other)),
        pair(Some(other), None)
    );
    assert_eq!(reads(Some("error: x"), Some("{}")), pair(None, None));
    assert_eq!(reads(None, None), pair(None, None));
}

#[test]
fn an_in_ear_latency_unread_in_one_read_only_is_a_flow_just_started() {
    let unread = STATUS.replace("\"receiver_latency_us\":250", "\"receiver_latency_us\":0");
    let guard = GuardRead::Read(box_guard(128));
    let run = |shown: &str, before: Option<&str>| {
        judge(&engine_up(), &engine_reads(), &same()).with_latency(
            &guard,
            latency::Reads {
                shown: Some(shown),
                before,
            },
        )
    };
    // Unread now, read a second before (a receiver asked again) or the
    // other way round: no reason; the numbers stay the shown read's.
    let v = run(&unread, Some(STATUS));
    assert_eq!(judged(&v), (State::Healthy, vec![]));
    assert_eq!(v.latency.map(|l| l.unread_tx), Some(1));
    assert_eq!(
        judged(&run(STATUS, Some(&unread))),
        (State::Healthy, vec![])
    );
    // Unread in both, or with no read before that reads as flows: the reason.
    for before in [Some(unread.as_str()), None, Some("{\"rx\":7}")] {
        assert_eq!(
            judged(&run(&unread, before)),
            (State::Degraded, vec![Reason::LatencyUnread]),
            "{before:?}"
        );
    }
    // A path over its budget needs one read only.
    let over = GuardRead::Read(box_guard(127));
    let v = judge(&engine_up(), &engine_reads(), &same()).with_latency(
        &over,
        latency::Reads {
            shown: Some(STATUS),
            before: Some(&unread),
        },
    );
    assert_eq!(
        judged(&v),
        (State::Degraded, vec![Reason::LatencyOverBudget])
    );
}

#[test]
fn the_guard_s_line_is_numbers_and_fixed_words() {
    let guarded = judge(&engine_up(), &engine_reads(), &same())
        .with_latency(&GuardRead::Read(box_guard(127)), shown(STATUS));
    let unreadable = judge(&engine_up(), &engine_reads(), &same())
        .with_latency(&GuardRead::Unreadable, shown(STATUS));
    for v in [&guarded, &unreadable] {
        let parsed: serde_json::Value = serde_json::from_str(&v.line()).unwrap();
        let mut words = Vec::new();
        strings(&parsed, &mut words);
        for word in words {
            assert!(WORDS.contains(&word.as_str()), "{word:?} in {}", v.line());
        }
    }
    for reason in [Reason::LatencyOverBudget, Reason::LatencyUnread] {
        let word = serde_json::to_value(reason).unwrap();
        assert!(WORDS.contains(&word.as_str().unwrap()), "{word}");
    }
    // The guard's numbers, pinned once.
    assert!(
        guarded.line().ends_with(
            "\"server\":{\"answers\":1,\"build\":\"same\"},\"latency\":{\"inputs\":2,\
             \"known\":2,\"over\":1,\"unread_tx\":0,\"worst_samples\":128,\"budget_samples\":96}}}"
        ),
        "{}",
        guarded.line()
    );
    assert!(
        guarded
            .line()
            .contains("\"reasons\":[\"latency_over_budget\"],"),
        "{}",
        guarded.line()
    );
    assert!(
        unreadable.line().ends_with(
            "\"latency\":{\"inputs\":0,\"known\":0,\"over\":0,\"unread_tx\":0,\
             \"worst_samples\":null,\"budget_samples\":null}}}"
        ),
        "{}",
        unreadable.line()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn the_status_line_is_read_whole_from_the_control_socket() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    let dir = std::env::temp_dir().join(format!("iem-health-status-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("control.sock");
    let _ = std::fs::remove_file(&path);
    // No socket: no line.
    assert_eq!(status_line(&path, BOUND), None);
    let listener = UnixListener::bind(&path).unwrap();
    let served = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(BOUND)).unwrap();
        let mut request = String::new();
        BufReader::new(&stream).read_line(&mut request).unwrap();
        (&stream).write_all(b"{\"rx\":[]}\nmore\n").unwrap();
        request
    });
    assert_eq!(status_line(&path, BOUND).as_deref(), Some("{\"rx\":[]}\n"));
    assert_eq!(served.join().unwrap(), "status\n");
    std::fs::remove_dir_all(&dir).unwrap();
}
