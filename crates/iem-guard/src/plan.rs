//! The switch planner and its error policy (S6 design note §5.2).
//!
//! Pure: facts in, ordered steps out. Every step re-reads its own facts before
//! it acts, so re-running a plan is safe; a failed or interrupted switch into
//! dev/live unwinds with `plan(Mode::Event, facts)`. The mode it starts
//! from changes nothing: the facts say what runs.
//!
//! Its parts: `policy.rs` (the error policy: `on_error`) and
//! `activation.rs` (what `activate` does per mode).

use serde::{Deserialize, Deserializer, Serialize};

mod activation;
mod policy;

pub use self::activation::{Activation, Busy, activation};
pub use self::policy::{Health, OnError, PrefFail, on_error};

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

#[cfg(test)]
mod activation_tests;
#[cfg(test)]
mod tests;
