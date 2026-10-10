//! The rollback on the daemon thread (S8 lane 3, design note §3.3):
//! `iemmode rollback`, and `iemmode event` as `rollback::on_event` reads it
//! (in prod the engineer's "Back to REAPER" rolls back, the owner's "ide
//! event" never does). `crate::rollback` decides the refusal, the steps,
//! the file moves and the texts; here each step's `Pc` calls and their
//! read-back, the record saved with `RollingBack` before the first step and
//! after each, and a rollback a starting guard continues ([`resume`]).
//! REAPER runs at the end whatever a step did: see `crate::rollback`.

use tracing::info;

use super::cutover::{save_checked, set_pins};
use super::reply::switch_text;
use super::requests::{Entry, dry_event, entry, event_now};
use super::runner::{failure, run_step, run_switch};
use super::{Guard, Outcome};
use crate::cancel::Cancel;
use crate::cutover::export_name;
use crate::lifecycle::{self, Lifecycle};
use crate::pc::Pc;
use crate::plan::{Health, Mode, OnError};
use crate::rollback::{self, OnEvent, Placed, RollStep, Want};

/// `iemmode event [--signal] [--dry-run]`: the event plan before the
/// cutover; in prod the button's rollback, or "ide event"'s live on the pin
/// (from maintenance), a healthy live left as it is, or the event plan
/// (REAPER for this event) when iemmixer does not play.
pub(super) fn event(pc: &mut dyn Pc, g: &mut Guard, dry_run: bool, signal: bool) -> (bool, String) {
    // The pipe pre-empted the token as it routed this request ("ide event"
    // and the button alike): that pre-emption is this request's, whatever it
    // does (a stay, a refusal), never the next switch's.
    if !dry_run {
        g.cancel.clear();
    }
    let lc = g.state.lifecycle.clone();
    let on = rollback::on_event(&lc, g.state.mode, signal, || {
        pc.engine_health() == Ok(Health::Healthy)
    });
    info!("event (signal {signal}) in {lc:?}: {on:?}");
    match on {
        OnEvent::Plan if dry_run => dry_event(pc),
        OnEvent::Plan => event_now(pc, g),
        OnEvent::Rollback => rollback(pc, g, dry_run),
        OnEvent::Stay => (
            true,
            format!(
                "ide event in prod: iemmixer already serves the band live ({}); nothing switched",
                lifecycle::status(&lc).unwrap_or_default()
            ),
        ),
        OnEvent::Live => {
            // A second "ide event" routed from here on waits for this switch
            // to live and never cancels it (S8 lane 5).
            if !dry_run {
                g.shared.claim_live();
            }
            let ask = Entry {
                to: Mode::Live,
                build: None,
                trial: false,
                dry_run,
            };
            let (ok, detail) = entry(pc, g, ask);
            if ok || dry_run || g.state.mode != Mode::Dev {
                return (ok, detail);
            }
            // The pin may not go live (refused before any step): REAPER
            // serves this event.
            let (ok, plan) = event_now(pc, g);
            (ok, format!("{detail}; {plan}"))
        }
    }
}

/// `iemmode rollback [--dry-run]`: refused before the cutover; otherwise
/// the record and `RollingBack` saved and read back (a record that does not
/// save changes nothing), then every step not done yet.
pub(super) fn rollback(pc: &mut dyn Pc, g: &mut Guard, dry_run: bool) -> (bool, String) {
    let run = match rollback::begin(&g.state.lifecycle, g.state.rollback.as_ref(), g.now()) {
        Ok(run) => run,
        Err(why) => return (false, why),
    };
    if dry_run {
        return (
            true,
            format!("dry run: {}", rollback::plan_text(&run, None)),
        );
    }
    if g.state.rollback.is_none() {
        info!("rollback from the pin {} begins", run.pin);
        let before = g.state.lifecycle.clone();
        g.state.rollback = Some(run);
        g.state.lifecycle = Lifecycle::RollingBack;
        if let Err(why) = save_checked(g) {
            g.state.rollback = None;
            g.state.lifecycle = before;
            g.save();
            return (
                false,
                format!("the rollback's record does not save ({why}): nothing changed"),
            );
        }
    }
    proceed(pc, g)
}

/// A starting guard with a rollback left (a crash, a reboot, a step that
/// failed) or `RollingBack` without its record continues it; its stops and
/// its event plan run again (`rollback::resumed`), so the start's event
/// plan is the rollback's. `None`: nothing to continue.
pub(super) fn resume(pc: &mut dyn Pc, g: &mut Guard) -> Option<Outcome> {
    if g.state.rollback.is_none() && g.state.lifecycle != Lifecycle::RollingBack {
        return None;
    }
    g.state.rollback = g.state.rollback.as_ref().map(rollback::resumed);
    g.info("a rollback to REAPER was left: it goes on");
    let (ok, detail) = rollback(pc, g, false);
    info!("{detail}");
    Some(if ok {
        Outcome::Done
    } else {
        Outcome::NeedsOwner
    })
}

/// What a step did.
enum Did {
    Done,
    /// Not done; the rollback goes on, and stays rolling back at its end.
    Left(String),
    /// The rollback stops here (iemmixer may still hold the card).
    Halt(String),
}

/// Every step not done yet, the record saved after each; then trial,
/// saved and read back. `notes`: what fell back (the export, REAPER on
/// the original), the owner's question.
fn proceed(pc: &mut dyn Pc, g: &mut Guard) -> (bool, String) {
    let mut notes: Vec<String> = Vec::new();
    let mut left: Vec<String> = Vec::new();
    let mut failed: Vec<RollStep> = Vec::new();
    // The event plan ended with REAPER but needs the owner (the app does not
    // serve, #10): the rollback ends, not ok.
    let mut owner = false;
    for step in rollback::STEPS {
        let Some(run) = g.state.rollback.clone() else {
            break;
        };
        if run.done.contains(&step) {
            continue;
        }
        info!("rollback step {step:?}");
        let did = match step {
            RollStep::Stop => stop(pc, g),
            RollStep::Export => export(pc, g, run.at, &mut notes),
            RollStep::Project => project(pc, g, &run, &mut notes),
            RollStep::Reaper => reaper(pc, g, &run, &mut notes, &mut owner),
            RollStep::Autostarts => match run.since {
                None => {
                    notes.push(
                        "the cutover's export is not known (the record of prod was lost): the \
                         predecessor's autostarts are not restored and the guard keeps its \
                         logon trigger"
                            .to_owned(),
                    );
                    Did::Done
                }
                Some(since) => match pc.autostarts_on(&export_name(since)) {
                    Ok(did) => {
                        g.info(format!("the predecessor's autostarts: {did}"));
                        Did::Done
                    }
                    Err(e) => Did::Left(e.to_string()),
                },
            },
            RollStep::GuardLogon => {
                if failed.contains(&RollStep::Autostarts) {
                    Did::Left(
                        "kept while the predecessor's autostarts are not back: the guard still \
                         starts at the logon"
                            .to_owned(),
                    )
                } else if run.since.is_none() {
                    Did::Done
                } else {
                    match pc.guard_logon(false) {
                        Ok(()) => Did::Done,
                        Err(e) => Did::Left(e.to_string()),
                    }
                }
            }
            RollStep::PinChanges => match set_pins(pc, false) {
                Ok(()) => Did::Done,
                Err(e) => Did::Left(e),
            },
        };
        match did {
            Did::Done => {
                if let Some(r) = g.state.rollback.as_mut() {
                    r.done.push(step);
                }
            }
            Did::Left(why) => {
                failed.push(step);
                left.push(format!("{step:?}: {why}"));
            }
            Did::Halt(why) => {
                g.save();
                let head = format!("{step:?}: {why}");
                let text = rollback::unfinished(Some(head.as_str()), &[], &notes);
                g.raise(None, &text, true);
                return (false, text);
            }
        }
        g.save();
    }
    let Some(run) = g.state.rollback.clone() else {
        return (false, "the rollback's record is gone".to_owned());
    };
    if !left.is_empty() {
        let text = rollback::unfinished(None, &left, &notes);
        g.raise(None, &text, true);
        return (false, text);
    }
    g.state.lifecycle = Lifecycle::Trial;
    g.state.rollback = None;
    if let Err(why) = save_checked(g) {
        g.state.lifecycle = Lifecycle::RollingBack;
        g.state.rollback = Some(run);
        g.save();
        let end = [format!("its end does not save ({why})")];
        let text = rollback::unfinished(None, &end, &notes);
        g.raise(None, &text, true);
        return (false, text);
    }
    let text = rollback::ended(&run, &notes);
    info!("{text}");
    if !notes.is_empty() {
        // REAPER runs, but not as asked: the owner hears why.
        g.raise(None, &text, true);
    }
    (!owner, text)
}

/// iemmixer stopped by the event plan's stops and error policy (a step
/// that fails alarms and the next goes on; a healthy engine that does not
/// stop keeps serving and a dead or parked one may hold the card: both stop
/// the rollback, no REAPER, and a dead one leaves the mode event as the
/// event plan does, so the watch starts no engine). The stops run on a
/// token of their own and finish as the event plan's do (the pipe never
/// pre-empts those): an "ide event" routed meanwhile waits behind the
/// rollback, whose end is REAPER anyway.
fn stop(pc: &mut dyn Pc, g: &mut Guard) -> Did {
    let routed = std::mem::take(&mut g.cancel);
    let did = stops(pc, g);
    g.cancel = routed;
    did
}

fn stops(pc: &mut dyn Pc, g: &mut Guard) -> Did {
    for step in rollback::stops(&pc.facts()) {
        let Err(e) = run_step(pc, g, step, Mode::Event) else {
            continue;
        };
        let (why, health, policy) = failure(pc, g, Mode::Event, step, &e);
        match policy {
            OnError::KeepServing => {
                return Did::Halt(format!(
                    "{step:?}: {why}; the engine is healthy and iemmixer keeps serving"
                ));
            }
            OnError::StopAskOwner => {
                g.state.mode = Mode::Event;
                return Did::Halt(format!(
                    "{step:?}: {why}; health {health:?}: no REAPER while the card may be held"
                ));
            }
            _ => g.alarm(step, &format!("rollback: {why}"), false),
        }
    }
    g.state.pids = pc.children();
    Did::Done
}

/// The band's data into a new project; a failed export leaves the
/// original (noted: iemmixer's changes are not carried back).
fn export(pc: &mut dyn Pc, g: &mut Guard, at: u64, notes: &mut Vec<String>) -> Did {
    match pc.export_project(at) {
        Ok(said) => {
            g.info(said);
            if let Some(r) = g.state.rollback.as_mut() {
                r.exported = true;
            }
        }
        Err(e) => notes.push(format!(
            "the band's data was not exported ({e}): iemmixer's changes are not carried back"
        )),
    }
    Did::Done
}

/// The export in the project's place while REAPER is down; a swap that
/// fails puts the original back (noted). A REAPER that runs (the unwind of
/// a dev or live entry the button cancelled started it on the original,
/// S8 lane 5) is saved and quit gracefully first, as the `Reaper` step's
/// fallback does; one that does not quit keeps running on the original
/// (noted), and the `Reaper` step's event plan finds it there.
fn project(pc: &mut dyn Pc, g: &mut Guard, run: &rollback::Run, notes: &mut Vec<String>) -> Did {
    if !run.exported {
        return Did::Done;
    }
    if pc.facts().reaper {
        // Not pre-empted: REAPER must be quit before the files move.
        let quit = pc.reaper_save_quit(&Cancel::default());
        if let Err(e) = quit {
            notes.push(format!(
                "REAPER runs and could not be quit ({e}): the export does not take the project's \
                 place"
            ));
            return Did::Done;
        }
        g.info("REAPER ran on the original project: saved and quit before the swap");
        if pc.facts().reaper {
            notes.push(
                "REAPER still runs after its quit: the export does not take the project's place"
                    .to_owned(),
            );
            return Did::Done;
        }
    }
    let placed = match pc.swap_project(Want::Export, run.at) {
        Ok(p) => Ok(p),
        Err(e) => {
            notes.push(format!(
                "the export could not take the project's place ({e})"
            ));
            pc.swap_project(Want::Original, run.at)
        }
    };
    let on_export = match placed {
        Ok(p) => p == Placed::Export,
        Err(e) => {
            notes.push(format!("the project files could not be settled ({e})"));
            false
        }
    };
    if let Some(r) = g.state.rollback.as_mut() {
        r.on_export = on_export;
    }
    Did::Done
}

/// The event plan: REAPER, the app, the handover checks. Before it, when
/// the export did not take the project's place, the original is settled in
/// its place once more (a no-op when it is there; a swap that failed both
/// ways may have left the path empty). A REAPER that does not run on what
/// the path holds (it could not open the export) is saved and quit when it
/// runs, the original goes back and the event plan runs again: REAPER is
/// started whatever the files say. A plan that ends with REAPER but needs
/// the owner (`owner`) ends the rollback not ok.
fn reaper(
    pc: &mut dyn Pc,
    g: &mut Guard,
    run: &rollback::Run,
    notes: &mut Vec<String>,
    owner: &mut bool,
) -> Did {
    if run.exported
        && !run.on_export
        && let Err(e) = pc.swap_project(Want::Original, run.at)
    {
        notes.push(format!(
            "the original project could not be settled in its place before REAPER's start ({e})"
        ));
    }
    let (runs, done, said) = event_plan(pc, g);
    if runs {
        return with_reaper(g, done, said, notes, owner);
    }
    if !run.exported {
        return Did::Left(format!("REAPER does not run: {said}"));
    }
    if pc.facts().reaper {
        // Not pre-empted: REAPER must be quit before the files move.
        if let Err(e) = pc.reaper_save_quit(&Cancel::default()) {
            return Did::Left(format!(
                "REAPER did not open the project ({said}) and could not be quit ({e})"
            ));
        }
    }
    let what = if run.on_export {
        "REAPER could not open the export"
    } else {
        "REAPER did not run on what a failed swap left in the project's place"
    };
    match pc.swap_project(Want::Original, run.at) {
        Ok(_) => {
            if let Some(r) = g.state.rollback.as_mut() {
                r.on_export = false;
            }
            notes.push(format!(
                "{what} ({said}): the original project is back in its place, the export kept \
                 under its own name (REAPER's save on its quit may have written over it; the \
                 engine's state can be exported again)"
            ));
        }
        Err(e) => notes.push(format!(
            "{what} ({said}) and the original could not be put back ({e})"
        )),
    }
    g.save();
    let (runs, done, again) = event_plan(pc, g);
    if runs {
        with_reaper(g, done, again, notes, owner)
    } else {
        Did::Left(format!(
            "REAPER does not run on the original either: {again}"
        ))
    }
}

/// The event plan from the mode the PC is in: whether REAPER runs after it
/// (`rollback::reaper_runs`), whether it ended done in event, and its text.
fn event_plan(pc: &mut dyn Pc, g: &mut Guard) -> (bool, bool, String) {
    let from = g.state.mode;
    let out = run_switch(pc, g, from, Mode::Event);
    let said = switch_text(Mode::Event, out, g.state.mode, &g.owner_failed);
    let done = out == Outcome::Done && g.state.mode == Mode::Event;
    (
        rollback::reaper_runs(&pc.facts(), &g.owner_failed),
        done,
        said,
    )
}

/// REAPER runs: the step is done; an event plan that needs the owner (the
/// app, #10) is noted and ends the rollback not ok.
fn with_reaper(
    g: &mut Guard,
    done: bool,
    said: String,
    notes: &mut Vec<String>,
    owner: &mut bool,
) -> Did {
    if !done {
        *owner = true;
        notes.push(format!("the event plan needs the owner: {said}"));
    }
    g.info(format!("the event plan: {said}"));
    Did::Done
}
