//! The engine in the guard's reply (S7, #10): the fields HIL reads and the
//! largest reply. Apart from `proto.rs`'s own tests to keep that file under
//! its size budget (#36).

use super::*;
use crate::alarms::Alarms;
use crate::plan::Step;
use crate::switch_log::{StepTime, SwitchOutcome};

pub(super) fn an_engine() -> EngineStatus {
    EngineStatus {
        build: "0123456789abcdef0123456789abcdef01234567".into(),
        frames: 32,
        callbacks: 360_000,
        missed: 1,
        resets: 2,
        parked: false,
        faulted: true,
        pipe_private: true,
        spawns: 3,
        last_exit: Some(70),
        hil: vec![
            HilOut {
                tx: 94,
                peak: 0.0316,
            },
            HilOut { tx: 95, peak: 0.0 },
        ],
        loopback_samples: 0,
        loopback_ms: 0.0,
        pid: Some(4242),
        late: 5,
        overruns: 1,
        process_max_us: 61.5,
        hist_top_us: 667,
        interval_hist: vec![(333, 359_990), (400, 9)],
        process_hist: vec![(60, 360_000)],
        pipe_server_pid: Some(4242),
        last_reopen_us: 104_000,
        last_fault_us: Some(412.5),
    }
}

#[test]
fn the_engine_carries_the_fields_hil_v1_reads() {
    // hil-v1.ps1 reads these names from `iemmode status` (design §7).
    let reply = Reply {
        ok: true,
        mode: Mode::Dev,
        switching: None,
        alarms: Vec::new(),
        detail: String::new(),
        engine: Some(an_engine()),
        guard_build: None,
        last_switch: None,
    };
    let v = serde_json::to_value(&reply).unwrap();
    assert_eq!(
        v["engine"],
        serde_json::json!({
            "build": "0123456789abcdef0123456789abcdef01234567",
            "frames": 32,
            "callbacks": 360_000,
            "missed": 1,
            "resets": 2,
            "parked": false,
            "faulted": true,
            "pipe_private": true,
            "spawns": 3,
            "last_exit": 70,
            "hil": [{"tx": 94, "peak": 0.0316}, {"tx": 95, "peak": 0.0}],
            "loopback_samples": 0,
            "loopback_ms": 0.0,
            "pid": 4242,
            "late": 5,
            "overruns": 1,
            "process_max_us": 61.5,
            "hist_top_us": 667,
            "interval_hist": [[333, 359_990], [400, 9]],
            "process_hist": [[60, 360_000]],
            "pipe_server_pid": 4242,
            "last_reopen_us": 104_000,
            "last_fault_us": 412.5,
        })
    );
    assert_eq!(
        decode::<Reply>(&serde_json::to_vec(&reply).unwrap())
            .unwrap()
            .engine,
        Some(an_engine())
    );
    // No engine: no key at all, so replies without one stay as before.
    let idle = Reply {
        engine: None,
        ..reply
    };
    assert_eq!(
        serde_json::to_string(&idle).unwrap(),
        r#"{"ok":true,"mode":"dev","switching":null,"alarms":[],"detail":""}"#
    );
    // A partial engine (an older guard) defaults the rest; no exit yet is null.
    assert_eq!(
        decode::<Reply>(br#"{"ok":true,"mode":"dev","switching":null,"engine":{"frames":32}}"#)
            .unwrap()
            .engine,
        Some(EngineStatus {
            frames: 32,
            ..EngineStatus::default()
        })
    );
    let fresh = serde_json::to_value(EngineStatus::default()).unwrap();
    assert_eq!(fresh["last_exit"], serde_json::Value::Null);
    assert_eq!(fresh["spawns"], 0);
    assert_eq!(fresh["hil"], serde_json::json!([]));
    // S7: no pid known is null; without histograms (an older engine) no
    // histogram keys and the top 0.
    assert_eq!(fresh["pid"], serde_json::Value::Null);
    assert_eq!(fresh["hist_top_us"], 0);
    assert_eq!(fresh.get("interval_hist"), None);
    assert_eq!(fresh.get("process_hist"), None);
    // S7 HIL v2 (#10): no server pid read and no fault kept are null, no
    // reopen 0, so HIL v2 names what an older guard lacks.
    assert_eq!(fresh["pipe_server_pid"], serde_json::Value::Null);
    assert_eq!(fresh["last_reopen_us"], 0);
    assert_eq!(fresh["last_fault_us"], serde_json::Value::Null);
}

/// The guard's largest reply fits one frame (S7, #10): every kept alarm
/// and the detail at their character caps, a switch with every step,
/// and an engine with every spare output (8), both histograms as long as
/// `effects::engine::parse` reads them (1001 buckets each, the 1 ms cap)
/// and its counters at their largest, and an unwound last switch of
/// `2 × Step::ALL` steps (an unwind's record holds the entry's steps,
/// then its own; a plan runs each step once, the health read included,
/// S7 part 3). The texts go through `cut` as the
/// guard's do (it counts characters): once four-byte characters, the
/// longest a character is in UTF-8, once C0 control characters, which
/// JSON would escape to six bytes each and `cut` makes spaces (S7 Task 3
/// review). Each is above the old 64 KiB cap: the reason the cap is 256
/// KiB.
#[test]
fn the_largest_reply_fits_a_frame() {
    use crate::daemon::{ALARM_CHARS, DETAIL_CHARS, cut};
    use crate::effects::engine::{HIST_LEN_MAX, HIST_TOP_MAX};
    let longest = Step::ALL
        .into_iter()
        .max_by_key(|s| serde_json::to_string(s).unwrap().len())
        .unwrap();
    let full = vec![(HIST_TOP_MAX, u64::MAX); HIST_LEN_MAX];
    // An unwind's record: the entry's steps, then the unwind's; a plan
    // runs a step at most once (S7 part 3).
    let record = LastSwitch {
        from: Mode::Event,
        to: Mode::Event,
        ended_in: Mode::Event,
        outcome: SwitchOutcome::KeptServing,
        started: u64::MAX,
        ended: u64::MAX,
        steps: vec![
            StepTime {
                step: longest,
                ms: u64::MAX,
            };
            2 * Step::ALL.len()
        ],
        silence_ms: Some(u64::MAX),
        unwound: Some(Mode::Live),
    };
    for chars in ["\u{1F3A7}", "\u{0}\u{1f}"] {
        let text = |n: usize| cut(&chars.repeat(n), n);
        let mut alarms = Alarms::default();
        for _ in 0..Alarms::KEEP {
            alarms.raise(u64::MAX, Some(longest), text(ALARM_CHARS), true);
        }
        let reply = Reply {
            ok: false,
            mode: Mode::Live,
            switching: Some(Switching {
                from: Mode::Live,
                to: Mode::Event,
                done: Step::ALL.to_vec(),
                started: u64::MAX,
            }),
            alarms: alarms.all().to_vec(),
            detail: text(DETAIL_CHARS),
            engine: Some(EngineStatus {
                frames: u32::MAX,
                callbacks: u64::MAX,
                missed: u64::MAX,
                resets: u64::MAX,
                spawns: u64::MAX,
                last_exit: Some(i32::MIN),
                hil: vec![
                    HilOut {
                        tx: u16::MAX,
                        peak: 0.0316,
                    };
                    8
                ],
                loopback_samples: u64::MAX,
                loopback_ms: 333.25,
                pid: Some(u32::MAX),
                late: u64::MAX,
                overruns: u64::MAX,
                process_max_us: 61.5,
                hist_top_us: u32::MAX,
                interval_hist: full.clone(),
                process_hist: full.clone(),
                pipe_server_pid: Some(u32::MAX),
                last_reopen_us: u64::MAX,
                last_fault_us: Some(f64::MAX),
                ..an_engine()
            }),
            guard_build: Some(GUARD_BUILD.into()),
            last_switch: Some(record.clone()),
        };
        let mut wire = Vec::new();
        write_frame(&mut wire, &reply).unwrap_or_else(|e| panic!("{chars:?}: {e}"));
        let body = wire.len() - 4;
        assert!(
            body > 64 * 1024,
            "{chars:?}: {body} bytes: the old cap would do"
        );
        assert_eq!(read_msg::<Reply, _>(&mut wire.as_slice()).unwrap(), reply);
    }
}
