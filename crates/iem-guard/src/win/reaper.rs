//! REAPER (design §5.2): its web control, the stage meters, the save and
//! quit by its own actions, its start, and the facts of the handover.
//! Nothing here ends REAPER.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use iem_win::process::{self, Handle};
use iem_win::window;
use tracing::{info, warn};

use super::procs;
use super::{WinPc, tasks, web};
use crate::cancel::Cancel;
use crate::effects::app::{holders_text, one_pid};
use crate::effects::{reaper, tasks as task_names, web as decide};
use crate::handover::{self, Bridge, ReaperFacts};
use crate::pc::{R, StepError};

/// Save project, and quit.
const SAVE: &str = "40026";
const QUIT: &str = "40004";
/// How often the stage meters are read.
const METER_POLL: Duration = Duration::from_millis(250);
/// REAPER's web control silent this long fails a meter read.
const SILENT: Duration = Duration::from_secs(5);
/// The project loads within this (design §11: ≤ 120 s).
const LOAD: Duration = Duration::from_secs(120);
/// The meter bridge's heartbeat moves within this after a trigger.
const HEARTBEAT: Duration = Duration::from_secs(20);

fn get(pc: &WinPc, command: &str) -> Result<String, String> {
    let url = reaper::url(&pc.s.guard.reaper_url, command);
    match web::get(pc, &url)? {
        (status, body) if decide::is_success(status) => Ok(body),
        (status, _) => Err(format!("{url}: HTTP {status}")),
    }
}

/// Runs a REAPER action. A failed request is only logged: the step's own
/// observation decides (a slow answer may still act).
fn action(pc: &WinPc, id: &str) {
    match get(pc, id) {
        Ok(_) => info!("REAPER action {id} sent"),
        Err(e) => warn!("REAPER action {id}: {e}"),
    }
}

fn extstate(pc: &WinPc, section_key: &str) -> R<String> {
    get(pc, &reaper::extstate_command(section_key))
        .map(|body| reaper::extstate(&body))
        .map_err(StepError::Failed)
}

fn mtime(path: &Path) -> R<SystemTime> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map_err(|e| procs::failed(&path.display().to_string(), e))
}

fn reaper_pid(pc: &WinPc) -> R<u32> {
    one_pid(&procs::list(pc).reaper, "REAPER").map_err(StepError::Failed)
}

fn holders(pc: &WinPc) -> R<Vec<(u32, String)>> {
    process::module_holders(&pc.s.card.module)
        .map_err(|e| procs::failed("the driver module's holders", e))
}

/// The titles of REAPER's visible dialogs (`pid`).
fn dialog_titles(pid: u32) -> R<Vec<String>> {
    window::dialog_titles(pid).map_err(|e| procs::failed("REAPER's windows", e))
}

/// Fails with `why` while REAPER shows a dialog other than its evaluation
/// notice (`handover::dialogs`): that dialog needs a person, and quitting
/// would leave it up. The notice is left open and never stops the save or
/// the quit (#9, 2026-09-28). The titles go to the log only.
fn no_blocking_dialog(pid: u32, why: &str) -> R<()> {
    let blocking = handover::dialogs(&dialog_titles(pid)?).blocking;
    if blocking.is_empty() {
        Ok(())
    } else {
        warn!("REAPER's dialogs that need a person: {blocking:?}");
        Err(StepError::failed(why))
    }
}

/// The loudest reading of each stage track over `seconds`;
/// `f64::NEG_INFINITY` for a track that gave none.
fn sample(pc: &WinPc, seconds: u32, c: &Cancel) -> R<Vec<f64>> {
    let stage = &pc.s.guard.stage_tracks;
    let mut loudest = vec![f64::NEG_INFINITY; stage.len()];
    let end = Instant::now() + Duration::from_secs(u64::from(seconds));
    let mut answered = Instant::now();
    loop {
        match get(pc, "TRACK") {
            Ok(body) => {
                reaper::keep_max(&mut loudest, &reaper::stage_peaks(&body, stage));
                answered = Instant::now();
            }
            Err(e) => warn!("REAPER's meters: {e}"),
        }
        if answered.elapsed() >= SILENT {
            return Err(StepError::failed(format!(
                "REAPER's web control did not answer for {} s",
                SILENT.as_secs()
            )));
        }
        if Instant::now() >= end {
            return Ok(loudest);
        }
        c.sleep(METER_POLL)?;
    }
}

/// The interlock's reading: every stage track must have given meters, or
/// its silence proves nothing.
pub(super) fn meters(pc: &WinPc, seconds: u32, c: &Cancel) -> R<Vec<f64>> {
    let loudest = sample(pc, seconds, c)?;
    let silent = reaper::unmetered(&pc.s.guard.stage_tracks, &loudest);
    if silent.is_empty() {
        Ok(loudest)
    } else {
        Err(StepError::failed(format!(
            "REAPER gave no meters for stage tracks {silent:?} (not armed?)"
        )))
    }
}

/// 40026; the project's mtime changes ≤ 15 s; no dialog but the evaluation
/// notice; 40004; gone ≤ 30 s; the driver module unheld.
pub(super) fn save_quit(pc: &WinPc, c: &Cancel) -> R<()> {
    let pid = reaper_pid(pc)?;
    let handle = Handle::open_waitable(pid).map_err(|e| procs::failed("REAPER", e))?;
    let project = &pc.s.guard.reaper_project;
    let before = mtime(project)?;
    action(pc, SAVE);
    // A dialog during the save (a save error, a prompt) holds it and needs
    // a person: the step fails at once and says so; 40004 is never sent.
    let saved = procs::poll(Duration::from_secs(15), c, || {
        if mtime(project)? != before {
            return Ok(true);
        }
        no_blocking_dialog(
            pid,
            "a REAPER dialog is open during the save: REAPER is not quit",
        )?;
        Ok(false)
    })?;
    if !saved {
        return Err(StepError::failed(
            "REAPER did not save the project within 15 s",
        ));
    }
    // A dialog after the save needs a person; quitting now would leave it
    // up (the 2026-09-27 lesson on #9).
    no_blocking_dialog(
        pid,
        "a REAPER dialog is open after the save: REAPER is not quit",
    )?;
    action(pc, QUIT);
    if procs::wait_exit(&handle, Duration::from_secs(30), c)?.is_none() {
        return Err(StepError::failed("REAPER did not quit within 30 s"));
    }
    let left = holders(pc)?;
    if !left.is_empty() {
        return Err(StepError::failed(format!(
            "REAPER quit, but the driver module is still held by {}",
            holders_text(&left)
        )));
    }
    info!("REAPER saved and quit");
    Ok(())
}

/// Our task (or the direct start); never with an engine or another holder
/// of the driver module (I3).
pub(super) fn start(pc: &WinPc) -> R<()> {
    let p = procs::list(pc);
    if !p.engine.is_empty() {
        return Err(StepError::failed(
            "an engine runs: REAPER may not start (I3)",
        ));
    }
    if !p.reaper.is_empty() {
        return Err(StepError::failed("REAPER already runs"));
    }
    let held = holders(pc)?;
    if !held.is_empty() {
        return Err(StepError::failed(format!(
            "the driver module is held by {}: REAPER may not start (I3)",
            holders_text(&held)
        )));
    }
    if pc.s.guard.start_direct {
        procs::start_detached(&pc.s.pc.reaper_exe)
    } else {
        tasks::run_task(task_names::REAPER)
    }
}

/// ≤ 120 s for the track count; REAPER's visible dialogs by title (the
/// verdict sorts out its evaluation notice); the meter bridge at most once
/// and only while its state is empty (the 2026-09-27 lesson); the
/// heartbeat; the driver module; a few seconds of stage meters. A project that never
/// reaches the track count (another project, still loading) leaves the
/// bridge alone: the facts say so and the verdict fails on the tracks.
pub(super) fn facts(pc: &WinPc, c: &Cancel) -> R<ReaperFacts> {
    let g = &pc.s.guard;
    let mut tracks = None;
    let loaded = procs::poll(LOAD, c, || {
        if let Ok(body) = get(pc, "NTRACK") {
            tracks = reaper::ntrack(&body);
        }
        Ok(handover::project_loaded(tracks, g.reaper_tracks))
    })?;
    let pid = reaper_pid(pc)?;
    let dialogs = dialog_titles(pid)?;
    if !dialogs.is_empty() {
        info!("REAPER's dialogs: {dialogs:?}");
    }
    if !loaded {
        warn!(
            "REAPER reports {tracks:?} tracks, not {}: the meter bridge is left alone",
            g.reaper_tracks
        );
        return Ok(ReaperFacts {
            tracks,
            expected_tracks: g.reaper_tracks,
            dialogs,
            heartbeat_advanced: false,
            holds_module: module_held_by(pc, pid)?,
            peaks: Vec::new(),
        });
    }
    match handover::bridge(&extstate(pc, &g.bridge_state)?) {
        Bridge::Running => info!("the meter bridge runs"),
        Bridge::TriggerOnce => action(pc, &g.bridge_action),
        Bridge::Refuse(why) => return Err(StepError::Failed(why)),
    }
    let first = extstate(pc, &g.bridge_heartbeat)?;
    let heartbeat_advanced = procs::poll(HEARTBEAT, c, || {
        Ok(extstate(pc, &g.bridge_heartbeat)? != first)
    })?;
    let holds_module = module_held_by(pc, pid)?;
    let peaks = sample(pc, 3, c)?;
    Ok(ReaperFacts {
        tracks,
        expected_tracks: g.reaper_tracks,
        dialogs,
        heartbeat_advanced,
        holds_module,
        peaks,
    })
}

/// Whether REAPER (`pid`) holds the driver module.
fn module_held_by(pc: &WinPc, pid: u32) -> R<bool> {
    Ok(holders(pc)?.iter().any(|(holder, _)| *holder == pid))
}
