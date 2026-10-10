//! `activate` per mode (`plan/activation.rs`), every combination of facts.

use super::tests::{band_up, iemmixer_up};
use super::*;

// ---- activate per mode (design §5.5; #9 2026-09-28) ----

const IDLE: Busy = Busy {
    switching: false,
    job: None,
};

const LIVE_REFUSAL: &str = "activate is for dev and an idle event; the mode is live \
                            (live --build activates its bundle)";

fn refused(why: &str) -> Activation {
    Activation::Refused(why.to_owned())
}

fn ours(f: &Facts) -> bool {
    f.engine || f.server || f.tray || f.runner
}

/// A guard fix reaches a guard in event only through `activate` (a
/// guard that refuses the dev entry can never enter dev to take it):
/// in event it copies files and hands over while the guard runs none
/// of iemmixer's processes. REAPER and the app may run and serve.
#[test]
fn activate_in_an_idle_event_copies_whatever_reaper_and_the_app_do() {
    for bits in 0..(1u32 << FACT_BITS) {
        let f = Facts::from_bits(bits);
        if !ours(&f) {
            assert_eq!(
                activation(Mode::Event, &f, IDLE),
                Activation::Files,
                "{f:?}"
            );
        }
    }
    assert_eq!(activation(Mode::Event, &band_up(), IDLE), Activation::Files);
    assert_eq!(
        activation(Mode::Event, &Facts::default(), IDLE),
        Activation::Files
    );
}

#[test]
fn activate_in_event_is_refused_while_an_iemmixer_process_runs() {
    let running = |f: Facts| activation(Mode::Event, &f, IDLE);
    let text = |names: &str| {
        refused(&format!(
            "activate in event needs no iemmixer process; running: {names}"
        ))
    };
    assert_eq!(
        running(Facts {
            engine: true,
            ..band_up()
        }),
        text("engine")
    );
    assert_eq!(
        running(Facts {
            server: true,
            ..band_up()
        }),
        text("server")
    );
    assert_eq!(
        running(Facts {
            tray: true,
            ..band_up()
        }),
        text("tray")
    );
    assert_eq!(
        running(Facts {
            runner: true,
            ..band_up()
        }),
        text("runner")
    );
    assert_eq!(running(iemmixer_up()), text("engine, server, tray, runner"));
    for bits in 0..(1u32 << FACT_BITS) {
        let f = Facts::from_bits(bits);
        if ours(&f) {
            assert!(matches!(running(f), Activation::Refused(_)), "{f:?}");
        }
    }
}

#[test]
fn activate_in_event_is_refused_while_a_switch_or_a_job_waits() {
    let busy = |b: Busy| activation(Mode::Event, &band_up(), b);
    assert_eq!(
        busy(Busy {
            switching: true,
            ..IDLE
        }),
        refused("a switch is in progress: activate waits for its end")
    );
    assert_eq!(
        busy(Busy {
            job: Some(7),
            ..IDLE
        }),
        refused("HIL job 7 runs: activate waits for its end")
    );
    // A process of iemmixer's is named first.
    assert_eq!(
        activation(
            Mode::Event,
            &Facts {
                runner: true,
                ..band_up()
            },
            Busy {
                switching: true,
                job: Some(7),
            }
        ),
        refused("activate in event needs no iemmixer process; running: runner")
    );
}

/// Dev as before: whatever runs, inside a HIL job the engine and the
/// server restart with the new bundle.
#[test]
fn activate_in_dev_is_as_before() {
    for bits in 0..(1u32 << FACT_BITS) {
        let f = Facts::from_bits(bits);
        for switching in [false, true] {
            let b = Busy {
                switching,
                job: None,
            };
            assert_eq!(activation(Mode::Dev, &f, b), Activation::Files, "{f:?}");
            let b = Busy { job: Some(3), ..b };
            assert_eq!(
                activation(Mode::Dev, &f, b),
                Activation::FilesThenJobRestart,
                "{f:?}"
            );
        }
    }
}

/// Live activates its bundle through `live --build`, never here.
#[test]
fn activate_in_live_is_refused() {
    for bits in 0..(1u32 << FACT_BITS) {
        let f = Facts::from_bits(bits);
        for b in [
            IDLE,
            Busy {
                switching: true,
                job: Some(3),
            },
        ] {
            assert_eq!(
                activation(Mode::Live, &f, b),
                refused(LIVE_REFUSAL),
                "{f:?} {b:?}"
            );
        }
    }
}
