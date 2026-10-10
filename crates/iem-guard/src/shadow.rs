//! The report-only shadow import, the guard's part (S8 lane 4, #11; design
//! note `docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md`
//! §3.5). Pure (mutated): where an entry runs it, what its history line
//! holds, and that it never fails an entry. `WinPc` runs the command and
//! appends the line.
//!
//! `pc.toml`'s `shadow` command (`iem-migrate shadow`, which writes nothing)
//! runs at each entry from event into dev or live, as the step
//! [`Step::Shadow`] right after `TuningEnter`: REAPER has saved and quit,
//! iemmixer is stopped, and the data refresh (`Step::Data`, before the
//! cutover) has not yet replaced the engine's state, so it reads the same
//! project the refresh imports and the state the engine last saved. In prod
//! there is no refresh (`lifecycle::refreshes_data`), and the shadow runs
//! all the same: a report of how far REAPER's project has drifted from what
//! iemmixer serves, the evidence a rollback's export starts from. No
//! `shadow` key, no step.
//!
//! It never fails an entry and never holds one up past [`LIMIT`]: a run
//! that fails, exits non-zero, prints no report or outlives the limit (left
//! to end by itself, never ended by force) is one `error` line in the
//! history and a log line, never an alarm, and the entry goes on. "Ide
//! event" during it ends the wait at once (the run is a wait, not a
//! mutation) and pre-empts the entry, as it does any step.

use std::time::Duration;

use serde_json::{Map, Value};

use crate::pc::{R, StepError};
use crate::plan::{Mode, Step};
use crate::view::mode_name;

/// How long an entry waits for the shadow: past it the run is left to end
/// by itself and its report is dropped (an `error` line). It reads one
/// project and one state file: well under a second on the PC.
pub const LIMIT: Duration = Duration::from_secs(5);
/// The history's folder under `pc.toml`'s `root`, and its file: one JSON
/// line per entry, appended, never rewritten.
pub const DIR: &str = "shadow";
pub const HISTORY: &str = "history.jsonl";

/// `steps` with [`Step::Shadow`] right after `TuningEnter`, for an entry
/// from event into dev or live when `pc.toml` names a shadow command
/// (`configured`); every other switch as it is.
pub fn plan(mut steps: Vec<Step>, from: Mode, to: Mode, configured: bool) -> Vec<Step> {
    if configured
        && from == Mode::Event
        && to != Mode::Event
        && let Some(at) = steps.iter().position(|s| *s == Step::TuningEnter)
    {
        steps.insert(at + 1, Step::Shadow);
    }
    steps
}

/// The entry a shadow belongs to: `event→dev`, `event→live`.
pub fn entry(to: Mode) -> String {
    format!("event→{}", mode_name(to))
}

/// How the shadow's command ended (`WinPc`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    /// Exit 0: what it printed, its report (one JSON object).
    Printed(String),
    /// Another exit (none: no code), and the end of its stderr.
    Exited(Option<i32>, String),
    /// It could not start, or did not end within [`LIMIT`] (left to end by
    /// itself): why.
    Failed(String),
    /// "Ide event" came while it ran: the entry unwinds.
    Preempted,
}

fn error(line: &mut Map<String, Value>, code: &str, why: String) -> String {
    line.insert("error".into(), code.into());
    line.insert("why".into(), why.into());
    format!("not recorded ({code}), see {DIR}\\{HISTORY}")
}

/// What a report says in a few words: the import's verdict and how many
/// differences it names.
fn summary(report: &Map<String, Value>) -> String {
    let count = |key: &str| {
        report
            .get(key)
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
    };
    let import = report.get("import").and_then(Value::as_str).unwrap_or("?");
    format!(
        "import {import}, {} site and {} state difference(s)",
        count("site"),
        count("state")
    )
}

/// One shadow import's history line and the text the switch's report
/// names it by. The line: `at` (Unix ms), `entry`, `bundle`, then the
/// report's own fields (it cannot replace those three), or `error` (a
/// fixed code: `report`, `exit` with its `exit` code, `failed`,
/// `preempted`) and `why`.
pub fn record(at: u64, to: Mode, bundle: Option<&str>, run: Run) -> (String, String) {
    let mut line = Map::new();
    line.insert("at".into(), at.into());
    line.insert("entry".into(), entry(to).into());
    line.insert("bundle".into(), bundle.map_or(Value::Null, Value::from));
    let said = match run {
        Run::Printed(out) => match serde_json::from_str::<Value>(out.trim()) {
            Ok(Value::Object(report)) => {
                let said = summary(&report);
                for (key, value) in report {
                    line.entry(key).or_insert(value);
                }
                said
            }
            _ => error(&mut line, "report", "it printed no JSON object".into()),
        },
        Run::Exited(code, why) => {
            line.insert("exit".into(), code.map_or(Value::Null, Value::from));
            error(&mut line, "exit", why)
        }
        Run::Failed(why) => error(&mut line, "failed", why),
        Run::Preempted => error(
            &mut line,
            "preempted",
            "\"ide event\" came while it ran".into(),
        ),
    };
    (
        Value::Object(line).to_string(),
        format!("shadow import ({}): {said}", entry(to)),
    )
}

/// The step's result: only "ide event" (`Preempted`) ends it early; any
/// other failure of the shadow is a line in the switch's report, never the
/// entry's failure.
pub fn never_fails(r: R<String>) -> R<String> {
    match r {
        Err(StepError::Failed(why)) => Ok(format!("shadow import not recorded: {why}")),
        other => other,
    }
}

#[cfg(test)]
mod tests;
