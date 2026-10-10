//! The precheck of a dev or live entry (design §5.2 step 1) and where the
//! alarms go.

use super::*;

fn ready() -> PrecheckFacts {
    PrecheckFacts {
        to: Mode::Dev,
        trial: false,
        bundle: true,
        pc_tests_passed: false,
        subscriptions: Some(1),
        foreign_engine: false,
        app_binary: Ok(()),
    }
}

#[test]
fn a_complete_precheck_passes() {
    assert_eq!(precheck(&ready()), Ok(None));
    // A live entry that is not a trial needs no PC tests.
    let live = PrecheckFacts {
        to: Mode::Live,
        ..ready()
    };
    assert_eq!(precheck(&live), Ok(None));
    let trial = PrecheckFacts {
        to: Mode::Live,
        trial: true,
        pc_tests_passed: true,
        ..ready()
    };
    assert_eq!(precheck(&trial), Ok(None));
    let many = PrecheckFacts {
        subscriptions: Some(3),
        ..ready()
    };
    assert_eq!(precheck(&many), Ok(None));
}

/// The precheck's texts for the PWA notification subscriptions.
const NO_SUBSCRIPTION: &str =
    "no PWA notification subscription: no engineer device allowed notifications";
const SUBSCRIPTIONS_UNREADABLE: &str = "the PWA notification subscriptions cannot be read";

/// The alarms go to the engineer's PWA notification subscriptions (#9
/// 2026-09-28). The predecessor's arrive with the band import, a later
/// step of the entry, and a new one only through iem-server, which runs
/// only in dev and live: one is required for live and live trials; dev
/// goes on and names what is missing, the alarms stay in the guard's
/// alarm file.
#[test]
fn a_pwa_subscription_is_required_for_live_and_named_for_dev() {
    let dev = |subscriptions| PrecheckFacts {
        subscriptions,
        ..ready()
    };
    assert_eq!(
        precheck(&dev(Some(0))),
        Ok(Some(format!(
            "{NO_SUBSCRIPTION} (not needed for dev: the alarms stay in the guard's alarm file)"
        )))
    );
    assert_eq!(
        precheck(&dev(None)),
        Ok(Some(format!(
            "{SUBSCRIPTIONS_UNREADABLE} \
             (not needed for dev: the alarms stay in the guard's alarm file)"
        )))
    );
    assert_eq!(precheck(&dev(Some(1))), Ok(None));
    // Another refusal of a dev entry takes precedence over the note.
    let unbundled = PrecheckFacts {
        bundle: false,
        ..dev(Some(0))
    };
    assert_eq!(
        precheck(&unbundled),
        Err(StepError::Failed("no installed bundle is active".into()))
    );
    // Live and live trials refuse as before.
    let live = |subscriptions| PrecheckFacts {
        to: Mode::Live,
        subscriptions,
        ..ready()
    };
    let trial = |subscriptions| PrecheckFacts {
        trial: true,
        pc_tests_passed: true,
        ..live(subscriptions)
    };
    for f in [live(Some(0)), trial(Some(0))] {
        assert_eq!(
            precheck(&f),
            Err(StepError::Failed(NO_SUBSCRIPTION.into())),
            "{f:?}"
        );
    }
    for f in [live(None), trial(None)] {
        assert_eq!(
            precheck(&f),
            Err(StepError::Failed(SUBSCRIPTIONS_UNREADABLE.into())),
            "{f:?}"
        );
    }
}

#[test]
fn every_precheck_problem_is_named() {
    let cases = [
        (
            PrecheckFacts {
                to: Mode::Event,
                ..ready()
            },
            "the precheck is for dev and live",
        ),
        (
            PrecheckFacts {
                bundle: false,
                ..ready()
            },
            "no installed bundle is active",
        ),
        (
            PrecheckFacts {
                trial: true,
                pc_tests_passed: true,
                ..ready()
            },
            "a trial is a live switch",
        ),
        (
            PrecheckFacts {
                to: Mode::Live,
                trial: true,
                ..ready()
            },
            "[guard] pc_tests_passed is false: no trial before the owner-approved PC tests",
        ),
        (
            PrecheckFacts {
                to: Mode::Live,
                subscriptions: None,
                ..ready()
            },
            SUBSCRIPTIONS_UNREADABLE,
        ),
        (
            PrecheckFacts {
                to: Mode::Live,
                subscriptions: Some(0),
                ..ready()
            },
            NO_SUBSCRIPTION,
        ),
        (
            PrecheckFacts {
                foreign_engine: true,
                ..ready()
            },
            "an engine the guard did not start runs",
        ),
        (
            PrecheckFacts {
                app_binary: Err("predecessor exe changed".into()),
                ..ready()
            },
            "predecessor exe changed",
        ),
    ];
    for (f, want) in cases {
        assert_eq!(precheck(&f), Err(StepError::Failed(want.into())), "{want}");
    }
    let all = PrecheckFacts {
        to: Mode::Dev,
        trial: true,
        bundle: false,
        pc_tests_passed: false,
        subscriptions: Some(0),
        foreign_engine: true,
        app_binary: Err("changed".into()),
    };
    assert_eq!(
        precheck(&all),
        Err(StepError::Failed(format!(
            "no installed bundle is active; a trial is a live switch; {NO_SUBSCRIPTION}; \
             an engine the guard did not start runs; changed"
        )))
    );
}
