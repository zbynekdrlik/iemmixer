//! Handover verdicts (design §5.2, §5.3; the 2026-09-27 lesson on #9).
//!
//! The effects read the facts; these functions decide whether REAPER and the
//! predecessor app took the band over, and whether the app left cleanly.

/// What to do with REAPER's meter bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bridge {
    Running,
    TriggerOnce,
    Refuse(String),
}

/// The meter bridge may be triggered only while its state is empty: a second
/// trigger opens a blocking dialog in REAPER.
pub fn bridge(state: &str) -> Bridge {
    match state {
        "1" => Bridge::Running,
        "" => Bridge::TriggerOnce,
        other => Bridge::Refuse(format!("meter bridge state {other:?}: not triggered")),
    }
}

/// The start of the title of REAPER's evaluation-license notice ("About
/// REAPER v7.65/win64 rev …"), REAPER's own UI text. An unlicensed REAPER
/// shows it at every start and runs normally with it open (audio on the
/// card, the web control, the save and quit actions), so it is no blocking
/// dialog: the guard names it and never closes it (#9, 2026-09-28).
pub const EVALUATION_NOTICE: &str = "About REAPER";

/// How the handover's report and `iemmode status` name the notice.
pub const NOTICE_REPORT: &str = r#"reaper_notice: "evaluation""#;

/// REAPER's visible dialogs, sorted by what they mean.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dialogs {
    /// Every dialog but the notice, by title: each needs a person, so the
    /// handover fails on it and the save never goes on to the quit.
    pub blocking: Vec<String>,
    /// REAPER's evaluation notice is among them.
    pub notice: bool,
}

/// Sorts the titles of REAPER's visible dialogs
/// (`iem_win::window::dialog_titles`): only a title that starts with
/// [`EVALUATION_NOTICE`] is the notice; the words anywhere else, or in
/// another case, do not make one.
pub fn dialogs(titles: &[String]) -> Dialogs {
    let mut out = Dialogs::default();
    for title in titles {
        if title.starts_with(EVALUATION_NOTICE) {
            out.notice = true;
        } else {
            out.blocking.push(title.clone());
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReaperFacts {
    pub tracks: Option<u32>,
    pub expected_tracks: u32,
    /// The titles of REAPER's visible dialogs, sorted by [`dialogs`].
    pub dialogs: Vec<String>,
    pub heartbeat_advanced: bool,
    pub holds_module: bool,
    /// Stage-input peaks in dBFS (REAPER meters).
    pub peaks: Vec<f64>,
}

/// Whether the stage inputs carried any signal during the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audio {
    Confirmed,
    /// Every stage input at the meter floor: `UNCONFIRMED-AUDIO`, reported,
    /// never a failure (the stage may simply be silent).
    Unconfirmed,
}

/// Peaks at or below this are silence (the meters' floor).
pub const FLOOR_DB: f64 = -150.0;

/// REAPER's project is loaded: it reports the expected track count. Until
/// then the meter bridge is left alone (it may be another project).
pub fn project_loaded(tracks: Option<u32>, expected: u32) -> bool {
    tracks == Some(expected)
}

pub fn reaper_handover(f: &ReaperFacts) -> Result<Audio, Vec<String>> {
    let mut bad = Vec::new();
    if !project_loaded(f.tracks, f.expected_tracks) {
        bad.push(format!(
            "tracks {:?}, expected {}",
            f.tracks, f.expected_tracks
        ));
    }
    if !dialogs(&f.dialogs).blocking.is_empty() {
        bad.push("a REAPER dialog is open".into());
    }
    if !f.heartbeat_advanced {
        bad.push("the meter heartbeat does not advance".into());
    }
    if !f.holds_module {
        bad.push("REAPER does not hold the driver module".into());
    }
    if !bad.is_empty() {
        return Err(bad);
    }
    Ok(if f.peaks.iter().any(|p| p.is_finite() && *p > FLOOR_DB) {
        Audio::Confirmed
    } else {
        Audio::Unconfirmed
    })
}

/// What the guard observed after posting the tray's Exit command (design §5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppExit {
    /// From a handle opened before the post: `None` = not signalled within 30 s.
    pub exit_code: Option<u32>,
    pub ports_free: bool,
    pub newer_temp: bool,
    /// Corroboration only: the app's buffered logger may never flush this line.
    pub logged: bool,
}

pub fn app_exit(f: AppExit) -> Result<(), Vec<&'static str>> {
    let mut bad = Vec::new();
    match f.exit_code {
        None => bad.push("the app did not exit within 30 s"),
        Some(0) => {}
        Some(_) => bad.push("the app exited with a non-zero code: not the tray Exit path"),
    }
    if !f.ports_free {
        bad.push("ports 80/443 still held");
    }
    if f.newer_temp {
        bad.push("a temp file newer than the command: a write was cut");
    }
    if bad.is_empty() { Ok(()) } else { Err(bad) }
}

/// The precheck's binary identity (design §5.3): refuse before REAPER is quit.
pub fn app_binary(recorded: &str, now: &str) -> Result<(), String> {
    if recorded.eq_ignore_ascii_case(now) {
        Ok(())
    } else {
        Err(format!(
            "predecessor exe changed ({now}); the exit id must be re-derived"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_project_is_loaded_at_the_expected_track_count() {
        assert!(project_loaded(Some(40), 40));
        assert!(!project_loaded(Some(39), 40));
        assert!(!project_loaded(Some(41), 40));
        assert!(!project_loaded(None, 40));
        assert!(!project_loaded(None, 0));
        assert!(project_loaded(Some(0), 0));
    }

    #[test]
    fn bridge_running_is_left_alone() {
        assert_eq!(bridge("1"), Bridge::Running);
    }

    #[test]
    fn bridge_empty_is_triggered_once() {
        assert_eq!(bridge(""), Bridge::TriggerOnce);
    }

    #[test]
    fn bridge_zero_refuses() {
        assert_eq!(
            bridge("0"),
            Bridge::Refuse(r#"meter bridge state "0": not triggered"#.into())
        );
    }

    #[test]
    fn bridge_two_refuses() {
        assert_eq!(
            bridge("2"),
            Bridge::Refuse(r#"meter bridge state "2": not triggered"#.into())
        );
        assert!(matches!(bridge(" 1"), Bridge::Refuse(_)));
    }

    fn good() -> ReaperFacts {
        ReaperFacts {
            tracks: Some(40),
            expected_tracks: 40,
            dialogs: Vec::new(),
            heartbeat_advanced: true,
            holds_module: true,
            peaks: vec![f64::NEG_INFINITY, -40.0],
        }
    }

    #[test]
    fn a_good_handover_with_signal_is_confirmed() {
        assert_eq!(reaper_handover(&good()), Ok(Audio::Confirmed));
    }

    #[test]
    fn a_silent_stage_is_unconfirmed_not_failed() {
        for peaks in [
            vec![],
            vec![f64::NEG_INFINITY, f64::NEG_INFINITY],
            vec![FLOOR_DB],
            vec![f64::INFINITY],
            vec![f64::NAN],
        ] {
            let f = ReaperFacts {
                peaks: peaks.clone(),
                ..good()
            };
            assert_eq!(reaper_handover(&f), Ok(Audio::Unconfirmed), "{peaks:?}");
        }
        let just_above = ReaperFacts {
            peaks: vec![-149.9],
            ..good()
        };
        assert_eq!(reaper_handover(&just_above), Ok(Audio::Confirmed));
    }

    #[test]
    fn every_single_failure_is_named() {
        let cases = [
            (
                ReaperFacts {
                    tracks: None,
                    ..good()
                },
                "tracks None, expected 40",
            ),
            (
                ReaperFacts {
                    tracks: Some(39),
                    ..good()
                },
                "tracks Some(39), expected 40",
            ),
            (
                ReaperFacts {
                    dialogs: titles(&["Save changes?"]),
                    ..good()
                },
                "a REAPER dialog is open",
            ),
            (
                ReaperFacts {
                    heartbeat_advanced: false,
                    ..good()
                },
                "the meter heartbeat does not advance",
            ),
            (
                ReaperFacts {
                    holds_module: false,
                    ..good()
                },
                "REAPER does not hold the driver module",
            ),
        ];
        for (f, want) in cases {
            assert_eq!(reaper_handover(&f), Err(vec![want.to_owned()]), "{want}");
        }
    }

    #[test]
    fn all_failures_are_named_together() {
        let f = ReaperFacts {
            tracks: Some(1),
            expected_tracks: 2,
            dialogs: titles(&[NOTICE, "REAPER"]),
            heartbeat_advanced: false,
            holds_module: false,
            peaks: vec![-3.0],
        };
        assert_eq!(
            reaper_handover(&f),
            Err(vec![
                "tracks Some(1), expected 2".to_owned(),
                "a REAPER dialog is open".to_owned(),
                "the meter heartbeat does not advance".to_owned(),
                "REAPER does not hold the driver module".to_owned(),
            ])
        );
    }

    /// REAPER's evaluation notice as the first guard start on the PC saw it
    /// (#9, 2026-09-28): REAPER shows it at every start and runs normally
    /// with it open.
    const NOTICE: &str = "About REAPER v7.65/win64 rev 0a1b2c";

    fn titles(t: &[&str]) -> Vec<String> {
        t.iter().map(|&s| s.to_owned()).collect()
    }

    #[test]
    fn the_notice_is_known_by_reapers_own_title_and_named_in_one_way() {
        assert_eq!(EVALUATION_NOTICE, "About REAPER");
        assert_eq!(NOTICE_REPORT, r#"reaper_notice: "evaluation""#);
    }

    #[test]
    fn no_dialog_is_clear() {
        assert_eq!(
            dialogs(&[]),
            Dialogs {
                blocking: Vec::new(),
                notice: false
            }
        );
    }

    #[test]
    fn the_evaluation_notice_alone_blocks_nothing_and_is_named() {
        assert_eq!(
            dialogs(&titles(&[NOTICE])),
            Dialogs {
                blocking: Vec::new(),
                notice: true
            }
        );
        // The bare prefix is the notice too; so are two of them (REAPER's
        // Help > About shows the same window).
        assert_eq!(
            dialogs(&titles(&["About REAPER", NOTICE])),
            Dialogs {
                blocking: Vec::new(),
                notice: true
            }
        );
    }

    #[test]
    fn a_dialog_beside_the_notice_blocks_in_either_order() {
        for t in [
            titles(&["Save changes?", NOTICE]),
            titles(&[NOTICE, "Save changes?"]),
        ] {
            assert_eq!(
                dialogs(&t),
                Dialogs {
                    blocking: titles(&["Save changes?"]),
                    notice: true
                },
                "{t:?}"
            );
        }
    }

    #[test]
    fn every_other_dialog_blocks() {
        for t in [
            "Save changes?",
            "REAPER",
            "ReaScript: error",
            "",
            // The words elsewhere than at the start, or in another case,
            // are not the notice.
            "Save changes? (About REAPER project)",
            "REAPER - About REAPER",
            " About REAPER v7.65/win64",
            "about REAPER v7.65/win64",
            "About Reaper v7.65/win64",
        ] {
            assert_eq!(
                dialogs(&titles(&[t])),
                Dialogs {
                    blocking: titles(&[t]),
                    notice: false
                },
                "{t:?}"
            );
        }
        assert_eq!(
            dialogs(&titles(&["ReaScript: error", "Save changes?"])),
            Dialogs {
                blocking: titles(&["ReaScript: error", "Save changes?"]),
                notice: false
            }
        );
    }

    #[test]
    fn the_notice_passes_the_handover_and_every_other_dialog_fails_it() {
        let with = |t: &[&str]| ReaperFacts {
            dialogs: titles(t),
            ..good()
        };
        let open = || Err(vec!["a REAPER dialog is open".to_owned()]);
        assert_eq!(reaper_handover(&with(&[])), Ok(Audio::Confirmed));
        assert_eq!(reaper_handover(&with(&[NOTICE])), Ok(Audio::Confirmed));
        assert_eq!(reaper_handover(&with(&[NOTICE, "Save changes?"])), open());
        assert_eq!(reaper_handover(&with(&["Save changes?"])), open());
        assert_eq!(
            reaper_handover(&with(&["Save changes? (About REAPER project)"])),
            open()
        );
        // The notice with a silent stage: still no failure, still unconfirmed.
        let silent = ReaperFacts {
            peaks: vec![f64::NEG_INFINITY],
            ..with(&[NOTICE])
        };
        assert_eq!(reaper_handover(&silent), Ok(Audio::Unconfirmed));
    }

    fn clean() -> AppExit {
        AppExit {
            exit_code: Some(0),
            ports_free: true,
            newer_temp: false,
            logged: true,
        }
    }

    #[test]
    fn app_exit_clean_passes() {
        assert_eq!(app_exit(clean()), Ok(()));
    }

    #[test]
    fn app_exit_without_the_log_line_still_passes() {
        let f = AppExit {
            logged: false,
            ..clean()
        };
        assert_eq!(app_exit(f), Ok(()));
    }

    #[test]
    fn app_exit_that_timed_out_fails() {
        let f = AppExit {
            exit_code: None,
            ..clean()
        };
        assert_eq!(app_exit(f), Err(vec!["the app did not exit within 30 s"]));
    }

    #[test]
    fn app_exit_with_a_non_zero_code_fails() {
        for code in [1, 259, u32::MAX] {
            let f = AppExit {
                exit_code: Some(code),
                ..clean()
            };
            assert_eq!(
                app_exit(f),
                Err(vec![
                    "the app exited with a non-zero code: not the tray Exit path"
                ]),
                "{code}"
            );
        }
    }

    #[test]
    fn app_exit_with_ports_held_fails() {
        let f = AppExit {
            ports_free: false,
            ..clean()
        };
        assert_eq!(app_exit(f), Err(vec!["ports 80/443 still held"]));
    }

    #[test]
    fn app_exit_with_a_newer_temp_file_fails() {
        let f = AppExit {
            newer_temp: true,
            ..clean()
        };
        assert_eq!(
            app_exit(f),
            Err(vec!["a temp file newer than the command: a write was cut"])
        );
    }

    #[test]
    fn app_exit_names_every_problem() {
        let f = AppExit {
            exit_code: None,
            ports_free: false,
            newer_temp: true,
            logged: false,
        };
        assert_eq!(app_exit(f).unwrap_err().len(), 3);
    }

    #[test]
    fn the_app_binary_must_be_the_recorded_one() {
        let a = "ab".repeat(32);
        assert_eq!(app_binary(&a, &a), Ok(()));
        assert_eq!(app_binary(&a.to_uppercase(), &a), Ok(()));
        let b = "cd".repeat(32);
        assert_eq!(
            app_binary(&a, &b),
            Err(format!(
                "predecessor exe changed ({b}); the exit id must be re-derived"
            ))
        );
        // An unrecorded hash refuses too.
        assert!(app_binary("", &a).is_err());
    }

    fn procs(running: u32, ending: u32) -> ReaperProcs {
        ReaperProcs { running, ending }
    }

    /// #10, 2026-10-08: the unwind's plan saw a REAPER that was crashing on
    /// quit (Windows Error Reporting held it), so it planned no start; the
    /// handover first waits for such a REAPER to be gone, once, whatever
    /// else runs or was started.
    #[test]
    fn a_reaper_still_ending_is_waited_for_once() {
        for running in 0..3 {
            for started in [false, true] {
                for ending in [1, 2] {
                    assert_eq!(
                        ensure_reaper(procs(running, ending), started, false),
                        Ensure::AwaitEnd,
                        "{running} {ending} {started}"
                    );
                    assert_eq!(
                        ensure_reaper(procs(running, ending), started, true),
                        Ensure::StillEnding,
                        "{running} {ending} {started}"
                    );
                }
            }
        }
    }

    /// With none running and none ending, REAPER is started, once: a start
    /// of this plan (`ReaperStart`) or of this handover whose process does
    /// not show yet is never followed by a second one.
    #[test]
    fn with_none_running_reaper_is_started_once() {
        for waited in [false, true] {
            assert_eq!(ensure_reaper(procs(0, 0), false, waited), Ensure::Start);
            assert_eq!(ensure_reaper(procs(0, 0), true, waited), Ensure::Check);
        }
    }

    /// A REAPER that runs (and is not ending) gets the handover's checks.
    #[test]
    fn a_running_reaper_is_checked() {
        for running in [1, 2] {
            for started in [false, true] {
                for waited in [false, true] {
                    assert_eq!(
                        ensure_reaper(procs(running, 0), started, waited),
                        Ensure::Check,
                        "{running} {started} {waited}"
                    );
                }
            }
        }
        assert_eq!(ReaperProcs::default(), procs(0, 0));
    }
}
