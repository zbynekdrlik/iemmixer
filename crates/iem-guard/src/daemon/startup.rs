//! A starting guard (design §5.2): its job, the reboot rule, the children
//! a previous guard started, the logon task's result and the start's
//! checks; and `iemmode event --direct`, the event plan without a guard.

use tracing::{info, warn};

use super::requests::{dry_event, event_now};
use super::runner::switch;
use super::{Guard, Outcome, send_notices};
use crate::lifecycle::{self, Start, Started};
use crate::pc::{Pc, job_note};
use crate::plan::Mode;
use crate::proto::Reply;
use crate::state;

/// What the elevated logon task (G1) left, taken once per run of it (its
/// `at`, `GuardState::logon_seen`), at the guard's start and hourly (#9
/// 2026-09-28). The task follows `PrefCheck`'s rule, so a preference it did
/// not write under a holder of the driver module is remembered and alarmed
/// once with `PrefCheck`'s text (the event plan's check that finds the same
/// adds no alarm); a run at REAPER's original drops what was remembered; a
/// failed run is logged (the next plan's `PrefCheck` reads the preference).
pub(super) fn take_logon(pc: &mut dyn Pc, g: &mut Guard) {
    let Some(logon) = pc.logon() else {
        return;
    };
    if g.state.logon_seen.as_deref() == Some(logon.at.as_str()) {
        return;
    }
    info!("the logon task's run of {}: {:?}", logon.at, logon.pref);
    g.state.logon_seen = Some(logon.at);
    match logon.pref {
        crate::effects::tuning::LogonPref::Original => g.state.pref_held = None,
        crate::effects::tuning::LogonPref::Held(held) => {
            let text = held.text();
            if g.state.pref_held.as_deref() != Some(text.as_str()) {
                g.state.pref_held = Some(text.clone());
                g.raise(None, &format!("logon task: {text}"), false);
            }
        }
        crate::effects::tuning::LogonPref::Failed(why) => {
            warn!("the logon task did not restore the preference: {why}");
        }
    }
    g.save();
}

/// A starting guard (design §5.2): the job its children start in (logged,
/// and named in the status while they stay in it, §5.1), the reboot rule
/// as the lifecycle applies it (S8: `lifecycle::start`; before the cutover
/// event, in prod a reboot goes live on the pin), then the children a
/// previous guard started, then an unfinished switch unwinds to event (or
/// resumes, when it was one). From event the event plan is the start's
/// checks: a dev or live entry the pipe queues meanwhile runs after them
/// (#42). The outcome of a plan that ran.
pub fn start(pc: &mut dyn Pc, g: &mut Guard, boot: u64) -> Option<Outcome> {
    let job = pc.job();
    if let Err(e) = &job {
        warn!("the guard's job could not be read: {e}");
    }
    g.job_note = job_note(&job);
    if let Some(n) = g.job_note {
        info!("{n}");
    }
    // A cutover cut off between two steps (a crash, a power loss): its
    // local undos (the lifecycle back to trial) before anything else, and
    // the PC goes to event (S8); its elevated undos after the event plan.
    let cut_off = super::cutover::recover(pc, g);
    let p = pc.procs();
    let reset = cut_off.is_some()
        || state::reset_to_event(&g.state, boot, p.band_up(), !p.engine.is_empty());
    let rebooted = boot > g.state.written_at;
    // S8: the lifecycle decides (before the cutover: event, as G1 says).
    let Started {
        start: go,
        lifecycle: next,
        note,
        alarm,
    } = lifecycle::start(&g.state.lifecycle, reset, rebooted, |sha| {
        g.state.bundles.get(sha)
    });
    // A pin the boot's live entry would end maintenance on is logged once
    // the PC is live, as the entry does.
    let (note, live_note) = if matches!(go, Start::Live(_)) {
        (None, note)
    } else {
        (note, None)
    };
    if let Some(n) = note {
        g.info(n);
    }
    if let Some(why) = alarm {
        g.raise(None, &why, false);
    }
    let target = match go {
        Start::Keep => None,
        Start::Event => {
            if reset {
                g.info("after a reboot, or with the band's system up, the PC is in event");
            }
            g.state.reset();
            Some(Mode::Event)
        }
        Start::Live(pin) => {
            g.info(format!(
                "after a reboot in prod the PC goes live on the pin {pin}"
            ));
            g.state.reset();
            g.state.set_active(&pin);
            Some(Mode::Live)
        }
    };
    let saved = g.state.pids.clone();
    g.state.pids = pc.adopt(&saved);
    pc.set_bundle(g.state.active_bundle());
    g.look(pc);
    // Before the event plan: its check then adds no alarm for the same value.
    take_logon(pc, g);
    g.save();
    // S8 lane 3: a rollback left (a crash, a reboot, a step that failed)
    // goes on; its event plan is the start's.
    let out = match super::rollback::resume(pc, g) {
        Some(out) => Some(out),
        None => start_plan(pc, g, target, next, live_note),
    };
    // S8 lane 5: the cut-off cutover's elevated undos once REAPER is back.
    if let Some(found) = cut_off {
        super::cutover::finish_recovery(pc, g, &found);
    }
    send_notices(pc, g);
    out
}

/// The start's plan: the pin's own live entry in prod after a reboot, else
/// the event plan when the start goes to event or a switch was left.
fn start_plan(
    pc: &mut dyn Pc,
    g: &mut Guard,
    target: Option<Mode>,
    next: crate::lifecycle::Lifecycle,
    live_note: Option<String>,
) -> Option<Outcome> {
    let resume = g.state.switching.is_some();
    match target {
        // Prod after a reboot: no trial, the pin's own entry. The pin a
        // maintenance session ended on holds once the PC is live.
        Some(Mode::Live) => {
            let out = switch(pc, g, Mode::Event, Mode::Live, false);
            if g.state.mode == Mode::Live {
                g.state.lifecycle = next;
                if let Some(n) = live_note {
                    g.info(n);
                }
                g.save();
            }
            Some(out)
        }
        _ if target.is_some() || resume => {
            let from = g.state.mode;
            Some(switch(pc, g, from, Mode::Event, from == Mode::Event))
        }
        _ => None,
    }
}

/// `iemmode event --direct` (design §5.1): without a guard, the same event
/// plan in this process. `lock` is the guard's mutex; `None`: a guard holds
/// it.
pub fn direct_event<L>(pc: &mut dyn Pc, g: &mut Guard, lock: Option<L>, dry_run: bool) -> Reply {
    let Some(_held) = lock else {
        return g.reply(false, "a guard runs; use the pipe");
    };
    let saved = g.state.pids.clone();
    g.state.pids = pc.adopt(&saved);
    pc.set_bundle(g.state.active_bundle());
    let (ok, detail) = if dry_run {
        dry_event(pc)
    } else {
        event_now(pc, g)
    };
    send_notices(pc, g);
    g.look(pc);
    g.reply(ok, &format!("direct: {detail}"))
}
