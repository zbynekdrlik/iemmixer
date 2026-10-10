//! The guard daemon (S6 design note §5.1–§5.4; plan Task 10): the switch
//! runner with its error policy and pre-emption, the requests of the guard
//! pipe, the once-a-second watch, the start after a reboot or a guard
//! restart, and `iemmode event --direct`.
//!
//! Everything here is portable and acts through [`Pc`]: `WinPc` on the PC,
//! `FakePc` in the tests. Nothing ends a process: every stop is a `Pc`
//! request with a bounded wait, then an alarm.
//!
//! Threads: the daemon thread owns the [`Guard`] and the `Pc` and handles
//! one request at a time; the pipe's connection threads answer from the
//! [`Shared`] view while a switch runs ("ide event" pre-empts it through the
//! [`Cancel`] token, a dev or live entry is queued behind the start's
//! checks, everything else is refused) and hand every other request to the
//! daemon thread.
//!
//! A request that is not a switch runs with no switch marked as running: an
//! "ide event" meanwhile pre-empts the token and queues behind it. Its waits
//! end at once (install-site's `check-site`, the runner's stop); its
//! mutations finish first, and they bound the longest
//! an "ide event" waits behind a request: activate's Defender exclusion task
//! (≤ 120 s), a bundle's unzip (local files), the probe task (≤ 15 s).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracing::{info, warn};

use crate::alarms::Alarms;
use crate::cancel::Cancel;
use crate::crash::{self, After, CrashLoop};
use crate::pc::{Audience, EngineSeen, Kid, Pc, Procs, job_note};
use crate::plan::{Mode, PrefFail, Step};
use crate::proto::{EngineStatus, Reply};
use crate::site::GuardSite;
use crate::state::{self, GuardState};
use crate::switch_log::Laps;

mod activation;
mod hil;
mod reaper;
mod reply;
mod requests;
mod runner;
mod shared;

pub use self::activation::{activate_files, activate_offline, install_bundle, install_offline};
pub use self::reply::{Outcome, cut, mode_name, status_text};
pub use self::requests::{Job, handle};
pub use self::runner::run_switch;
pub use self::shared::{Generation, Route, Shared, View, while_switching};

use self::requests::{dry_event, event_now};
use self::runner::{pref_step, switch};

/// The engine's warm-up window before `Arm` (design §5.2 step 7).
pub const READY_S: u32 = 10;
/// The HIL test signal's ceiling (design §7).
pub const HIL_MAX_DBFS: f64 = -20.0;
/// The longest HIL test signal (s): the routing proof needs seconds, and the
/// engine's own cap is 120 s.
pub const HIL_MAX_TTL_S: f64 = 60.0;
/// The watch's period (P10: the process list only).
pub const TICK: Duration = Duration::from_secs(1);
/// Tuning drift is read this often, besides after every switch.
pub const DRIFT_EVERY: Duration = Duration::from_secs(3600);
/// At the end of the session the engine gets this long to exit by itself.
pub const SESSION_ENGINE_WAIT: Duration = Duration::from_secs(10);
/// The files under `<root>\guard\`.
pub const STATE_FILE: &str = "guard-state.json";
pub const ALARMS_FILE: &str = "alarms.json";
/// Alarm texts and reply details are cut to these many characters, so a
/// reply with every kept alarm fits one frame.
pub const ALARM_CHARS: usize = 600;
pub const DETAIL_CHARS: usize = 8000;
/// The watch's alarm on a parked engine outside a HIL job (#35).
pub const PARKED_ALARM: &str = "the engine's stream is parked outside a HIL job: it holds the \
                                card and nothing plays until the engine ends; nothing is ended";

/// The guard's own site settings the daemon decides with (`[guard]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteConf {
    pub on_pref_fail: PrefFail,
    /// The card outputs a HIL test signal may reach.
    pub hil_tx: Vec<u16>,
    /// After the cutover (S8): a crash loop falls back to the previous pin.
    pub prod: bool,
}

impl SiteConf {
    pub fn from_site(g: &GuardSite) -> Self {
        Self {
            on_pref_fail: g.on_pref_fail,
            hil_tx: g.hil_tx.clone(),
            prod: false,
        }
    }
}

impl Default for SiteConf {
    /// Without a site (`iemmixer-guard install` of the first bundle): the
    /// choice recorded on #9, no HIL outputs, before the cutover.
    fn default() -> Self {
        Self {
            on_pref_fail: PrefFail::StartReaperWithAlarm,
            hil_tx: Vec::new(),
            prod: false,
        }
    }
}

/// Seconds since the epoch: the system's, or a test's.
#[derive(Debug, Clone)]
pub enum Clock {
    System,
    Fixed(Arc<AtomicU64>),
}

impl Clock {
    pub fn now(&self) -> u64 {
        match self {
            Self::System => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            Self::Fixed(t) => t.load(Ordering::SeqCst),
        }
    }
}

/// The daemon's state: the persistent state, the alarms and what the
/// running switch needs.
#[derive(Debug)]
pub struct Guard {
    pub state: GuardState,
    pub alarms: Alarms,
    pub site: SiteConf,
    pub cancel: Cancel,
    pub shared: Arc<Shared>,
    /// Set by the session window's thread at the end of the session.
    pub session_ending: Arc<AtomicBool>,
    /// `Quit`: the daemon stops, its children keep running.
    pub quit: bool,
    /// After an activation that changed the guard's exe: hand over to it.
    pub handover: Option<PathBuf>,
    /// How long the engine gets at the end of the session.
    pub session_wait: Duration,
    /// A failure stops the plan instead of unwinding to event (the
    /// rehearsal's re-entry into dev never starts REAPER).
    hold_unwind: bool,
    /// `%LOCALAPPDATA%\iemmixer` (`bundles\`, `bin\`, `guard\`); none: no files.
    root: Option<PathBuf>,
    clock: Clock,
    /// The request of the switch in progress (`live --trial`), read by its
    /// precheck.
    trial: bool,
    /// What the request being handled did (the reply's detail).
    report: Vec<String>,
    /// What the last precheck named without refusing (a dev entry without
    /// a PWA notification subscription, #9 2026-09-28); dropped by the next
    /// precheck and once an alarm reaches a device.
    subscriptions_note: Option<String>,
    /// What the last identity check named about the LAN certificate
    /// without failing (outside its validity, `tls::note`; #9 2026-09-28);
    /// dropped by the next check.
    lan_note: Option<String>,
    /// The children stay in the guard task's job (`pc::job_note`, read at
    /// the start; #9 2026-09-28).
    job_note: Option<&'static str>,
    /// REAPER's evaluation notice was open at the last handover check
    /// (`handover::dialogs`); dropped when the guard quits REAPER and while
    /// a check cannot read REAPER.
    reaper_notice: bool,
    crash: CrashLoop,
    respawn_at: Option<Instant>,
    band_seen: bool,
    /// The engine start (`spawns`) whose parked engine the watch alarmed
    /// (#35); cleared once an engine is seen unparked.
    parked_alarmed: Option<u64>,
    last_drift: Option<Instant>,
    session_done: bool,
    /// The newest alarm whose notice was tried.
    noticed: u64,
    /// The running engine as the supervisor connection saw it last.
    seen: Option<EngineSeen>,
    /// Engine processes this guard started (a respawn adds one).
    spawns: u64,
    /// The exit code of the engine that ended last (the watch's).
    last_exit: Option<i32>,
    /// The step clock of the switch running now (`LastSwitch.steps`, S7).
    laps: Laps,
    /// The entry the running switch unwinds (`back_to_event`): its target
    /// and its `Switching.started`, so the unwind's record spans it.
    unwinding: Option<(Mode, u64)>,
    /// The running switch's steps that asked the owner while its plan went
    /// on (`ContinueAskOwner`, `SkipAskOwner`; #10), each "<Step> failed:
    /// <why>": any makes the switch end `needs_owner`. Kept until the next
    /// switch begins, for the reply.
    owner_failed: Vec<String>,
}

impl Guard {
    fn with(
        state: GuardState,
        alarms: Alarms,
        site: SiteConf,
        clock: Clock,
        root: Option<PathBuf>,
    ) -> Self {
        let cancel = Cancel::default();
        let g = Self {
            state,
            alarms,
            site,
            shared: Arc::new(Shared::new(cancel.clone())),
            cancel,
            session_ending: Arc::new(AtomicBool::new(false)),
            quit: false,
            handover: None,
            session_wait: SESSION_ENGINE_WAIT,
            hold_unwind: false,
            root,
            clock,
            trial: false,
            report: Vec::new(),
            subscriptions_note: None,
            lan_note: None,
            job_note: None,
            reaper_notice: false,
            crash: CrashLoop::default(),
            respawn_at: None,
            band_seen: false,
            parked_alarmed: None,
            last_drift: None,
            session_done: false,
            noticed: 0,
            seen: None,
            spawns: 0,
            last_exit: None,
            laps: Laps::default(),
            unwinding: None,
            owner_failed: Vec::new(),
        };
        g.publish(|_| {});
        g
    }

    /// The guard whose state and alarms live in `<root>\guard\`; an
    /// unreadable file starts from the defaults (mode `event`) with an alarm.
    pub fn open(root: &Path, site: SiteConf, clock: Clock) -> Self {
        let dir = root.join("guard");
        let (st, bad_state) = GuardState::load(&dir.join(STATE_FILE));
        let (alarms, bad_alarms) = state::load_json::<Alarms>(&dir.join(ALARMS_FILE));
        let mut g = Self::with(st, alarms, site, clock, Some(root.to_path_buf()));
        if let Some(why) = bad_state {
            g.raise(None, &why, false);
        }
        if let Some(why) = bad_alarms {
            g.raise(None, &format!("guard alarms unreadable ({why})"), false);
        }
        g
    }

    /// A guard in `mode` without files, at a fixed time (tests).
    #[cfg(test)]
    pub fn for_test(mode: Mode) -> Self {
        let state = GuardState {
            mode,
            ..GuardState::default()
        };
        let site = SiteConf {
            hil_tx: vec![94],
            ..SiteConf::default()
        };
        let clock = Clock::Fixed(Arc::new(AtomicU64::new(1_790_000_000)));
        Self::with(state, Alarms::default(), site, clock, None)
    }

    pub fn now(&self) -> u64 {
        self.clock.now()
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// A line of the request's report, logged.
    fn info(&mut self, text: impl Into<String>) {
        let text = text.into();
        info!("{text}");
        self.report.push(text);
    }

    fn publish(&self, extra: impl FnOnce(&mut View)) {
        let status = status_text(self);
        let engine = self.engine_status();
        self.shared.update(|v| {
            v.mode = self.state.mode;
            v.switching.clone_from(&self.state.switching);
            v.last_switch.clone_from(&self.state.last_switch);
            v.alarms = self.alarms.all().to_vec();
            v.status = status;
            v.engine = engine;
            extra(v);
        });
    }

    /// The running engine as `Reply.engine` shows it (design §7: HIL v1
    /// reads it through `iemmode status`); `None` while none runs.
    pub fn engine_status(&self) -> Option<EngineStatus> {
        let pid = self.state.pids.engine.as_ref().map(|c| c.pid);
        self.seen.as_ref().map(|seen| {
            crate::effects::engine::engine_status(seen, self.spawns, self.last_exit, pid)
        })
    }

    /// Looks at what the supervisor connection holds of the engine (no
    /// wait, no process read) for the next replies.
    fn look(&mut self, pc: &mut dyn Pc) {
        self.seen = pc.engine_seen();
        self.shared.set_engine(self.engine_status());
    }

    /// The engine's HIL flags (test signal, fault injection): only in dev
    /// while a HIL job runs, never in live (design §7).
    fn hil_engine(&self, to: Mode) -> bool {
        to == Mode::Dev && self.state.job.is_some()
    }

    /// Writes the state and the alarms (when the guard has files).
    fn store(&mut self) {
        let now = self.now();
        let Some(root) = &self.root else {
            self.state.written_at = now;
            return;
        };
        let dir = root.join("guard");
        let saved = fs::create_dir_all(&dir)
            .and_then(|()| self.state.save(&dir.join(STATE_FILE), now))
            .and_then(|()| state::save_json(&dir.join(ALARMS_FILE), &self.alarms));
        if let Err(e) = saved {
            warn!("saving the guard state in {}: {e}", dir.display());
        }
    }

    fn save(&mut self) {
        self.store();
        self.publish(|_| {});
    }

    /// Raises an alarm: kept in the alarm file and shown on every reply.
    /// Its notice to the engineer's devices goes out with [`send_notices`]
    /// once the request or the watch is done, so a notice never delays
    /// REAPER. Returns its id.
    pub fn raise(&mut self, step: Option<Step>, text: &str, owner_question: bool) -> u64 {
        let text = cut(text, ALARM_CHARS);
        warn!("alarm: {text}");
        let at = self.now();
        let id = self.alarms.raise(at, step, text, owner_question);
        self.save();
        id
    }

    fn alarm(&mut self, step: Step, why: &str, owner_question: bool) {
        self.raise(Some(step), &format!("{step:?}: {why}"), owner_question);
    }

    fn need_dev(&self, what: &str) -> Result<(), String> {
        if self.state.mode == Mode::Dev {
            Ok(())
        } else {
            Err(format!(
                "{what} is for dev; the mode is {}",
                mode_name(self.state.mode)
            ))
        }
    }
}

/// Sends the notices of the open alarms raised since the last try, once
/// each, to the engineer's devices: the PWA's notification subscriptions
/// (design §5.4, #9 2026-09-28). An acknowledged alarm is never sent (a new
/// guard reads the alarm file and tries again what reached no device).
pub fn send_notices(pc: &mut dyn Pc, g: &mut Guard) {
    let due: Vec<(u64, String)> = g
        .alarms
        .iter()
        .filter(|a| a.id > g.noticed && !a.notified && !a.acked)
        .map(|a| (a.id, a.text.clone()))
        .collect();
    let mut sent = false;
    for (id, text) in due {
        g.noticed = g.noticed.max(id);
        match pc.notify(Audience::Alarm, "iemmixer alarm", &text) {
            Ok(()) => sent |= g.alarms.mark_notified(id),
            Err(e) => warn!("the notice of alarm {id} failed: {e}"),
        }
    }
    if sent {
        // A phone took it: the guard has a PWA notification subscription now.
        g.subscriptions_note = None;
        g.save();
    }
}

/// What the elevated logon task (G1) left, taken once per run of it (its
/// `at`, `GuardState::logon_seen`), at the guard's start and hourly (#9
/// 2026-09-28). The task follows `PrefCheck`'s rule, so a preference it did
/// not write under a holder of the driver module is remembered and alarmed
/// once with `PrefCheck`'s text (the event plan's check that finds the same
/// adds no alarm); a run at REAPER's original drops what was remembered; a
/// failed run is logged (the next plan's `PrefCheck` reads the preference).
fn take_logon(pc: &mut dyn Pc, g: &mut Guard) {
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

/// `iemmode alarm-test`. Kept here with the alarms: HIL's `alarm-ack`
/// acknowledges only this text, and `scripts/iem-pc/test_iempc_hil.py` and
/// `Test-IemHil.ps1` read it from this file.
fn alarm_test(pc: &mut dyn Pc, g: &mut Guard) -> (bool, String) {
    let id = g.raise(None, "alarm test (iemmode alarm-test)", false);
    send_notices(pc, g);
    let sent = g.alarms.iter().any(|a| a.id == id && a.notified);
    let text = if sent {
        "the test alarm reached the engineer's devices"
    } else {
        "the test alarm was not delivered"
    };
    (sent, text.to_owned())
}

// ---- the watch ----

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
    match crash::after_exit(code, mode, g.site.prod, session, looped, n, streak) {
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
        After::ToEvent => {
            g.raise(
                None,
                &format!("the engine crashed {n} times in 10 min: back to REAPER"),
                false,
            );
            run_switch(pc, g, mode, Mode::Event);
        }
        After::PreviousPin => match g.state.pins.revert() {
            Ok(sha) => {
                g.raise(
                    None,
                    &format!(
                        "the engine crashed {n} times in 10 min: back to the previous pin {sha}"
                    ),
                    false,
                );
                pc.set_bundle(Some(sha.as_str()));
                g.respawn_at = Some(at);
            }
            Err(why) => {
                g.raise(None, &format!("crash loop in prod: {why}"), false);
            }
        },
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

// ---- start, loop, direct ----

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

/// The daemon's loop: requests one at a time, the watch once a second.
/// Ends on `Quit`, after an activation that hands over, or when the pipe
/// is gone; the caller then waits for the last reply to be written
/// ([`Shared::await_replies`]) before the process ends.
pub fn serve_requests(pc: &mut dyn Pc, g: &mut Guard, jobs: &Receiver<Job>) {
    let mut next = Instant::now();
    while !g.quit && g.handover.is_none() {
        match jobs.recv_timeout(next.saturating_duration_since(Instant::now())) {
            Ok(job) => {
                let reply = handle(pc, g, job.req, job.generation);
                g.shared.reply_sent();
                if job.reply.send(reply).is_err() {
                    info!("a client left before its reply");
                    g.shared.reply_done();
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let now = Instant::now();
        if now >= next {
            tick(pc, g, now);
            next = now + TICK;
        }
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

#[cfg(test)]
mod probe_tests;
#[cfg(test)]
mod reaper_tests;
#[cfg(test)]
mod record_tests;
#[cfg(test)]
mod tests;
