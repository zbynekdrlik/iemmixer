//! The switch planner and its error policy (S6 design note §5.2).
//!
//! Pure: facts in, ordered steps out. Every step re-reads its own facts before
//! it acts, so re-running a plan is safe; a failed or interrupted switch into
//! dev/live unwinds with `plan(Mode::Event, facts)`. The mode it starts
//! from changes nothing: the facts say what runs.

use serde::{Deserialize, Deserializer, Serialize};

/// What the PC runs: REAPER and the predecessor app (`event`), or iemmixer
/// (`dev` before cutover, `live` after it or as a trial). After a reboot the
/// PC is always in `event` (spec §4.1), hence the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Event,
    Dev,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Precheck,
    AppStop,
    ReaperSaveQuit,
    TuningEnter,
    Data,
    EngineStart,
    EngineArm,
    ServerStart,
    TrayStart,
    IdentityCheck,
    RunnerStart,
    JobsCancel,
    RunnerStop,
    EngineStop,
    /// Inserted by the runner after a failed `EngineStop` (never planned).
    EngineHealth,
    ServerStop,
    TrayStop,
    TuningExit,
    PrefCheck,
    HolderGone,
    ReaperStart,
    ReaperHandover,
    AppStart,
    AppHandover,
    Fingerprint,
}

impl Step {
    /// Every step, in declaration order (tests and status listings).
    pub const ALL: [Step; 25] = [
        Step::Precheck,
        Step::AppStop,
        Step::ReaperSaveQuit,
        Step::TuningEnter,
        Step::Data,
        Step::EngineStart,
        Step::EngineArm,
        Step::ServerStart,
        Step::TrayStart,
        Step::IdentityCheck,
        Step::RunnerStart,
        Step::JobsCancel,
        Step::RunnerStop,
        Step::EngineStop,
        Step::EngineHealth,
        Step::ServerStop,
        Step::TrayStop,
        Step::TuningExit,
        Step::PrefCheck,
        Step::HolderGone,
        Step::ReaperStart,
        Step::ReaperHandover,
        Step::AppStart,
        Step::AppHandover,
        Step::Fingerprint,
    ];

    /// The step of this name, if this guard has it.
    fn named(name: String) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(name)).ok()
    }
}

/// An alarm's step as saved (`alarms.json`) or sent (a reply): a step an
/// older guard had and this one has not (the interlock, gone with #38) reads
/// as none, so that guard's alarms still load.
pub fn known_step<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Step>, D::Error> {
    Ok(Option::<String>::deserialize(d)?.and_then(Step::named))
}

/// A switch's steps done as saved (`guard-state.json`) or sent: the steps
/// this guard has, in order; one only an older guard had is left out.
pub fn known_steps<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Step>, D::Error> {
    Ok(Vec::<String>::deserialize(d)?
        .into_iter()
        .filter_map(Step::named)
        .collect())
}

/// Read once per plan (module holders and ports included); the once-a-second
/// watch reads the process list only (design §5.1, P10).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Facts {
    pub reaper: bool,
    pub app: bool,
    pub engine: bool,
    pub server: bool,
    pub tray: bool,
    pub runner: bool,
    /// REAPER holds the driver module: it opened the card.
    pub reaper_holds_module: bool,
    /// The app owns ports 80/443 (the listening pid is the app's).
    pub app_serves: bool,
    /// A process other than REAPER and our engine holds the driver module.
    pub other_module_holder: bool,
}

/// The number of fields of [`Facts`].
pub const FACT_BITS: u32 = 9;

impl Facts {
    /// Every combination for the exhaustive tests (2^9 = 512): bit `n` sets
    /// the `n`-th field in declaration order.
    pub fn from_bits(b: u32) -> Self {
        let bit = |n: u32| (b & (1 << n)) != 0;
        Self {
            reaper: bit(0),
            app: bit(1),
            engine: bit(2),
            server: bit(3),
            tray: bit(4),
            runner: bit(5),
            reaper_holds_module: bit(6),
            app_serves: bit(7),
            other_module_holder: bit(8),
        }
    }
}

fn stop_iemmixer(f: &Facts, out: &mut Vec<Step>) {
    if f.runner {
        out.extend([Step::JobsCancel, Step::RunnerStop]);
    }
    if f.engine {
        out.push(Step::EngineStop);
    }
    if f.server {
        out.push(Step::ServerStop);
    }
    if f.tray {
        out.push(Step::TrayStop);
    }
}

pub fn plan(to: Mode, f: &Facts) -> Vec<Step> {
    let mut out = Vec::new();
    match to {
        Mode::Event => {
            stop_iemmixer(f, &mut out);
            out.push(Step::TuningExit);
            // Every other holder of the driver module leaves before the
            // preference is checked: the check never writes while a process
            // has the driver open (#9 2026-09-28), so it must not find one
            // the plan could have waited for.
            if f.other_module_holder {
                out.push(Step::HolderGone);
            }
            out.push(Step::PrefCheck);
            // A REAPER that runs without the card (its time trigger, or a start
            // while our engine held it) is saved, quit and started again.
            let reaper_ok = f.reaper && f.reaper_holds_module;
            if f.reaper && !reaper_ok {
                out.push(Step::ReaperSaveQuit);
            }
            if !reaper_ok {
                out.push(Step::ReaperStart);
            }
            out.push(Step::ReaperHandover);
            // An app that runs but does not serve (redeployed while iem-server
            // held the ports) is stopped through its tray command and started again.
            let app_ok = f.app && f.app_serves;
            if f.app && !app_ok {
                out.push(Step::AppStop);
            }
            if !app_ok {
                out.push(Step::AppStart);
            }
            out.extend([Step::AppHandover, Step::Fingerprint]);
        }
        Mode::Dev | Mode::Live => {
            // No step reads the stage (#38, owner 2026-10-06): only the
            // owner's signal decides whether the PC may change, and other
            // devices on the Dante network feed the card's inputs.
            out.push(Step::Precheck);
            // The app first: after it nothing writes to REAPER, so the save
            // cannot be dirtied before the quit (deviation from spec §4.3).
            if f.app {
                out.push(Step::AppStop);
            }
            if f.reaper {
                out.push(Step::ReaperSaveQuit);
            }
            stop_iemmixer(f, &mut out);
            out.extend([
                Step::TuningEnter,
                Step::Data,
                // REAPER's preferred buffer back (read back) right before the
                // engine opens the card: an engine that ended while it held
                // the card (a hard kill, a power loss) left 32, and a new one
                // refuses the card unless it finds the original (#9
                // 2026-09-28). REAPER has quit and our engine stopped here,
                // so no open driver sees the write.
                Step::PrefCheck,
                Step::EngineStart,
                Step::EngineArm,
                Step::ServerStart,
                Step::TrayStart,
                Step::IdentityCheck,
            ]);
            if to == Mode::Dev {
                out.push(Step::RunnerStart);
            }
        }
    }
    out
}

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

/// What the guard has going on when `activate` arrives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Busy {
    /// A switch is persisted as unfinished.
    pub switching: bool,
    /// The HIL job that began and has not ended.
    pub job: Option<u64>,
}

/// What `activate <sha>` does (design §5.5; #9 2026-09-28).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activation {
    /// The bundle's guard and `iemmode` into `bin\`, the pin, its Defender
    /// exclusions, then the hand-over to a changed guard exe.
    Files,
    /// Dev inside a HIL job: the same, with the engine and the server
    /// started again from the new bundle before the hand-over.
    FilesThenJobRestart,
    /// Nothing is done, for this reason.
    Refused(String),
}

/// `activate` per mode. In `dev` as always. In `event` only while the
/// guard runs none of iemmixer's processes (in event it has none) and no
/// switch or HIL job waits: then it only copies files and hands over, and
/// REAPER and the predecessor app are never touched (in event the guard
/// only reads them). This is how a guard fix reaches a guard in event,
/// whose own code may refuse the dev entry. In `live` never: `live
/// --build` activates its bundle.
pub fn activation(mode: Mode, f: &Facts, busy: Busy) -> Activation {
    match mode {
        Mode::Dev if busy.job.is_some() => Activation::FilesThenJobRestart,
        Mode::Dev => Activation::Files,
        Mode::Live => Activation::Refused(
            "activate is for dev and an idle event; the mode is live \
             (live --build activates its bundle)"
                .to_owned(),
        ),
        Mode::Event => idle_event(f, busy).map_or(Activation::Files, Activation::Refused),
    }
}

/// Why an event guard is not idle enough to activate, if it is not.
fn idle_event(f: &Facts, busy: Busy) -> Option<String> {
    let running: Vec<&str> = [
        (f.engine, "engine"),
        (f.server, "server"),
        (f.tray, "tray"),
        (f.runner, "runner"),
    ]
    .into_iter()
    .filter_map(|(runs, name)| runs.then_some(name))
    .collect();
    if !running.is_empty() {
        return Some(format!(
            "activate in event needs no iemmixer process; running: {}",
            running.join(", ")
        ));
    }
    if busy.switching {
        return Some("a switch is in progress: activate waits for its end".to_owned());
    }
    busy.job
        .map(|run| format!("HIL job {run} runs: activate waits for its end"))
}

#[cfg(test)]
mod activation_tests;
#[cfg(test)]
mod tests;
