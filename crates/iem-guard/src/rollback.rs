//! The rollback to REAPER (S8 design note
//! `docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md` §3.3;
//! program spec §4.3): `iemmode rollback`, the engineer's "Back to REAPER"
//! in prod (`iemmode event`, the guard decides) and `iempc rollback` turn
//! the PC from `Prod` back into `Trial` and `event`, REAPER on the band's
//! data as iemmixer left it.
//!
//! Pure: the refusal, the steps in their order, the record a restarted
//! guard continues from, the names of the export and of the kept original,
//! the moves that put the export in the project's place (or the original
//! back), the stops before the export, whether REAPER runs, what an "ide
//! event" or the button means in each lifecycle, and the texts. The daemon
//! (`daemon::rollback`) runs each step through `Pc`, reads it back and saves
//! the record after it.
//!
//! **REAPER at the end, whatever happens.** The record and `RollingBack`
//! are saved before anything changes; iemmixer stops by the event plan's
//! own rules (a healthy engine that does not stop keeps serving, a dead or
//! parked one stops the rollback: no REAPER while the card may be held);
//! a failed export or swap leaves the original in the project's place; a
//! REAPER that cannot open the export is quit and started on the original
//! (an alarm either way); the predecessor's autostarts come back before
//! the guard's logon trigger goes (at every moment the next boot starts the
//! predecessor or the guard, which continues the rollback). Anything left
//! keeps `RollingBack` and the record: a guard restart or another
//! `iemmode rollback` continues it, its event plan first.

use serde::{Deserialize, Deserializer, Serialize};

use crate::lifecycle::Lifecycle;
use crate::plan::{self, Facts, Mode, Step};

/// The rollback's steps once its record is saved (lifecycle `RollingBack`).
/// REAPER comes back before the persistent changes (the autostarts, the
/// guard's logon trigger, `pin_changes`), so the in-ears are silent only for
/// the stops, the export and the event plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollStep {
    /// iemmixer stopped by the event plan's stops (`stops`), with its
    /// error policy: the engine saves its state as it ends.
    Stop,
    /// The band's data from the engine's saved state into a new project
    /// (`iem-migrate export`, self-checked; `export_path`).
    Export,
    /// The export in the project's place, the original kept beside it
    /// (`kept_path`): every start of REAPER, the guard's save check, the
    /// trial's import and the predecessor's autostarts name the project's
    /// path, so they all meet the band's data. Skipped without an export.
    Project,
    /// The event plan: REAPER, the predecessor app, the handover checks;
    /// a REAPER that cannot open the export is quit and started on the
    /// original.
    Reaper,
    /// The predecessor's autostarts back from the cutover's export.
    Autostarts,
    /// The guard task's logon trigger off, only once the autostarts are
    /// back.
    GuardLogon,
    /// `pin_changes = false` in the server's config (before the cutover
    /// the server refuses `true`).
    PinChanges,
}

/// The steps in their order.
pub const STEPS: [RollStep; 7] = [
    RollStep::Stop,
    RollStep::Export,
    RollStep::Project,
    RollStep::Reaper,
    RollStep::Autostarts,
    RollStep::GuardLogon,
    RollStep::PinChanges,
];

/// What a reply says when REAPER runs on the export (the drill reads it).
pub const ON_EXPORT: &str = "REAPER runs on the export";
/// What a reply says when REAPER runs on the original project.
pub const ON_ORIGINAL: &str = "REAPER runs on the original project";

/// The steps done as this guard reads them: one it does not know (a newer
/// guard's) is left out, so it runs again (each step is safe to repeat).
fn known_done<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<RollStep>, D::Error> {
    Ok(Vec::<serde_json::Value>::deserialize(d)?
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}

/// A rollback in progress (`GuardState.rollback`): saved with
/// `RollingBack` before any step and after each, dropped when it ends in
/// `Trial`. A starting guard that finds one continues it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// The pin prod ran.
    pub pin: String,
    /// The cutover's `Prod.since`: the autostart export's name
    /// (`cutover::export_name`); none when the record of prod was lost.
    pub since: Option<u64>,
    /// When the rollback began, seconds since the epoch: the names of the
    /// band data's export and of the kept original.
    pub at: u64,
    /// The steps done, in order.
    #[serde(default, deserialize_with = "known_done")]
    pub done: Vec<RollStep>,
    /// The export was written and checked.
    #[serde(default)]
    pub exported: bool,
    /// The project's path holds the export (the original is kept).
    #[serde(default)]
    pub on_export: bool,
}

/// The rollback to run, or why none may run: the record in progress, or a
/// new one from prod. Before the cutover there is nothing to roll back. A
/// `RollingBack` whose record is lost (an older guard dropped it) rolls
/// back again without the cutover's export (its autostarts are not known).
pub fn begin(lc: &Lifecycle, run: Option<&Run>, now: u64) -> Result<Run, String> {
    if let Some(run) = run {
        return Ok(run.clone());
    }
    let (pin, since) = match lc {
        Lifecycle::Trial => {
            return Err(
                "there is no cutover to roll back: before the cutover `iemmode event` goes back \
                 to REAPER"
                    .to_owned(),
            );
        }
        Lifecycle::Prod(p) => (p.pin.clone(), Some(p.since)),
        Lifecycle::RollingBack => ("unknown".to_owned(), None),
    };
    Ok(Run {
        pin,
        since,
        at: now,
        done: Vec::new(),
        exported: false,
        on_export: false,
    })
}

/// The rollback a starting guard continues: its event plan runs again
/// whatever the record says (the boot may have stopped REAPER, and
/// nothing else starts it while the autostarts are not back), and so do
/// the stops (a guard restart may have left iemmixer running).
pub fn resumed(run: &Run) -> Run {
    let mut next = run.clone();
    next.done
        .retain(|s| !matches!(s, RollStep::Stop | RollStep::Reaper));
    next
}

/// `project` with `.<tag>-<at>` before its extension, in its folder (either
/// separator: the PC's paths are Windows paths, the tests run anywhere).
fn sibling(project: &str, tag: &str, at: u64) -> String {
    let cut = project.rfind(['\\', '/']).map_or(0, |i| i + 1);
    let (dir, name) = project.split_at(cut);
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };
    format!("{dir}{stem}.{tag}-{at}{ext}")
}

/// The band data's export (decided for lane 3, recorded on #11): a new
/// file in the project's own folder, `<stem>.rollback-<at>.<ext>`. Not in
/// the elevated root: the guard runs as the user and may not write there,
/// and REAPER, which saves into the project it runs, may not either.
pub fn export_path(project: &str, at: u64) -> String {
    sibling(project, "rollback", at)
}

/// Where the original project is kept once the export takes its place:
/// `<stem>.before-rollback-<at>.<ext>` beside it, never overwritten.
pub fn kept_path(project: &str, at: u64) -> String {
    sibling(project, "before-rollback", at)
}

/// Which of the three files exist.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Files {
    pub project: bool,
    pub export: bool,
    pub kept: bool,
}

/// What the project's path should hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Want {
    /// The export, the original kept.
    Export,
    /// The original, the export under its own name again.
    Original,
}

/// What the project's path holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placed {
    Export,
    Original,
}

/// One rename; its target never exists (nothing is ever overwritten).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    ProjectToKept,
    ExportToProject,
    ProjectToExport,
    KeptToProject,
}

impl Move {
    /// `(from, to)` among the project, the export and the kept original.
    pub fn paths<'a>(self, project: &'a str, export: &'a str, kept: &'a str) -> (&'a str, &'a str) {
        match self {
            Self::ProjectToKept => (project, kept),
            Self::ExportToProject => (export, project),
            Self::ProjectToExport => (project, export),
            Self::KeptToProject => (kept, project),
        }
    }

    /// The files after it.
    pub fn apply(self, f: Files) -> Files {
        match self {
            Self::ProjectToKept => Files {
                project: false,
                kept: true,
                ..f
            },
            Self::ExportToProject => Files {
                project: true,
                export: false,
                ..f
            },
            Self::ProjectToExport => Files {
                project: false,
                export: true,
                ..f
            },
            Self::KeptToProject => Files {
                project: true,
                kept: false,
                ..f
            },
        }
    }
}

/// The renames from `f` to what `want` asks, each target absent at its
/// turn. The kept original tells which file the project's path holds: with
/// it the path holds the export, without it the original. A swap cut off
/// between its two renames (the path empty) is finished either way. A
/// state no rename can settle is refused, nothing moved.
pub fn moves(want: Want, f: Files) -> Result<Vec<Move>, String> {
    let at = |f: Files| (f.project, f.export, f.kept);
    let plan = match (want, at(f)) {
        (Want::Export, (true, true, false)) => vec![Move::ProjectToKept, Move::ExportToProject],
        (Want::Export, (false, true, true)) => vec![Move::ExportToProject],
        (Want::Export, (true, false, true)) | (Want::Original, (true, _, false)) => Vec::new(),
        (Want::Original, (true, false, true)) => {
            vec![Move::ProjectToExport, Move::KeptToProject]
        }
        (Want::Original, (false, _, true)) => vec![Move::KeptToProject],
        _ => {
            return Err(format!(
                "the project files cannot be settled (project {}, export {}, kept original {})",
                f.project, f.export, f.kept
            ));
        }
    };
    Ok(plan)
}

/// What the project's path holds when `f` is settled for `want`; `None`
/// when it is not.
pub fn placed(want: Want, f: Files) -> Option<Placed> {
    match (want, at_rest(f)) {
        (Want::Export, Some(Placed::Export)) => Some(Placed::Export),
        (Want::Original, Some(Placed::Original)) => Some(Placed::Original),
        _ => None,
    }
}

/// What the path holds when no rename is half done: the export (the
/// original kept, no export left under its own name) or the original (no
/// kept original).
fn at_rest(f: Files) -> Option<Placed> {
    match (f.project, f.export, f.kept) {
        (true, false, true) => Some(Placed::Export),
        (true, _, false) => Some(Placed::Original),
        _ => None,
    }
}

/// iemmixer's stops, as the event plan has them before it turns to REAPER
/// (jobs and runner, engine, server, tray): the engine's stop saves its
/// state, which the export then reads.
pub fn stops(f: &Facts) -> Vec<Step> {
    plan::plan(Mode::Event, f)
        .into_iter()
        .take_while(|s| *s != Step::TuningExit)
        .collect()
}

/// Whether REAPER runs after an event plan: it runs and holds the card, and
/// the plan's REAPER handover did not ask the owner (`owner_failed`, the
/// runner's "<Step> failed: <why>").
pub fn reaper_runs(f: &Facts, owner_failed: &[String]) -> bool {
    let handover = format!("{:?} failed", Step::ReaperHandover);
    f.reaper && f.reaper_holds_module && !owner_failed.iter().any(|s| s.starts_with(&handover))
}

/// What `iemmode event` does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnEvent {
    /// The event plan, as before the cutover.
    Plan,
    /// The rollback (or the one in progress, continued).
    Rollback,
    /// Prod's band system: live on the pin (a maintenance session ends).
    Live,
    /// Prod live with a healthy engine: iemmixer already serves the band.
    Stay,
}

/// Whether a plain `iemmode event` (the engineer's button) is a rollback:
/// in prod and while rolling back. The pipe's routing reads it from the
/// view (`View::rolls_back`): such a request is queued even while a switch
/// runs, never answered as the end of that switch.
pub fn button_rolls_back(lc: &Lifecycle) -> bool {
    matches!(lc, Lifecycle::Prod(_) | Lifecycle::RollingBack)
}

/// What `iemmode event` means (S8 lane 3). `signal`: the owner's "ide
/// event" (`iempc event` sends `--signal`); without it the request is the
/// engineer's "Back to REAPER" (`POST /api/mode/event`, the site's
/// `back_to_reaper`) or a person's `iemmode event`. Before the cutover both
/// are the event plan. In prod the button is the rollback (program spec
/// §4.3), and "ide event" never is: after the cutover iemmixer serves the
/// band at an event. So "ide event" in prod ends a maintenance session
/// (dev) with live on the pin, leaves a healthy live as it is, and keeps
/// whatever plays when iemmixer does not: an engine that does not play (a
/// crash loop left it down) gets the event plan, REAPER for this event,
/// the PC still in prod, and a PC already in event (a red pin at the boot,
/// a failed or pre-empted entry) keeps REAPER, its checks only: an event
/// never waits on a switch that could fail. While rolling back both
/// continue the rollback, which ends with REAPER. `healthy` reads the
/// engine (two statuses about a second apart), only for prod live.
pub fn on_event(
    lc: &Lifecycle,
    mode: Mode,
    signal: bool,
    healthy: impl FnOnce() -> bool,
) -> OnEvent {
    match lc {
        Lifecycle::Trial => OnEvent::Plan,
        Lifecycle::RollingBack => OnEvent::Rollback,
        Lifecycle::Prod(_) if !signal => OnEvent::Rollback,
        Lifecycle::Prod(_) => match mode {
            Mode::Dev => OnEvent::Live,
            Mode::Event => OnEvent::Plan,
            Mode::Live => {
                if healthy() {
                    OnEvent::Stay
                } else {
                    OnEvent::Plan
                }
            }
        },
    }
}

/// The record in `iemmode status` while a rollback runs or is left.
pub fn status(run: Option<&Run>) -> Option<String> {
    run.map(|r| {
        format!(
            "rollback since {} from the pin {}: {:?} done (in progress, or left: a guard \
             restart or iemmode rollback continues it)",
            r.at, r.pin, r.done
        )
    })
}

/// `activate` is refused while a rollback record is kept: an older guard
/// taking over would drop it, and with it the rest of the rollback.
pub fn activation_refusal(run: Option<&Run>) -> Option<String> {
    run.map(|r| {
        format!(
            "the rollback from the pin {} is not finished ({:?} done): no activation until it \
             ends",
            r.pin, r.done
        )
    })
}

/// `rollback --dry-run`: the steps as they would run.
pub fn plan_text(run: &Run, project: Option<&str>) -> String {
    let export = project.map_or_else(
        || "<project>.rollback-<at>".to_owned(),
        |p| export_path(p, run.at),
    );
    let autostarts = run.since.map_or_else(
        || "unknown (the record of prod was lost): not restored".to_owned(),
        |s| format!("back from {}", crate::cutover::export_name(s)),
    );
    format!(
        "rollback from the pin {pin}: RollingBack saved; Stop (iemmixer, the event plan's stops); \
         Export (the band's data to {export}); Project (the export in the project's place, the \
         original kept); Reaper (the event plan: REAPER, the app, the handover checks; the \
         original if REAPER cannot open the export); Autostarts ({autostarts}); GuardLogon (off \
         once the autostarts are back); PinChanges (false); then trial and event",
        pin = run.pin
    )
}

/// The reply of a rollback that ended in trial: REAPER on the export or on
/// the original, and what it noted.
pub fn ended(run: &Run, notes: &[String]) -> String {
    let mut text = if run.on_export {
        format!(
            "rollback done: trial, event; {ON_EXPORT}; the original project is kept as \
             before-rollback-{}",
            run.at
        )
    } else {
        format!("rollback done: trial, event; {ON_ORIGINAL}")
    };
    for n in notes {
        text.push_str("; ");
        text.push_str(n);
    }
    text
}

/// The reply and the alarm of a rollback that stopped (`halt`) or left
/// steps (`left`): a guard restart or `iemmode rollback` continues it.
pub fn unfinished(halt: Option<&str>, left: &[String], notes: &[String]) -> String {
    let mut text = match halt {
        Some(why) => format!("rollback stopped: {why}"),
        None => format!("rollback not finished: {}", left.join("; ")),
    };
    for n in notes {
        text.push_str("; ");
        text.push_str(n);
    }
    text.push_str("; the PC stays rolling back: a guard restart or iemmode rollback continues it");
    text
}

#[cfg(test)]
mod tests;
