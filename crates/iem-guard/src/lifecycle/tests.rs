//! The lifecycle's rules (S8 design note §3.1, §3.4, §5): every lifecycle
//! against every start, entry and crash loop, and the state file's backward
//! compatibility.

use std::collections::BTreeMap;

use super::*;
use crate::bundle::{Hil, Pins};
use crate::state::GuardState;

const PIN: &str = "0123456789abcdef0123456789abcdef01234567";
const PREV: &str = "89abcdef0123456789abcdef0123456789abcdef";
const NEW: &str = "fedcba9876543210fedcba9876543210fedcba98";
const DEV: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SINCE: u64 = 1_790_000_000;

fn rec(sha: &str, branch: &str, hil: Hil) -> Record {
    Record {
        sha: sha.to_owned(),
        branch: branch.to_owned(),
        run: 7,
        installed_at: SINCE,
        hil,
    }
}

/// `PIN`, `PREV` and `NEW` green on main, `DEV` green on dev.
fn installed() -> BTreeMap<String, Record> {
    let mut b = BTreeMap::new();
    for sha in [PIN, PREV, NEW] {
        b.insert(sha.to_owned(), rec(sha, "main", Hil::Green));
    }
    b.insert(DEV.to_owned(), rec(DEV, "dev", Hil::Green));
    b
}

fn prod(previous: Option<&str>, maintenance: Option<&str>) -> Prod {
    Prod {
        since: SINCE,
        pin: PIN.to_owned(),
        previous: previous.map(str::to_owned),
        maintenance: maintenance.map(str::to_owned),
    }
}

fn ask(to: Mode, build: Option<&str>, trial: bool) -> Ask<'_> {
    Ask { to, build, trial }
}

fn entered(lc: &Lifecycle, a: Ask<'_>, b: &BTreeMap<String, Record>) -> Result<Entered, String> {
    entry(lc, a, |sha| b.get(sha))
}

// ---- maintenance ----

#[test]
fn maintenance_ends_on_a_green_main_build_as_the_pin() {
    let b = installed();
    let (next, note) = prod(Some(PREV), Some(NEW)).end_maintenance(&|s: &str| b.get(s));
    assert_eq!(
        next,
        Prod {
            pin: NEW.into(),
            previous: Some(PIN.into()),
            ..prod(None, None)
        }
    );
    assert_eq!(
        note.as_deref(),
        Some(
            format!("maintenance build {NEW} becomes the pin; {PIN} is the previous pin").as_str()
        )
    );
}

#[test]
fn maintenance_on_anything_else_leaves_the_pin() {
    let mut b = installed();
    b.insert(NEW.into(), rec(NEW, "main", Hil::Red));
    // Red HIL, a dev build, a build no longer installed: the pin stays.
    for (m, why) in [
        (NEW, format!("{NEW}: HIL Red; live needs green")),
        (DEV, format!("{DEV} is from \"dev\"; live needs main")),
        (
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb is not installed".to_owned(),
        ),
    ] {
        let (next, note) = prod(Some(PREV), Some(m)).end_maintenance(&|s: &str| b.get(s));
        assert_eq!(next, prod(Some(PREV), None), "{m}");
        assert_eq!(
            note,
            Some(format!("maintenance build {m}: {why}; the pin {PIN} stays")),
            "{m}"
        );
    }
    // No session's build, or the pin itself: nothing to say.
    for m in [None, Some(PIN)] {
        let (next, note) = prod(Some(PREV), m).end_maintenance(&|s: &str| b.get(s));
        assert_eq!((next, note), (prod(Some(PREV), None), None), "{m:?}");
    }
}

// ---- the start ----

#[test]
fn before_the_cutover_a_start_is_event_on_reset_as_before() {
    let b = installed();
    for (reset, rebooted, want) in [
        (true, true, Start::Event),
        (true, false, Start::Event),
        (false, false, Start::Keep),
    ] {
        let s = start(&Lifecycle::Trial, reset, rebooted, |sha| b.get(sha));
        assert_eq!(
            s,
            Started {
                start: want,
                lifecycle: Lifecycle::Trial,
                note: None,
                alarm: None
            },
            "{reset} {rebooted}"
        );
    }
}

#[test]
fn in_prod_a_reboot_goes_live_on_the_pin_and_a_guard_restart_keeps_event() {
    let b = installed();
    let lc = Lifecycle::Prod(prod(Some(PREV), None));
    let s = start(&lc, true, true, |sha| b.get(sha));
    assert_eq!(
        s,
        Started {
            start: Start::Live(PIN.into()),
            lifecycle: lc.clone(),
            note: None,
            alarm: None
        }
    );
    // A guard restart with the band's system up: event stands.
    let s = start(&lc, true, false, |sha| b.get(sha));
    assert_eq!((s.start, s.lifecycle), (Start::Event, lc.clone()));
    let s = start(&lc, false, false, |sha| b.get(sha));
    assert_eq!((s.start, s.lifecycle), (Start::Keep, lc));
}

#[test]
fn a_reboot_ends_a_maintenance_session() {
    let b = installed();
    let lc = Lifecycle::Prod(prod(None, Some(NEW)));
    let s = start(&lc, true, true, |sha| b.get(sha));
    assert_eq!(s.start, Start::Live(NEW.into()));
    assert_eq!(
        s.lifecycle,
        Lifecycle::Prod(Prod {
            pin: NEW.into(),
            previous: Some(PIN.into()),
            ..prod(None, None)
        })
    );
    assert!(s.note.is_some_and(|n| n.contains("becomes the pin")));
    // A guard restart does not end it.
    let s = start(&lc, false, false, |sha| b.get(sha));
    assert_eq!(s.lifecycle, lc);
}

/// G8 at the boot (review of lane 1): the state file is the user's, and
/// HIL may report the pin red later. A pin that is no installed green main
/// build goes nowhere: event, an alarm, the lifecycle as it was.
#[test]
fn in_prod_a_reboot_on_a_pin_that_may_not_go_live_stays_in_event() {
    let mut b = installed();
    b.insert(PIN.into(), rec(PIN, "main", Hil::Red));
    let lc = Lifecycle::Prod(prod(Some(PREV), None));
    assert_eq!(
        start(&lc, true, true, |sha| b.get(sha)),
        Started {
            start: Start::Event,
            lifecycle: lc.clone(),
            note: None,
            alarm: Some(format!(
                "after a reboot in prod: {PIN}: HIL Red; live needs green; the PC stays in \
                 event"
            ))
        }
    );
    b.remove(PIN);
    let s = start(&lc, true, true, |sha| b.get(sha));
    assert_eq!((s.start, s.lifecycle), (Start::Event, lc.clone()));
    assert_eq!(
        s.alarm,
        Some(format!(
            "after a reboot in prod: the pin {PIN} is not installed; the PC stays in event"
        ))
    );
    // A maintenance build that becomes the pin is checked by then.
    let mut b = installed();
    b.insert(PIN.into(), rec(PIN, "main", Hil::Red));
    let s = start(&Lifecycle::Prod(prod(None, Some(NEW))), true, true, |sha| {
        b.get(sha)
    });
    assert_eq!((s.start, s.alarm), (Start::Live(NEW.into()), None));
}

#[test]
fn a_rollback_goes_on_to_event_at_every_start() {
    let b = installed();
    for (reset, rebooted) in [(true, true), (true, false), (false, false)] {
        let s = start(&Lifecycle::RollingBack, reset, rebooted, |sha| b.get(sha));
        assert_eq!(
            s,
            Started {
                start: Start::Event,
                lifecycle: Lifecycle::RollingBack,
                note: Some("a rollback to REAPER runs: the PC goes to event".into()),
                alarm: None
            }
        );
    }
}

// ---- the entry gates ----

#[test]
fn a_build_must_be_installed_in_every_lifecycle() {
    let b = installed();
    let missing = "cccccccccccccccccccccccccccccccccccccccc";
    for lc in [
        Lifecycle::Trial,
        Lifecycle::Prod(prod(None, None)),
        Lifecycle::RollingBack,
    ] {
        for to in [Mode::Dev, Mode::Live] {
            assert_eq!(
                entered(&lc, ask(to, Some(missing), to == Mode::Live), &b),
                Err(format!("bundle {missing} is not installed")),
                "{lc:?} {to:?}"
            );
        }
    }
}

#[test]
fn an_entry_goes_to_dev_or_live() {
    let b = installed();
    for lc in [Lifecycle::Trial, Lifecycle::Prod(prod(None, None))] {
        assert_eq!(
            entered(&lc, ask(Mode::Event, None, false), &b),
            Err("an entry goes to dev or live".to_owned())
        );
    }
}

#[test]
fn before_the_cutover_dev_runs_any_build_and_live_only_a_green_main_trial() {
    let b = installed();
    let lc = Lifecycle::Trial;
    for build in [None, Some(DEV), Some(PIN)] {
        assert_eq!(
            entered(&lc, ask(Mode::Dev, build, false), &b),
            Ok(Entered {
                runs: build.map(str::to_owned),
                lifecycle: Lifecycle::Trial,
                note: None
            }),
            "{build:?}"
        );
    }
    assert_eq!(
        entered(&lc, ask(Mode::Live, Some(NEW), true), &b),
        Ok(Entered {
            runs: Some(NEW.into()),
            lifecycle: Lifecycle::Trial,
            note: None
        })
    );
    assert_eq!(
        entered(&lc, ask(Mode::Live, Some(NEW), false), &b),
        Err(format!(
            "before the cutover live is a trial: live --build {NEW} --trial"
        ))
    );
    // G8 comes first: a trial too needs a green main build.
    for trial in [false, true] {
        assert_eq!(
            entered(&lc, ask(Mode::Live, Some(DEV), trial), &b),
            Err(format!("{DEV} is from \"dev\"; live needs main"))
        );
    }
    assert_eq!(
        entered(&lc, ask(Mode::Live, None, true), &b),
        Err("live needs --build SHA".to_owned())
    );
}

#[test]
fn in_prod_dev_is_maintenance_on_its_build() {
    let b = installed();
    let lc = Lifecycle::Prod(prod(Some(PREV), None));
    assert_eq!(
        entered(&lc, ask(Mode::Dev, Some(DEV), false), &b),
        Ok(Entered {
            runs: Some(DEV.into()),
            lifecycle: Lifecycle::Prod(prod(Some(PREV), Some(DEV))),
            note: None
        })
    );
    // Without a build it runs the active bundle and keeps the session's build.
    let lc = Lifecycle::Prod(prod(Some(PREV), Some(NEW)));
    assert_eq!(
        entered(&lc, ask(Mode::Dev, None, false), &b),
        Ok(Entered {
            runs: None,
            lifecycle: lc.clone(),
            note: None
        })
    );
}

#[test]
fn in_prod_live_runs_the_pin_and_ends_maintenance() {
    let b = installed();
    let lc = Lifecycle::Prod(prod(Some(PREV), None));
    assert_eq!(
        entered(&lc, ask(Mode::Live, Some(PIN), false), &b),
        Ok(Entered {
            runs: Some(PIN.into()),
            lifecycle: lc.clone(),
            note: None
        })
    );
    assert_eq!(
        entered(&lc, ask(Mode::Live, Some(NEW), false), &b),
        Err(format!(
            "in prod live runs the pin {PIN}: live --build {PIN}"
        ))
    );
    // `live` without a build runs the pin.
    assert_eq!(
        entered(&lc, ask(Mode::Live, None, false), &b),
        Ok(Entered {
            runs: Some(PIN.into()),
            lifecycle: lc.clone(),
            note: None
        })
    );
    // A green maintenance build becomes the pin: live runs it.
    let lc = Lifecycle::Prod(prod(Some(PREV), Some(NEW)));
    let e = entered(&lc, ask(Mode::Live, Some(NEW), false), &b).unwrap();
    // Without a build live runs the pin the session ends on (lane 2: `live
    // --build` is optional in prod).
    assert_eq!(
        entered(&lc, ask(Mode::Live, None, false), &b),
        Ok(e.clone())
    );
    assert_eq!(e.runs.as_deref(), Some(NEW));
    assert_eq!(
        e.lifecycle,
        Lifecycle::Prod(Prod {
            pin: NEW.into(),
            previous: Some(PIN.into()),
            ..prod(None, None)
        })
    );
    assert!(e.note.is_some_and(|n| n.contains("becomes the pin")));
    let refused = entered(&lc, ask(Mode::Live, Some(PIN), false), &b);
    assert_eq!(
        refused,
        Err(format!(
            "in prod live runs the pin {NEW} (maintenance build {NEW} becomes the pin; {PIN} is \
             the previous pin): live --build {NEW}"
        ))
    );
}

#[test]
fn in_prod_live_is_refused_on_a_pin_that_may_not_go_live() {
    let mut b = installed();
    b.insert(PIN.into(), rec(PIN, "main", Hil::Red));
    let lc = Lifecycle::Prod(prod(None, None));
    assert_eq!(
        entered(&lc, ask(Mode::Live, Some(PIN), false), &b),
        Err(format!(
            "in prod live runs the pin, and {PIN}: HIL Red; live needs green"
        ))
    );
    // Dev (maintenance) is no live: it runs.
    assert!(entered(&lc, ask(Mode::Dev, Some(NEW), false), &b).is_ok());
}

#[test]
fn in_prod_a_red_maintenance_build_leaves_the_pin() {
    let mut b = installed();
    b.insert(NEW.into(), rec(NEW, "main", Hil::Red));
    let lc = Lifecycle::Prod(prod(Some(PREV), Some(NEW)));
    let why =
        format!("maintenance build {NEW}: {NEW}: HIL Red; live needs green; the pin {PIN} stays");
    assert_eq!(
        entered(&lc, ask(Mode::Live, Some(NEW), false), &b),
        Err(format!(
            "in prod live runs the pin {PIN} ({why}): live --build {PIN}"
        ))
    );
    assert_eq!(
        entered(&lc, ask(Mode::Live, Some(PIN), false), &b),
        Ok(Entered {
            runs: Some(PIN.into()),
            lifecycle: Lifecycle::Prod(prod(Some(PREV), None)),
            note: Some(why)
        })
    );
}

#[test]
fn in_prod_there_are_no_trials_and_in_a_rollback_no_entries() {
    let b = installed();
    let lc = Lifecycle::Prod(prod(None, None));
    for to in [Mode::Dev, Mode::Live] {
        assert_eq!(
            entered(&lc, ask(to, Some(PIN), true), &b),
            Err(format!(
                "after the cutover there are no trials: live runs the pin {PIN}"
            ))
        );
    }
    for (to, build, trial) in [
        (Mode::Dev, None, false),
        (Mode::Dev, Some(DEV), false),
        (Mode::Live, Some(PIN), false),
        (Mode::Live, Some(PIN), true),
    ] {
        assert_eq!(
            entered(&Lifecycle::RollingBack, ask(to, build, trial), &b),
            Err(ROLLING_BACK.to_owned())
        );
    }
}

// ---- the crash rules ----

#[test]
fn a_crash_loop_in_every_lifecycle_and_mode() {
    let b = installed();
    let at = |lc: &Lifecycle, mode| crash_loop(lc, mode, |sha| b.get(sha));
    let modes = [Mode::Event, Mode::Dev, Mode::Live];
    for lc in [Lifecycle::Trial, Lifecycle::RollingBack] {
        for mode in modes {
            assert_eq!(at(&lc, mode), (Fallback::Event, lc.clone()));
        }
    }
    let lc = Lifecycle::Prod(prod(Some(PREV), Some(NEW)));
    assert_eq!(at(&lc, Mode::Event), (Fallback::Event, lc.clone()));
    // Maintenance: live on the pin, the session's build dropped.
    assert_eq!(
        at(&lc, Mode::Dev),
        (
            Fallback::Pin(PIN.into()),
            Lifecycle::Prod(prod(Some(PREV), None))
        )
    );
    // Prod live: the previous pin, which is the pin from now on.
    let lc = Lifecycle::Prod(prod(Some(PREV), None));
    let (fallback, next) = at(&lc, Mode::Live);
    assert_eq!(fallback, Fallback::Previous(PREV.into()));
    assert_eq!(
        next,
        Lifecycle::Prod(Prod {
            pin: PREV.into(),
            ..prod(None, None)
        })
    );
    // It loops too: down, the pin stays.
    assert_eq!(
        at(&next, Mode::Live),
        (
            Fallback::Down(format!("on the pin {PREV}, and no previous pin is left")),
            next.clone()
        )
    );
}

/// G8 on the crash paths (the review of lane 1): a pin the fallback would
/// go live on is checked like at the boot. Maintenance on a red pin goes
/// back to REAPER; prod live with a red previous pin stays down.
#[test]
fn a_crash_loop_never_falls_back_live_on_a_pin_that_may_not_go_live() {
    let mut b = installed();
    b.insert(PIN.into(), rec(PIN, "main", Hil::Red));
    b.insert(PREV.into(), rec(PREV, "dev", Hil::Green));
    let lc = Lifecycle::Prod(prod(Some(PREV), Some(NEW)));
    assert_eq!(
        crash_loop(&lc, Mode::Dev, |sha| b.get(sha)),
        (
            Fallback::Reaper(format!("{PIN}: HIL Red; live needs green")),
            Lifecycle::Prod(prod(Some(PREV), None))
        )
    );
    let lc = Lifecycle::Prod(prod(Some(PREV), None));
    assert_eq!(
        crash_loop(&lc, Mode::Live, |sha| b.get(sha)),
        (
            Fallback::Down(format!(
                "on the pin {PIN}, and the previous pin may not go live ({PREV} is from \
                 \"dev\"; live needs main)"
            )),
            lc.clone()
        )
    );
    b.remove(PREV);
    assert_eq!(
        crash_loop(&lc, Mode::Live, |sha| b.get(sha)).0,
        Fallback::Down(format!(
            "on the pin {PIN}, and the previous pin may not go live (the pin {PREV} is not \
             installed)"
        ))
    );
}

// ---- exclusions and status ----

#[test]
fn the_active_bundle_before_and_the_prod_pins_keep_their_exclusions() {
    assert_eq!(kept(&Lifecycle::Trial, Some(PREV), PIN), [PREV]);
    assert_eq!(kept(&Lifecycle::Trial, None, PIN), Vec::<String>::new());
    assert_eq!(
        kept(&Lifecycle::RollingBack, Some(PIN), PIN),
        Vec::<String>::new()
    );
    let lc = Lifecycle::Prod(prod(Some(PREV), None));
    assert_eq!(kept(&lc, Some(DEV), NEW), [DEV, PIN, PREV]);
    // Each once, never the bundle activated.
    assert_eq!(kept(&lc, Some(PIN), PREV), [PIN]);
    assert_eq!(kept(&lc, None, NEW), [PIN, PREV]);
    let lc = Lifecycle::Prod(prod(None, None));
    assert_eq!(kept(&lc, Some(PIN), DEV), [PIN]);
}

#[test]
fn the_status_names_the_lifecycle_after_the_trial_only() {
    assert_eq!(status(&Lifecycle::Trial), None);
    assert_eq!(
        status(&Lifecycle::RollingBack).as_deref(),
        Some("rolling back to REAPER")
    );
    assert_eq!(
        status(&Lifecycle::Prod(prod(None, None))),
        Some(format!("prod since {SINCE}: pin {PIN}, previous none"))
    );
    assert_eq!(
        status(&Lifecycle::Prod(prod(Some(PREV), Some(NEW)))),
        Some(format!(
            "prod since {SINCE}: pin {PIN}, previous {PREV}, maintenance {NEW}"
        ))
    );
}

// ---- the state file ----

#[test]
fn the_lifecycle_and_the_active_bundle_round_trip_and_a_reset_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("guard-state.json");
    for lc in [
        Lifecycle::Trial,
        Lifecycle::Prod(prod(Some(PREV), Some(NEW))),
        Lifecycle::RollingBack,
    ] {
        let mut st = GuardState {
            mode: Mode::Live,
            active: Some(NEW.into()),
            lifecycle: lc.clone(),
            ..GuardState::default()
        };
        st.save(&path, SINCE).unwrap();
        let (back, err) = GuardState::load(&path);
        assert_eq!(err, None);
        assert_eq!(back, st);
        st.reset();
        assert_eq!((st.lifecycle, st.active.as_deref()), (lc, Some(NEW)));
    }
    // The JSON people read on the PC.
    let text = serde_json::to_string(&Lifecycle::Prod(prod(None, None))).unwrap();
    assert_eq!(
        text,
        format!(
            r#"{{"prod":{{"since":{SINCE},"pin":"{PIN}","previous":null,"maintenance":null}}}}"#
        )
    );
    assert_eq!(
        serde_json::to_string(&Lifecycle::Trial).unwrap(),
        r#""trial""#
    );
    assert_eq!(
        serde_json::to_string(&Lifecycle::RollingBack).unwrap(),
        r#""rolling_back""#
    );
}

/// A state an older guard saved (no lifecycle, no active bundle, its pins)
/// loads as `Trial`, and its `pins.current` is the active bundle.
#[test]
fn an_older_guards_state_is_trial_and_its_pin_is_the_active_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("guard-state.json");
    let old = format!(
        r#"{{"mode": "dev", "written_at": 7, "pins": {{"current": "{PIN}", "previous": "{PREV}"}}}}"#
    );
    std::fs::write(&path, old).unwrap();
    let (st, err) = GuardState::load(&path);
    assert_eq!(err, None);
    assert_eq!(st.lifecycle, Lifecycle::Trial);
    assert_eq!(st.active, None);
    assert_eq!(st.active_bundle(), Some(PIN));
    // Once set, the active bundle wins over the older record.
    let st = GuardState {
        active: Some(NEW.into()),
        ..st
    };
    assert_eq!(st.active_bundle(), Some(NEW));
    assert_eq!(GuardState::default().active_bundle(), None);
}

/// A lifecycle this guard cannot read (a newer guard's shape, a pin
/// missing) is `Trial`, never an unreadable state: every boot is event, and
/// the load names it (the guard raises it); a null one is trial quietly.
#[test]
fn an_unreadable_lifecycle_is_trial_not_an_unreadable_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("guard-state.json");
    for (lc, alarmed) in [
        (r#""drill""#, true),
        (r#"{"prod": {"since": 1}}"#, true),
        (r#"{"later": {}}"#, true),
        ("7", true),
        ("null", false),
    ] {
        std::fs::write(&path, format!(r#"{{"mode": "live", "lifecycle": {lc}}}"#)).unwrap();
        let (st, err) = GuardState::load(&path);
        assert_eq!(err.is_some(), alarmed, "{lc}: {err:?}");
        if let Some(e) = err {
            assert!(e.starts_with("the saved lifecycle is unreadable ("), "{e}");
            assert!(e.ends_with("): trial, every boot in event"), "{e}");
        }
        assert_eq!(
            st,
            GuardState {
                mode: Mode::Live,
                ..GuardState::default()
            },
            "{lc}"
        );
    }
}

/// An older guard reading this guard's state: `GuardState` (the older
/// guard's too, it has no `deny_unknown_fields`) skips fields it does not
/// know, and `pins` is written as it was read, so the older guard finds its
/// own record. Shown here with a field this guard does not know.
#[test]
fn a_newer_guards_fields_are_skipped_and_the_pins_kept() {
    let st = GuardState {
        pins: Pins {
            current: Some(PIN.into()),
            previous: Some(PREV.into()),
        },
        active: Some(NEW.into()),
        lifecycle: Lifecycle::Prod(prod(None, None)),
        ..GuardState::default()
    };
    let mut value = serde_json::to_value(&st).unwrap();
    value["a_later_field"] = serde_json::json!({"x": 1});
    let back: GuardState = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(back, st);
    assert_eq!(
        value["pins"],
        serde_json::json!({"current": PIN, "previous": PREV})
    );
}

/// The way back (review of lane 1): as `Pins::promote` kept it before S8,
/// the bundle active before the active one, moved only when the active
/// bundle changes to another build. HIL's `dev --build B` then `activate B`
/// keeps A's Defender exclusions.
#[test]
fn the_way_back_moves_only_when_the_active_bundle_changes() {
    // A state an older guard saved: its pins are the active bundle and the
    // way back.
    let mut st = GuardState {
        pins: Pins {
            current: Some(PIN.into()),
            previous: Some(PREV.into()),
        },
        ..GuardState::default()
    };
    assert_eq!(
        (st.active_bundle(), st.way_back_bundle()),
        (Some(PIN), Some(PREV))
    );
    // The same build again: nothing moves.
    st.set_active(PIN);
    assert_eq!(
        (st.active_bundle(), st.way_back_bundle()),
        (Some(PIN), Some(PREV))
    );
    // Another build: the active one becomes the way back.
    st.set_active(NEW);
    assert_eq!(
        (st.active_bundle(), st.way_back_bundle()),
        (Some(NEW), Some(PIN))
    );
    st.set_active(NEW);
    assert_eq!(
        (st.active_bundle(), st.way_back_bundle()),
        (Some(NEW), Some(PIN))
    );
    // The older record mirrors them (lane 2, the PC on 2026-10-10): an
    // older guard that takes over runs the active bundle.
    assert_eq!(
        st.pins,
        Pins {
            current: Some(NEW.into()),
            previous: Some(PIN.into()),
        }
    );
    // From nothing: no way back.
    let mut st = GuardState::default();
    assert_eq!(st.way_back_bundle(), None);
    st.set_active(DEV);
    assert_eq!(
        (st.active_bundle(), st.way_back_bundle()),
        (Some(DEV), None)
    );
    assert_eq!(
        st.pins,
        Pins {
            current: Some(DEV.into()),
            previous: None,
        }
    );
}

/// The prod data rule (ROZHODNUTÉ on #11): only a trial's entries refresh
/// the data from the predecessor; in prod and while rolling back the plan
/// has no data step and is otherwise the planner's, step for step.
#[test]
fn only_a_trial_refreshes_the_data_from_the_predecessor() {
    let lifecycles = [
        Lifecycle::Trial,
        Lifecycle::Prod(prod(None, None)),
        Lifecycle::Prod(prod(Some(PREV), Some(NEW))),
        Lifecycle::RollingBack,
    ];
    for lc in &lifecycles {
        let trial = *lc == Lifecycle::Trial;
        assert_eq!(refreshes_data(lc), trial, "{lc:?}");
        for to in [Mode::Event, Mode::Dev, Mode::Live] {
            for bits in 0..(1 << plan::FACT_BITS) {
                let facts = Facts::from_bits(bits);
                let want: Vec<Step> = plan::plan(to, &facts)
                    .into_iter()
                    .filter(|s| trial || *s != Step::Data)
                    .collect();
                assert_eq!(super::plan(lc, to, &facts), want, "{lc:?} {to:?} {bits}");
            }
        }
    }
    let entry = super::plan(&Lifecycle::Trial, Mode::Live, &Facts::default());
    assert!(entry.contains(&Step::Data), "a trial imports: {entry:?}");
}

/// S8 lane 5: in prod and while rolling back `activate` takes only a bundle
/// whose guard keeps the lifecycle; before the cutover the manifest is not
/// even read.
#[test]
fn activate_after_the_cutover_takes_only_a_guard_that_keeps_the_lifecycle() {
    let unread = || -> Result<bool, String> { panic!("read in trial") };
    assert_eq!(activation_refusal(&Lifecycle::Trial, NEW, unread), None);
    for lc in [Lifecycle::Prod(prod(None, None)), Lifecycle::RollingBack] {
        assert_eq!(activation_refusal(&lc, NEW, || Ok(true)), None, "{lc:?}");
        let older = activation_refusal(&lc, NEW, || Ok(false)).unwrap();
        assert!(
            older.starts_with(&format!(
                "{NEW}'s guard predates the lifecycle (its manifest names no guard_lifecycle): in "
            )),
            "{older}"
        );
        assert!(older.contains(&status(&lc).unwrap()), "{older}");
        let unreadable =
            activation_refusal(&lc, NEW, || Err("manifest.json: not found".to_owned())).unwrap();
        assert!(
            unreadable.starts_with(&format!(
                "{NEW}'s manifest cannot be read (manifest.json: not found)"
            )),
            "{unreadable}"
        );
    }
}
