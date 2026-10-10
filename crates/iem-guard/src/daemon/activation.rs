//! Bundles and sites (design §5.3, §7; #9 2026-09-28): a bundle zip
//! installed, a bundle activated (in dev, in an idle event, without a guard,
//! inside a HIL job) and a site installed (F30).

use std::path::Path;

use super::reply::switch_text;
use super::runner::run_step;
use super::{Guard, Outcome, mode_name, run_switch};
use crate::bundle::Record;
use crate::install::{self, InstallError};
use crate::lifecycle;
use crate::pc::{Pc, StepError};
use crate::plan::{Activation, Busy, Mode, Step, activation};
use crate::proto::Reply;

/// Installs a bundle zip: a new one gets its record, HIL pending. Returns
/// its SHA and what happened.
fn install_record(g: &mut Guard, zip: &Path) -> Result<(String, &'static str), String> {
    let Some(root) = g.root.clone() else {
        return Err("the guard has no bundle directory".to_owned());
    };
    match install::install(&install::bundles_dir(&root), zip) {
        Ok(done) => {
            let sha = done.manifest.sha.clone();
            if !g.state.bundles.contains_key(&sha) {
                let record = Record::installed(&done.manifest, g.now());
                g.state.bundles.insert(sha.clone(), record);
                g.save();
            }
            let what = if done.fresh {
                "installed"
            } else {
                "already installed"
            };
            Ok((sha, what))
        }
        Err(InstallError::Refused(why)) => Err(format!("install refused: {why}")),
        Err(InstallError::Conflict(why)) => {
            g.raise(None, &why, false);
            Err(why)
        }
    }
}

/// The pipe's `Install` (`iemmode install <zip>`).
pub fn install_bundle(g: &mut Guard, zip: &Path) -> (bool, String) {
    match install_record(g, zip) {
        Ok((sha, what)) => (true, format!("bundle {sha} {what}")),
        Err(why) => (false, why),
    }
}

/// `iemmixer-guard install <zip>` without a guard: the install, and for the
/// very first bundle (no active bundle yet) its activation into `bin\`, so
/// `iemmode` exists from then on.
pub fn install_offline(g: &mut Guard, zip: &Path) -> (bool, String) {
    let (sha, what) = match install_record(g, zip) {
        Ok(done) => done,
        Err(why) => return (false, why),
    };
    let detail = format!("bundle {sha} {what}");
    if g.state.active_bundle().is_some() {
        return (true, detail);
    }
    match activate_files(g, &sha) {
        Ok(_) => (true, format!("{detail}; activated into bin")),
        Err(why) => (false, format!("{detail}; activation failed: {why}")),
    }
}

/// Copies the bundle's guard and `iemmode` into `bin\` and makes it the
/// active bundle (never the pin, S8 design §3.4); true when the guard's exe
/// changed.
pub fn activate_files(g: &mut Guard, sha: &str) -> Result<bool, String> {
    let root = g
        .root
        .clone()
        .ok_or_else(|| "the guard has no bundle directory".to_owned())?;
    let changed = install::activate_bins(
        &install::bundles_dir(&root).join(sha),
        &install::bin_dir(&root),
        sha,
    )?;
    g.state.set_active(sha);
    g.save();
    Ok(changed)
}

/// `plan::activation` on this guard's state and the process list.
fn activation_now(pc: &mut dyn Pc, g: &Guard) -> Activation {
    let busy = Busy {
        switching: g.state.switching.is_some(),
        job: g.state.job,
    };
    activation(g.state.mode, &pc.facts(), busy)
}

/// `activate <sha>` in dev, or in an idle event (`plan::activation`; #9
/// 2026-09-28): [`activate_bundle`], then the hand-over to a changed guard
/// exe (a HIL job is in the state, so the new guard serves it). In event
/// nothing else happens: REAPER and the app are not touched, and the new
/// guard starts as after any restart (in event, the event plan's checks).
/// So a guard fix reaches a guard in event, whose own code may refuse the
/// dev entry.
pub(super) fn activate(pc: &mut dyn Pc, g: &mut Guard, sha: &str) -> (bool, String) {
    // An older guard taking over would drop a kept cutover record (S8).
    if let Some(why) = crate::cutover::activation_refusal(g.state.cutover.as_ref()) {
        return (false, why);
    }
    let restart_job = match activation_now(pc, g) {
        Activation::Files => false,
        Activation::FilesThenJobRestart => true,
        Activation::Refused(why) => return (false, why),
    };
    match activate_bundle(pc, g, sha, restart_job) {
        Ok((detail, false)) => (true, detail),
        Ok((detail, true)) => {
            g.handover = g
                .root
                .as_ref()
                .map(|r| install::bin_dir(r).join(install::GUARD_EXE));
            (
                true,
                format!("{detail}; the guard hands over to its new exe"),
            )
        }
        Err(why) => (false, why),
    }
}

/// The activation `plan::activation` allowed: the bundle's guard and
/// `iemmode` into `bin\`, the active bundle, its Defender exclusions (a
/// failure alarms, the activation stands; the way back and, in prod, the
/// pins keep theirs: `lifecycle::kept`); with `restart_job`
/// (dev, inside a HIL job) the engine and the server then run the new
/// bundle (HIL checks their versions, design §7). The detail and whether
/// the guard's exe changed.
fn activate_bundle(
    pc: &mut dyn Pc,
    g: &mut Guard,
    sha: &str,
    restart_job: bool,
) -> Result<(String, bool), String> {
    if !g.state.bundles.contains_key(sha) {
        return Err(format!("bundle {sha} is not installed"));
    }
    let changed = activate_files(g, sha).map_err(|why| format!("activation failed: {why}"))?;
    pc.set_bundle(Some(sha));
    let keep = lifecycle::kept(&g.state.lifecycle, g.state.way_back_bundle(), sha);
    if let Err(e) = pc.exclude(sha, &keep) {
        g.raise(None, &format!("Defender exclusions for {sha}: {e}"), false);
    }
    let mut detail = format!("activated {sha}");
    if restart_job {
        match restart_in_job(pc, g) {
            Ok(()) => detail.push_str("; the engine and the server run it"),
            Err(why) => return Err(format!("{detail}; {why}")),
        }
    }
    Ok((detail, changed))
}

/// `iemmixer-guard activate <sha>` while no guard runs (#9 2026-09-28),
/// from a bundle's own exe: the way to a guard too old to activate in
/// event (its own code refuses it). `lock` is the guard's mutex, held for
/// the whole run (`None`: a guard holds it). Only in an idle event: the
/// saved mode must be event, then the same `plan::activation` on the saved
/// state and the process list; then `activate_bundle` (the bins, the active
/// bundle, the exclusions through the elevated task as online; the state
/// and any alarm are saved). It starts no guard: the next `iemmode` call starts
/// the guard's task, which runs the new exe from `bin\`.
pub fn activate_offline<L>(pc: &mut dyn Pc, g: &mut Guard, lock: Option<L>, sha: &str) -> Reply {
    let Some(_held) = lock else {
        return g.reply(false, "a guard runs; use iemmode activate");
    };
    g.report.clear();
    let (ok, detail) = offline_activation(pc, g, sha);
    g.reply(ok, &detail)
}

fn offline_activation(pc: &mut dyn Pc, g: &mut Guard, sha: &str) -> (bool, String) {
    if let Some(why) = crate::cutover::activation_refusal(g.state.cutover.as_ref()) {
        return (false, why);
    }
    if g.state.mode != Mode::Event {
        return (
            false,
            format!(
                "the saved mode is {}: without a guard only an idle event activates",
                mode_name(g.state.mode)
            ),
        );
    }
    match activation_now(pc, g) {
        Activation::Refused(why) => (false, why),
        Activation::Files | Activation::FilesThenJobRestart => {
            match activate_bundle(pc, g, sha, false) {
                Ok((detail, _)) => (
                    true,
                    format!(
                        "{detail} without a guard; the next iemmode call starts the guard from bin"
                    ),
                ),
                Err(why) => (false, why),
            }
        }
    }
}

/// Inside a HIL job: the engine and the server start again from the active
/// bundle and site, the engine with the job's HIL flags, after `PrefCheck`
/// (REAPER's original back, as before every engine start). The runner (it
/// runs the job) and the tray keep running, so this is no dev entry. A
/// failure alarms and unwinds to event, as a failed dev entry does; "ide
/// event" ends a wait and is served next.
fn restart_in_job(pc: &mut dyn Pc, g: &mut Guard) -> Result<(), String> {
    let f = pc.facts();
    let mut steps = Vec::new();
    if f.engine {
        steps.push(Step::EngineStop);
    }
    if f.server {
        steps.push(Step::ServerStop);
    }
    // REAPER's original back right before the engine starts, as in every
    // entry (#9 2026-09-28): the old engine stopped above.
    steps.extend([
        Step::PrefCheck,
        Step::EngineStart,
        Step::EngineArm,
        Step::ServerStart,
    ]);
    for step in steps {
        match run_step(pc, g, step, Mode::Dev) {
            // The children are saved after every step, so the guard an
            // activation hands over to adopts the new engine and server.
            Ok(()) => g.done(pc, step),
            Err(StepError::Preempted) => return Err("pre-empted by event".to_owned()),
            Err(StepError::Failed(why)) => {
                g.alarm(step, &why, false);
                let from = g.state.mode;
                let out = run_switch(pc, g, from, Mode::Event);
                return Err(format!(
                    "{step:?}: {why}; {}",
                    switch_text(Mode::Event, out, g.state.mode, &g.owner_failed)
                ));
            }
        }
    }
    Ok(())
}

/// F30: the new site checked and installed, then dev entered again (the
/// engine and the server restart with it). Inside a HIL job (HIL applies
/// and reverts a synthetic change, design §7) only the engine and the
/// server restart: a dev entry would stop the runner that runs the job.
pub(super) fn install_site(pc: &mut dyn Pc, g: &mut Guard, path: &str) -> (bool, String) {
    if let Err(why) = g.need_dev("install-site") {
        return (false, why);
    }
    let c = g.cancel.clone();
    match pc.install_site(path, &c) {
        Ok(r) => g.info(r),
        Err(e) => return (false, format!("site refused: {e}")),
    }
    if g.state.job.is_some() {
        return match restart_in_job(pc, g) {
            Ok(()) => (
                true,
                "site installed; the engine and the server run it (HIL job)".to_owned(),
            ),
            Err(why) => (false, format!("site installed; {why}")),
        };
    }
    let out = run_switch(pc, g, Mode::Dev, Mode::Dev);
    (
        out == Outcome::Done && g.state.mode == Mode::Dev,
        format!(
            "site installed; {}",
            switch_text(Mode::Dev, out, g.state.mode, &g.owner_failed)
        ),
    )
}
