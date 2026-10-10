//! The cutover on the daemon thread (S8 design note §3.2; lane 2): the
//! owner's message (`iemmode cutover --build SHA`, which `iempc cutover`
//! wraps) turns the PC from trial into prod. `crate::cutover` decides the
//! refusals, the steps, what a failed one changes back and what the reply
//! says; here each step's `Pc` calls and its read-back, the record saved and
//! read back before every step (`GuardState.cutover`: no step runs that a
//! restart could not find), and the unwind to trial and event when a step
//! fails or "ide event" pre-empts it. A guard that starts with a record
//! saved (a crash or a power loss between two steps) unwinds it first
//! ([`recover`]).

use tracing::info;

use super::reply::switch_text;
use super::runner::run_switch;
use super::{Guard, Outcome, STATE_FILE};
use crate::cutover::{self, CutStep, Run};
use crate::lifecycle::{self, Lifecycle, Prod};
use crate::pc::{Pc, StepError};
use crate::plan::Mode;
use crate::state::GuardState;

fn text(e: StepError) -> String {
    e.to_string()
}

/// `iemmode cutover --build SHA [--dry-run]`: refused unless
/// `cutover::refusal` passes; then every step in order, the record saved
/// and read back before each (a record that does not save refuses the
/// cutover before anything changes, or unwinds it). A failed or pre-empted
/// step unwinds ([`unwind`]).
pub(super) fn cutover(
    pc: &mut dyn Pc,
    g: &mut Guard,
    build: &str,
    dry_run: bool,
) -> (bool, String) {
    let facts = cutover::Facts {
        lifecycle: &g.state.lifecycle,
        unwinding: g.state.cutover.as_ref(),
        mode: g.state.mode,
        active: g.state.active_bundle(),
        job: g.state.job,
        record: g.state.bundles.get(build),
    };
    if let Some(why) = cutover::refusal(build, &facts) {
        return (false, why);
    }
    let since = g.now();
    if dry_run {
        let check = pc.precheck(Mode::Live, true);
        let verdict = match &check {
            Ok(None) => "ok".to_owned(),
            Ok(Some(note)) => format!("ok; {note}"),
            Err(why) => why.to_string(),
        };
        return (
            check.is_ok(),
            format!(
                "dry run: cutover of {build}: {}; precheck {verdict}",
                cutover::plan_text(build, since)
            ),
        );
    }
    info!("cutover of {build} begins");
    g.state.cutover = Some(Run {
        build: build.to_owned(),
        since,
        begun: Vec::new(),
    });
    if let Err(why) = save_checked(g) {
        g.state.cutover = None;
        g.save();
        return (
            false,
            format!("the cutover's record does not save ({why}): nothing changed"),
        );
    }
    for step in cutover::STEPS {
        if g.cancel.preempted() {
            return unwind(pc, g, step, "pre-empted by event");
        }
        if let Some(run) = g.state.cutover.as_mut() {
            run.begun.push(step);
        }
        if let Err(why) = save_checked(g) {
            return unwind(pc, g, step, &format!("its record does not save ({why})"));
        }
        info!("cutover step {step:?}");
        if let Err(why) = run_step(pc, g, step, build, since) {
            return unwind(pc, g, step, &why);
        }
    }
    let run = g.state.cutover.take();
    if let Err(why) = save_checked(g) {
        g.state.cutover = run;
        return unwind(
            pc,
            g,
            CutStep::Checks,
            &format!("the end of the record does not save ({why})"),
        );
    }
    let done = format!(
        "cutover done: prod since {since} on the pin {build}; the predecessor's autostarts \
         are saved in {}",
        cutover::export_name(since)
    );
    info!("{done}");
    (true, done)
}

/// One step, read back.
fn run_step(
    pc: &mut dyn Pc,
    g: &mut Guard,
    step: CutStep,
    build: &str,
    since: u64,
) -> Result<(), String> {
    match step {
        CutStep::Import => import(pc, g, build),
        CutStep::GuardLogon => pc.guard_logon(true).map_err(text),
        CutStep::Autostarts => {
            let did = pc
                .autostarts_off(&cutover::export_name(since))
                .map_err(text)?;
            g.info(format!("the predecessor's autostarts: {did}"));
            Ok(())
        }
        CutStep::PinChanges => {
            set_pins(pc, true)?;
            restart_server(pc, g)
        }
        CutStep::Lifecycle => {
            g.state.lifecycle = Lifecycle::Prod(Prod {
                since,
                pin: build.to_owned(),
                previous: None,
                maintenance: None,
            });
            save_checked(g)
        }
        CutStep::Checks => {
            let c = g.cancel.clone();
            if let Some(note) = pc.identity(build, &c).map_err(text)? {
                g.info(note);
            }
            pc.member_page().map_err(text)?;
            let health = pc.engine_health().map_err(text)?;
            cutover::engine_check(health, pc.engine_seen().as_ref())
        }
    }
}

/// The final import: a live trial entry on the build, by the entry's own
/// gate (`lifecycle::entry`) and plan (its data refresh imports the saved
/// project as `data_live`); the build is the active bundle.
fn import(pc: &mut dyn Pc, g: &mut Guard, build: &str) -> Result<(), String> {
    let ask = lifecycle::Ask {
        to: Mode::Live,
        build: Some(build),
        trial: true,
    };
    lifecycle::entry(&Lifecycle::Trial, ask, |sha| g.state.bundles.get(sha))?;
    pc.set_bundle(Some(build));
    g.state.set_active(build);
    g.trial = true;
    let from = g.state.mode;
    let out = run_switch(pc, g, from, Mode::Live);
    if out == Outcome::Done && g.state.mode == Mode::Live {
        Ok(())
    } else {
        Err(format!(
            "the live entry: {}",
            switch_text(Mode::Live, out, g.state.mode, &g.owner_failed)
        ))
    }
}

/// The server config's `pin_changes` set to `open` (`cutover::set_pins`:
/// that value only, so closing gives the bytes back), read back.
pub(super) fn set_pins(pc: &mut dyn Pc, open: bool) -> Result<(), String> {
    let config = pc.server_config().map_err(text)?;
    if let Some(edited) = cutover::set_pins(&config, open)? {
        pc.write_server_config(&edited).map_err(text)?;
    }
    let back = pc.server_config().map_err(text)?;
    match cutover::pins_open(&back)? {
        Some(now) if now == open => Ok(()),
        now => Err(format!(
            "the server config's pin_changes reads back as {now:?}, not {open}"
        )),
    }
}

/// The server started again with the config just written (its PIN rule
/// as in prod), so PIN changes reach the band.
fn restart_server(pc: &mut dyn Pc, g: &mut Guard) -> Result<(), String> {
    let c = g.cancel.clone();
    if pc.facts().server {
        pc.server_stop(&c).map_err(text)?;
    }
    let pid = pc.server_start(Mode::Live, true).map_err(text)?;
    g.state.pids = pc.children();
    g.save();
    g.info(format!(
        "the server started again with pin_changes = true (pid {pid})"
    ));
    Ok(())
}

/// Saves the state and reads it back from the state file (the guard's in
/// memory when it has no files): the cutover's and the rollback's records
/// and the lifecycle must be what the guard holds.
pub(super) fn save_checked(g: &mut Guard) -> Result<(), String> {
    g.save();
    let saved = match g.root() {
        Some(root) => {
            let (st, bad) = GuardState::load(&root.join("guard").join(STATE_FILE));
            if let Some(why) = bad {
                return Err(why);
            }
            st
        }
        None => g.state.clone(),
    };
    if saved.cutover != g.state.cutover || saved.lifecycle != g.state.lifecycle {
        return Err(format!(
            "the guard state reads back with the record {:?} and the lifecycle {:?}",
            saved.cutover, saved.lifecycle
        ));
    }
    if saved.rollback != g.state.rollback {
        return Err(format!(
            "the guard state reads back with the rollback record {:?}",
            saved.rollback
        ));
    }
    Ok(())
}

/// A failed or pre-empted step: the event plan first (the band back on
/// REAPER; after a failed import the entry's own unwind already ran it),
/// then every begun step that changed something changed back
/// ([`undo`]), the step that failed included; the reply and the alarm
/// (`cutover::unwound`) name the step, why, and anything left.
fn unwind(pc: &mut dyn Pc, g: &mut Guard, step: CutStep, why: &str) -> (bool, String) {
    let Some(run) = g.state.cutover.clone() else {
        return (false, why.to_owned());
    };
    let head = format!("cutover of {} failed at {step:?}: {why}", run.build);
    info!("{head}");
    // "ide event" that came meanwhile runs after this request (it queued):
    // the plan here is the unwind's own.
    g.cancel.clear();
    let from = g.state.mode;
    let event = from == Mode::Event || {
        let out = run_switch(pc, g, from, Mode::Event);
        out == Outcome::Done && g.state.mode == Mode::Event
    };
    let left = undo(pc, g, &run);
    let (detail, ask) = cutover::unwound(&head, event, &left);
    g.raise(None, &detail, ask);
    (false, detail)
}

/// Changes back what the begun steps of `run` changed, newest first
/// (`cutover::undo`): the lifecycle to trial (read back), `pin_changes`
/// closed, the autostarts back from their export, then the guard's logon
/// trigger off, unless the autostarts are not back (`cutover::may_undo`:
/// the next boot must start something). Every undo is a no-op on what is
/// already as before, so a step that failed half-way is undone too. The
/// record is dropped, or kept with the steps left (the next start tries
/// them again). What is left, by step.
fn undo(pc: &mut dyn Pc, g: &mut Guard, run: &Run) -> Vec<String> {
    let mut left = Vec::new();
    let mut kept = Vec::new();
    for step in cutover::undo(&run.begun) {
        if !cutover::may_undo(step, &kept) {
            left.push(format!(
                "{step:?}: kept while the autostarts are not back (the guard starts at the \
                 logon and tries again)"
            ));
            kept.push(step);
            continue;
        }
        let done = match step {
            CutStep::Lifecycle => {
                g.state.lifecycle = Lifecycle::Trial;
                save_checked(g)
            }
            CutStep::PinChanges => set_pins(pc, false),
            CutStep::GuardLogon => pc.guard_logon(false).map_err(text),
            CutStep::Autostarts => pc
                .autostarts_on(&cutover::export_name(run.since))
                .map(|did| g.info(format!("the predecessor's autostarts: {did}")))
                .map_err(text),
            CutStep::Import | CutStep::Checks => Ok(()),
        };
        if let Err(e) = done {
            left.push(format!("{step:?}: {e}"));
            kept.push(step);
        }
    }
    kept.reverse();
    g.state.cutover = (!kept.is_empty()).then(|| Run {
        begun: kept,
        ..run.clone()
    });
    g.save();
    left
}

/// A starting guard that finds a cutover record (cut off between two
/// steps, or an unwind that left steps) changes back what it begun, before
/// anything else, and alarms (`cutover::recovered`); the start then goes
/// to event. Whether it found one.
pub(super) fn recover(pc: &mut dyn Pc, g: &mut Guard) -> bool {
    let Some(run) = g.state.cutover.clone() else {
        return false;
    };
    let left = undo(pc, g, &run);
    let (detail, ask) = cutover::recovered(&run, &left);
    g.raise(None, &detail, ask);
    true
}
