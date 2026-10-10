//! The shadow import's place in an entry, its history line and its
//! never-failing step result (S8 lane 4, #11). Synthetic values only (P6).

use serde_json::json;

use super::*;
use crate::plan::{Facts, plan as planned};

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

/// A dev entry's plan from event with REAPER and the app up.
fn entry_steps(to: Mode) -> Vec<Step> {
    let up = Facts {
        reaper: true,
        app: true,
        reaper_holds_module: true,
        app_serves: true,
        ..Facts::default()
    };
    planned(to, &up)
}

#[test]
fn an_entry_from_event_runs_the_shadow_right_after_tuning_enter() {
    for to in [Mode::Dev, Mode::Live] {
        let steps = entry_steps(to);
        let at = steps.iter().position(|s| *s == Step::TuningEnter).unwrap();
        let mut want = steps.clone();
        want.insert(at + 1, Step::Shadow);
        let got = plan(steps, Mode::Event, to, true);
        assert_eq!(got, want, "{to:?}");
        // Before the data refresh, after REAPER's save and quit.
        let pos = |s: Step| got.iter().position(|x| *x == s).unwrap();
        assert!(pos(Step::ReaperSaveQuit) < pos(Step::Shadow));
        assert!(pos(Step::Shadow) < pos(Step::Data));
    }
}

#[test]
fn no_shadow_unless_configured_from_event_into_dev_or_live() {
    for to in [Mode::Dev, Mode::Live] {
        let steps = entry_steps(to);
        assert_eq!(plan(steps.clone(), Mode::Event, to, false), steps);
        assert_eq!(plan(steps.clone(), Mode::Dev, to, true), steps);
        assert_eq!(plan(steps.clone(), Mode::Live, to, true), steps);
    }
    let event = planned(Mode::Event, &Facts::default());
    for from in [Mode::Event, Mode::Dev, Mode::Live] {
        assert_eq!(plan(event.clone(), from, Mode::Event, true), event);
    }
    // A plan without TuningEnter gets none either.
    let bare = vec![Step::Precheck, Step::Data];
    assert_eq!(plan(bare.clone(), Mode::Event, Mode::Dev, true), bare);
}

#[test]
fn the_entry_is_named_from_event() {
    assert_eq!(entry(Mode::Dev), "event→dev");
    assert_eq!(entry(Mode::Live), "event→live");
    assert_eq!(entry(Mode::Event), "event→event");
}

fn line(text: &str) -> serde_json::Value {
    assert!(!text.contains('\n'), "one line: {text}");
    serde_json::from_str(text).unwrap()
}

#[test]
fn a_report_is_recorded_after_the_entry_it_belongs_to() {
    let report = r#"{"import":"writes","counts":{"tracks":45},"site":[],
        "state":[{"kind":"mix","id":"member1","field":"volume"},
                 {"kind":"level","id":"member1/mic1","field":"gain"}],
        "at":1,"entry":"x","bundle":"y"}"#;
    let (text, said) = record(
        1_790_000_000_123,
        Mode::Dev,
        Some(SHA),
        Run::Printed(format!("{report}\n")),
    );
    assert_eq!(
        line(&text),
        json!({
            "at": 1_790_000_000_123_u64,
            "entry": "event→dev",
            "bundle": SHA,
            "import": "writes",
            "counts": {"tracks": 45},
            "site": [],
            "state": [
                {"kind": "mix", "id": "member1", "field": "volume"},
                {"kind": "level", "id": "member1/mic1", "field": "gain"},
            ],
        })
    );
    assert_eq!(
        said,
        "shadow import (event→dev): import writes, 0 site and 2 state difference(s)"
    );
    // No bundle, and a report without its lists.
    let (text, said) = record(7, Mode::Live, None, Run::Printed("{}".into()));
    assert_eq!(
        line(&text),
        json!({"at": 7, "entry": "event→live", "bundle": null})
    );
    assert_eq!(
        said,
        "shadow import (event→live): import ?, 0 site and 0 state difference(s)"
    );
    let (_, said) = record(
        7,
        Mode::Live,
        None,
        Run::Printed(r#"{"import":"refuses_topology","site":[1,2,3]}"#.into()),
    );
    assert_eq!(
        said,
        "shadow import (event→live): import refuses_topology, 3 site and 0 state difference(s)"
    );
}

#[test]
fn every_failure_is_a_line_with_its_code_and_why() {
    let see = "see shadow\\history.jsonl";
    let cases = [
        (
            Run::Printed("not json".into()),
            json!({"error": "report", "why": "it printed no JSON object"}),
            "report",
        ),
        (
            Run::Printed("[1, 2]".into()),
            json!({"error": "report", "why": "it printed no JSON object"}),
            "report",
        ),
        (
            Run::Exited(Some(2), "iem-migrate: bad aliases".into()),
            json!({"error": "exit", "exit": 2, "why": "iem-migrate: bad aliases"}),
            "exit",
        ),
        (
            Run::Exited(None, String::new()),
            json!({"error": "exit", "exit": null, "why": ""}),
            "exit",
        ),
        (
            Run::Failed("iem-migrate.exe did not finish within 5 s".into()),
            json!({"error": "failed", "why": "iem-migrate.exe did not finish within 5 s"}),
            "failed",
        ),
        (
            Run::Preempted,
            json!({"error": "preempted", "why": "\"ide event\" came while it ran"}),
            "preempted",
        ),
    ];
    for (run, extra, code) in cases {
        let (text, said) = record(9, Mode::Dev, Some(SHA), run);
        let mut want = json!({"at": 9, "entry": "event→dev", "bundle": SHA});
        for (k, v) in extra.as_object().unwrap() {
            want[k] = v.clone();
        }
        assert_eq!(line(&text), want, "{code}");
        assert_eq!(
            said,
            format!("shadow import (event→dev): not recorded ({code}), {see}")
        );
    }
}

#[test]
fn the_shadow_never_fails_an_entry_but_event_still_preempts_it() {
    assert_eq!(never_fails(Ok("said".into())), Ok("said".into()));
    assert_eq!(
        never_fails(Err(StepError::failed("no history"))),
        Ok("shadow import not recorded: no history".into())
    );
    assert_eq!(
        never_fails(Err(StepError::Preempted)),
        Err(StepError::Preempted)
    );
}

#[test]
fn the_bound_keeps_the_entry_short() {
    assert_eq!(LIMIT.as_secs(), 5);
    assert_eq!((DIR, HISTORY), ("shadow", "history.jsonl"));
}
