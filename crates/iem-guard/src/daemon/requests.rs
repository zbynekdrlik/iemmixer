//! The guard pipe's requests on the daemon thread (design §5.1): a request
//! queued before a switch began is answered as during it (#42), every
//! request's handling, the switches the owner asks for, and the rehearsal
//! of the event plan's teardown.

use std::path::Path;
use std::sync::mpsc::SyncSender;

use tracing::info;

use super::activation::{activate, install_site};
use super::cutover::cutover;
use super::hil::{
    inject_fault, inject_park, inject_seh, job_begin, job_end, report, runner_stop, test_signal,
};
use super::reply::{outcome, switch_text};
use super::runner::{failure, run_step};
use super::{
    Generation, Guard, Outcome, View, alarm_test, install_bundle, mode_name, run_switch,
    send_notices, status_text, while_switching,
};
use crate::lifecycle;
use crate::pc::{Pc, PrefSeen};
use crate::plan::{Mode, OnError, Step, plan};
use crate::proto::{Reply, Request};

/// A request from the pipe with the switch generation it saw, and where
/// its reply goes.
#[derive(Debug)]
pub struct Job {
    pub req: Request,
    pub generation: Generation,
    pub reply: SyncSender<Reply>,
}

/// A request queued before a switch began is answered as during it; a dev
/// or live entry only once the fence moved (#42): queued before or during
/// the start's checks, it runs after them.
fn stale(req: &Request, seen: Generation, v: &View) -> Option<Reply> {
    match req {
        Request::Status | Request::Subscribe => None,
        Request::Dev { .. } | Request::Live { .. } | Request::Cutover { .. } => {
            (seen.fence != v.fence).then(|| v.reply(false, &fenced(seen, v)))
        }
        _ if seen.epoch == v.epoch => None,
        Request::Event { dry_run: false } => Some(v.event_reply("a switch ran meanwhile")),
        _ => Some(v.reply(false, while_switching(req))),
    }
}

/// Why a dev or live entry queued before the fence moved does not run: the
/// switch begun last, when one began since it was queued and it was not the
/// start's checks (they move no fence), else the "ide event" that came
/// meanwhile.
fn fenced(seen: Generation, v: &View) -> String {
    match v.began {
        Some((from, to)) if seen.epoch != v.epoch && !v.start_checks => format!(
            "busy: a switch ran meanwhile ({} → {})",
            mode_name(from),
            mode_name(to)
        ),
        _ => "busy: a switch to event was asked meanwhile".to_owned(),
    }
}

/// A dev or live entry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    to: Mode,
    build: Option<String>,
    trial: bool,
    dry_run: bool,
}

/// Handles one request on the daemon thread.
pub fn handle(pc: &mut dyn Pc, g: &mut Guard, req: Request, seen: Generation) -> Reply {
    let v = g.shared.view();
    if seen != v.generation()
        && let Some(reply) = stale(&req, seen, &v)
    {
        info!("a request answered as stale: {}", reply.detail);
        return reply;
    }
    g.report.clear();
    let (ok, detail) = match req {
        Request::Status => (true, status_text(g)),
        Request::Subscribe => (true, "subscriptions are served by the pipe".to_owned()),
        Request::Event { dry_run: true } => dry_event(pc),
        Request::Event { dry_run: false } => event_now(pc, g),
        Request::Dev { build, dry_run } => entry(
            pc,
            g,
            Entry {
                to: Mode::Dev,
                build,
                trial: false,
                dry_run,
            },
        ),
        Request::Live {
            build,
            trial,
            dry_run,
        } => entry(
            pc,
            g,
            Entry {
                to: Mode::Live,
                build,
                trial,
                dry_run,
            },
        ),
        Request::Cutover { build, dry_run } => cutover(pc, g, &build, dry_run),
        Request::Install { zip } => install_bundle(g, Path::new(&zip)),
        Request::Activate { sha } => activate(pc, g, &sha),
        Request::TestSignal {
            input,
            dbfs,
            ttl_s,
            listen,
        } => test_signal(pc, g, &input, dbfs, ttl_s, listen),
        Request::Report { sha, hil, detail } => report(g, &sha, &hil, &detail),
        Request::JobBegin { run } => job_begin(g, run),
        Request::JobEnd { run } => job_end(g, run),
        Request::InstallSite { path } => install_site(pc, g, &path),
        Request::ForceReopen => match g.need_dev("force-reopen") {
            Ok(()) => outcome(pc.engine_force_reopen(), "the engine reopened the driver"),
            Err(why) => (false, why),
        },
        Request::InjectFault => inject_fault(pc, g),
        Request::InjectSeh => inject_seh(pc, g),
        Request::InjectPark => inject_park(pc, g),
        Request::RunnerStop => runner_stop(pc, g),
        Request::ProbeTask => outcome(pc.probe_task(), "the probe task ended with 0"),
        Request::RehearseTeardown => rehearse(pc, g),
        Request::AlarmTest => alarm_test(pc, g),
        Request::AlarmAck { id } => {
            if g.alarms.ack(id) {
                g.save();
                (true, format!("alarm {id} acknowledged"))
            } else {
                (false, format!("no alarm {id}"))
            }
        }
        Request::Quit => {
            g.quit = true;
            (
                true,
                "the guard stops; its children keep running".to_owned(),
            )
        }
    };
    send_notices(pc, g);
    g.look(pc);
    g.reply(ok, &detail)
}

fn plan_text(steps: &[Step]) -> String {
    let names: Vec<String> = steps.iter().map(|s| format!("{s:?}")).collect();
    names.join(", ")
}

/// `event --dry-run`: the plan from the facts, nothing changed.
pub(super) fn dry_event(pc: &mut dyn Pc) -> (bool, String) {
    let facts = pc.facts();
    let steps = plan(Mode::Event, &facts);
    (true, format!("dry run: {}", plan_text(&steps)))
}

/// "ide event": the event plan from the current mode (in `event` its
/// checks, plus a restart of what runs but does not serve).
pub(super) fn event_now(pc: &mut dyn Pc, g: &mut Guard) -> (bool, String) {
    let from = g.state.mode;
    let out = run_switch(pc, g, from, Mode::Event);
    (
        out == Outcome::Done && g.state.mode == Mode::Event,
        switch_text(Mode::Event, out, g.state.mode, &g.owner_failed),
    )
}

/// A dev or live entry runs at once: only the owner's signal decides
/// whether the PC may change, so no step waits for a quiet stage or refuses
/// on activity (#38, owner 2026-10-06). The lifecycle's gates decide
/// whether it may run and on which build (S8: `lifecycle::entry`); the
/// build becomes the active bundle, never the pin, and the lifecycle's
/// change (a maintenance build, the pin it ends on) holds only once the
/// entry is in.
fn entry(pc: &mut dyn Pc, g: &mut Guard, e: Entry) -> (bool, String) {
    let ask = lifecycle::Ask {
        to: e.to,
        build: e.build.as_deref(),
        trial: e.trial,
    };
    let entered = match lifecycle::entry(&g.state.lifecycle, ask, |sha| g.state.bundles.get(sha)) {
        Ok(entered) => entered,
        Err(why) => return (false, why),
    };
    if e.dry_run {
        return dry_entry(pc, g, &e, entered.runs, entered.note);
    }
    if let Some(sha) = entered.runs {
        pc.set_bundle(Some(sha.as_str()));
        g.state.set_active(&sha);
    }
    g.trial = e.trial;
    let from = g.state.mode;
    let out = run_switch(pc, g, from, e.to);
    let done = out == Outcome::Done && g.state.mode == e.to;
    if done {
        g.state.lifecycle = entered.lifecycle;
        if let Some(n) = entered.note {
            g.info(n);
        }
        g.save();
    }
    (done, switch_text(e.to, out, g.state.mode, &g.owner_failed))
}

/// `dev|live --dry-run`: the plan and the read-only checks (the precheck's
/// bundle, PWA notification subscriptions, foreign engine and app exe),
/// nothing changed. `runs`: the build the entry would run; `note`: what it
/// would decide about the pin.
fn dry_entry(
    pc: &mut dyn Pc,
    g: &mut Guard,
    e: &Entry,
    runs: Option<String>,
    note: Option<String>,
) -> (bool, String) {
    // `trial` decides only the precheck (below), never a step of the plan.
    let steps = lifecycle::plan(&g.state.lifecycle, e.to, &pc.facts());
    let bundle = runs
        .or_else(|| g.state.active_bundle().map(str::to_owned))
        .unwrap_or_else(|| "none".to_owned());
    let check = pc.precheck(e.to, e.trial);
    let verdict = match &check {
        Ok(None) => "ok".to_owned(),
        Ok(Some(note)) => format!("ok; {note}"),
        Err(why) => why.to_string(),
    };
    let pin = note.map_or_else(String::new, |n| format!("; {n}"));
    (
        check.is_ok(),
        format!(
            "dry run: {}; bundle {bundle}; precheck {verdict}{pin}",
            plan_text(&steps)
        ),
    )
}

/// The teardown half of the event plan without REAPER or the app (design
/// §11): engine, server and tray stop, tuning `exit`, the preference check,
/// each with the event error policy; then the module must be unheld and the
/// preference original, and dev is entered again. Not a switch. Ports
/// 80/443 are checked by `ServerStop` itself when a server ran. Refused
/// inside a HIL job: the dev re-entry cancels the jobs and stops the runner
/// that runs the job (as `runner-stop` is refused).
fn rehearse(pc: &mut dyn Pc, g: &mut Guard) -> (bool, String) {
    if let Err(why) = g.need_dev("rehearse-teardown") {
        return (false, why);
    }
    if let Some(run) = g.state.job {
        return (
            false,
            format!("HIL job {run} runs: the rehearsal's dev entry would stop its runner"),
        );
    }
    let f = pc.facts();
    let mut steps = Vec::new();
    if f.engine {
        steps.push(Step::EngineStop);
    }
    if f.server {
        steps.push(Step::ServerStop);
    }
    if f.tray {
        steps.push(Step::TrayStop);
    }
    steps.extend([Step::TuningExit, Step::PrefCheck]);
    for step in steps {
        let Err(e) = run_step(pc, g, step, Mode::Event) else {
            continue;
        };
        let (why, health, policy) = failure(pc, g, Mode::Event, step, &e);
        match policy {
            OnError::KeepServing => {
                g.alarm(
                    step,
                    &format!("rehearsal: {why}; engine healthy, iemmixer keeps serving"),
                    true,
                );
                return (false, format!("rehearsal stopped at {step:?}: {why}"));
            }
            OnError::StopAskOwner | OnError::ContinueAskOwner | OnError::SkipAskOwner(_) => {
                g.alarm(step, &format!("rehearsal: {why}; health {health:?}"), true);
                return (false, format!("rehearsal stopped at {step:?}: {why}"));
            }
            OnError::Unwind | OnError::Continue => {
                g.alarm(step, &format!("rehearsal: {why}"), false);
            }
        }
    }
    let after = pc.facts();
    let mut bad: Vec<String> = Vec::new();
    if after.engine || after.server || after.tray {
        bad.push("iemmixer processes still run".to_owned());
    }
    if after.reaper_holds_module || after.other_module_holder {
        bad.push("the driver module is held".to_owned());
    }
    match pc.pref_check() {
        Ok(PrefSeen::Original(0)) => {}
        Ok(PrefSeen::Original(writes)) => {
            bad.push(format!("the preference needed {writes} writes"));
        }
        Ok(PrefSeen::Held(held)) => bad.push(format!("the preference: {}", held.text())),
        Err(e) => bad.push(format!("the preference: {e}")),
    }
    match pc.web_ports() {
        Ok((None, None)) => {}
        Ok((http, https)) => {
            let pid = |p: Option<u32>| p.map_or_else(|| "free".to_owned(), |p| p.to_string());
            bad.push(format!(
                "ports 80/443 are still held (80: {}, 443: {})",
                pid(http),
                pid(https)
            ));
        }
        Err(e) => bad.push(format!("ports 80/443: {e}")),
    }
    let verdict = if bad.is_empty() {
        "teardown clean: module unheld, preference original, ports 80/443 free".to_owned()
    } else {
        format!("teardown problems: {}", bad.join("; "))
    };
    if !bad.is_empty() {
        g.raise(None, &format!("rehearsal: {verdict}"), false);
    }
    g.hold_unwind = true;
    let out = run_switch(pc, g, Mode::Dev, Mode::Dev);
    g.hold_unwind = false;
    (
        bad.is_empty() && out == Outcome::Done && g.state.mode == Mode::Dev,
        format!(
            "{verdict}; {}",
            switch_text(Mode::Dev, out, g.state.mode, &g.owner_failed)
        ),
    )
}
