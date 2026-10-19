//! What a prod dev or live entry that stops leaves and what its alarm says
//! (the owner's decision on #11, 2026-10-19, "in prod nothing goes to REAPER
//! on its own"; the review of PR #56): `ways_on`, `after_stop`,
//! `holds_cut_off`, `stops_in` and `cut_off_in`.

use super::tests::{NEW, PIN, PREV, installed, prod, rec};
use super::*;
use crate::bundle::Hil;

/// In prod a stopped entry names the ways on (the owner's decision on #11,
/// 2026-10-19: in prod nothing goes to REAPER on its own): a live entry on
/// the pin it runs, a green maintenance build that becomes the pin
/// included, and the explicit event plan; before the cutover none (the
/// event plan, as before).
#[test]
fn in_prod_a_stopped_entry_names_the_ways_on() {
    let mut b = installed();
    assert_eq!(ways_on(&Lifecycle::Trial, |sha| b.get(sha)), None);
    let ways = |pin: &str| {
        format!(
            "in prod nothing goes to REAPER on its own: iemmode live --build {pin} tries again, \
             iemmode event starts REAPER (the PC stays prod)"
        )
    };
    let lc = Lifecycle::Prod(prod(Some(PREV), None));
    assert_eq!(ways_on(&lc, |sha| b.get(sha)), Some(ways(PIN)));
    let lc = Lifecycle::Prod(prod(None, Some(NEW)));
    assert_eq!(ways_on(&lc, |sha| b.get(sha)), Some(ways(NEW)));
    b.insert(NEW.into(), rec(NEW, "main", Hil::Red));
    assert_eq!(ways_on(&lc, |sha| b.get(sha)), Some(ways(PIN)));
}

/// What a prod alarm says after an entry stopped (the review of PR #56,
/// finding 6): after a stop that changed nothing in live or dev, that the PC
/// stays there as it was and asking again tries again, never a live entry
/// that would restart a healthy engine (nor a claim that anything plays);
/// after a dev entry whose engine was armed, that the PC is in dev with it;
/// after a live entry whose engine was armed, that the PC is in live with
/// it, which "ide event" keeps as it is, and the ways that start it again or
/// go to REAPER (asking "ide event" again would retry nothing); after one
/// that changed what runs, or from event, the ways on ([`ways_on`]). Before
/// the cutover none.
#[test]
fn a_stop_that_changed_nothing_says_the_pc_stays_as_it_was() {
    let b = installed();
    let lc = Lifecycle::Prod(prod(None, None));
    let ways = ways_on(&lc, |sha| b.get(sha));
    assert!(ways.is_some());
    for left in [Left::Changed, Left::Unchanged(Mode::Event)] {
        assert_eq!(after_stop(&lc, left, |sha| b.get(sha)), ways, "{left:?}");
    }
    let said = |what: &str| {
        Some(format!(
            "in prod nothing goes to REAPER on its own: {what} (asking for it again tries \
             again)"
        ))
    };
    for (left, what) in [
        (
            Left::Unchanged(Mode::Live),
            "no step done changed what runs, so the PC stays in live as it was",
        ),
        (
            Left::Unchanged(Mode::Dev),
            "no step done changed what runs, so the PC stays in dev as it was",
        ),
        (
            Left::Armed(Mode::Dev),
            "its engine was armed, so the PC is in dev with it",
        ),
    ] {
        assert_eq!(
            after_stop(&lc, left, |sha| b.get(sha)),
            said(what),
            "{left:?}"
        );
    }
    let armed_live = |pin: &str| {
        Some(format!(
            "in prod nothing goes to REAPER on its own: its engine was armed, so the PC is in \
             live with it (ide event keeps it as it is): iemmode live --build {pin} starts it \
             all again, iemmode event starts REAPER (the PC stays prod)"
        ))
    };
    assert_eq!(
        after_stop(&lc, Left::Armed(Mode::Live), |sha| b.get(sha)),
        armed_live(PIN)
    );
    // The pin a live entry runs: a maintenance build that becomes it.
    let maintenance = Lifecycle::Prod(prod(None, Some(NEW)));
    assert_eq!(
        after_stop(&maintenance, Left::Armed(Mode::Live), |sha| b.get(sha)),
        armed_live(NEW)
    );
    for left in [
        Left::Changed,
        Left::Unchanged(Mode::Event),
        Left::Unchanged(Mode::Dev),
        Left::Unchanged(Mode::Live),
        Left::Armed(Mode::Live),
    ] {
        assert_eq!(
            after_stop(&Lifecycle::Trial, left, |sha| b.get(sha)),
            None,
            "{left:?}"
        );
    }
}

/// A restarted guard (no reboot) holds a dev or live entry it finds left in
/// prod before the reboot rule could reset it to the event plan (the review
/// of PR #56, finding 5): never after a reboot (the boot's live entry),
/// never an event plan that was left (it resumes), never before the cutover
/// (it unwinds).
#[test]
fn a_restarted_guard_in_prod_holds_a_left_entry_before_any_reset() {
    let p = Lifecycle::Prod(prod(None, None));
    for to in [Mode::Dev, Mode::Live] {
        assert!(holds_cut_off(&p, false, Some(to)), "{to:?}");
        assert!(!holds_cut_off(&p, true, Some(to)), "{to:?}");
        assert!(!holds_cut_off(&Lifecycle::Trial, false, Some(to)), "{to:?}");
    }
    assert!(!holds_cut_off(&p, false, Some(Mode::Event)));
    assert!(!holds_cut_off(&p, false, None));
}

/// Where a prod dev or live entry that stops ends (the review of PR #56,
/// finding 1, and of its fix): with the engine it armed, in its target
/// (the engine plays; the crash watch keeps it); in event once it stopped
/// or started what plays the band or serves its phones (the app, REAPER,
/// the old engine, server or tray stopped, its own engine started held) or
/// its failed step was such a stop (the request went out), where nothing
/// respawns; else in the mode it began from. The HIL job, the runner, the
/// tuning, the shadow import, the data and the preference change nothing
/// that plays.
#[test]
fn a_stopped_entry_ends_where_what_runs_says() {
    let quiet: [&[Step]; 4] = [
        &[],
        &[Step::Precheck],
        &[Step::Precheck, Step::JobsCancel, Step::RunnerStop],
        &[
            Step::Precheck,
            Step::TuningEnter,
            Step::Shadow,
            Step::PrefCheck,
        ],
    ];
    let changed: [&[Step]; 5] = [
        &[Step::Precheck, Step::AppStop],
        &[Step::Precheck, Step::ReaperSaveQuit],
        &[Step::Precheck, Step::EngineStop],
        &[Step::Precheck, Step::ServerStop, Step::TrayStop],
        &[Step::Precheck, Step::PrefCheck, Step::EngineStart],
    ];
    let armed: [&[Step]; 2] = [
        &[Step::Precheck, Step::EngineStart, Step::EngineArm],
        &[
            Step::Precheck,
            Step::EngineStop,
            Step::EngineStart,
            Step::EngineArm,
            Step::ServerStart,
            Step::TrayStart,
        ],
    ];
    let stops = [
        Step::AppStop,
        Step::ReaperSaveQuit,
        Step::EngineStop,
        Step::ServerStop,
        Step::TrayStop,
    ];
    let reads = [
        Step::Precheck,
        Step::RunnerStop,
        Step::TuningEnter,
        Step::PrefCheck,
        Step::EngineStart,
        Step::EngineArm,
    ];
    for from in [Mode::Event, Mode::Dev, Mode::Live] {
        for to in [Mode::Dev, Mode::Live] {
            let at = |done: &[Step]| format!("{from:?} → {to:?} {done:?}");
            for done in quiet {
                for failed in reads {
                    let left = stops_in(from, to, done, failed);
                    assert_eq!(left, Left::Unchanged(from), "{} {failed:?}", at(done));
                    assert_eq!(left.mode(), from, "{}", at(done));
                }
                for failed in stops {
                    let left = stops_in(from, to, done, failed);
                    assert_eq!(left, Left::Changed, "{} {failed:?}", at(done));
                }
            }
            for done in changed {
                let left = stops_in(from, to, done, Step::Precheck);
                assert_eq!(left, Left::Changed, "{}", at(done));
                assert_eq!(left.mode(), Mode::Event, "{}", at(done));
            }
            for done in armed {
                for failed in [Step::ServerStart, Step::IdentityCheck, Step::RunnerStart] {
                    let left = stops_in(from, to, done, failed);
                    assert_eq!(left, Left::Armed(to), "{} {failed:?}", at(done));
                    assert_eq!(left.mode(), to, "{}", at(done));
                }
            }
            // Cut off by a guard restart: the step in flight is unknown, so
            // only the precheck, the first, is sure to have changed nothing.
            assert_eq!(cut_off_in(from, to, &[]), Left::Unchanged(from));
            for done in [&[Step::Precheck][..], changed[2]] {
                assert_eq!(cut_off_in(from, to, done), Left::Changed, "{}", at(done));
            }
            assert_eq!(cut_off_in(from, to, armed[0]), Left::Armed(to));
        }
    }
}
