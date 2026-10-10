//! The error policy (S6 design note §5.2): what a failed step means.

use serde::{Deserialize, Serialize};

use super::{Mode, Step};

/// `[guard] on_pref_fail`: required in the site, decided on #9
/// (`start_reaper_with_alarm`, 2026-09-27).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrefFail {
    StartReaperWithAlarm,
    KeepReaperDown,
}

/// The engine as read over the supervisor pipe after a failed release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// Callbacks advancing, not faulted, not parked: it still serves the band.
    Healthy,
    Dead,
    Parked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnError {
    /// Into dev/live: alarm, then the event plan.
    Unwind,
    /// Event plan: alarm and go on with the next step.
    Continue,
    /// Event plan: alarm with the prepared ❓ and drop these later steps
    /// (they would act on a stale process); the switch ends `needs_owner`,
    /// never `done` (#10: a failed app stop leaves no app serving).
    SkipAskOwner(&'static [Step]),
    /// Event plan: iemmixer keeps serving the band; alarm; the plan ends here.
    KeepServing,
    /// Event plan: alarm, the plan ends here, the agent sends the prepared ❓.
    StopAskOwner,
    /// Event plan: alarm with the prepared ❓ and go on with the next step
    /// (the band keeps what still works); the switch ends `needs_owner`,
    /// never `done` (#10: a failed REAPER or app handover).
    ContinueAskOwner,
}

/// What a failed step means. `health` is read only after a failed `EngineStop`.
/// Every failure of a dev or live entry unwinds, its `PrefCheck` before the
/// engine included; `on_pref_fail` is the event plan's rule only. A failed
/// REAPER handover (REAPER could not be made to run, or a check failed)
/// asks the owner and goes on to the app, as it went on before #10, but the
/// switch no longer ends `done` (2026-10-08: it did, in event without
/// REAPER). An event switch that ends without the predecessor app serving
/// is not done either (the coordinator's decision on #10, 2026-10-08:
/// REAPER keeps playing the band's mixes, but the phones cannot change
/// them): a failed app handover asks the owner and goes on; a failed app
/// stop skips the app start (the old app may still run) and asks the owner,
/// since the event plan stops only an app that does not serve.
pub fn on_error(to: Mode, step: Step, health: Option<Health>, pref_fail: PrefFail) -> OnError {
    if to != Mode::Event {
        return OnError::Unwind;
    }
    match step {
        Step::EngineStop | Step::EngineHealth => match health {
            Some(Health::Healthy) => OnError::KeepServing,
            _ => OnError::StopAskOwner,
        },
        Step::PrefCheck => match pref_fail {
            PrefFail::StartReaperWithAlarm => OnError::Continue,
            PrefFail::KeepReaperDown => OnError::StopAskOwner,
        },
        Step::HolderGone | Step::ReaperSaveQuit | Step::ReaperStart => OnError::StopAskOwner,
        Step::ReaperHandover | Step::AppHandover => OnError::ContinueAskOwner,
        Step::AppStop => OnError::SkipAskOwner(&[Step::AppStart]),
        _ => OnError::Continue,
    }
}
