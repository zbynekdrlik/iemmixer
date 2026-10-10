//! The cutover (S8 design note
//! `docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md` §3.2):
//! the owner's message turns the PC from `Trial` into `Prod` on the active
//! `main` bundle (`iemmode cutover --build SHA`, `iempc cutover --sha`).
//!
//! Pure: the refusals, the steps in their order, which begun steps a failed
//! cutover changes back, the elevated cutover task's request, the server
//! config's `pin_changes` edit and the post-cutover engine check. The daemon
//! (`daemon::cutover`) runs each step through `Pc`, reads it back, saves
//! the record of the cutover in progress before every step
//! (`GuardState.cutover`), and on any failure unwinds to `Trial` and
//! `event`; a guard that starts with a record still saved (a crash or a
//! power loss between two steps) unwinds it first.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;

use crate::bundle::{Record, may_go_live};
use crate::lifecycle::Lifecycle;
use crate::pc::EngineSeen;
use crate::plan::{Health, Mode};

/// The elevated task that exports, disables and re-enables the
/// predecessor's autostarts and sets the guard task's logon trigger
/// (`IemCutover.psm1`, installed by `iempc cutover`; RunLevel Highest, the
/// Limited guard may run it).
pub const TASK: &str = r"\iemmixer\iemmixer-cutover";
/// Its request and result files' kind (`<root>\guard\tasks\cutover.request.json`,
/// `<elevated root>\tasks\out\cutover.result.json`).
pub const KIND: &str = "cutover";
/// The period the engine must run at once the PC is in prod (frames).
pub const FRAMES: u32 = 32;
/// The prefix of an autostart export's folder in `<elevated root>\cutover`.
pub const EXPORT_PREFIX: &str = "autostarts-";

/// The cutover's steps (design §3.2), each read back. The guard's logon
/// trigger comes before the predecessor's autostarts go (the review of lane
/// 2; the design note lists them the other way round): at every moment
/// something starts at the next boot, the predecessor or the guard, whose
/// start unwinds a cutover that was cut off and runs the event plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutStep {
    /// The final import: a live trial entry on the build (its data refresh
    /// imports the saved REAPER project as `data_live`).
    Import,
    /// The guard task's logon trigger.
    GuardLogon,
    /// The predecessor's autostarts exported to
    /// `<elevated root>\cutover\autostarts-<since>` and disabled.
    Autostarts,
    /// `pin_changes = true` in the server's config; the server started
    /// again so it serves with it.
    PinChanges,
    /// `Prod { since, pin = build }`, saved and read back.
    Lifecycle,
    /// The band's address names the build, a member page loads, the engine
    /// plays at 32.
    Checks,
}

/// The steps in their order.
pub const STEPS: [CutStep; 6] = [
    CutStep::Import,
    CutStep::GuardLogon,
    CutStep::Autostarts,
    CutStep::PinChanges,
    CutStep::Lifecycle,
    CutStep::Checks,
];

impl CutStep {
    /// Whether a begun step changed something a failed cutover changes
    /// back. The import's live entry is undone by the event plan, the
    /// checks change nothing.
    pub fn changes(self) -> bool {
        !matches!(self, CutStep::Import | CutStep::Checks)
    }

    /// Whether its undo is the guard's own, quick and local (the state
    /// file, the server's config), not the elevated cutover task's (up to
    /// `win::cutover::LIMIT` each, not cancellable). A starting guard undoes
    /// the local ones before its event plan and the elevated ones after it
    /// (S8 lane 5), so a boot after a cut-off cutover is never silent for
    /// them.
    pub fn local(self) -> bool {
        matches!(self, CutStep::Lifecycle | CutStep::PinChanges)
    }
}

/// What a failed cutover changes back, newest first: every begun step that
/// changes something, the one that failed included (it may have changed
/// part of it; every undo is a no-op on what is already as before).
pub fn undo(begun: &[CutStep]) -> Vec<CutStep> {
    begun
        .iter()
        .rev()
        .copied()
        .filter(|s| s.changes())
        .collect()
}

/// Whether `step` may be changed back now that the undos before it left
/// `kept`: the guard's logon trigger stays while the predecessor's
/// autostarts are not back, so the next boot still starts the guard, which
/// tries again.
pub fn may_undo(step: CutStep, kept: &[CutStep]) -> bool {
    !(step == CutStep::GuardLogon && kept.contains(&CutStep::Autostarts))
}

/// A record's begun steps as this guard reads them: a step it does not
/// know (a newer guard's) reads as every step that changes something, so
/// the undo leaves nothing behind (each undo is a no-op where nothing
/// changed).
fn known_begun<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<CutStep>, D::Error> {
    let names = Vec::<serde_json::Value>::deserialize(d)?;
    let steps: Option<Vec<CutStep>> = names
        .into_iter()
        .map(|v| serde_json::from_value(v).ok())
        .collect();
    Ok(steps.unwrap_or_else(|| STEPS.iter().copied().filter(|s| s.changes()).collect()))
}

/// A cutover in progress (`GuardState.cutover`): saved before each step,
/// dropped once it is done or unwound. A guard that starts with one saved
/// unwinds it before anything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub build: String,
    /// When it began, seconds since the epoch: `Prod.since` and the name of
    /// the autostart export ([`export_name`]).
    pub since: u64,
    /// The steps begun, in order: after an unwind, those whose undo
    /// failed (the next start tries them again).
    #[serde(deserialize_with = "known_begun")]
    pub begun: Vec<CutStep>,
}

/// The record in `iemmode status` (in progress, or not fully unwound).
pub fn status(run: Option<&Run>) -> Option<String> {
    run.map(|r| {
        format!(
            "cutover of {} since {}: {:?} begun (in progress, or not fully unwound: a guard \
             restart tries again)",
            r.build, r.since, r.begun
        )
    })
}

/// `activate` is refused while a record is kept: an older guard taking
/// over would drop it, and with it the undo.
pub fn activation_refusal(run: Option<&Run>) -> Option<String> {
    run.map(|r| {
        format!(
            "the cutover of {} is not fully unwound ({:?} left): no activation until a guard \
             restart has unwound it",
            r.build, r.begun
        )
    })
}

/// A failed cutover's reply and alarm: `head` (the step and why), whether
/// the event plan ended done in event, what could not be undone; and
/// whether the alarm is the owner's question (anything left, or event not
/// done).
pub fn unwound(head: &str, event: bool, left: &[String]) -> (String, bool) {
    let mut detail = format!("{head}; unwound to trial");
    detail.push_str(if event {
        " and event"
    } else {
        "; the event plan did not end done"
    });
    if !left.is_empty() {
        detail.push_str(&format!(
            "; not undone: {}; a guard restart tries again",
            left.join("; ")
        ));
    }
    (detail, !(event && left.is_empty()))
}

/// The alarm of a start that found `run` (cut off between two steps, or an
/// unwind that left steps) and what it could not undo; the owner's question
/// when anything is left.
pub fn recovered(run: &Run, left: &[String]) -> (String, bool) {
    let mut detail = format!(
        "the cutover of {} was cut off (begun: {:?}): unwound to trial; the PC goes to event",
        run.build, run.begun
    );
    if !left.is_empty() {
        detail.push_str(&format!(
            "; not undone: {}; a guard restart tries again",
            left.join("; ")
        ));
    }
    (detail, !left.is_empty())
}

/// What the refusals read.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    pub lifecycle: &'a Lifecycle,
    /// A cutover that was cut off and is not fully unwound.
    pub unwinding: Option<&'a Run>,
    pub mode: Mode,
    /// `GuardState::active_bundle`.
    pub active: Option<&'a str>,
    /// The HIL job that runs.
    pub job: Option<u64>,
    /// The build's record, if it is installed.
    pub record: Option<&'a Record>,
}

/// Why the cutover onto `build` may not run (design §3.2), or `None`:
/// only from `Trial`, with no earlier cutover left to unwind, on the active
/// bundle, a green `main` build (G8), with the guard in dev or in a live
/// trial on it (before the cutover a live entry is always a trial) and no
/// HIL job running (the cutover's live entry would stop its runner).
pub fn refusal(build: &str, f: &Facts<'_>) -> Option<String> {
    match f.lifecycle {
        Lifecycle::Trial => {}
        Lifecycle::Prod(p) => {
            return Some(format!(
                "the cutover is done: prod since {} on the pin {}",
                p.since, p.pin
            ));
        }
        Lifecycle::RollingBack => {
            return Some("a rollback to REAPER runs: no cutover until it ends".to_owned());
        }
    }
    if let Some(run) = f.unwinding {
        return Some(format!(
            "the cutover of {} that was cut off is not fully unwound ({:?} left): \
             a guard restart tries again",
            run.build, run.begun
        ));
    }
    let Some(rec) = f.record else {
        return Some(format!("bundle {build} is not installed"));
    };
    if let Err(why) = may_go_live(rec) {
        return Some(why);
    }
    if f.active != Some(build) {
        return Some(format!(
            "{build} is not the active bundle ({}): the cutover runs the build the PC runs",
            f.active.unwrap_or("none")
        ));
    }
    if f.mode == Mode::Event {
        return Some(format!(
            "the guard is in event: the cutover runs from dev or a live trial on {build}"
        ));
    }
    f.job
        .map(|run| format!("HIL job {run} runs: the cutover's live entry would stop its runner"))
}

/// The folder of the autostart export of the cutover begun at `since`
/// (`<elevated root>\cutover\<name>`): the rollback (lane 3) re-enables
/// from it.
pub fn export_name(since: u64) -> String {
    format!("{EXPORT_PREFIX}{since}")
}

/// A name [`export_name`] makes: the prefix and 1 to 20 digits (the
/// elevated task refuses anything else).
pub fn valid_export(name: &str) -> bool {
    name.strip_prefix(EXPORT_PREFIX)
        .is_some_and(|ts| (1..=20).contains(&ts.len()) && ts.bytes().all(|b| b.is_ascii_digit()))
}

/// The cutover task's verbs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// Export the predecessor's autostarts into the named export, then
    /// disable each, read back.
    AutostartsOff,
    /// Re-enable them exactly as the named export saved them (none saved:
    /// nothing to do), read back.
    AutostartsOn,
    /// The guard task's logon trigger on, read back.
    LogonOn,
    /// The guard task's logon trigger off, read back.
    LogonOff,
}

impl Verb {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AutostartsOff => "autostarts-off",
            Self::AutostartsOn => "autostarts-on",
            Self::LogonOn => "logon-on",
            Self::LogonOff => "logon-off",
        }
    }
}

/// The cutover task's request: `{"id", "verb", "export"}` (`export` null for
/// the logon verbs).
pub fn request(id: &str, verb: Verb, export: Option<&str>) -> String {
    json!({"id": id, "verb": verb.as_str(), "export": export}).to_string()
}

fn table(config: &str) -> Result<toml::Table, String> {
    toml::from_str(config).map_err(|e| format!("server config: {e}"))
}

/// The server config's top-level `pin_changes`: `None` when it names none;
/// an unparsable config or a value that is no boolean is an error.
pub fn pins_open(config: &str) -> Result<Option<bool>, String> {
    match table(config)?.get("pin_changes") {
        None => Ok(None),
        Some(toml::Value::Boolean(open)) => Ok(Some(*open)),
        Some(_) => Err("server config: pin_changes is not true or false".to_owned()),
    }
}

/// `line` with the value `from` of its `pin_changes = <from>` replaced by
/// `to`, every other byte kept (the spaces, a comment, the line end);
/// `None` for any other line.
fn edit_line(line: &str, from: &str, to: &str) -> Option<String> {
    let (key, rest) = line.split_once('=')?;
    if key.trim() != "pin_changes" {
        return None;
    }
    let value = rest.trim_start();
    let lead = rest.strip_suffix(value).unwrap_or_default();
    let after = value.strip_prefix(from)?;
    let ends = after.is_empty() || after.starts_with([' ', '\t', '#', '\r', '\n']);
    ends.then(|| format!("{key}={lead}{to}{after}"))
}

/// The server config with its top-level `pin_changes` set to `open`, or
/// `None` when it is so already. Only that value changes: the line
/// `pin_changes = false|true` before the first table, every other byte kept,
/// so the edit and its inverse give the original back byte for byte. Read
/// back: the result parses to the same table but for `pin_changes`.
/// Refused: a config that names no top-level `pin_changes`, or not on one
/// line of that form.
pub fn set_pins(config: &str, open: bool) -> Result<Option<String>, String> {
    let now = pins_open(config)?.ok_or("the server config names no pin_changes")?;
    if now == open {
        return Ok(None);
    }
    let (from, to) = if open {
        ("false", "true")
    } else {
        ("true", "false")
    };
    let mut out = String::new();
    let mut edits = 0;
    let mut top = true;
    for line in config.split_inclusive('\n') {
        top &= !line.trim_start().starts_with('[');
        match edit_line(line, from, to).filter(|_| top) {
            Some(edited) => {
                out.push_str(&edited);
                edits += 1;
            }
            None => out.push_str(line),
        }
    }
    if edits != 1 {
        return Err(format!(
            "the server config's pin_changes is not one `pin_changes = {from}` line before \
             its first table"
        ));
    }
    let mut before = table(config)?;
    let mut after = table(&out)?;
    before.remove("pin_changes");
    let set = after.remove("pin_changes");
    if set != Some(toml::Value::Boolean(open)) {
        return Err(format!(
            "the edited server config reads back with pin_changes {set:?}, not {open}"
        ));
    }
    if before != after {
        return Err("the edit of pin_changes would change more than its value".to_owned());
    }
    Ok(Some(out))
}

/// The post-cutover check of the engine: healthy (callbacks advancing, not
/// faulted, not parked) and at [`FRAMES`] frames per period.
pub fn engine_check(health: Health, seen: Option<&EngineSeen>) -> Result<(), String> {
    if health != Health::Healthy {
        return Err(format!("the engine is {health:?}"));
    }
    let frames = seen.ok_or("the engine has no status")?.status.frames;
    if frames != FRAMES {
        return Err(format!(
            "the engine runs at {frames} frames per period, not {FRAMES}"
        ));
    }
    Ok(())
}

/// `cutover --dry-run`: the steps as they would run.
pub fn plan_text(build: &str, since: u64) -> String {
    format!(
        "Import (a live trial entry on {build}: the final import), GuardLogon (the guard task \
         at logon), Autostarts (exported to {} and disabled), PinChanges (pin_changes = true, \
         the server started again), Lifecycle (prod since {since}, pin {build}), Checks (the \
         band's address, a member page, the engine at {FRAMES})",
        export_name(since)
    )
}

#[cfg(test)]
mod tests;
