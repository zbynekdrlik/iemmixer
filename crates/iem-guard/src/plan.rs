//! The switch planner and its error policy (S6 design note §5.2).
//!
//! Pure: facts in, ordered steps out. Every step re-reads its own facts before
//! it acts, so re-running a plan is safe; a failed or interrupted switch into
//! dev/live unwinds with `plan(current, Mode::Event, facts)`.

use serde::{Deserialize, Serialize};

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
    Interlock,
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
    pub const ALL: [Step; 26] = [
        Step::Precheck,
        Step::Interlock,
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
    /// `live` before cutover (a rehearsal): the band is there on purpose.
    pub trial: bool,
    /// Owner-instructed `--force`: skips the interlock only.
    pub force: bool,
}

/// The number of fields of [`Facts`].
pub const FACT_BITS: u32 = 11;

impl Facts {
    /// Every combination for the exhaustive tests (2^11 = 2048): bit `n` sets
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
            trial: bit(9),
            force: bit(10),
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

pub fn plan(from: Mode, to: Mode, f: &Facts) -> Vec<Step> {
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
            out.push(Step::Precheck);
            let band_there = to == Mode::Live && f.trial;
            // A running REAPER or app means the band's system is up, whatever
            // the saved mode says (a reboot restores `event`, spec §4.1).
            let from_band =
                f.reaper || f.app || from == Mode::Event || (from == Mode::Live && to == Mode::Dev);
            if from_band && !band_there && !f.force {
                out.push(Step::Interlock);
            }
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
    /// Event plan: alarm and drop these later steps (they would act on a stale process).
    Skip(&'static [Step]),
    /// Event plan: iemmixer keeps serving the band; alarm; the plan ends here.
    KeepServing,
    /// Event plan: alarm, the plan ends here, the agent sends the prepared ❓.
    StopAskOwner,
}

/// What a failed step means. `health` is read only after a failed `EngineStop`.
/// Every failure of a dev or live entry unwinds, its `PrefCheck` before the
/// engine included; `on_pref_fail` is the event plan's rule only.
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
        Step::AppStop => OnError::Skip(&[Step::AppStart]),
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
    /// A refused entry into this mode waits for its interlock retry.
    pub retry: Option<Mode>,
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
/// switch, HIL job or interlock retry waits: then it only copies files and
/// hands over, and REAPER and the predecessor app are never touched (in
/// event the guard only reads them). This is how a guard fix reaches a
/// guard in event, whose own code may refuse the dev entry. A hand-over's
/// new guard starts in event and drops a waiting retry
/// (`state::reset_to_event`), so a retry refuses instead. In `live` never:
/// `live --build` activates its bundle.
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
    if let Some(run) = busy.job {
        return Some(format!("HIL job {run} runs: activate waits for its end"));
    }
    busy.retry.map(|to| {
        format!(
            "the {} switch waits for its interlock retry: activate waits until it ran or was \
             dropped",
            crate::view::mode_name(to)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODES: [Mode; 3] = [Mode::Event, Mode::Dev, Mode::Live];

    fn at(p: &[Step], s: Step) -> Option<usize> {
        p.iter().position(|x| *x == s)
    }

    fn has(p: &[Step], s: Step) -> bool {
        at(p, s).is_some()
    }

    fn every(mut check: impl FnMut(Mode, Facts, Vec<Step>)) {
        for bits in 0..(1u32 << FACT_BITS) {
            let f = Facts::from_bits(bits);
            for from in MODES {
                check(from, f, plan(from, Mode::Event, &f));
            }
        }
    }

    fn every_entry(mut check: impl FnMut(Mode, Mode, Facts, Vec<Step>)) {
        for bits in 0..(1u32 << FACT_BITS) {
            let f = Facts::from_bits(bits);
            for from in MODES {
                for to in [Mode::Dev, Mode::Live] {
                    check(from, to, f, plan(from, to, &f));
                }
            }
        }
    }

    fn band_up() -> Facts {
        Facts {
            reaper: true,
            app: true,
            reaper_holds_module: true,
            app_serves: true,
            ..Facts::default()
        }
    }

    fn iemmixer_up() -> Facts {
        Facts {
            engine: true,
            server: true,
            tray: true,
            runner: true,
            ..Facts::default()
        }
    }

    #[test]
    fn from_bits_sets_each_field_from_its_own_bit() {
        assert_eq!(Facts::from_bits(0), Facts::default());
        let all = Facts::from_bits((1 << FACT_BITS) - 1);
        assert_eq!(
            all,
            Facts {
                reaper: true,
                app: true,
                engine: true,
                server: true,
                tray: true,
                runner: true,
                reaper_holds_module: true,
                app_serves: true,
                other_module_holder: true,
                trial: true,
                force: true,
            }
        );
        let one = |n: u32| Facts::from_bits(1 << n);
        assert_eq!(
            one(0),
            Facts {
                reaper: true,
                ..Facts::default()
            }
        );
        assert_eq!(
            one(1),
            Facts {
                app: true,
                ..Facts::default()
            }
        );
        assert_eq!(
            one(2),
            Facts {
                engine: true,
                ..Facts::default()
            }
        );
        assert_eq!(
            one(3),
            Facts {
                server: true,
                ..Facts::default()
            }
        );
        assert_eq!(
            one(4),
            Facts {
                tray: true,
                ..Facts::default()
            }
        );
        assert_eq!(
            one(5),
            Facts {
                runner: true,
                ..Facts::default()
            }
        );
        assert_eq!(
            one(6),
            Facts {
                reaper_holds_module: true,
                ..Facts::default()
            }
        );
        assert_eq!(
            one(7),
            Facts {
                app_serves: true,
                ..Facts::default()
            }
        );
        assert_eq!(
            one(8),
            Facts {
                other_module_holder: true,
                ..Facts::default()
            }
        );
        assert_eq!(
            one(9),
            Facts {
                trial: true,
                ..Facts::default()
            }
        );
        assert_eq!(
            one(10),
            Facts {
                force: true,
                ..Facts::default()
            }
        );
        // Bits above the facts change nothing.
        assert_eq!(Facts::from_bits(1 << FACT_BITS), Facts::default());
    }

    #[test]
    fn modes_and_steps_serialise_in_snake_case() {
        assert_eq!(Mode::default(), Mode::Event);
        assert_eq!(serde_json::to_string(&Mode::Live).unwrap(), r#""live""#);
        assert_eq!(
            serde_json::to_string(&Step::ReaperSaveQuit).unwrap(),
            r#""reaper_save_quit""#
        );
        assert_eq!(
            serde_json::to_string(&PrefFail::StartReaperWithAlarm).unwrap(),
            r#""start_reaper_with_alarm""#
        );
        assert_eq!(
            serde_json::to_string(&Health::Parked).unwrap(),
            r#""parked""#
        );
        for s in Step::ALL {
            let text = serde_json::to_string(&s).unwrap();
            assert_eq!(serde_json::from_str::<Step>(&text).unwrap(), s);
        }
        // ALL lists every step exactly once.
        for (i, s) in Step::ALL.iter().enumerate() {
            assert_eq!(at(&Step::ALL, *s), Some(i), "{s:?}");
        }
    }

    #[test]
    fn every_event_plan_checks_the_preference_before_reaper() {
        every(|from, f, p| {
            let pref = at(&p, Step::PrefCheck).expect("PrefCheck in every event plan");
            for s in [Step::ReaperStart, Step::ReaperHandover, Step::AppStart] {
                if let Some(i) = at(&p, s) {
                    assert!(pref < i, "{from:?} {f:?}: {s:?} before PrefCheck");
                }
            }
            if let Some(e) = at(&p, Step::EngineStop) {
                assert!(e < pref, "{from:?} {f:?}: EngineStop after PrefCheck");
            }
            // Every other holder of the driver module has left: the check
            // never writes while a process has the driver open (#9
            // 2026-09-28), so it must not find one it could have waited for.
            if let Some(h) = at(&p, Step::HolderGone) {
                assert!(h < pref, "{from:?} {f:?}: HolderGone after PrefCheck");
            }
        });
    }

    #[test]
    fn every_event_plan_ends_with_the_fingerprint() {
        every(|_, _, p| assert_eq!(p.last(), Some(&Step::Fingerprint)));
    }

    #[test]
    fn every_event_plan_starts_what_does_not_serve_and_keeps_what_does() {
        every(|from, f, p| {
            let why = format!("{from:?} {f:?}: {p:?}");
            let reaper_ok = f.reaper && f.reaper_holds_module;
            let app_ok = f.app && f.app_serves;
            assert_eq!(has(&p, Step::ReaperStart), !reaper_ok, "{why}");
            assert_eq!(
                has(&p, Step::ReaperSaveQuit),
                f.reaper && !f.reaper_holds_module,
                "{why}"
            );
            assert_eq!(has(&p, Step::AppStart), !app_ok, "{why}");
            assert_eq!(has(&p, Step::AppStop), f.app && !f.app_serves, "{why}");
            assert_eq!(has(&p, Step::HolderGone), f.other_module_holder, "{why}");
            assert_eq!(has(&p, Step::EngineStop), f.engine, "{why}");
            assert_eq!(has(&p, Step::ServerStop), f.server, "{why}");
            assert_eq!(has(&p, Step::TrayStop), f.tray, "{why}");
            assert_eq!(has(&p, Step::JobsCancel), f.runner, "{why}");
            assert_eq!(has(&p, Step::RunnerStop), f.runner, "{why}");
            for s in [
                Step::TuningExit,
                Step::PrefCheck,
                Step::ReaperHandover,
                Step::AppHandover,
                Step::Fingerprint,
            ] {
                assert!(has(&p, s), "{why}: {s:?} missing");
            }
            // Never planned: the entry steps and the runner's own EngineHealth.
            for s in [
                Step::Precheck,
                Step::Interlock,
                Step::TuningEnter,
                Step::Data,
                Step::EngineStart,
                Step::EngineArm,
                Step::ServerStart,
                Step::TrayStart,
                Step::IdentityCheck,
                Step::RunnerStart,
                Step::EngineHealth,
            ] {
                assert!(!has(&p, s), "{why}: {s:?} planned");
            }
            // Order: a restart quits before it starts, a foreign holder leaves
            // before REAPER is touched, REAPER is handed over before the app.
            let before = |a: Step, b: Step| {
                if let (Some(i), Some(j)) = (at(&p, a), at(&p, b)) {
                    assert!(i < j, "{why}: {a:?} after {b:?}");
                }
            };
            before(Step::JobsCancel, Step::RunnerStop);
            before(Step::RunnerStop, Step::EngineStop);
            before(Step::EngineStop, Step::ServerStop);
            before(Step::ServerStop, Step::TrayStop);
            before(Step::TrayStop, Step::TuningExit);
            before(Step::TuningExit, Step::HolderGone);
            before(Step::TuningExit, Step::PrefCheck);
            before(Step::HolderGone, Step::PrefCheck);
            before(Step::HolderGone, Step::ReaperSaveQuit);
            before(Step::HolderGone, Step::ReaperStart);
            before(Step::ReaperSaveQuit, Step::ReaperStart);
            before(Step::ReaperStart, Step::ReaperHandover);
            before(Step::ReaperHandover, Step::AppStop);
            before(Step::ReaperHandover, Step::AppStart);
            before(Step::AppStop, Step::AppStart);
            before(Step::AppStart, Step::AppHandover);
        });
    }

    #[test]
    fn failed_engine_stop_never_starts_the_app_without_reaper() {
        for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
            for h in [
                None,
                Some(Health::Healthy),
                Some(Health::Dead),
                Some(Health::Parked),
            ] {
                let e = on_error(Mode::Event, Step::EngineStop, h, pf);
                assert!(
                    matches!(e, OnError::KeepServing | OnError::StopAskOwner),
                    "{h:?}: {e:?}"
                );
            }
        }
        assert_eq!(
            on_error(
                Mode::Event,
                Step::EngineStop,
                Some(Health::Healthy),
                PrefFail::KeepReaperDown
            ),
            OnError::KeepServing
        );
    }

    #[test]
    fn a_failed_release_keeps_serving_only_while_the_engine_is_healthy() {
        for step in [Step::EngineStop, Step::EngineHealth] {
            for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
                assert_eq!(
                    on_error(Mode::Event, step, Some(Health::Healthy), pf),
                    OnError::KeepServing
                );
                assert_eq!(
                    on_error(Mode::Event, step, Some(Health::Dead), pf),
                    OnError::StopAskOwner
                );
                assert_eq!(
                    on_error(Mode::Event, step, Some(Health::Parked), pf),
                    OnError::StopAskOwner
                );
                assert_eq!(on_error(Mode::Event, step, None, pf), OnError::StopAskOwner);
            }
        }
    }

    /// #38 (owner, 2026-10-06): a dev or live entry is the precheck, the
    /// stops and the start, from any mode and with any facts. No step reads
    /// the stage: only the owner's signal decides whether the PC may change,
    /// and other devices on the Dante network feed the card's inputs.
    #[test]
    fn every_entry_is_the_precheck_the_stops_and_the_start() {
        every_entry(|from, to, f, p| {
            let mut want = vec![Step::Precheck];
            if f.app {
                want.push(Step::AppStop);
            }
            if f.reaper {
                want.push(Step::ReaperSaveQuit);
            }
            if f.runner {
                want.extend([Step::JobsCancel, Step::RunnerStop]);
            }
            if f.engine {
                want.push(Step::EngineStop);
            }
            if f.server {
                want.push(Step::ServerStop);
            }
            if f.tray {
                want.push(Step::TrayStop);
            }
            want.extend([
                Step::TuningEnter,
                Step::Data,
                Step::PrefCheck,
                Step::EngineStart,
                Step::EngineArm,
                Step::ServerStart,
                Step::TrayStart,
                Step::IdentityCheck,
            ]);
            if to == Mode::Dev {
                want.push(Step::RunnerStart);
            }
            assert_eq!(p, want, "{from:?}→{to:?} {f:?}");
        });
    }

    #[test]
    fn dev_entry_with_reaper_running_always_runs_the_interlock() {
        for from in MODES {
            for f in [
                Facts {
                    reaper: true,
                    ..Facts::default()
                },
                Facts {
                    app: true,
                    ..Facts::default()
                },
            ] {
                let p = plan(from, Mode::Dev, &f);
                assert!(at(&p, Step::Interlock).is_some(), "{from:?} {f:?}");
            }
        }
    }

    #[test]
    fn the_app_stops_before_reaper_saves() {
        let f = band_up();
        let p = plan(Mode::Event, Mode::Dev, &f);
        let (i, a, r) = (
            at(&p, Step::Interlock).unwrap(),
            at(&p, Step::AppStop).unwrap(),
            at(&p, Step::ReaperSaveQuit).unwrap(),
        );
        assert!(i < a && a < r, "{p:?}");
    }

    #[test]
    fn event_to_dev_is_the_whole_entry_in_order() {
        assert_eq!(
            plan(Mode::Event, Mode::Dev, &band_up()),
            [
                Step::Precheck,
                Step::Interlock,
                Step::AppStop,
                Step::ReaperSaveQuit,
                Step::TuningEnter,
                Step::Data,
                Step::PrefCheck,
                Step::EngineStart,
                Step::EngineArm,
                Step::ServerStart,
                Step::TrayStart,
                Step::IdentityCheck,
                Step::RunnerStart,
            ]
        );
    }

    /// An engine that ended while it held the card (a hard kill, a power
    /// loss) left 32, and a new engine refuses the card unless it finds
    /// REAPER's original (#9 2026-09-28). So every entry restores the
    /// preference right before the engine starts: once, after REAPER quit
    /// and after our own engine stopped (no open driver sees the write),
    /// never while REAPER may hold the card.
    #[test]
    fn every_entry_restores_the_preference_right_before_the_engine_starts() {
        every_entry(|from, to, f, p| {
            let why = format!("{from:?}→{to:?} {f:?}: {p:?}");
            let pref = at(&p, Step::PrefCheck).expect("PrefCheck in every entry");
            assert_eq!(
                p.iter().filter(|s| **s == Step::PrefCheck).count(),
                1,
                "{why}"
            );
            assert_eq!(at(&p, Step::EngineStart), Some(pref + 1), "{why}");
            for s in [
                Step::Interlock,
                Step::AppStop,
                Step::ReaperSaveQuit,
                Step::EngineStop,
                Step::TuningEnter,
                Step::Data,
            ] {
                if let Some(i) = at(&p, s) {
                    assert!(i < pref, "{why}: {s:?} after PrefCheck");
                }
            }
        });
        assert_eq!(
            plan(Mode::Event, Mode::Live, &band_up()),
            [
                Step::Precheck,
                Step::Interlock,
                Step::AppStop,
                Step::ReaperSaveQuit,
                Step::TuningEnter,
                Step::Data,
                Step::PrefCheck,
                Step::EngineStart,
                Step::EngineArm,
                Step::ServerStart,
                Step::TrayStart,
                Step::IdentityCheck,
            ]
        );
        // Our own engine stops (and restores the preference as it releases
        // the card) before the check.
        assert_eq!(
            plan(Mode::Dev, Mode::Dev, &iemmixer_up()),
            [
                Step::Precheck,
                Step::JobsCancel,
                Step::RunnerStop,
                Step::EngineStop,
                Step::ServerStop,
                Step::TrayStop,
                Step::TuningEnter,
                Step::Data,
                Step::PrefCheck,
                Step::EngineStart,
                Step::EngineArm,
                Step::ServerStart,
                Step::TrayStart,
                Step::IdentityCheck,
                Step::RunnerStart,
            ]
        );
    }

    /// A restore that fails before the engine starts unwinds to event like
    /// any entry step; the event plan's own `PrefCheck` then follows
    /// `on_pref_fail` as before.
    #[test]
    fn a_failed_pref_check_in_an_entry_unwinds_to_event() {
        for to in [Mode::Dev, Mode::Live] {
            for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
                assert_eq!(
                    on_error(to, Step::PrefCheck, None, pf),
                    OnError::Unwind,
                    "{to:?} {pf:?}"
                );
            }
        }
        let back = plan(Mode::Dev, Mode::Event, &Facts::default());
        assert!(has(&back, Step::PrefCheck), "{back:?}");
    }

    #[test]
    fn dev_to_event_is_the_whole_teardown_in_order() {
        assert_eq!(
            plan(Mode::Dev, Mode::Event, &iemmixer_up()),
            [
                Step::JobsCancel,
                Step::RunnerStop,
                Step::EngineStop,
                Step::ServerStop,
                Step::TrayStop,
                Step::TuningExit,
                Step::PrefCheck,
                Step::ReaperStart,
                Step::ReaperHandover,
                Step::AppStart,
                Step::AppHandover,
                Step::Fingerprint,
            ]
        );
    }

    #[test]
    fn every_entry_plan_has_its_shape() {
        every_entry(|from, to, f, p| {
            let why = format!("{from:?}→{to:?} {f:?}: {p:?}");
            assert_eq!(p.first(), Some(&Step::Precheck), "{why}");
            assert_eq!(has(&p, Step::AppStop), f.app, "{why}");
            assert_eq!(has(&p, Step::ReaperSaveQuit), f.reaper, "{why}");
            assert_eq!(has(&p, Step::EngineStop), f.engine, "{why}");
            assert_eq!(has(&p, Step::ServerStop), f.server, "{why}");
            assert_eq!(has(&p, Step::TrayStop), f.tray, "{why}");
            assert_eq!(has(&p, Step::JobsCancel), f.runner, "{why}");
            assert_eq!(has(&p, Step::RunnerStop), f.runner, "{why}");
            assert_eq!(has(&p, Step::RunnerStart), to == Mode::Dev, "{why}");
            assert_eq!(
                p.last(),
                Some(if to == Mode::Dev {
                    &Step::RunnerStart
                } else {
                    &Step::IdentityCheck
                }),
                "{why}"
            );
            if let Some(i) = at(&p, Step::Interlock) {
                assert_eq!(i, 1, "{why}: the interlock comes right after the precheck");
            }
            // Nothing of the band's system is started, and no event-only step
            // runs (`PrefCheck` runs in both: right before the engine here).
            for s in [
                Step::ReaperStart,
                Step::ReaperHandover,
                Step::AppStart,
                Step::AppHandover,
                Step::TuningExit,
                Step::HolderGone,
                Step::Fingerprint,
                Step::EngineHealth,
            ] {
                assert!(!has(&p, s), "{why}: {s:?} planned");
            }
            // The band's system is down and ours is stopped before tuning.
            let tuning = at(&p, Step::TuningEnter).expect("TuningEnter");
            for s in [
                Step::AppStop,
                Step::ReaperSaveQuit,
                Step::JobsCancel,
                Step::RunnerStop,
                Step::EngineStop,
                Step::ServerStop,
                Step::TrayStop,
            ] {
                if let Some(i) = at(&p, s) {
                    assert!(i < tuning, "{why}: {s:?} after TuningEnter");
                }
            }
            if let (Some(a), Some(r)) = (at(&p, Step::AppStop), at(&p, Step::ReaperSaveQuit)) {
                assert!(a < r, "{why}: the app stops before REAPER saves");
            }
        });
    }

    #[test]
    fn a_trial_skips_the_interlock() {
        for from in MODES {
            let f = Facts {
                trial: true,
                ..band_up()
            };
            assert!(
                !has(&plan(from, Mode::Live, &f), Step::Interlock),
                "{from:?}"
            );
            // `trial` only means something for `live`.
            assert!(has(&plan(from, Mode::Dev, &f), Step::Interlock), "{from:?}");
        }
        // A live entry that is not a trial checks the stage.
        assert!(has(
            &plan(Mode::Event, Mode::Live, &band_up()),
            Step::Interlock
        ));
    }

    #[test]
    fn force_skips_only_the_interlock() {
        for from in MODES {
            for to in [Mode::Dev, Mode::Live] {
                let forced = plan(
                    from,
                    to,
                    &Facts {
                        force: true,
                        ..band_up()
                    },
                );
                let mut normal = plan(from, to, &band_up());
                assert!(has(&normal, Step::Interlock), "{from:?}→{to:?}");
                normal.retain(|s| *s != Step::Interlock);
                assert_eq!(forced, normal, "{from:?}→{to:?}");
            }
        }
    }

    #[test]
    fn live_to_dev_runs_the_interlock() {
        assert!(has(
            &plan(Mode::Live, Mode::Dev, &Facts::default()),
            Step::Interlock
        ));
        assert!(has(
            &plan(Mode::Live, Mode::Dev, &iemmixer_up()),
            Step::Interlock
        ));
    }

    #[test]
    fn dev_to_live_does_not() {
        assert!(!has(
            &plan(Mode::Dev, Mode::Live, &Facts::default()),
            Step::Interlock
        ));
        assert!(!has(
            &plan(Mode::Dev, Mode::Live, &iemmixer_up()),
            Step::Interlock
        ));
    }

    #[test]
    fn staying_in_dev_or_live_with_the_band_down_skips_the_interlock() {
        for m in [Mode::Dev, Mode::Live] {
            assert!(
                !has(&plan(m, m, &Facts::default()), Step::Interlock),
                "{m:?}"
            );
            assert!(!has(&plan(m, m, &iemmixer_up()), Step::Interlock), "{m:?}");
        }
        // From event it always runs, even with REAPER and the app already down.
        for to in [Mode::Dev, Mode::Live] {
            assert!(
                has(&plan(Mode::Event, to, &Facts::default()), Step::Interlock),
                "{to:?}"
            );
        }
    }

    #[test]
    fn the_runner_starts_only_in_dev() {
        for from in MODES {
            for f in [Facts::default(), band_up(), iemmixer_up()] {
                let dev = plan(from, Mode::Dev, &f);
                assert_eq!(dev.last(), Some(&Step::RunnerStart), "{from:?} {f:?}");
                assert!(
                    !has(&plan(from, Mode::Live, &f), Step::RunnerStart),
                    "{from:?} {f:?}"
                );
                assert!(
                    !has(&plan(from, Mode::Event, &f), Step::RunnerStart),
                    "{from:?} {f:?}"
                );
            }
        }
    }

    #[test]
    fn event_in_event_only_checks() {
        assert_eq!(
            plan(Mode::Event, Mode::Event, &band_up()),
            [
                Step::TuningExit,
                Step::PrefCheck,
                Step::ReaperHandover,
                Step::AppHandover,
                Step::Fingerprint,
            ]
        );
    }

    #[test]
    fn a_stale_reaper_is_restarted() {
        let f = Facts {
            reaper_holds_module: false,
            ..band_up()
        };
        let p = plan(Mode::Event, Mode::Event, &f);
        let pref = at(&p, Step::PrefCheck).unwrap();
        let quit = at(&p, Step::ReaperSaveQuit).expect("ReaperSaveQuit");
        let start = at(&p, Step::ReaperStart).expect("ReaperStart");
        assert!(pref < quit && quit < start, "{p:?}");
        assert!(
            !has(&p, Step::AppStart),
            "the serving app is left alone: {p:?}"
        );
    }

    #[test]
    fn an_app_that_does_not_serve_is_restarted() {
        let f = Facts {
            app_serves: false,
            ..band_up()
        };
        let p = plan(Mode::Dev, Mode::Event, &f);
        let stop = at(&p, Step::AppStop).expect("AppStop");
        let start = at(&p, Step::AppStart).expect("AppStart");
        assert!(stop < start, "{p:?}");
        assert!(
            !has(&p, Step::ReaperStart),
            "the serving REAPER is left alone: {p:?}"
        );
    }

    #[test]
    fn another_holder_blocks_reaper() {
        let f = Facts {
            other_module_holder: true,
            ..Facts::default()
        };
        let p = plan(Mode::Dev, Mode::Event, &f);
        let gone = at(&p, Step::HolderGone).expect("HolderGone");
        assert!(gone < at(&p, Step::ReaperStart).unwrap(), "{p:?}");
        // The holder leaves before the preference is checked: the check
        // never writes while a process has the driver open (#9 2026-09-28).
        assert_eq!(
            p,
            [
                Step::TuningExit,
                Step::HolderGone,
                Step::PrefCheck,
                Step::ReaperStart,
                Step::ReaperHandover,
                Step::AppStart,
                Step::AppHandover,
                Step::Fingerprint,
            ]
        );
        for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
            assert_eq!(
                on_error(Mode::Event, Step::HolderGone, None, pf),
                OnError::StopAskOwner
            );
        }
    }

    #[test]
    fn a_failed_pref_check_follows_the_choice() {
        assert_eq!(
            on_error(
                Mode::Event,
                Step::PrefCheck,
                None,
                PrefFail::StartReaperWithAlarm
            ),
            OnError::Continue
        );
        assert_eq!(
            on_error(Mode::Event, Step::PrefCheck, None, PrefFail::KeepReaperDown),
            OnError::StopAskOwner
        );
    }

    #[test]
    fn a_failed_app_stop_skips_the_app_start() {
        for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
            assert_eq!(
                on_error(Mode::Event, Step::AppStop, None, pf),
                OnError::Skip(&[Step::AppStart])
            );
        }
    }

    #[test]
    fn a_failed_reaper_quit_or_start_asks_the_owner() {
        for s in [Step::ReaperSaveQuit, Step::ReaperStart] {
            for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
                assert_eq!(
                    on_error(Mode::Event, s, None, pf),
                    OnError::StopAskOwner,
                    "{s:?}"
                );
            }
        }
    }

    #[test]
    fn other_event_failures_alarm_and_go_on() {
        let special = [
            Step::EngineStop,
            Step::EngineHealth,
            Step::PrefCheck,
            Step::HolderGone,
            Step::ReaperSaveQuit,
            Step::ReaperStart,
            Step::AppStop,
        ];
        for s in Step::ALL {
            if special.contains(&s) {
                continue;
            }
            for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
                assert_eq!(
                    on_error(Mode::Event, s, None, pf),
                    OnError::Continue,
                    "{s:?}"
                );
                assert_eq!(
                    on_error(Mode::Event, s, Some(Health::Dead), pf),
                    OnError::Continue,
                    "{s:?}"
                );
            }
        }
    }

    #[test]
    fn every_dev_or_live_error_unwinds() {
        for to in [Mode::Dev, Mode::Live] {
            for s in Step::ALL {
                for h in [
                    None,
                    Some(Health::Healthy),
                    Some(Health::Dead),
                    Some(Health::Parked),
                ] {
                    for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
                        assert_eq!(
                            on_error(to, s, h, pf),
                            OnError::Unwind,
                            "{to:?} {s:?} {h:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_failed_dev_switch_unwinds_to_event() {
        // The facts after a failure at EngineStart: REAPER and the app were
        // quit, the engine never came up.
        let entry = plan(Mode::Event, Mode::Dev, &band_up());
        assert!(has(&entry, Step::EngineStart));
        let p = plan(Mode::Event, Mode::Event, &Facts::default());
        let (reaper, app) = (
            at(&p, Step::ReaperStart).expect("REAPER starts"),
            at(&p, Step::AppStart).expect("the app starts"),
        );
        assert!(reaper < app, "{p:?}");
        assert!(at(&p, Step::ReaperHandover).unwrap() < app, "{p:?}");
        assert_eq!(p.last(), Some(&Step::Fingerprint));
    }

    // ---- activate per mode (design §5.5; #9 2026-09-28) ----

    const IDLE: Busy = Busy {
        switching: false,
        job: None,
        retry: None,
    };

    const LIVE_REFUSAL: &str = "activate is for dev and an idle event; the mode is live \
                                (live --build activates its bundle)";

    const RETRY_REFUSAL: &str =
        "switch waits for its interlock retry: activate waits until it ran or was dropped";

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
    fn activate_in_event_is_refused_while_a_switch_a_job_or_a_retry_waits() {
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
        // A hand-over's new guard starts in event and drops a waiting
        // retry (state::reset_to_event): activate waits for it instead.
        assert_eq!(
            busy(Busy {
                retry: Some(Mode::Dev),
                ..IDLE
            }),
            refused(&format!("the dev {RETRY_REFUSAL}"))
        );
        assert_eq!(
            busy(Busy {
                retry: Some(Mode::Live),
                ..IDLE
            }),
            refused(&format!("the live {RETRY_REFUSAL}"))
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
                    retry: Some(Mode::Dev),
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
            for (switching, retry) in [(false, None), (true, Some(Mode::Live))] {
                let b = Busy {
                    switching,
                    job: None,
                    retry,
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
                    retry: Some(Mode::Dev),
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
}
