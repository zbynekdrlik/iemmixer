//! The once-a-second watch (P10: the process list only): the exits of the
//! guard's children, the respawn and the crash loop, REAPER or the app
//! appearing in dev or live, a parked engine, the hourly drift and the end
//! of the Windows session.

use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use super::runner::pref_step;
use super::startup::take_logon;
use super::{DRIFT_EVERY, Guard, PARKED_ALARM, run_switch, send_notices};
use crate::cancel::Cancel;
use crate::crash::{self, After, CrashLoop};
use crate::lifecycle::{self, Fallback};
use crate::pc::{Kid, Pc, Procs};
use crate::plan::Mode;

/// The once-a-second watch (P10: the process list only): exits of our
/// children, REAPER or the app appearing in dev/live, a due respawn, hourly
/// drift, the end of the session.
pub fn tick(pc: &mut dyn Pc, g: &mut Guard, at: Instant) {
    // What the watch did is logged; no request reads it.
    g.report.clear();
    let p = pc.procs();
    g.look(pc);
    for (kid, code) in &p.exited {
        exited(pc, g, *kid, *code, at);
    }
    if g.session_ending.load(Ordering::SeqCst) && !g.session_done {
        session_end(pc, g);
    }
    watch_band(g, &p);
    watch_parked(g);
    if g.respawn_at.is_some_and(|due| due <= at) {
        g.respawn_at = None;
        respawn(pc, g);
    }
    if g.last_drift
        .is_none_or(|t| at.saturating_duration_since(t) >= DRIFT_EVERY)
    {
        g.drift(pc, at);
        take_logon(pc, g);
    }
    send_notices(pc, g);
}

fn exited(pc: &mut dyn Pc, g: &mut Guard, kid: Kid, code: Option<i32>, at: Instant) {
    let session = g.session_ending.load(Ordering::SeqCst);
    info!("the {} ended with {code:?}", kid.id());
    match kid {
        Kid::Engine => engine_exited(pc, g, code, at, session),
        Kid::Server if g.state.mode != Mode::Event && !session => {
            g.raise(None, &format!("the server ended ({code:?})"), false);
        }
        Kid::Server | Kid::Tray | Kid::Runner => {}
    }
    g.state.pids = pc.children();
    g.save();
}

fn engine_exited(pc: &mut dyn Pc, g: &mut Guard, code: Option<i32>, at: Instant, session: bool) {
    g.last_exit = code;
    // #32 minor-4: a busy state directory is no crash; it is tried again,
    // up to `crash::BUSY_LIMIT` times in a row, then each counts as one
    // (F3-r4 3: something the guard does not watch holds it).
    let streak = g.crash.busy(!session && code == Some(crash::STATE_BUSY));
    let busy = !session && crash::busy_retry(code, streak);
    let abnormal = !session && !busy && !matches!(code, Some(0 | 2 | 3));
    let looped = abnormal && g.crash.record(at);
    if streak == crash::BUSY_ALARM {
        g.raise(
            None,
            &format!(
                "the engine's state directory stayed in use {} times in a row (exit {}): \
                 starting it again",
                crash::BUSY_ALARM,
                crash::STATE_BUSY
            ),
            false,
        );
    }
    let mode = g.state.mode;
    let n = g.crash.in_window();
    match crash::after_exit(code, session, looped, n, streak) {
        After::Stay { alarm } => {
            if let Some(why) = alarm {
                g.raise(None, &format!("{why} (exit {code:?})"), false);
            }
        }
        After::Respawn(delay) => {
            if mode != Mode::Event {
                g.respawn_at = Some(at + delay);
            }
        }
        After::Loop => crash_loop(pc, g, mode, n, at),
    }
}

/// A crash loop, as the lifecycle says (S8 design §3.4): back to REAPER
/// (before the cutover), live on the pin (maintenance), the previous pin
/// (prod live), or down with an alarm naming the way back to REAPER
/// (`iemmode event`, the PC stays prod); a pin that may
/// not go live is never a fallback (G8: REAPER from maintenance, down from
/// live). A new engine on a pin starts a new crash window: "if that loops
/// too" counts its own exits.
fn crash_loop(pc: &mut dyn Pc, g: &mut Guard, mode: Mode, n: usize, at: Instant) {
    let (fallback, next) =
        lifecycle::crash_loop(&g.state.lifecycle, mode, |sha| g.state.bundles.get(sha));
    g.state.lifecycle = next;
    let crashed = format!("the engine crashed {n} times in 10 min");
    match fallback {
        Fallback::Event => {
            g.raise(None, &format!("{crashed}: back to REAPER"), false);
            run_switch(pc, g, mode, Mode::Event);
        }
        Fallback::Reaper(why) => {
            g.raise(
                None,
                &format!("{crashed} in maintenance, and {why}: back to REAPER"),
                false,
            );
            run_switch(pc, g, mode, Mode::Event);
        }
        Fallback::Pin(pin) => {
            g.raise(
                None,
                &format!("{crashed} in maintenance: back to live on the pin {pin}"),
                false,
            );
            // The switch starts the pin's engine: no respawn of the
            // session's one, and a new crash window.
            g.respawn_at = None;
            g.crash = CrashLoop::default();
            pc.set_bundle(Some(pin.as_str()));
            g.state.set_active(&pin);
            run_switch(pc, g, mode, Mode::Live);
        }
        Fallback::Previous(sha) => {
            g.raise(
                None,
                &format!("{crashed}: back to the previous pin {sha}"),
                false,
            );
            g.crash = CrashLoop::default();
            pc.set_bundle(Some(sha.as_str()));
            g.state.set_active(&sha);
            g.respawn_at = Some(at);
        }
        Fallback::Down(why) => {
            g.raise(
                None,
                &format!(
                    "{crashed} {why}: the engine stays down; back to REAPER with iemmode \
                     event (the PC stays in prod)"
                ),
                false,
            );
            // Stops respawning: a respawn an earlier exit of this tick asked
            // for is dropped too.
            g.respawn_at = None;
        }
    }
}

fn respawn(pc: &mut dyn Pc, g: &mut Guard) {
    // "ide event" may have come between the exit and the respawn.
    if g.state.mode == Mode::Event {
        return;
    }
    // An engine that ended while it held the card left 32, and the new one
    // refuses the card unless REAPER's original is back (#9 2026-09-28).
    // The old engine is gone, so nothing of ours holds the driver; a failed
    // restore (or a holder) starts nothing, as a failed start.
    let mode = g.state.mode;
    if let Err(e) = pref_step(pc, g, mode) {
        g.raise(
            None,
            &format!("the engine could not be started again: {e}"),
            false,
        );
        return;
    }
    let hil = g.hil_engine(mode);
    match pc.engine_start(false, hil) {
        Ok(pid) => {
            g.spawns += 1;
            info!("the engine was started again, unheld (pid {pid})");
            g.state.pids = pc.children();
            g.save();
        }
        Err(e) => {
            g.raise(
                None,
                &format!("the engine could not be started again: {e}"),
                false,
            );
        }
    }
}

/// REAPER or the predecessor app appearing in dev/live alarms once per
/// appearance; nothing is ended.
fn watch_band(g: &mut Guard, p: &Procs) {
    let up = p.band_up() && g.state.mode != Mode::Event;
    if up && !g.band_seen {
        g.raise(
            None,
            "REAPER or the predecessor app started while iemmixer runs; nothing is ended",
            false,
        );
    }
    g.band_seen = up;
}

/// A parked engine outside a HIL job (#35, supervisor decision of
/// 2026-10-07): its stream stopped with the card held, so nothing plays
/// until the engine ends. One alarm per parked engine, by its state alone
/// (no level, #38); none inside a HIL job (test #2 parks it on purpose). An
/// engine is known by the guard's start count (`spawns`: every respawn and
/// plan start counts), so it alarms again only after an engine was seen
/// unparked or the guard started a new one; a look without an engine (one
/// coming up, or the connection renewed to the same engine) changes
/// nothing. Nothing is ended: an "ide event" or a job's engine restart ends
/// it with `Shutdown`.
fn watch_parked(g: &mut Guard) {
    let Some(seen) = &g.seen else {
        return;
    };
    if !seen.status.parked {
        if g.parked_alarmed.take().is_some() {
            info!("the engine is no longer parked: its parked alarm is armed again");
        }
    } else if g.state.job.is_none() && g.parked_alarmed != Some(g.spawns) {
        g.parked_alarmed = Some(g.spawns);
        g.raise(None, PARKED_ALARM, false);
    }
}

/// The end of the Windows session (design §5.4): no respawn, the engine
/// gets [`Guard::session_wait`] to release the card and exit by itself,
/// then the server and the tray are asked to stop.
pub fn session_end(pc: &mut dyn Pc, g: &mut Guard) {
    g.session_done = true;
    g.respawn_at = None;
    info!("the Windows session ends: no respawn");
    let start = Instant::now();
    let mut p = pc.procs();
    while !p.engine.is_empty() && start.elapsed() < g.session_wait {
        thread::sleep(Duration::from_millis(100));
        p = pc.procs();
    }
    info!(
        "engine processes left at the end of the session: {}",
        p.engine.len()
    );
    let c = Cancel::default();
    if !p.server.is_empty()
        && let Err(e) = pc.server_stop(&c)
    {
        warn!("the server at the end of the session: {e}");
    }
    if !p.tray.is_empty()
        && let Err(e) = pc.tray_stop(&c)
    {
        warn!("the tray at the end of the session: {e}");
    }
    g.state.pids = pc.children();
    g.save();
    g.shared.update(|v| v.session_done = true);
}
