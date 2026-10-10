//! The precheck of a dev or live entry (design §5.2 step 1).

use super::{R, StepError};
use crate::plan::Mode;

/// What the precheck of a dev/live entry reads (design §5.2 step 1). The
/// bundle's record and HIL result are the daemon's (`bundle::may_go_live`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecheckFacts {
    pub to: Mode,
    pub trial: bool,
    /// The current bundle's engine is installed.
    pub bundle: bool,
    /// `[guard] pc_tests_passed` (design §10).
    pub pc_tests_passed: bool,
    /// The PWA notification subscriptions an alarm goes to (`iem-server
    /// notify --count alarm`); `None`: unreadable.
    pub subscriptions: Option<u32>,
    /// An engine the guard did not start runs (ours is stopped by the plan).
    pub foreign_engine: bool,
    /// `handover::app_binary` of the predecessor's exe.
    pub app_binary: Result<(), String>,
}

/// Why the guard's alarms would reach no phone (design §5.4); `None`: at
/// least one PWA notification subscription.
fn no_subscription(subscriptions: Option<u32>) -> Option<&'static str> {
    match subscriptions {
        None => Some("the PWA notification subscriptions cannot be read"),
        Some(0) => {
            Some("no PWA notification subscription: no engineer device allowed notifications")
        }
        Some(_) => None,
    }
}

/// How a dev entry's note goes on after [`no_subscription`].
const DEV_WITHOUT_SUBSCRIPTION: &str =
    " (not needed for dev: the alarms stay in the guard's alarm file)";

/// The precheck's verdict: `Err` refuses the entry with every problem;
/// `Ok(Some(note))` lets it go on and names what it found. A PWA
/// notification subscription (the engineer's, where the alarms go, #9
/// 2026-09-28) is required for live and live trials only: the predecessor's
/// arrive with the band import, a later step of the entry, and a new one
/// only through iem-server, which runs only in dev and live (in event the
/// predecessor holds the band's address). Dev without one names it, and
/// the alarms stay in the guard's alarm file.
pub fn precheck(f: &PrecheckFacts) -> R<Option<String>> {
    let mut bad = Vec::new();
    let mut note = None;
    if f.to == Mode::Event {
        bad.push("the precheck is for dev and live".to_owned());
    }
    if !f.bundle {
        bad.push("no installed bundle is active".to_owned());
    }
    if f.trial && f.to != Mode::Live {
        bad.push("a trial is a live switch".to_owned());
    }
    if f.to == Mode::Live && f.trial && !f.pc_tests_passed {
        bad.push(
            "[guard] pc_tests_passed is false: no trial before the owner-approved PC tests"
                .to_owned(),
        );
    }
    if let Some(why) = no_subscription(f.subscriptions) {
        if f.to == Mode::Live || f.trial {
            bad.push(why.to_owned());
        } else {
            note = Some(format!("{why}{DEV_WITHOUT_SUBSCRIPTION}"));
        }
    }
    if f.foreign_engine {
        bad.push("an engine the guard did not start runs".to_owned());
    }
    if let Err(e) = &f.app_binary {
        bad.push(e.clone());
    }
    if bad.is_empty() {
        Ok(note)
    } else {
        Err(StepError::Failed(bad.join("; ")))
    }
}
