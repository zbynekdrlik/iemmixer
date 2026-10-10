//! A starting guard (design §5.2): its job, the reboot rule, the children
//! a previous guard started, the logon task's result and the start's
//! checks; and `iemmode event --direct`, the event plan without a guard.

use tracing::{info, warn};

use super::requests::{dry_event, event_now};
use super::runner::switch;
use super::{Guard, Outcome, send_notices};
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
/// and named in the status while they stay in it, §5.1), the reboot rule,
/// then the children a previous guard started, then an unfinished switch
/// unwinds to event (or resumes, when it was one). From event the event
/// plan is the start's checks: a dev or live entry the pipe queues
/// meanwhile runs after them (#42). The outcome of an event plan that ran.
pub fn start(pc: &mut dyn Pc, g: &mut Guard, boot: u64) -> Option<Outcome> {
    let job = pc.job();
    if let Err(e) = &job {
        warn!("the guard's job could not be read: {e}");
    }
    g.job_note = job_note(&job);
    if let Some(n) = g.job_note {
        info!("{n}");
    }
    let p = pc.procs();
    let reset = state::reset_to_event(&g.state, boot, p.band_up(), !p.engine.is_empty());
    if reset {
        g.info("after a reboot, or with the band's system up, the PC is in event");
        g.state.reset();
    }
    let saved = g.state.pids.clone();
    g.state.pids = pc.adopt(&saved);
    pc.set_bundle(g.state.pins.current.as_deref());
    g.look(pc);
    // Before the event plan: its check then adds no alarm for the same value.
    take_logon(pc, g);
    g.save();
    let resume = g.state.switching.is_some();
    let out = (reset || resume).then(|| {
        let from = g.state.mode;
        switch(pc, g, from, Mode::Event, from == Mode::Event)
    });
    send_notices(pc, g);
    out
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
    pc.set_bundle(g.state.pins.current.as_deref());
    let (ok, detail) = if dry_run {
        dry_event(pc)
    } else {
        event_now(pc, g)
    };
    send_notices(pc, g);
    g.look(pc);
    g.reply(ok, &format!("direct: {detail}"))
}
