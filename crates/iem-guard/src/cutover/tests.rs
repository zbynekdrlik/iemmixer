//! The cutover's pure rules (S8 design note §3.2): the refusals, the step
//! order and its undo, the cutover task's request and export names, the
//! server config's `pin_changes` edit and the engine check.

use super::*;
use crate::bundle::Hil;
use crate::lifecycle::Prod;
use crate::pc::Status;

const BUILD: &str = "0123456789abcdef0123456789abcdef01234567";
const OTHER: &str = "89abcdef0123456789abcdef0123456789abcdef";

fn rec(branch: &str, hil: Hil) -> Record {
    Record {
        sha: BUILD.to_owned(),
        branch: branch.to_owned(),
        run: 7,
        installed_at: 1,
        hil,
    }
}

/// The facts of a PC the cutover may run on: trial, dev on the build, a
/// green main build, no job.
fn facts<'a>(lc: &'a Lifecycle, r: &'a Record) -> Facts<'a> {
    Facts {
        lifecycle: lc,
        unwinding: None,
        mode: Mode::Dev,
        active: Some(BUILD),
        job: None,
        record: Some(r),
    }
}

#[test]
fn the_cutover_runs_from_dev_or_a_live_trial_on_the_active_green_main_build() {
    let (lc, r) = (Lifecycle::Trial, rec("main", Hil::Green));
    for mode in [Mode::Dev, Mode::Live] {
        let f = Facts {
            mode,
            ..facts(&lc, &r)
        };
        assert_eq!(refusal(BUILD, &f), None, "{mode:?}");
    }
}

#[test]
fn every_refusal_names_why() {
    let (trial, green) = (Lifecycle::Trial, rec("main", Hil::Green));
    let prod = Lifecycle::Prod(Prod {
        since: 5,
        pin: OTHER.into(),
        previous: None,
        maintenance: None,
    });
    let red = rec("main", Hil::Red);
    let dev = rec("dev", Hil::Green);
    let run = Run {
        build: OTHER.into(),
        since: 9,
        begun: vec![CutStep::GuardLogon],
    };
    let cases = [
        (
            Facts {
                lifecycle: &prod,
                ..facts(&trial, &green)
            },
            format!("the cutover is done: prod since 5 on the pin {OTHER}"),
        ),
        (
            Facts {
                lifecycle: &Lifecycle::RollingBack,
                ..facts(&trial, &green)
            },
            "a rollback to REAPER runs: no cutover until it ends".to_owned(),
        ),
        (
            Facts {
                unwinding: Some(&run),
                ..facts(&trial, &green)
            },
            format!(
                "the cutover of {OTHER} that was cut off is not fully unwound ([GuardLogon] \
                 left): a guard restart tries again"
            ),
        ),
        (
            Facts {
                record: None,
                ..facts(&trial, &green)
            },
            format!("bundle {BUILD} is not installed"),
        ),
        (
            facts(&trial, &red),
            format!("{BUILD}: HIL Red; live needs green"),
        ),
        (
            facts(&trial, &dev),
            format!("{BUILD} is from \"dev\"; live needs main"),
        ),
        (
            Facts {
                active: Some(OTHER),
                ..facts(&trial, &green)
            },
            format!(
                "{BUILD} is not the active bundle ({OTHER}): the cutover runs the build the PC runs"
            ),
        ),
        (
            Facts {
                active: None,
                ..facts(&trial, &green)
            },
            format!(
                "{BUILD} is not the active bundle (none): the cutover runs the build the PC runs"
            ),
        ),
        (
            Facts {
                mode: Mode::Event,
                ..facts(&trial, &green)
            },
            format!("the guard is in event: the cutover runs from dev or a live trial on {BUILD}"),
        ),
        (
            Facts {
                job: Some(42),
                ..facts(&trial, &green)
            },
            "HIL job 42 runs: the cutover's live entry would stop its runner".to_owned(),
        ),
    ];
    for (f, want) in cases {
        assert_eq!(refusal(BUILD, &f), Some(want));
    }
}

#[test]
fn the_steps_run_in_the_design_s_order_and_unwind_newest_first() {
    // The guard's logon trigger before the autostarts go: at every moment
    // something starts at the next boot (the review of lane 2).
    assert_eq!(
        STEPS,
        [
            CutStep::Import,
            CutStep::GuardLogon,
            CutStep::Autostarts,
            CutStep::PinChanges,
            CutStep::Lifecycle,
            CutStep::Checks,
        ]
    );
    assert_eq!(
        undo(&STEPS),
        [
            CutStep::Lifecycle,
            CutStep::PinChanges,
            CutStep::Autostarts,
            CutStep::GuardLogon,
        ]
    );
    // A step that failed half-way is undone too; the import and the checks
    // change nothing of their own (the event plan ends the live entry).
    assert_eq!(
        undo(&[CutStep::Import, CutStep::GuardLogon]),
        [CutStep::GuardLogon]
    );
    assert_eq!(undo(&[CutStep::Import]), Vec::<CutStep>::new());
    assert_eq!(undo(&[]), Vec::<CutStep>::new());
    let changes: Vec<bool> = STEPS.iter().map(|s| s.changes()).collect();
    assert_eq!(changes, [false, true, true, true, true, false]);
}

/// The guard's logon trigger stays while the autostarts are not back; every
/// other undo goes ahead whatever is kept.
#[test]
fn the_logon_trigger_is_undone_only_once_the_autostarts_are_back() {
    for step in STEPS {
        assert!(may_undo(step, &[]), "{step:?}");
        assert!(
            may_undo(step, &[CutStep::Lifecycle, CutStep::PinChanges]),
            "{step:?}"
        );
        let blocked = step == CutStep::GuardLogon;
        assert_eq!(may_undo(step, &[CutStep::Autostarts]), !blocked, "{step:?}");
    }
}

#[test]
fn the_record_round_trips_in_snake_case() {
    let run = Run {
        build: BUILD.into(),
        since: 1_790_000_000,
        begun: vec![CutStep::Import, CutStep::GuardLogon, CutStep::PinChanges],
    };
    let v = serde_json::to_value(&run).unwrap();
    assert_eq!(
        v,
        serde_json::json!({"build": BUILD, "since": 1_790_000_000u64,
                           "begun": ["import", "guard_logon", "pin_changes"]})
    );
    assert_eq!(serde_json::from_value::<Run>(v).unwrap(), run);
}

/// A record a newer guard saved with a step this guard does not know reads
/// as every step that changes something: the undo leaves nothing behind.
#[test]
fn a_record_with_a_step_this_guard_does_not_know_undoes_everything() {
    let v = serde_json::json!({"build": BUILD, "since": 5,
                               "begun": ["import", "later_step"]});
    let run: Run = serde_json::from_value(v).unwrap();
    assert_eq!(
        run.begun,
        [
            CutStep::GuardLogon,
            CutStep::Autostarts,
            CutStep::PinChanges,
            CutStep::Lifecycle,
        ]
    );
    let none: Run =
        serde_json::from_value(serde_json::json!({"build": BUILD, "since": 5, "begun": []}))
            .unwrap();
    assert_eq!(none.begun, Vec::<CutStep>::new());
}

#[test]
fn the_record_is_named_in_the_status_and_refuses_an_activation() {
    let run = Run {
        build: BUILD.into(),
        since: 5,
        begun: vec![CutStep::GuardLogon],
    };
    assert_eq!(status(None), None);
    assert_eq!(
        status(Some(&run)),
        Some(format!(
            "cutover of {BUILD} since 5: [GuardLogon] begun (in progress, or not fully unwound: \
             a guard restart tries again)"
        ))
    );
    assert_eq!(activation_refusal(None), None);
    assert_eq!(
        activation_refusal(Some(&run)),
        Some(format!(
            "the cutover of {BUILD} is not fully unwound ([GuardLogon] left): no activation until \
             a guard restart has unwound it"
        ))
    );
}

/// The reply and the alarm of a failed cutover, and whether the alarm is
/// the owner's question: anything left, or an event plan that did not end
/// done.
#[test]
fn an_unwind_says_what_is_left_and_asks_the_owner_only_then() {
    let left = vec!["GuardLogon: no answer".to_owned()];
    assert_eq!(
        unwound("h", true, &[]),
        ("h; unwound to trial and event".to_owned(), false)
    );
    assert_eq!(
        unwound("h", false, &[]),
        (
            "h; unwound to trial; the event plan did not end done".to_owned(),
            true
        )
    );
    assert_eq!(
        unwound("h", true, &left),
        (
            "h; unwound to trial and event; not undone: GuardLogon: no answer; a guard restart \
             tries again"
                .to_owned(),
            true
        )
    );
    let run = Run {
        build: BUILD.into(),
        since: 5,
        begun: vec![CutStep::Import, CutStep::GuardLogon],
    };
    let head = format!(
        "the cutover of {BUILD} was cut off (begun: [Import, GuardLogon]): unwound to trial; \
         the PC goes to event"
    );
    assert_eq!(recovered(&run, &[]), (head.clone(), false));
    assert_eq!(
        recovered(&run, &left),
        (
            format!("{head}; not undone: GuardLogon: no answer; a guard restart tries again"),
            true
        )
    );
}

#[test]
fn the_export_is_named_by_the_cutover_s_start() {
    assert_eq!(export_name(1_790_000_000), "autostarts-1790000000");
    assert!(valid_export(&export_name(0)));
    assert!(valid_export(&export_name(u64::MAX)));
    for bad in [
        "autostarts-",
        "autostarts-12a",
        "autostarts--1",
        "autostarts-123456789012345678901",
        "autostart-1",
        "xautostarts-1",
        "",
        "..",
    ] {
        assert!(!valid_export(bad), "{bad:?}");
    }
}

#[test]
fn the_task_request_names_its_verb_and_export() {
    let parse = |s: String| serde_json::from_str::<serde_json::Value>(&s).unwrap();
    assert_eq!(
        parse(request("7-x", Verb::AutostartsOff, Some("autostarts-5"))),
        serde_json::json!({"id": "7-x", "verb": "autostarts-off", "export": "autostarts-5"})
    );
    assert_eq!(
        parse(request("8", Verb::AutostartsOn, Some("autostarts-5"))),
        serde_json::json!({"id": "8", "verb": "autostarts-on", "export": "autostarts-5"})
    );
    assert_eq!(
        parse(request("9", Verb::LogonOn, None)),
        serde_json::json!({"id": "9", "verb": "logon-on", "export": null})
    );
    assert_eq!(
        parse(request("10", Verb::LogonOff, None)),
        serde_json::json!({"id": "10", "verb": "logon-off", "export": null})
    );
    assert_eq!(TASK, "\\iemmixer\\iemmixer-cutover");
    assert_eq!(KIND, "cutover");
}

/// A config as the ops site writes it: the switch on its own line, tables
/// after it, a comment, CRLF in places.
const FROZEN: &str = "port = 80\r\n# PINs change in the predecessor until the cutover\r\n\
                      pin_changes = false\r\ntls = true\n\n[[members]]\nid = \"member1\"\n";

#[test]
fn opening_the_pins_changes_that_value_only_and_closing_gives_the_bytes_back() {
    let open = set_pins(FROZEN, true).unwrap().unwrap();
    assert_eq!(
        open,
        FROZEN.replace("pin_changes = false", "pin_changes = true")
    );
    assert_eq!(pins_open(&open), Ok(Some(true)));
    assert_eq!(set_pins(&open, false).unwrap().as_deref(), Some(FROZEN));
    // Already so: nothing to write.
    assert_eq!(set_pins(FROZEN, false), Ok(None));
    assert_eq!(set_pins(&open, true), Ok(None));
    // Spacing, a comment and a last line without its end are kept.
    for (before, after) in [
        ("pin_changes=false", "pin_changes=true"),
        (
            "pin_changes   =\tfalse # frozen\n",
            "pin_changes   =\ttrue # frozen\n",
        ),
        (
            "  pin_changes = false\n[x]\ny = 1\n",
            "  pin_changes = true\n[x]\ny = 1\n",
        ),
    ] {
        assert_eq!(set_pins(before, true).unwrap().as_deref(), Some(after));
        assert_eq!(set_pins(after, false).unwrap().as_deref(), Some(before));
    }
}

#[test]
fn only_the_top_level_switch_is_edited() {
    // A table's own key of that name stays as it is.
    let config = "pin_changes = false\n[x]\npin_changes = false\n";
    assert_eq!(
        set_pins(config, true).unwrap().as_deref(),
        Some("pin_changes = true\n[x]\npin_changes = false\n")
    );
    // A switch only inside a table is no top-level one.
    assert_eq!(
        set_pins("[x]\npin_changes = false\n", true),
        Err("the server config names no pin_changes".to_owned())
    );
    assert_eq!(
        set_pins("port = 80\n", true),
        Err("the server config names no pin_changes".to_owned())
    );
}

#[test]
fn an_edit_that_would_change_anything_else_is_refused() {
    // The switch written as a quoted key, and a string holding a line that
    // reads like it: the line edit would change the string, not the key.
    let config = "note = \"\"\"\npin_changes = false\n\"\"\"\n\"pin_changes\" = false\n";
    assert_eq!(pins_open(config), Ok(Some(false)));
    assert_eq!(
        set_pins(config, true),
        Err(
            "the edited server config reads back with pin_changes Some(Boolean(false)), not true"
                .to_owned()
        )
    );
    // The quoted key alone: no line of the bare form.
    assert_eq!(
        set_pins("\"pin_changes\" = false\n", true),
        Err(
            "the server config's pin_changes is not one `pin_changes = false` line before its \
             first table"
                .to_owned()
        )
    );
}

#[test]
fn the_switch_must_be_a_boolean_in_a_config_that_parses() {
    assert_eq!(pins_open("pin_changes = true\n"), Ok(Some(true)));
    assert_eq!(pins_open("pin_changes = false\n"), Ok(Some(false)));
    assert_eq!(pins_open("port = 80\n"), Ok(None));
    assert_eq!(
        pins_open("pin_changes = \"true\"\n"),
        Err("server config: pin_changes is not true or false".to_owned())
    );
    assert!(pins_open("pin_changes = ").is_err_and(|e| e.starts_with("server config: ")));
    assert!(set_pins("pin_changes = ", true).is_err());
}

#[test]
fn the_engine_must_be_healthy_at_32() {
    let seen = |frames| EngineSeen {
        status: Status {
            frames,
            ..Status::default()
        },
        ..EngineSeen::default()
    };
    assert_eq!(engine_check(Health::Healthy, Some(&seen(32))), Ok(()));
    assert_eq!(
        engine_check(Health::Healthy, Some(&seen(64))),
        Err("the engine runs at 64 frames per period, not 32".to_owned())
    );
    assert_eq!(
        engine_check(Health::Healthy, None),
        Err("the engine has no status".to_owned())
    );
    assert_eq!(
        engine_check(Health::Parked, Some(&seen(32))),
        Err("the engine is Parked".to_owned())
    );
    assert_eq!(
        engine_check(Health::Dead, Some(&seen(32))),
        Err("the engine is Dead".to_owned())
    );
}

#[test]
fn the_dry_run_names_every_step() {
    assert_eq!(
        plan_text(BUILD, 5),
        format!(
            "Import (a live trial entry on {BUILD}: the final import), GuardLogon (the guard task \
             at logon), Autostarts (exported to autostarts-5 and disabled), PinChanges \
             (pin_changes = true, the server started again), Lifecycle (prod since 5, pin \
             {BUILD}), Checks (the band's address, a member page, the engine at 32)"
        )
    );
}
