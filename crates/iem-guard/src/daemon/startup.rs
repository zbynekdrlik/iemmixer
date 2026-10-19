//! A starting guard (design §5.2): its job, the reboot rule, the children
//! a previous guard started, the logon task's result and the start's
//! checks; and `iemmode event --direct`, the event plan without a guard.

use tracing::{info, warn};

use super::requests::{dry_event, event_now};
use super::runner::switch;
use super::{Guard, Outcome, mode_name, send_notices};
use crate::lifecycle::{self, Left, Lifecycle, Start, Started};
use crate::pc::{Pc, job_note};
use crate::plan::Mode;
use crate::proto::Reply;
use crate::state::{self, Switching};

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
/// resumes, when it was one; in prod an unfinished dev or live entry stops
/// instead, #11 2026-10-19, also with the band's system up: it is held
/// before the reboot rule could reset it, `lifecycle::holds_cut_off`). From
/// event the event plan is the start's checks: a dev or live entry the pipe
/// queues meanwhile runs after them (#42). The outcome of a plan that ran.
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
    let rebooted = boot > g.state.written_at;
    // In prod a left dev or live entry is held before the reboot rule could
    // reset it to the event plan (`lifecycle::holds_cut_off`).
    let left = g.state.switching.as_ref().map(|s| s.to);
    let held = lifecycle::holds_cut_off(&g.state.lifecycle, rebooted, left);
    let reset = cut_off.is_some()
        || (!held && state::reset_to_event(&g.state, boot, p.band_up(), !p.engine.is_empty()));
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
        // Nothing plays after a hold: the owner decides.
        g.raise(None, &why, matches!(go, Start::Hold));
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
        // Prod after a reboot on a pin that may not go live: in event with
        // nothing started, no event plan (#11, 2026-10-19: in prod nothing
        // goes to REAPER on its own); the alarm names the way on.
        Start::Hold => {
            g.info("after a reboot in prod on a pin that may not go live: nothing started");
            g.state.reset();
            None
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
    let out = start_plan(pc, g, target, next, live_note);
    // S8 lane 5: the cut-off cutover's elevated undos once REAPER is back.
    if let Some(found) = cut_off {
        super::cutover::finish_recovery(pc, g, &found);
    }
    send_notices(pc, g);
    out
}

/// The start's plan: the pin's own live entry in prod after a reboot, else
/// the event plan when the start goes to event or a switch was left (in
/// prod only an event plan that was left: [`hold_cut_off`]).
fn start_plan(
    pc: &mut dyn Pc,
    g: &mut Guard,
    target: Option<Mode>,
    next: crate::lifecycle::Lifecycle,
    live_note: Option<String>,
) -> Option<Outcome> {
    let resume = g.state.switching.is_some();
    if target.is_none()
        && let Some(left) = g.state.switching.clone()
        && left.to != Mode::Event
    {
        let stopped = cut_off_left(g, &left);
        if let Some(ways) =
            lifecycle::after_stop(&g.state.lifecycle, stopped, |sha| g.state.bundles.get(sha))
        {
            hold_cut_off(g, &left, stopped, &ways);
            return None;
        }
    }
    match target {
        // Prod after a reboot: no trial, the pin's own entry. The pin a
        // maintenance session ended on holds once the PC is live. Its arm
        // starts an engine that missed periods again (the boot's load,
        // #11 2026-10-19; `runner::engine_ready`), for this switch only.
        Some(Mode::Live) => {
            g.boot_entry = true;
            let out = switch(pc, g, Mode::Event, Mode::Live, false);
            g.boot_entry = false;
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

/// What a cut-off entry left (`lifecycle::cut_off_in`, the step it was in
/// being unknown): its armed engine only when this guard adopted an engine
/// (the second review of the fixes for PR #56), else nothing of it runs and
/// the PC is in event.
fn cut_off_left(g: &Guard, left: &Switching) -> Left {
    match lifecycle::cut_off_in(left.from, left.to, &left.done) {
        Left::Armed(_) if g.state.pids.engine.is_none() => Left::Changed,
        stopped => stopped,
    }
}

/// A dev or live entry a restarted guard (no reboot: a crash, a hand-over)
/// finds cut off in prod (#11, 2026-10-19: in prod nothing goes to REAPER
/// on its own): it stops where it was with what runs as it is, never the
/// event plan; as `stopped` says ([`cut_off_left`]): with its armed engine
/// in its target, whose entry's lifecycle change then holds as a failed
/// entry's does ([`armed_lifecycle`]), in the mode it began from only when
/// it was cut off in its precheck, else in event (a HIL job lives in dev
/// only). The alarm asks the owner with `ways` (`lifecycle::after_stop`).
/// An event plan that was left resumes as before: it was asked for.
fn hold_cut_off(g: &mut Guard, left: &Switching, stopped: Left, ways: &str) {
    let ends = stopped.mode();
    info!(
        "a switch {} → {} was left in prod: it stops in {}, no event plan",
        mode_name(left.from),
        mode_name(left.to),
        mode_name(ends)
    );
    g.state.switching = None;
    g.state.mode = ends;
    if ends != Mode::Dev {
        g.state.job = None;
    }
    if let Left::Armed(to) = stopped
        && let Some(next) = armed_lifecycle(g, to)
    {
        g.state.lifecycle = next;
    }
    g.raise(
        None,
        &format!(
            "the guard started again during the switch {} → {} ({} steps done); {ways}",
            mode_name(left.from),
            mode_name(left.to),
            left.done.len()
        ),
        true,
    );
}

/// The lifecycle a cut-off entry into `to` whose engine was armed is in:
/// its entry's own change (`lifecycle::entry` on the active bundle, the
/// build that engine runs since its `EngineStart`), when the entry would run
/// that build; none when it would not (the PC's lifecycle stays).
fn armed_lifecycle(g: &Guard, to: Mode) -> Option<Lifecycle> {
    let active = g.state.active_bundle()?;
    let ask = lifecycle::Ask {
        to,
        build: (to == Mode::Dev).then_some(active),
        trial: false,
    };
    let entered = lifecycle::entry(&g.state.lifecycle, ask, |sha| g.state.bundles.get(sha)).ok()?;
    (entered.runs.as_deref() == Some(active)).then_some(entered.lifecycle)
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
