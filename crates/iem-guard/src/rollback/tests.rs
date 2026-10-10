//! The rollback's rules (S8 design note §3.3): the refusal and the record,
//! the export's and the kept original's names, the moves of the project
//! files from every state, the stops, REAPER's verdict, what `iemmode
//! event` means in each lifecycle, and the texts.

use std::cell::Cell;

use super::*;
use crate::lifecycle::Prod;

const PIN: &str = "0123456789abcdef0123456789abcdef01234567";
const SINCE: u64 = 1_790_000_000;
const AT: u64 = 1_790_100_000;
const PROJECT: &str = r"C:\Band\project.rpp";

fn prod() -> Lifecycle {
    Lifecycle::Prod(Prod {
        since: SINCE,
        pin: PIN.into(),
        previous: None,
        maintenance: None,
    })
}

fn run() -> Run {
    Run {
        pin: PIN.into(),
        since: Some(SINCE),
        at: AT,
        done: Vec::new(),
        exported: false,
        on_export: false,
    }
}

#[test]
fn a_rollback_begins_from_prod_or_continues_its_record() {
    assert_eq!(begin(&prod(), None, AT), Ok(run()));
    let err = begin(&Lifecycle::Trial, None, AT).unwrap_err();
    assert!(err.starts_with("there is no cutover to roll back"), "{err}");
    // A record in progress is continued as it is, whatever the time.
    let mut going = run();
    going.done = vec![RollStep::Stop, RollStep::Export];
    going.exported = true;
    assert_eq!(
        begin(&Lifecycle::RollingBack, Some(&going), AT + 9),
        Ok(going.clone())
    );
    assert_eq!(begin(&prod(), Some(&going), AT + 9), Ok(going));
    // Rolling back without a record: again, without the cutover's export.
    let lost = begin(&Lifecycle::RollingBack, None, AT).unwrap();
    assert_eq!(
        (lost.pin.as_str(), lost.since, lost.at),
        ("unknown", None, AT)
    );
    assert!(lost.done.is_empty() && !lost.exported && !lost.on_export);
}

#[test]
fn a_continued_rollback_stops_and_brings_reaper_again() {
    let mut r = run();
    r.done = STEPS.to_vec();
    r.exported = true;
    r.on_export = true;
    let next = resumed(&r);
    assert_eq!(
        next.done,
        [
            RollStep::Export,
            RollStep::Project,
            RollStep::Autostarts,
            RollStep::GuardLogon,
            RollStep::PinChanges
        ]
    );
    assert!(next.exported && next.on_export);
}

#[test]
fn the_export_and_the_kept_original_are_new_names_beside_the_project() {
    assert_eq!(
        export_path(PROJECT, AT),
        format!(r"C:\Band\project.rollback-{AT}.rpp")
    );
    assert_eq!(
        kept_path(PROJECT, AT),
        format!(r"C:\Band\project.before-rollback-{AT}.rpp")
    );
    assert_eq!(
        export_path("/band/live.set.rpp", 7),
        "/band/live.set.rollback-7.rpp"
    );
    assert_eq!(export_path("project", 7), "project.rollback-7");
    assert_eq!(export_path(r"C:\Band\.rpp", 7), r"C:\Band\.rpp.rollback-7");
    assert_eq!(
        kept_path(r"C:\Band.d\project", 7),
        r"C:\Band.d\project.before-rollback-7"
    );
    assert_ne!(export_path(PROJECT, AT), PROJECT);
    assert_ne!(kept_path(PROJECT, AT), export_path(PROJECT, AT));
}

fn files(project: bool, export: bool, kept: bool) -> Files {
    Files {
        project,
        export,
        kept,
    }
}

/// Every state of the three files against both wants: the moves, then
/// that each move's target is absent at its turn and that the moves settle
/// the files as wanted.
#[test]
fn the_moves_settle_every_state_a_swap_can_leave() {
    use Move::*;
    let table: [(Want, Files, Option<Vec<Move>>); 16] = [
        (
            Want::Export,
            files(true, true, false),
            Some(vec![ProjectToKept, ExportToProject]),
        ),
        (
            Want::Export,
            files(false, true, true),
            Some(vec![ExportToProject]),
        ),
        (Want::Export, files(true, false, true), Some(vec![])),
        (Want::Export, files(true, false, false), None),
        (Want::Export, files(true, true, true), None),
        (Want::Export, files(false, false, true), None),
        (Want::Export, files(false, true, false), None),
        (Want::Export, files(false, false, false), None),
        (
            Want::Original,
            files(true, false, true),
            Some(vec![ProjectToExport, KeptToProject]),
        ),
        (
            Want::Original,
            files(false, true, true),
            Some(vec![KeptToProject]),
        ),
        (
            Want::Original,
            files(false, false, true),
            Some(vec![KeptToProject]),
        ),
        (Want::Original, files(true, true, false), Some(vec![])),
        (Want::Original, files(true, false, false), Some(vec![])),
        (Want::Original, files(true, true, true), None),
        (Want::Original, files(false, true, false), None),
        (Want::Original, files(false, false, false), None),
    ];
    for (want, start, expect) in table {
        let got = moves(want, start);
        match expect {
            None => {
                let err = got.unwrap_err();
                assert!(
                    err.contains("cannot be settled"),
                    "{want:?} {start:?}: {err}"
                );
            }
            Some(plan) => {
                assert_eq!(got.as_ref(), Ok(&plan), "{want:?} {start:?}");
                let mut f = start;
                for m in plan {
                    let target = match m {
                        ProjectToKept => f.kept,
                        ExportToProject | KeptToProject => f.project,
                        ProjectToExport => f.export,
                    };
                    assert!(!target, "{m:?} would overwrite, from {start:?}");
                    f = m.apply(f);
                }
                let want_placed = match want {
                    Want::Export => Placed::Export,
                    Want::Original => Placed::Original,
                };
                assert_eq!(placed(want, f), Some(want_placed), "{want:?} {start:?}");
                assert_eq!(
                    moves(want, f),
                    Ok(Vec::new()),
                    "settled twice: {want:?} {start:?}"
                );
            }
        }
    }
}

#[test]
fn placed_names_only_a_settled_project() {
    assert_eq!(
        placed(Want::Export, files(true, false, true)),
        Some(Placed::Export)
    );
    assert_eq!(
        placed(Want::Original, files(true, true, false)),
        Some(Placed::Original)
    );
    assert_eq!(
        placed(Want::Original, files(true, false, false)),
        Some(Placed::Original)
    );
    assert_eq!(placed(Want::Export, files(true, true, false)), None);
    assert_eq!(placed(Want::Original, files(true, false, true)), None);
    assert_eq!(placed(Want::Export, files(false, true, true)), None);
    assert_eq!(placed(Want::Original, files(false, true, true)), None);
    assert_eq!(placed(Want::Export, files(true, true, true)), None);
}

#[test]
fn a_move_names_its_paths() {
    let (p, e, k) = ("p", "e", "k");
    assert_eq!(Move::ProjectToKept.paths(p, e, k), (p, k));
    assert_eq!(Move::ExportToProject.paths(p, e, k), (e, p));
    assert_eq!(Move::ProjectToExport.paths(p, e, k), (p, e));
    assert_eq!(Move::KeptToProject.paths(p, e, k), (k, p));
    let all = files(true, true, true);
    assert_eq!(Move::ProjectToKept.apply(all), files(false, true, true));
    assert_eq!(
        Move::ExportToProject.apply(files(false, true, false)),
        files(true, false, false)
    );
    assert_eq!(
        Move::ProjectToExport.apply(files(true, false, true)),
        files(false, true, true)
    );
    assert_eq!(
        Move::KeptToProject.apply(files(false, true, true)),
        files(true, true, false)
    );
}

/// The stops are the event plan's, up to its turn to REAPER, for every
/// combination of facts; with iemmixer up, all four.
#[test]
fn the_stops_are_the_event_plans_up_to_its_turn_to_reaper() {
    for bits in 0..(1 << plan::FACT_BITS) {
        let f = Facts::from_bits(bits);
        let event = plan::plan(Mode::Event, &f);
        let s = stops(&f);
        assert_eq!(event.get(..s.len()), Some(s.as_slice()), "{f:?}");
        assert_eq!(event.get(s.len()), Some(&Step::TuningExit), "{f:?}");
    }
    let up = Facts {
        engine: true,
        server: true,
        tray: true,
        runner: true,
        ..Facts::default()
    };
    assert_eq!(
        stops(&up),
        [
            Step::JobsCancel,
            Step::RunnerStop,
            Step::EngineStop,
            Step::ServerStop,
            Step::TrayStop
        ]
    );
    assert!(stops(&Facts::default()).is_empty());
}

#[test]
fn reaper_runs_only_with_the_card_and_a_handover_that_passed() {
    let on = Facts {
        reaper: true,
        reaper_holds_module: true,
        ..Facts::default()
    };
    assert!(reaper_runs(&on, &[]));
    let app = ["AppHandover failed: the app does not answer".to_owned()];
    assert!(reaper_runs(&on, &app));
    let handover = ["ReaperHandover failed: tracks 3, want 40".to_owned()];
    assert!(!reaper_runs(&on, &handover));
    let no_card = Facts {
        reaper_holds_module: false,
        ..on
    };
    assert!(!reaper_runs(&no_card, &[]));
    let none = Facts {
        reaper: false,
        ..on
    };
    assert!(!reaper_runs(&none, &[]));
}

/// The button and "ide event" in every lifecycle and mode; the engine's
/// health is read only in prod live on "ide event".
#[test]
fn ide_event_never_rolls_back_and_the_button_does_in_prod() {
    let rb = Lifecycle::RollingBack;
    for mode in [Mode::Event, Mode::Dev, Mode::Live] {
        for signal in [false, true] {
            let read = Cell::new(0);
            let healthy = || {
                read.set(read.get() + 1);
                true
            };
            assert_eq!(
                on_event(&Lifecycle::Trial, mode, signal, healthy),
                OnEvent::Plan
            );
            assert_eq!(
                on_event(&rb, mode, signal, || panic!("read")),
                OnEvent::Rollback
            );
            assert_eq!(read.get(), 0);
            assert_eq!(
                on_event(&prod(), mode, false, || panic!("read")),
                OnEvent::Rollback
            );
        }
    }
    let read = Cell::new(0);
    let count = |answer: bool| {
        read.set(read.get() + 1);
        answer
    };
    assert_eq!(
        on_event(&prod(), Mode::Dev, true, || count(true)),
        OnEvent::Live
    );
    assert_eq!(
        on_event(&prod(), Mode::Event, true, || count(true)),
        OnEvent::Plan
    );
    assert_eq!(read.get(), 0);
    assert_eq!(
        on_event(&prod(), Mode::Live, true, || count(true)),
        OnEvent::Stay
    );
    assert_eq!(
        on_event(&prod(), Mode::Live, true, || count(false)),
        OnEvent::Plan
    );
    assert_eq!(read.get(), 2);
}

#[test]
fn the_record_shows_in_the_status_and_refuses_an_activation() {
    assert_eq!(status(None), None);
    assert_eq!(activation_refusal(None), None);
    let mut r = run();
    r.done = vec![RollStep::Stop];
    let s = status(Some(&r)).unwrap();
    assert!(
        s.starts_with(&format!(
            "rollback since {AT} from the pin {PIN}: [Stop] done"
        )),
        "{s}"
    );
    let a = activation_refusal(Some(&r)).unwrap();
    assert!(
        a.starts_with(&format!(
            "the rollback from the pin {PIN} is not finished ([Stop] done)"
        )),
        "{a}"
    );
}

#[test]
fn the_dry_run_names_every_step_and_the_files() {
    let text = plan_text(&run(), Some(PROJECT));
    for want in [
        format!("rollback from the pin {PIN}: RollingBack saved; Stop"),
        format!("Export (the band's data to {})", export_path(PROJECT, AT)),
        format!(
            "Autostarts (back from {})",
            crate::cutover::export_name(SINCE)
        ),
        "GuardLogon (off once the autostarts are back); PinChanges (false); then trial and event"
            .to_owned(),
    ] {
        assert!(text.contains(&want), "{want} not in {text}");
    }
    let lost = Run {
        since: None,
        ..run()
    };
    let text = plan_text(&lost, None);
    assert!(text.contains("<project>.rollback-<at>"), "{text}");
    assert!(
        text.contains("Autostarts (unknown (the record of prod was lost): not restored)"),
        "{text}"
    );
}

#[test]
fn the_replies_say_where_reaper_runs_and_what_is_left() {
    let mut r = run();
    r.on_export = true;
    let notes = ["a note".to_owned()];
    assert_eq!(
        ended(&r, &notes),
        format!(
            "rollback done: trial, event; {ON_EXPORT}; the original project is kept as \
             before-rollback-{AT}; a note"
        )
    );
    r.on_export = false;
    assert_eq!(
        ended(&r, &[]),
        format!("rollback done: trial, event; {ON_ORIGINAL}")
    );
    let tail = "; the PC stays rolling back: a guard restart or iemmode rollback continues it";
    assert_eq!(
        unfinished(Some("EngineStop: no"), &[], &notes),
        format!("rollback stopped: EngineStop: no; a note{tail}")
    );
    let left = ["Autostarts: x".to_owned(), "GuardLogon: y".to_owned()];
    assert_eq!(
        unfinished(None, &left, &[]),
        format!("rollback not finished: Autostarts: x; GuardLogon: y{tail}")
    );
}

/// The record in the state file: saved and read back; a step a newer guard
/// has is left out (it runs again); an older record without the flags.
#[test]
fn the_record_reads_back_and_reads_a_newer_guards_step_as_not_done() {
    let mut r = run();
    r.done = vec![RollStep::Stop, RollStep::Export];
    r.exported = true;
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(json["done"], serde_json::json!(["stop", "export"]));
    assert_eq!(serde_json::from_value::<Run>(json).unwrap(), r);
    let newer = serde_json::json!({
        "pin": PIN, "since": SINCE, "at": AT, "done": ["stop", "shadow_check", "project"]
    });
    let back: Run = serde_json::from_value(newer).unwrap();
    assert_eq!(back.done, [RollStep::Stop, RollStep::Project]);
    assert!(!back.exported && !back.on_export);
}

/// The pipe queues the button as a rollback in prod and while rolling back,
/// never before the cutover.
#[test]
fn the_button_rolls_back_in_prod_and_while_rolling_back() {
    assert!(!button_rolls_back(&Lifecycle::Trial));
    assert!(button_rolls_back(&prod()));
    assert!(button_rolls_back(&Lifecycle::RollingBack));
}
