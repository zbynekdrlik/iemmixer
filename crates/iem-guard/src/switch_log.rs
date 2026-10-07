//! The guard's record of the last switch (S7 design note §5): each step with
//! its time and the in-ear silence it caused. Pure: the daemon times the
//! steps (`Laps`) and keeps the record in its state and its replies; `iempc
//! switch-test` (S7 part 3) reads it.

use std::time::Instant;

use serde::{Deserialize, Deserializer, Serialize};

use crate::plan::{Mode, Step};
use crate::state::Switching;

/// A step and its time: from the end of the step before it (the switch's
/// start for the first) to its own end, so a record's steps add up to the
/// switch (the state save between two steps counts toward the second).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepTime {
    pub step: Step,
    pub ms: u64,
}

/// How the switch ended (`daemon::Outcome`, as saved and replied).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchOutcome {
    Done,
    KeptServing,
    NeedsOwner,
    /// An outcome a newer guard saved.
    #[serde(other)]
    Unknown,
}

/// The last switch that ended: in `GuardState` and in every `Reply`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastSwitch {
    pub from: Mode,
    pub to: Mode,
    /// The mode the switch left the guard in (an unwound or stopped switch
    /// ends elsewhere than `to`).
    pub ended_in: Mode,
    pub outcome: SwitchOutcome,
    /// Seconds since the epoch.
    pub started: u64,
    pub ended: u64,
    /// Every step that ran, failed ones included, in order.
    pub steps: Vec<StepTime>,
    /// [`silence_ms`] of the steps.
    pub silence_ms: Option<u64>,
}

impl LastSwitch {
    pub fn new(
        sw: &Switching,
        ended_in: Mode,
        outcome: SwitchOutcome,
        ended: u64,
        steps: Vec<StepTime>,
    ) -> Self {
        let silence_ms = silence_ms(sw.to, &steps);
        Self {
            from: sw.from,
            to: sw.to,
            ended_in,
            outcome,
            started: sw.started,
            ended,
            steps,
            silence_ms,
        }
    }
}

/// The in-ear silence of a switch to `to` (ms): from the first step that
/// silences the in-ears (the engine's stop and fade-out, or REAPER's save
/// and quit, whichever ran first) through the step after which the other
/// side plays (REAPER's handover for `event`, the engine's arm for `dev` and
/// `live`), both included. `None` when either end is missing.
pub fn silence_ms(to: Mode, steps: &[StepTime]) -> Option<u64> {
    let start = steps
        .iter()
        .position(|s| matches!(s.step, Step::EngineStop | Step::ReaperSaveQuit))?;
    let last = if to == Mode::Event {
        Step::ReaperHandover
    } else {
        Step::EngineArm
    };
    let window = steps.get(start..)?;
    let end = window.iter().position(|s| s.step == last)?;
    Some(window.get(..=end)?.iter().map(|s| s.ms).sum())
}

/// The step clock of the switch running now (not persisted: a restarted
/// guard re-plans to event and times that switch).
#[derive(Debug, Default)]
pub struct Laps {
    last: Option<Instant>,
    steps: Vec<StepTime>,
}

impl Laps {
    /// A switch begins: its first step is timed from `now`, and the steps
    /// of one that never ended (an unwind is a switch of its own) are gone.
    pub fn start(&mut self, now: Instant) {
        self.last = Some(now);
        self.steps.clear();
    }

    /// `step` ended at `now` (0 ms without a start).
    pub fn lap(&mut self, step: Step, now: Instant) {
        let ms = self.last.map_or(0, |t| {
            u64::try_from(now.saturating_duration_since(t).as_millis()).unwrap_or(u64::MAX)
        });
        self.steps.push(StepTime { step, ms });
        self.last = Some(now);
    }

    /// The steps timed since the start; the clock stops.
    pub fn take(&mut self) -> Vec<StepTime> {
        self.last = None;
        std::mem::take(&mut self.steps)
    }
}

/// `GuardState.last_switch` and `Reply.last_switch` as read: a record this
/// guard cannot read is none, never an unreadable state or reply.
pub fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<Option<LastSwitch>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value(v).ok()))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn st(step: Step, ms: u64) -> StepTime {
        StepTime { step, ms }
    }

    /// The event plan from dev with a runner: the in-ears go quiet with the
    /// engine's stop and fade-out and play again once REAPER's handover
    /// passed; both ends count.
    #[test]
    fn the_silence_of_a_switch_to_event_runs_from_the_engine_stop_through_reapers_handover() {
        let steps = [
            st(Step::JobsCancel, 5),
            st(Step::RunnerStop, 900),
            st(Step::EngineStop, 600),
            st(Step::ServerStop, 300),
            st(Step::TrayStop, 100),
            st(Step::TuningExit, 2000),
            st(Step::PrefCheck, 50),
            st(Step::ReaperStart, 15_000),
            st(Step::ReaperHandover, 7000),
            st(Step::AppStart, 9000),
            st(Step::AppHandover, 3000),
            st(Step::Fingerprint, 200),
        ];
        assert_eq!(silence_ms(Mode::Event, &steps), Some(25_050));
    }

    /// The dev entry from the band's system: the app's stop leaves REAPER
    /// playing; its save and quit silences, the engine's arm plays again.
    #[test]
    fn the_silence_of_a_switch_to_dev_runs_from_reapers_save_and_quit_through_the_engine_arm() {
        let steps = [
            st(Step::Precheck, 400),
            st(Step::AppStop, 3000),
            st(Step::ReaperSaveQuit, 8000),
            st(Step::TuningEnter, 2000),
            st(Step::Data, 1500),
            st(Step::PrefCheck, 50),
            st(Step::EngineStart, 700),
            st(Step::EngineArm, 10_500),
            st(Step::ServerStart, 900),
            st(Step::TrayStart, 300),
            st(Step::IdentityCheck, 2000),
            st(Step::RunnerStart, 800),
        ];
        assert_eq!(silence_ms(Mode::Dev, &steps), Some(22_750));
        assert_eq!(silence_ms(Mode::Live, &steps), Some(22_750));
    }

    /// An event plan that meets a REAPER without the card saves and quits
    /// it after the engine's stop: the engine's fade-out opened the window.
    /// Without the engine, REAPER's save and quit opens it.
    #[test]
    fn the_first_silencing_step_opens_the_window() {
        let steps = [
            st(Step::EngineStop, 600),
            st(Step::TuningExit, 2000),
            st(Step::PrefCheck, 50),
            st(Step::ReaperSaveQuit, 4000),
            st(Step::ReaperStart, 15_000),
            st(Step::ReaperHandover, 7000),
            st(Step::AppHandover, 3000),
        ];
        assert_eq!(silence_ms(Mode::Event, &steps), Some(28_650));
        assert_eq!(silence_ms(Mode::Event, &steps[1..]), Some(26_000));
    }

    /// Every step inside the window counts, the health read the runner
    /// inserts after a failed engine stop included.
    #[test]
    fn a_failed_engine_stop_and_its_health_read_count_toward_the_silence() {
        let steps = [
            st(Step::RunnerStop, 900),
            st(Step::EngineStop, 10_000),
            st(Step::EngineHealth, 200),
            st(Step::ServerStop, 300),
            st(Step::TrayStop, 100),
            st(Step::TuningExit, 2000),
            st(Step::PrefCheck, 50),
            st(Step::ReaperStart, 15_000),
            st(Step::ReaperHandover, 7000),
            st(Step::AppHandover, 3000),
        ];
        assert_eq!(silence_ms(Mode::Event, &steps), Some(34_650));
    }

    #[test]
    fn a_switch_that_silenced_nothing_or_never_played_again_has_no_window() {
        // Nothing of ours ran and REAPER kept the card: nothing went quiet.
        let kept = [
            st(Step::TuningExit, 2000),
            st(Step::PrefCheck, 50),
            st(Step::ReaperHandover, 7000),
        ];
        assert_eq!(silence_ms(Mode::Event, &kept), None);
        // The engine stopped, the plan stopped for the owner before REAPER.
        let stopped = [st(Step::EngineStop, 10_000), st(Step::EngineHealth, 200)];
        assert_eq!(silence_ms(Mode::Event, &stopped), None);
        // A dev entry that unwound before the engine's arm.
        let unwound = [st(Step::ReaperSaveQuit, 8000), st(Step::EngineStart, 700)];
        assert_eq!(silence_ms(Mode::Dev, &unwound), None);
        // The end before the start is no window.
        let reversed = [st(Step::ReaperHandover, 7000), st(Step::EngineStop, 600)];
        assert_eq!(silence_ms(Mode::Event, &reversed), None);
        assert_eq!(silence_ms(Mode::Event, &[]), None);
    }

    #[test]
    fn laps_time_each_step_from_the_end_of_the_one_before() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut laps = Laps::default();
        laps.start(t0);
        laps.lap(Step::EngineStop, at(600));
        laps.lap(Step::ServerStop, at(900));
        assert_eq!(
            laps.take(),
            [st(Step::EngineStop, 600), st(Step::ServerStop, 300)]
        );
        // Taken: empty, and a lap without a start counts nothing.
        assert_eq!(laps.take(), Vec::<StepTime>::new());
        laps.lap(Step::EngineHealth, at(1500));
        assert_eq!(laps.take(), [st(Step::EngineHealth, 0)]);
        // A start forgets the steps of a switch that never ended (an unwind
        // is a switch of its own).
        laps.start(at(2000));
        laps.lap(Step::EngineArm, at(2500));
        laps.start(at(3000));
        laps.lap(Step::EngineStop, at(3100));
        assert_eq!(laps.take(), [st(Step::EngineStop, 100)]);
        // A clock read before the last one counts 0, never wraps.
        laps.start(at(4000));
        laps.lap(Step::TrayStop, at(3900));
        assert_eq!(laps.take(), [st(Step::TrayStop, 0)]);
    }

    #[test]
    fn outcomes_read_and_an_unknown_one_reads_as_unknown() {
        let read = |s: &str| serde_json::from_value::<SwitchOutcome>(serde_json::json!(s)).unwrap();
        assert_eq!(read("done"), SwitchOutcome::Done);
        assert_eq!(read("kept_serving"), SwitchOutcome::KeptServing);
        assert_eq!(read("needs_owner"), SwitchOutcome::NeedsOwner);
        assert_eq!(read("later"), SwitchOutcome::Unknown);
        assert_eq!(
            serde_json::to_value(SwitchOutcome::KeptServing).unwrap(),
            serde_json::json!("kept_serving")
        );
    }

    fn a_record() -> LastSwitch {
        let sw = Switching {
            from: Mode::Event,
            to: Mode::Dev,
            done: vec![Step::Precheck],
            started: 1_790_000_100,
        };
        let steps = vec![
            st(Step::Precheck, 400),
            st(Step::ReaperSaveQuit, 8000),
            st(Step::EngineArm, 10_500),
        ];
        LastSwitch::new(&sw, Mode::Dev, SwitchOutcome::Done, 1_790_000_119, steps)
    }

    #[test]
    fn a_record_names_the_switch_its_steps_and_its_silence() {
        let r = a_record();
        assert_eq!(
            (r.from, r.to, r.ended_in, r.outcome),
            (Mode::Event, Mode::Dev, Mode::Dev, SwitchOutcome::Done)
        );
        assert_eq!((r.started, r.ended), (1_790_000_100, 1_790_000_119));
        assert_eq!(r.steps.len(), 3);
        assert_eq!(r.silence_ms, Some(18_500));
    }

    /// `lenient` reads `GuardState.last_switch` and `Reply.last_switch`: a
    /// record this guard cannot read (an older or newer shape, a step it
    /// does not have) is none, never an unreadable state or reply.
    #[test]
    fn a_record_a_guard_cannot_read_is_none() {
        let read = |v: serde_json::Value| lenient(v).unwrap();
        assert_eq!(read(serde_json::json!({"from": 7})), None);
        assert_eq!(read(serde_json::Value::Null), None);
        let mut newer = serde_json::to_value(a_record()).unwrap();
        newer["steps"][1]["step"] = serde_json::json!("a_newer_step");
        assert_eq!(read(newer), None);
        let r = a_record();
        assert_eq!(read(serde_json::to_value(&r).unwrap()), Some(r));
    }
}
