//! REAPER (design §5.2): its web control, the stage meters, the save and
//! quit by its own actions, its crash on quit (#10: Windows Error Reporting
//! may hold the crashed process), its start, and the facts of the handover.
//! Nothing here ends REAPER or Windows Error Reporting: the guard only
//! waits for them.

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
use crate::effects::reaper::{CRASH_HOLD, Load, QUIT_WAIT, QuitStep, ReaperState};
use crate::effects::{reaper, tasks as task_names, web as decide};
use crate::handover::{self, Bridge, ReaperFacts, ReaperProcs};
use crate::pc::{Procs, R, StepError};

/// Save project, and quit.
const SAVE: &str = "40026";
const QUIT: &str = "40004";
/// How often the stage meters are read.
const METER_POLL: Duration = Duration::from_millis(250);
/// REAPER's web control silent this long fails a meter read.
const SILENT: Duration = Duration::from_secs(5);
/// The project loads within this: 60 s since S7 (#10; the S6 design note's
/// §5.2 step 7 and §11 set ≤ 120 s before the PC was measured).
/// `reaper_handover` took 6.0–6.3 s over six runs on 2026-10-08 (three
/// switch tests, the live → event, two event entries), so 60 s is about
/// 10× the worst, inside spec §4.3's ≤ 90 s for the whole handover.
const LOAD: Duration = Duration::from_secs(60);
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

/// The REAPER this guard asked to quit (40004) and has not seen gone
/// (`WinPc::quitting`, #10): the handover waits for it before it starts
/// REAPER, and the event plan's quit only waits for it.
pub(super) struct Quitting {
    handle: Handle,
    /// When 40004 was sent.
    asked: Instant,
}

/// The REAPER this guard asked to quit while it still counts as ending
/// (`reaper::asked_still_ending`): its pid, and whether its process has
/// ended since. A memory past that is dropped: from then on only Windows
/// Error Reporting makes a REAPER ending.
fn asked(pc: &mut WinPc) -> Option<(u32, bool)> {
    let (pid, ago, ended) = pc.quitting.as_ref().map(|q| {
        let ended = matches!(q.handle.wait(Duration::ZERO), Ok(Some(_)));
        (q.handle.pid(), q.asked.elapsed(), ended)
    })?;
    if reaper::asked_still_ending(ago) {
        Some((pid, ended))
    } else {
        info!(
            "REAPER (pid {pid}) was asked to quit {} s ago: no longer counted as ending",
            ago.as_secs()
        );
        pc.quitting = None;
        None
    }
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

/// `ReaperSaveQuit` (#10, `reaper::quit_step`): a REAPER that is already
/// ending (this guard asked it to quit, or Windows Error Reporting reports
/// its crash) gets no save and no quit, only the wait for it to be gone;
/// the REAPER this guard asked to quit that has ended is a quit done;
/// anything else is saved and quit ([`save_then_quit`]). An unreadable
/// process list fails the step.
pub(super) fn save_quit(pc: &mut WinPc, c: &Cancel) -> R<()> {
    let asked = asked(pc);
    let pids =
        process::pids(&pc.images.reaper).map_err(|e| procs::failed("the process list", e))?;
    let one = match pids.as_slice() {
        [pid] => Some(*pid),
        _ => None,
    };
    let listed = match (pids.len(), one) {
        (0, _) => None,
        (_, Some(pid)) => Some(reaper::reaper_state(
            asked == Some((pid, false)),
            wer_for(pid).is_some(),
            has_ended(pid),
        )),
        // Several: saved and quit as before (the save refuses them).
        (_, None) => Some(ReaperState::Running),
    };
    let asked_ended = asked.is_some_and(|(_, ended)| ended);
    match (reaper::quit_step(listed, asked_ended), one) {
        (QuitStep::SaveQuit, _) => save_then_quit(pc, c),
        (QuitStep::AwaitEnd, Some(pid)) => await_ending(pc, pid, c),
        (QuitStep::AwaitEnd, None) => Err(StepError::failed(
            "REAPER is ending, but its process could not be told apart",
        )),
        (QuitStep::Done, _) => {
            info!("REAPER, asked to quit by this guard (its save verified then), has ended");
            pc.quitting = None;
            unheld(pc)
        }
    }
}

/// A REAPER that is already ending (#10): no save and no quit (it cannot
/// answer them), only the wait for it to be gone, bounded by `CRASH_HOLD`;
/// "ide event" ends the wait. Then the driver module unheld.
fn await_ending(pc: &mut WinPc, pid: u32, c: &Cancel) -> R<()> {
    warn!(
        "REAPER (pid {pid}) is already ending (asked to quit before, or a crash Windows Error \
         Reporting reports): no save or quit is sent; waiting up to {} s for it to be gone",
        CRASH_HOLD.as_secs()
    );
    match Handle::open_waitable(pid) {
        Ok(handle) => {
            let start = Instant::now();
            if procs::wait_exit(&handle, CRASH_HOLD, c)?.is_none() {
                return Err(StepError::failed(format!(
                    "REAPER is still ending {} s later: it is not saved or quit again",
                    CRASH_HOLD.as_secs()
                )));
            }
            info!(
                "REAPER (pid {pid}) is gone {} ms later",
                start.elapsed().as_millis()
            );
        }
        Err(e) => info!("REAPER (pid {pid}) cannot be opened any more ({e}): it has ended"),
    }
    pc.quitting = None;
    unheld(pc)
}

/// The driver module unheld once REAPER is gone.
fn unheld(pc: &WinPc) -> R<()> {
    let left = holders(pc)?;
    if left.is_empty() {
        Ok(())
    } else {
        Err(StepError::failed(format!(
            "REAPER quit, but the driver module is still held by {}",
            holders_text(&left)
        )))
    }
}

/// 40026; the project's mtime changes ≤ 15 s; no dialog but the evaluation
/// notice; 40004; gone ≤ 30 s, or within `CRASH_HOLD` more when Windows
/// Error Reporting reports its crash on quit (#10); the driver module
/// unheld. A REAPER that has not ended when the step fails or is pre-empted
/// is remembered (`WinPc::quitting`): the handover waits for it.
fn save_then_quit(pc: &mut WinPc, c: &Cancel) -> R<()> {
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
    let asked = Instant::now();
    let ended = await_quit(&handle, asked, c);
    if ended.is_err() {
        // Still ending, or the wait was pre-empted: the event plan's
        // handover (or quit) that follows waits for it before it starts
        // REAPER.
        pc.quitting = Some(Quitting { handle, asked });
    }
    let code = ended?;
    pc.quitting = None;
    if reaper::crashed(code) {
        warn!(
            "REAPER (pid {pid}) crashed on quit (exit {code:#x}); the project's save was \
             verified before the quit"
        );
    }
    unheld(pc)?;
    info!("REAPER saved and quit");
    Ok(())
}

/// After 40004 (sent at `asked`): REAPER's exit code once it is gone within
/// `QUIT_WAIT`. A REAPER still there whose crash Windows Error Reporting
/// reports (REAPER crashes on quit routinely, #10) is waited for up to
/// `CRASH_HOLD` more, and the log says how long WER held it; "ide event"
/// ends either wait.
fn await_quit(handle: &Handle, asked: Instant, c: &Cancel) -> R<u32> {
    if let Some(code) = procs::wait_exit(handle, QUIT_WAIT, c)? {
        return Ok(code);
    }
    let pid = handle.pid();
    let Some(wer) = wer_for(pid) else {
        return Err(StepError::failed(format!(
            "REAPER did not quit within {} s",
            QUIT_WAIT.as_secs()
        )));
    };
    warn!(
        "REAPER (pid {pid}) crashed on quit: Windows Error Reporting (WerFault pid {wer}) \
         holds it; waiting up to {} s more for it to be gone",
        CRASH_HOLD.as_secs()
    );
    let held = Instant::now();
    match procs::wait_exit(handle, CRASH_HOLD, c)? {
        Some(code) => {
            info!(
                "REAPER (pid {pid}) is gone (exit {code:#x}) {} ms after 40004; Windows Error \
                 Reporting held it {} ms past the {} s quit bound",
                asked.elapsed().as_millis(),
                held.elapsed().as_millis(),
                QUIT_WAIT.as_secs()
            );
            Ok(code)
        }
        None => Err(StepError::failed(format!(
            "REAPER crashed on quit and Windows Error Reporting still holds it after {} + {} s",
            QUIT_WAIT.as_secs(),
            CRASH_HOLD.as_secs()
        ))),
    }
}

/// The WerFault process that reports a crash of `pid` (Windows Error
/// Reporting holds the crashed process until its report is done), if any.
/// An unreadable process list or command line is logged and counts as none.
fn wer_for(pid: u32) -> Option<u32> {
    let wer = match process::pids(reaper::WER_IMAGE) {
        Ok(pids) => pids,
        Err(e) => {
            warn!("Windows Error Reporting's processes could not be read: {e}");
            return None;
        }
    };
    wer.into_iter().find(|&w| match process::command_line(w) {
        Ok(line) => reaper::wer_reports(&line, pid),
        Err(e) => {
            warn!("the command line of WerFault (pid {w}) could not be read: {e}");
            false
        }
    })
}

/// Whether the process `pid` the list showed has ended since (between the
/// list and this look). One that cannot be opened counts as not ended, as
/// the list says.
fn has_ended(pid: u32) -> bool {
    Handle::open_waitable(pid).is_ok_and(|h| matches!(h.wait(Duration::ZERO), Ok(Some(_))))
}

/// REAPER's processes for the handover's first part (#10), each read by
/// `reaper::reaper_state`: gone, still ending (asked to quit, or a crash
/// Windows Error Reporting reports) or running. An unreadable process list
/// fails the step: it never reads as "no REAPER", which would start a
/// second one.
pub(super) fn seen(pc: &mut WinPc) -> R<ReaperProcs> {
    let asked = asked(pc);
    if let Some((pid, true)) = asked {
        info!("REAPER (pid {pid}), asked to quit, has ended");
        pc.quitting = None;
    }
    let pids =
        process::pids(&pc.images.reaper).map_err(|e| procs::failed("the process list", e))?;
    let mut out = ReaperProcs::default();
    for pid in pids {
        let wer = wer_for(pid);
        let state =
            reaper::reaper_state(asked == Some((pid, false)), wer.is_some(), has_ended(pid));
        match (state, wer) {
            (ReaperState::Running, _) => out.running += 1,
            (ReaperState::Ending, Some(w)) => {
                info!(
                    "REAPER (pid {pid}) is still ending: Windows Error Reporting (WerFault pid \
                     {w}) reports its crash"
                );
                out.ending += 1;
            }
            (ReaperState::Ending, None) => {
                info!("REAPER (pid {pid}) is still ending: asked to quit, it has not ended");
                out.ending += 1;
            }
            (ReaperState::Gone, _) => info!("REAPER (pid {pid}) has ended"),
        }
    }
    Ok(out)
}

/// Waits up to `CRASH_HOLD` for every REAPER that is still ending to be
/// gone ("ide event" ends the wait), logging how long each took. Then the
/// guard's quit request no longer marks a REAPER as ending (one that never
/// quit is checked like any other); a crash Windows Error Reporting still
/// reports does.
pub(super) fn await_end(pc: &mut WinPc, c: &Cancel) -> R<()> {
    let start = Instant::now();
    let asked = asked(pc);
    let pids =
        process::pids(&pc.images.reaper).map_err(|e| procs::failed("the process list", e))?;
    let mut held = Vec::new();
    for pid in pids {
        // The REAPER asked to quit is waited for through its own handle.
        if asked.is_some_and(|(asked_pid, _)| asked_pid == pid) {
            continue;
        }
        let Ok(h) = Handle::open_waitable(pid) else {
            continue;
        };
        let ended = matches!(h.wait(Duration::ZERO), Ok(Some(_)));
        if reaper::reaper_state(false, wer_for(pid).is_some(), ended) == ReaperState::Ending {
            held.push(h);
        }
    }
    if let Some(q) = pc.quitting.as_ref() {
        wait_gone(&q.handle, start, c)?;
    }
    for h in &held {
        wait_gone(h, start, c)?;
    }
    pc.quitting = None;
    Ok(())
}

/// Waits for `h`'s process to end until `CRASH_HOLD` after `start`.
fn wait_gone(h: &Handle, start: Instant, c: &Cancel) -> R<()> {
    let left = CRASH_HOLD.saturating_sub(start.elapsed());
    match procs::wait_exit(h, left, c)? {
        Some(code) => info!(
            "REAPER (pid {}) is gone (exit {code:#x}) {} ms after the handover began to wait",
            h.pid(),
            start.elapsed().as_millis()
        ),
        None => warn!(
            "REAPER (pid {}) is still there {} s after the handover began to wait",
            h.pid(),
            CRASH_HOLD.as_secs()
        ),
    }
    Ok(())
}

/// Our task (or the direct start); never with an engine or another holder
/// of the driver module (I3). An unreadable process list refuses the start
/// (#10: it never reads as "no REAPER, no engine").
pub(super) fn start(pc: &WinPc) -> R<()> {
    let all = process::list().map_err(|e| procs::failed("the process list", e))?;
    let p = Procs::from_list(&all, &pc.images);
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

/// ≤ [`LOAD`] (60 s) for the track count, ended at once when REAPER's
/// process has ended (#10); REAPER's visible dialogs by title (the
/// verdict sorts out its evaluation notice); the meter bridge at most once
/// and only while its state is empty (the 2026-09-27 lesson); the
/// heartbeat; the driver module; a few seconds of stage meters. A project that never
/// reaches the track count (another project, still loading) leaves the
/// bridge alone: the facts say so and the verdict fails on the tracks.
pub(super) fn facts(pc: &WinPc, c: &Cancel) -> R<ReaperFacts> {
    let g = &pc.s.guard;
    let mut tracks = None;
    // REAPER's process, watched from the moment it shows (a start may show
    // late): its end ends the wait at once (#10, 2026-10-08: the handover
    // waited the full bound on a REAPER that had crashed).
    let mut watched: Option<Handle> = None;
    let loaded = procs::poll(LOAD, c, || {
        if let Ok(body) = get(pc, "NTRACK") {
            tracks = reaper::ntrack(&body);
        }
        if watched.is_none() {
            let p = procs::list(pc);
            if let [pid] = p.reaper.as_slice() {
                watched = Handle::open_waitable(*pid).ok();
            }
        }
        let ended = watched
            .as_ref()
            .and_then(|h| h.wait(Duration::ZERO).ok().flatten());
        match reaper::load_look(handover::project_loaded(tracks, g.reaper_tracks), ended) {
            Load::Loaded => Ok(true),
            Load::Waiting => Ok(false),
            Load::Ended(code) => Err(StepError::failed(format!(
                "REAPER's process ended (exit {code:#x}) before its project loaded"
            ))),
        }
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
