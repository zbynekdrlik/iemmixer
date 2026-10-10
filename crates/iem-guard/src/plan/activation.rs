//! What `activate <sha>` does per mode (design §5.5; #9 2026-09-28).

use super::{Facts, Mode};

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
