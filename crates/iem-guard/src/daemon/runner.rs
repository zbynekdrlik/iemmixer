//! The switch runner (design §5.2): a plan's steps one at a time with the
//! error policy (`plan::on_error`), "ide event"'s pre-emption, the unwind of
//! a failed or pre-empted dev or live entry, and each step's `Pc` call.

use std::time::Instant;

use tracing::{info, warn};

use super::reply::outcome_text;
use super::{Guard, Outcome, READY_S, mode_name, reaper};
use crate::cancel::Cancel;
use crate::crash;
use crate::handover::{self, Audio};
use crate::lifecycle::{self, Lifecycle};
use crate::pc::{Pc, PrefSeen, R, Status, StepError};
use crate::plan::{Health, Mode, OnError, Step, on_error};
use crate::state::Switching;
use crate::switch_log::LastSwitch;

impl Guard {
    /// The switch is persisted as it begins and after every step (the
    /// steps done), so a restarted guard re-plans it to event. Every switch
    /// but the start's checks (`checks`) moves the fence (#42).
    pub(super) fn begin(&mut self, from: Mode, to: Mode, steps: &[Step], checks: bool) {
        let what = if checks { " (the start's checks)" } else { "" };
        info!(
            "switch {} → {}{what}: {steps:?}",
            mode_name(from),
            mode_name(to)
        );
        self.state.switching = Some(Switching {
            from,
            to,
            done: Vec::new(),
            started: self.now(),
        });
        self.owner_failed.clear();
        // An unwind goes on with the entry's clock.
        if self.unwinding.is_some() {
            self.laps.resume(Instant::now());
        } else {
            self.laps.start(Instant::now());
        }
        self.store();
        let cancel = self.cancel.clone();
        self.publish(|v| {
            v.running = Some(to);
            v.epoch += 1;
            if !checks {
                v.fence += 1;
            }
            v.start_checks = checks;
            v.began = Some((from, to));
            if to == Mode::Event {
                cancel.clear();
            }
        });
    }

    pub(super) fn done(&mut self, pc: &mut dyn Pc, step: Step) {
        if let Some(s) = self.state.switching.as_mut() {
            s.done.push(step);
        }
        self.state.pids = pc.children();
        self.save();
    }

    pub(super) fn finish(&mut self, pc: &mut dyn Pc, outcome: Outcome, mode: Mode) -> Outcome {
        let sw = self.state.switching.take();
        let from = sw.as_ref().map(|s| s.from);
        self.state.mode = mode;
        // A HIL job lives in dev only (an event plan without a runner has
        // no JobsCancel step).
        if mode != Mode::Dev {
            self.state.job = None;
        }
        self.trial = false;
        // The record of this switch (S7 design note §5), saved and replied
        // from here on; an unwind's spans the entry it unwinds.
        let entry = self.unwinding.take();
        if let Some(s) = &sw {
            let steps = self.laps.take();
            let ended = self.now();
            let record = LastSwitch::new(s, mode, outcome.into(), ended, steps);
            self.state.last_switch = Some(record.unwinding(entry));
        }
        let how = if self.owner_failed.is_empty() {
            outcome_text(Some(outcome)).to_owned()
        } else {
            format!("needs the owner: {}", self.owner_failed.join("; "))
        };
        info!("switch ended in {}: {how}", mode_name(mode));
        self.store();
        let owner = self.owner_failed.clone();
        self.publish(|v| {
            v.running = None;
            v.last = Some(outcome);
            v.last_owner = owner;
        });
        // Tuning drift after every switch that ran to its end, one that went
        // on after asking the owner included (#10). After any other change
        // of the mode (a plan that stopped for the owner) the watch reads it
        // at its next tick: nothing follows a stopped plan's last step.
        if outcome == Outcome::Done || !self.owner_failed.is_empty() {
            self.drift(pc, Instant::now());
        } else if from != Some(mode) {
            self.last_drift = None;
        }
        outcome
    }

    pub(super) fn drift(&mut self, pc: &mut dyn Pc, at: Instant) {
        self.last_drift = Some(at);
        match pc.tuning_drift() {
            Ok(None) => {}
            Ok(Some(d)) => {
                self.raise(None, &format!("tuning drift: {d}"), false);
            }
            Err(e) => warn!("the tuning drift could not be read: {e}"),
        }
    }
}

/// Runs the plan from `from` to `to` with the error policy of design §5.2
/// (`plan::on_error`). "ide event" pre-empts a switch into dev/live within
/// 1 s of a waiting step, after a mutating one.
pub fn run_switch(pc: &mut dyn Pc, g: &mut Guard, from: Mode, to: Mode) -> Outcome {
    switch(pc, g, from, to, false)
}

/// [`run_switch`]; `checks`: the start's event checks, which a dev or live
/// entry queued meanwhile follows (#42).
pub(super) fn switch(
    pc: &mut dyn Pc,
    g: &mut Guard,
    from: Mode,
    to: Mode,
    checks: bool,
) -> Outcome {
    // The prod data rule: the lifecycle drops the data step after the
    // cutover (`lifecycle::refreshes_data`).
    let steps = lifecycle::plan(&g.state.lifecycle, to, &pc.facts());
    g.begin(from, to, &steps, checks);
    let mut skip: Vec<Step> = Vec::new();
    for step in steps {
        if skip.contains(&step) {
            continue;
        }
        if to != Mode::Event && g.cancel.preempted() {
            return back_to_event(pc, g, "pre-empted by event");
        }
        info!("step {step:?}");
        let r = run_step(pc, g, step, to);
        g.laps.lap(step, Instant::now());
        match r {
            Ok(()) => g.done(pc, step),
            Err(StepError::Preempted) if to != Mode::Event => {
                return back_to_event(pc, g, "pre-empted by event");
            }
            Err(e) => {
                let (why, health, policy) = failure(pc, g, to, step, &e);
                if health.is_some() {
                    // The health read inserted after a failed engine stop.
                    g.laps.lap(Step::EngineHealth, Instant::now());
                }
                match policy {
                    OnError::Unwind => {
                        // The rehearsal's re-entry stops for the owner,
                        // unless "ide event" came meanwhile.
                        if g.hold_unwind && may_end(g, to) {
                            g.alarm(
                                step,
                                &format!("{why}; the rehearsal never starts REAPER"),
                                true,
                            );
                            return g.finish(pc, Outcome::NeedsOwner, from);
                        }
                        g.alarm(step, &why, false);
                        return back_to_event(pc, g, &why);
                    }
                    OnError::Continue => g.alarm(step, &why, false),
                    // The plan goes on, but the switch is not done (#10).
                    OnError::ContinueAskOwner => {
                        g.alarm(step, &why, true);
                        g.owner_failed.push(format!("{step:?} failed: {why}"));
                    }
                    OnError::SkipAskOwner(later) => {
                        g.alarm(step, &why, true);
                        g.owner_failed.push(format!("{step:?} failed: {why}"));
                        skip.extend_from_slice(later);
                    }
                    OnError::KeepServing => {
                        g.alarm(
                            step,
                            &format!("{why}; engine healthy, iemmixer keeps serving"),
                            true,
                        );
                        return g.finish(pc, Outcome::KeptServing, from);
                    }
                    OnError::StopAskOwner => {
                        g.alarm(step, &format!("{why}; health {health:?}"), true);
                        return g.finish(pc, Outcome::NeedsOwner, Mode::Event);
                    }
                }
            }
        }
    }
    if !may_end(g, to) {
        return back_to_event(pc, g, "pre-empted by event");
    }
    let outcome = if g.owner_failed.is_empty() {
        Outcome::Done
    } else {
        Outcome::NeedsOwner
    };
    g.finish(pc, outcome, to)
}

/// Whether a switch into `to` may end as it is: an event plan always; one
/// into dev or live unless "ide event" pre-empted it after its last look at
/// the token (during a mutation that finishes first, or after the last
/// step). From then on "ide event" queues behind its end.
fn may_end(g: &Guard, to: Mode) -> bool {
    to == Mode::Event || g.shared.end_unless_preempted()
}

/// The unwind of a failed or pre-empted dev or live entry: a switch to
/// event whose record spans the entry (S7 part 3, `LastSwitch.unwound`).
fn back_to_event(pc: &mut dyn Pc, g: &mut Guard, why: &str) -> Outcome {
    g.cancel.clear();
    g.info(format!("unwinding to event: {why}"));
    g.unwinding = g.state.switching.as_ref().map(|s| (s.to, s.started));
    let now = g.state.mode;
    run_switch(pc, g, now, Mode::Event)
}

/// Why `step` failed, the engine's health after a failed release at "ide
/// event" (read over the supervisor pipe), and what that means.
pub(super) fn failure(
    pc: &mut dyn Pc,
    g: &mut Guard,
    to: Mode,
    step: Step,
    e: &StepError,
) -> (String, Option<Health>, OnError) {
    let why = match e {
        StepError::Failed(s) => s.clone(),
        StepError::Preempted => "pre-empted inside the event plan".to_owned(),
    };
    let health = (to == Mode::Event && step == Step::EngineStop).then(|| {
        info!("step {:?}", Step::EngineHealth);
        pc.engine_health().unwrap_or(Health::Dead)
    });
    (why, health, on_error(to, step, health, g.site.on_pref_fail))
}

/// `EngineArm`'s readiness. An engine that ended with exit 75 before it
/// was ready (another process still held its state directory) is started
/// again, held, once, after `crash::BUSY_RETRY`, inside the step, with the
/// preference checked right before as for every start (#32 F3-r4 4); any
/// other end, or a second busy one, is the step's failure (the plan's
/// error policy unwinds).
fn engine_ready(pc: &mut dyn Pc, g: &mut Guard, to: Mode, c: &Cancel) -> R<Status> {
    let mut restarted = false;
    loop {
        let e = match pc.engine_ready(READY_S, c) {
            Err(StepError::Failed(why)) => why,
            done => return done,
        };
        if !crash::ready_restart(pc.engine_exit(), restarted) {
            return Err(StepError::Failed(e));
        }
        restarted = true;
        g.info(format!(
            "the engine ended with exit {} before it was ready (its state directory was \
             held): starting it again in {} s",
            crash::STATE_BUSY,
            crash::BUSY_RETRY.as_secs()
        ));
        c.sleep(crash::BUSY_RETRY)?;
        pref_step(pc, g, to)?;
        let pid = pc.engine_start(true, g.hil_engine(to))?;
        g.spawns += 1;
        g.info(format!("engine started again, held (pid {pid})"));
    }
}

/// One step, one `Pc` call (plus the verdicts of `handover`).
pub(super) fn run_step(pc: &mut dyn Pc, g: &mut Guard, step: Step, to: Mode) -> R<()> {
    let c = g.cancel.clone();
    match step {
        Step::Precheck => {
            g.subscriptions_note = None;
            g.subscriptions_note = pc.precheck(to, g.trial)?;
            if let Some(n) = g.subscriptions_note.clone() {
                g.info(n);
            }
            Ok(())
        }
        Step::AppStop => {
            let exit = pc.app_stop(&c)?;
            handover::app_exit(exit).map_err(|bad| StepError::Failed(bad.join("; ")))
        }
        Step::ReaperSaveQuit => {
            pc.reaper_save_quit(&c)?;
            g.reaper_notice = false;
            Ok(())
        }
        Step::TuningEnter => {
            let r = pc.tuning("enter", &c)?;
            g.info(format!("tuning enter: {r}"));
            Ok(())
        }
        Step::Data => {
            let r = pc.data(to, &c)?;
            g.info(r);
            Ok(())
        }
        Step::EngineStart => {
            let hil = g.hil_engine(to);
            let pid = pc.engine_start(true, hil)?;
            g.spawns += 1;
            if hil {
                g.info(format!(
                    "engine started, held, with its HIL flags (pid {pid})"
                ));
            } else {
                g.info(format!("engine started, held (pid {pid})"));
            }
            Ok(())
        }
        Step::EngineArm => {
            let s = engine_ready(pc, g, to, &c)?;
            g.info(format!(
                "engine ready: {} frames, {} callbacks, {} missed",
                s.frames, s.callbacks, s.missed
            ));
            pc.engine_arm()
        }
        Step::ServerStart => {
            // In prod the cutover's `pin_changes = true` is allowed (S8).
            let prod = matches!(g.state.lifecycle, Lifecycle::Prod(_));
            let pid = pc.server_start(to, prod)?;
            g.info(format!("server started (pid {pid})"));
            Ok(())
        }
        Step::TrayStart => {
            g.shared.clear_tray_quit();
            pc.tray_start()
        }
        Step::IdentityCheck => {
            // Only what this check names stays named.
            g.lan_note = None;
            let sha = g
                .state
                .active_bundle()
                .map(str::to_owned)
                .ok_or_else(|| StepError::failed("no active bundle"))?;
            g.lan_note = pc.identity(&sha, &c)?;
            if let Some(n) = g.lan_note.clone() {
                g.info(n);
            }
            Ok(())
        }
        Step::RunnerStart => pc.runner_start(),
        Step::JobsCancel => {
            if let Some(run) = g.state.job.take() {
                g.info(format!("HIL job {run} cancelled"));
            }
            Ok(())
        }
        Step::RunnerStop => pc.runner_stop(&c),
        Step::EngineStop => pc.engine_stop(&c),
        Step::EngineHealth => pc.engine_health().map(|_| ()),
        Step::ServerStop => pc.server_stop(&c),
        Step::TrayStop => pc.tray_stop(&c),
        Step::TuningExit => {
            let r = pc.tuning("exit", &c)?;
            g.info(format!("tuning exit: {r}"));
            Ok(())
        }
        Step::PrefCheck => pref_step(pc, g, to),
        Step::HolderGone => pc.holder_gone(&c),
        Step::ReaperStart => pc.reaper_start(),
        Step::ReaperHandover => {
            // Unknown until this check has read REAPER's dialogs.
            g.reaper_notice = false;
            // A REAPER runs first (#10: it may have ended, or still be
            // ending, since the plan read its facts).
            reaper::ensure(pc, g, &c)?;
            let f = pc.reaper_facts(&c)?;
            // REAPER's evaluation notice is named, never an alarm and never
            // closed (#9, 2026-09-28); every other dialog fails the verdict.
            g.reaper_notice = handover::dialogs(&f.dialogs).notice;
            if g.reaper_notice {
                g.info(handover::NOTICE_REPORT);
            }
            match handover::reaper_handover(&f) {
                Ok(Audio::Confirmed) => Ok(()),
                Ok(Audio::Unconfirmed) => {
                    g.info("UNCONFIRMED-AUDIO: every stage input at the meter floor");
                    Ok(())
                }
                Err(bad) => Err(StepError::Failed(bad.join("; "))),
            }
        }
        Step::AppStart => pc.app_start(),
        Step::AppHandover => pc.app_answers(&c),
        Step::Fingerprint => pc.fingerprint(),
    }
}

/// `PrefCheck` (design §5.2; #9 2026-09-28): REAPER's original, or restored
/// while nothing holds the driver module. Nothing is ever written while
/// something holds it (its driver would most likely ask it for a reset):
/// the guard remembers what it left (`GuardState::pref_held`, named in the
/// status). In the event plan that is no failure: it alarms once and goes
/// on (REAPER keeps its sound; the check after REAPER's quit restores it).
/// Before an engine start (`to` dev or live) it fails the step: the engine
/// would refuse the card.
pub(super) fn pref_step(pc: &mut dyn Pc, g: &mut Guard, to: Mode) -> R<()> {
    match pc.pref_check()? {
        PrefSeen::Original(writes) => {
            if writes > 0 {
                g.info(format!(
                    "the preferred buffer was restored ({writes} writes)"
                ));
            }
            g.state.pref_held = None;
            Ok(())
        }
        PrefSeen::Held(held) => {
            let text = held.text();
            let new = g.state.pref_held.as_deref() != Some(text.as_str());
            g.state.pref_held = Some(text.clone());
            if to != Mode::Event {
                return Err(StepError::Failed(text));
            }
            g.info(text.clone());
            if new {
                g.alarm(Step::PrefCheck, &text, false);
            }
            Ok(())
        }
    }
}
