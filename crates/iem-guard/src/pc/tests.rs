//! The PC's portable decisions: step errors, names, the process list, the
//! facts of a plan, the module's holders, adoption and the job's note; the
//! fixtures the other test parts share.

use std::time::Duration;

use super::fake::FakePc;
use super::*;

fn images() -> Images {
    Images {
        reaper: "reaper.exe".into(),
        app: "app.exe".into(),
        engine: "iem-engine.exe".into(),
        server: "iem-server.exe".into(),
        tray: "iem-tray.exe".into(),
        runner: "Runner.Listener.exe".into(),
    }
}

/// No holder of the driver module.
pub(super) const NO_HOLDER: &[(u32, String)] = &[];

fn list() -> Vec<(u32, String)> {
    vec![
        (4, "System".into()),
        (11, "REAPER.EXE".into()),
        (12, "app.exe".into()),
        (13, "iem-engine.exe".into()),
        (14, "iem-server.exe".into()),
        (15, "iem-tray.exe".into()),
        (16, "runner.listener.exe".into()),
        (17, "app.exe".into()),
        (18, "notepad.exe".into()),
    ]
}

#[test]
fn step_errors_read_as_sentences_and_come_from_a_preemption() {
    assert_eq!(StepError::from(Preempted), StepError::Preempted);
    assert_eq!(StepError::Preempted.to_string(), "pre-empted by event");
    assert_eq!(
        StepError::failed("no DriverReleased within 10 s").to_string(),
        "no DriverReleased within 10 s"
    );
    assert_eq!(
        StepError::failed(String::from("x")),
        StepError::Failed("x".into())
    );
    let c = Cancel::default();
    c.preempt();
    let r: R<()> = c.sleep(Duration::from_secs(1)).map_err(StepError::from);
    assert_eq!(r, Err(StepError::Preempted));
}

#[test]
fn audiences_and_kids_have_their_names() {
    assert_eq!(Audience::Alarm.arg(), "alarm");
    let ids: Vec<&str> = Kid::ALL.iter().map(|k| k.id()).collect();
    assert_eq!(ids, ["engine", "server", "tray", "runner"]);
}

#[test]
fn children_are_found_by_kid() {
    let child = |pid: u32| Child {
        pid,
        start_time: u64::from(pid) * 10,
        image: format!("C:\\IEM\\{pid}.exe"),
    };
    let kids = Children {
        engine: Some(child(1)),
        server: Some(child(2)),
        tray: Some(child(3)),
        runner: Some(child(4)),
    };
    let pids: Vec<u32> = Kid::ALL
        .iter()
        .map(|k| kids.of(*k).map_or(0, |c| c.pid))
        .collect();
    assert_eq!(pids, [1, 2, 3, 4]);
    assert_eq!(Children::default().of(Kid::Tray), None);
}

#[test]
fn the_process_list_is_read_by_image_without_case() {
    let p = Procs::from_list(&list(), &images());
    assert_eq!(p.reaper, [11]);
    assert_eq!(p.app, [12, 17]);
    assert_eq!(p.engine, [13]);
    assert_eq!(p.server, [14]);
    assert_eq!(p.tray, [15]);
    assert_eq!(p.runner, [16]);
    assert!(p.exited.is_empty());
    assert_eq!(p.of(Kid::Engine), [13]);
    assert_eq!(p.of(Kid::Server), [14]);
    assert_eq!(p.of(Kid::Tray), [15]);
    assert_eq!(p.of(Kid::Runner), [16]);
    assert_eq!(Procs::from_list(&[], &images()), Procs::default());
}

#[test]
fn the_band_is_up_with_reaper_or_the_app() {
    let with = |reaper: Vec<u32>, app: Vec<u32>| Procs {
        reaper,
        app,
        ..Procs::default()
    };
    assert!(with(vec![1], vec![]).band_up());
    assert!(with(vec![], vec![2]).band_up());
    assert!(with(vec![1], vec![2]).band_up());
    assert!(!with(vec![], vec![]).band_up());
    let engine_only = Procs {
        engine: vec![3],
        ..Procs::default()
    };
    assert!(!engine_only.band_up());
}

fn band() -> Procs {
    Procs {
        reaper: vec![11],
        app: vec![12],
        engine: vec![13],
        server: vec![14],
        tray: vec![15],
        runner: vec![16],
        exited: Vec::new(),
    }
}

#[test]
fn facts_name_what_runs() {
    let f = facts_from(&band(), Some(NO_HOLDER), Some((None, None)));
    assert!(f.reaper && f.app && f.engine && f.server && f.tray && f.runner);
    let nothing = facts_from(&Procs::default(), Some(NO_HOLDER), Some((None, None)));
    assert_eq!(nothing, Facts::default());
    for (p, want) in [
        (
            Procs {
                server: vec![1],
                ..Procs::default()
            },
            Facts {
                server: true,
                ..Facts::default()
            },
        ),
        (
            Procs {
                tray: vec![1],
                ..Procs::default()
            },
            Facts {
                tray: true,
                ..Facts::default()
            },
        ),
        (
            Procs {
                runner: vec![1],
                ..Procs::default()
            },
            Facts {
                runner: true,
                ..Facts::default()
            },
        ),
        (
            Procs {
                engine: vec![1],
                ..Procs::default()
            },
            Facts {
                engine: true,
                ..Facts::default()
            },
        ),
    ] {
        assert_eq!(facts_from(&p, Some(NO_HOLDER), Some((None, None))), want);
    }
}

#[test]
fn module_holders_split_into_reaper_and_foreign() {
    let holders = |pids: &[u32]| -> Vec<(u32, String)> {
        pids.iter().map(|p| (*p, format!("{p}.exe"))).collect()
    };
    let facts = |pids: &[u32]| {
        let h = holders(pids);
        facts_from(&band(), Some(h.as_slice()), Some((None, None)))
    };
    let f = facts(&[11]);
    assert!(f.reaper_holds_module && !f.other_module_holder);
    // Our engine is neither REAPER nor foreign.
    let f = facts(&[13]);
    assert!(!f.reaper_holds_module && !f.other_module_holder);
    let f = facts(&[99]);
    assert!(!f.reaper_holds_module && f.other_module_holder);
    let f = facts(&[11, 99]);
    assert!(f.reaper_holds_module && f.other_module_holder);
    let f = facts(&[]);
    assert!(!f.reaper_holds_module && !f.other_module_holder);
}

#[test]
fn unreadable_holders_never_restart_a_running_reaper() {
    let f = facts_from(&band(), None, Some((None, None)));
    assert!(f.reaper_holds_module && !f.other_module_holder);
    let without = facts_from(&Procs::default(), None, Some((None, None)));
    assert!(!without.reaper_holds_module && !without.other_module_holder);
}

#[test]
fn the_app_serves_when_it_owns_both_ports() {
    let app = |pids: Vec<u32>| Procs {
        app: pids,
        ..Procs::default()
    };
    let serves = |p: &Procs, ports: Option<Ports>| facts_from(p, Some(NO_HOLDER), ports).app_serves;
    assert!(serves(&app(vec![12]), Some((Some(12), Some(12)))));
    assert!(!serves(&app(vec![12]), Some((Some(12), None))));
    assert!(!serves(&app(vec![12]), Some((None, Some(12)))));
    assert!(!serves(&app(vec![12]), Some((Some(14), Some(12)))));
    assert!(!serves(&app(vec![12]), Some((Some(12), Some(14)))));
    assert!(!serves(&app(vec![12]), Some((None, None))));
    // Two instances: neither is known to serve.
    assert!(!serves(&app(vec![12, 17]), Some((Some(12), Some(12)))));
    assert!(!serves(&app(vec![]), Some((Some(12), Some(12)))));
    // Unreadable ports: a running app is assumed to serve.
    assert!(serves(&app(vec![12]), None));
    assert!(!serves(&app(vec![]), None));
}

/// The app handover's own check (#10): the app's one process owns
/// both ports. An iem-server that did not stop keeps them and answers
/// `/api/version` and `/api/members` itself, which the HTTP checks
/// alone took for the app.
#[test]
fn the_app_serves_only_while_its_one_process_owns_both_ports() {
    assert!(app_serves(&[12], (Some(12), Some(12))));
    assert!(!app_serves(&[12], (Some(14), Some(14))));
    assert!(!app_serves(&[12], (Some(14), Some(12))));
    assert!(!app_serves(&[12], (Some(12), Some(14))));
    assert!(!app_serves(&[12], (None, Some(12))));
    assert!(!app_serves(&[12], (Some(12), None)));
    assert!(!app_serves(&[12], (None, None)));
    assert!(!app_serves(&[], (Some(12), Some(12))));
    assert!(!app_serves(&[12, 17], (Some(12), Some(12))));
}

#[test]
fn foreign_holders_are_all_but_reaper() {
    let h = vec![
        (11, "reaper.exe".to_owned()),
        (99, "spike.exe".to_owned()),
        (13, "iem-engine.exe".to_owned()),
    ];
    assert_eq!(
        foreign_holders(&h, &[11]),
        [
            (99, "spike.exe".to_owned()),
            (13, "iem-engine.exe".to_owned())
        ]
    );
    assert_eq!(
        foreign_holders(&h, &[11, 99, 13]),
        Vec::<(u32, String)>::new()
    );
    assert_eq!(foreign_holders(&h, &[]), h);
}

#[test]
fn an_engine_is_foreign_unless_it_is_our_child() {
    assert!(!foreign_engine(&[], None));
    assert!(!foreign_engine(&[], Some(13)));
    assert!(!foreign_engine(&[13], Some(13)));
    assert!(foreign_engine(&[13], None));
    assert!(foreign_engine(&[14], Some(13)));
    assert!(foreign_engine(&[13, 14], Some(13)));
}

#[test]
fn adoption_needs_the_same_image_and_start_time() {
    let saved = Child {
        pid: 13,
        start_time: 133_000_000_000,
        image: "C:\\IEM\\bundles\\a\\iem-engine.exe".into(),
    };
    assert!(adoptable(
        &saved,
        "C:\\IEM\\bundles\\a\\iem-engine.exe",
        133_000_000_000
    ));
    assert!(adoptable(
        &saved,
        "c:\\iem\\BUNDLES\\a\\IEM-ENGINE.EXE",
        133_000_000_000
    ));
    assert!(!adoptable(
        &saved,
        "C:\\IEM\\bundles\\a\\iem-engine.exe",
        133_000_000_001
    ));
    assert!(!adoptable(
        &saved,
        "C:\\IEM\\bundles\\a\\iem-engine.exe",
        132_999_999_999
    ));
    assert!(!adoptable(
        &saved,
        "C:\\Other\\iem-engine.exe",
        133_000_000_000
    ));
}

pub(super) fn up() -> Facts {
    Facts {
        reaper: true,
        app: true,
        reaper_holds_module: true,
        app_serves: true,
        ..Facts::default()
    }
}

/// The PC's task job allows no breakaway (#9 2026-09-28). Children that
/// stay in a job that does not end its processes when it closes are
/// named in `iemmode status`; every other reading names nothing (a
/// refusal is each start's step error, an unreadable job is logged).
#[test]
fn only_children_that_stay_in_the_guards_job_are_named() {
    assert_eq!(
        JOB_NOTE,
        "children stay in the guard task's job (no breakaway)"
    );
    assert_eq!(job_note(&Ok(Placement::InJob)), Some(JOB_NOTE));
    for other in [
        Ok(Placement::Breakaway),
        Ok(Placement::NoJob),
        Ok(Placement::Refuse("the job ends its processes")),
        Err("the job could not be read".to_owned()),
    ] {
        assert_eq!(job_note(&other), None, "{other:?}");
    }
    // The fake reads no job unless a test sets one, and never records
    // the read.
    let mut pc = FakePc::new(Facts::default());
    assert_eq!(pc.job(), Ok(Placement::NoJob));
    pc.job = Ok(Placement::InJob);
    assert_eq!(pc.job(), Ok(Placement::InJob));
    assert!(pc.calls().is_empty());
}
