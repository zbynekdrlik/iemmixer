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
//! [`Cancel`] token, everything else is refused) and hand every other
//! request to the daemon thread.
//!
//! A request that is not a switch runs with no switch marked as running: an
//! "ide event" meanwhile pre-empts the token and queues behind it. Its waits
//! end at once (install-site's `check-site`, a HIL job's stage peaks, the
//! runner's stop); its mutations finish first, and they bound the longest
//! an "ide event" waits behind a request: activate's Defender exclusion task
//! (≤ 120 s), a bundle's unzip (local files), the probe task (≤ 15 s).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracing::{info, warn};

use crate::alarms::{Alarm, Alarms};
use crate::bundle::{self, Hil, Record};
use crate::cancel::Cancel;
use crate::crash::{self, After, CrashLoop};
use crate::effects::engine::ACTIVE_DB;
use crate::handover::{self, Audio};
use crate::install::{self, InstallError};
use crate::pc::{Audience, EngineSeen, Kid, Pc, PrefSeen, Procs, R, StepError, job_note};
use crate::plan::{
    Activation, Busy, Facts, Health, Mode, OnError, PrefFail, Step, activation, on_error, plan,
};
use crate::proto::{self, EngineStatus, Reply, Request};
use crate::site::GuardSite;
use crate::state::{self, GuardState, InterlockRetry, Switching};

/// The interlock's length in seconds (design §5.2 step 2).
pub const INTERLOCK_S: u32 = 60;
/// A refused entry is tried again after this many seconds.
pub const RETRY_S: u64 = 15 * 60;
/// The refusal that sends the owner one notice.
pub const RETRY_NOTICE_AT: u32 = 4;
/// The refusal after which the entry is dropped.
pub const RETRY_LAST: u32 = 8;
/// The owner's notice on the fourth refusal (he reads Slovak).
pub const RETRY_NOTICE: &str = "na pódiu je signál, prepnutie čaká";
/// The engine's warm-up window before `Arm` (design §5.2 step 7).
pub const READY_S: u32 = 10;
/// A HIL job needs this much band quiet (design §7)…
pub const JOB_QUIET: Duration = Duration::from_secs(300);
/// …and a quiet stage over this many seconds of the engine's meters.
pub const JOB_PEAKS_S: u32 = 60;
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

/// How a switch ended; the mode is in `g.state.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// "ide event" found a healthy engine that did not release: iemmixer keeps serving.
    KeptServing,
    /// The plan stopped; the owner gets the prepared ❓ (alarm flagged `owner_question`).
    NeedsOwner,
    /// The interlock heard the band: nothing was touched, the entry waits
    /// for its retry (design §5.2 step 2).
    Refused,
}

fn outcome_text(o: Option<Outcome>) -> &'static str {
    match o {
        Some(Outcome::Done) => "done",
        Some(Outcome::KeptServing) => "the engine did not release; iemmixer keeps serving",
        Some(Outcome::NeedsOwner) => "stopped; the owner decides",
        Some(Outcome::Refused) => "refused: activity on stage, tried again every 15 min",
        None => "no switch yet",
    }
}

/// How a switch to `to` went, the mode now being `now`.
fn switch_text(to: Mode, out: Outcome, now: Mode) -> String {
    let target = mode_name(to);
    if out == Outcome::Done && now == to {
        format!("{target}: done")
    } else if out == Outcome::Done {
        format!("{target}: not entered; unwound to {}", mode_name(now))
    } else {
        format!(
            "{target}: {}; the mode is {}",
            outcome_text(Some(out)),
            mode_name(now)
        )
    }
}

/// A mode as the guard pipe names it.
pub fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Event => "event",
        Mode::Dev => "dev",
        Mode::Live => "live",
    }
}

/// The first `max` characters of `text`.
pub fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Every stage peak (dBFS) at or below the band-activity level; no peak at
/// all is not quiet.
pub fn stage_quiet(peaks: &[f64]) -> bool {
    !peaks.is_empty() && peaks.iter().all(|p| *p <= ACTIVE_DB)
}

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

/// What the pipe's threads see of the guard.
#[derive(Debug, Clone, Default)]
pub struct View {
    pub mode: Mode,
    pub switching: Option<Switching>,
    pub alarms: Vec<Alarm>,
    pub status: String,
    /// The target of the switch running now.
    pub running: Option<Mode>,
    /// How the last switch ended.
    pub last: Option<Outcome>,
    /// Counts the switches begun: a request queued before one began is
    /// answered as during it.
    pub epoch: u64,
    /// Counts the changes (subscribers wait on it).
    pub version: u64,
    /// Counts the requests to the tray to quit.
    pub tray_quits: u64,
    /// A quit no subscription has delivered yet (the tray may be in its
    /// retry sleep): the next subscription delivers it, once.
    pub tray_quit_pending: bool,
    pub subscribers: u32,
    /// The session ended and the guard stopped what it stops.
    pub session_done: bool,
    /// The running engine (`Reply.engine`), refreshed by the watch.
    pub engine: Option<EngineStatus>,
    /// Replies the daemon thread handed to the pipe's threads…
    pub replies_sent: u64,
    /// …and those the pipe's threads have written (or found their client
    /// gone).
    pub replies_done: u64,
}

impl View {
    pub fn reply(&self, ok: bool, detail: &str) -> Reply {
        Reply {
            ok,
            mode: self.mode,
            switching: self.switching.clone(),
            alarms: self.alarms.clone(),
            detail: cut(detail, DETAIL_CHARS),
            engine: self.engine.clone(),
            guard_build: Some(proto::GUARD_BUILD.to_owned()),
        }
    }

    /// "ide event" once no switch runs: done when the last switch ended in
    /// `event`.
    pub fn event_reply(&self, note: &str) -> Reply {
        let ok =
            self.running.is_none() && self.mode == Mode::Event && self.last == Some(Outcome::Done);
        self.reply(ok, &format!("{note}; event: {}", outcome_text(self.last)))
    }
}

/// Why a request is refused while a switch runs.
pub fn while_switching(req: &Request) -> &'static str {
    match req {
        Request::Dev { .. } | Request::Live { .. } => "busy",
        _ => "switching",
    }
}

/// Where the pipe sends a request.
#[derive(Debug, Clone, PartialEq)]
// `Now(Reply)` carries the whole engine status (control thread only, one at a
// time): the size gap to the small variants is expected.
#[allow(clippy::large_enum_variant)]
pub enum Route {
    /// Answered at once from the view.
    Now(Reply),
    /// Answered once the switch running now has ended.
    AwaitEnd(&'static str),
    /// To the daemon thread, with the switch generation seen.
    Queue(u64),
    Subscribe,
}

/// The guard's state as the pipe's threads share it.
#[derive(Debug, Default)]
pub struct Shared {
    view: Mutex<View>,
    changed: Condvar,
    cancel: Cancel,
}

impl Shared {
    pub fn new(cancel: Cancel) -> Self {
        Self {
            view: Mutex::new(View::default()),
            changed: Condvar::new(),
            cancel,
        }
    }

    fn lock(&self) -> MutexGuard<'_, View> {
        self.view.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn view(&self) -> View {
        self.lock().clone()
    }

    pub fn epoch(&self) -> u64 {
        self.lock().epoch
    }

    /// Changes the view and wakes its waiters.
    pub fn update(&self, f: impl FnOnce(&mut View)) {
        let mut v = self.lock();
        f(&mut v);
        v.version += 1;
        drop(v);
        self.changed.notify_all();
    }

    fn wait_while(&self, limit: Duration, cond: impl FnMut(&mut View) -> bool) -> View {
        let guard = self.lock();
        match self.changed.wait_timeout_while(guard, limit, cond) {
            Ok((v, _)) => v.clone(),
            Err(poisoned) => poisoned.into_inner().0.clone(),
        }
    }

    /// Decides under the view's lock, so "ide event" either pre-empts a
    /// switch that is not an event plan or waits for the event plan, never
    /// both: an event plan clears the token under the same lock as it
    /// begins.
    pub fn route(&self, req: &Request) -> Route {
        let v = self.lock();
        match (req, v.running) {
            (Request::Subscribe, _) => Route::Subscribe,
            (Request::Status, _) => Route::Now(v.reply(true, &v.status)),
            (Request::Event { dry_run: false }, Some(Mode::Event)) => {
                Route::AwaitEnd("already switching to event")
            }
            (Request::Event { dry_run: false }, Some(_)) => {
                self.cancel.preempt();
                Route::AwaitEnd("pre-empted the switch in progress")
            }
            (Request::Event { dry_run: false }, None) => {
                // A request queued before it pre-empts at its start.
                self.cancel.preempt();
                Route::Queue(v.epoch)
            }
            (_, Some(_)) => Route::Now(v.reply(false, while_switching(req))),
            (_, None) => Route::Queue(v.epoch),
        }
    }

    /// Waits up to `limit` for the switch running now to end.
    pub fn await_end(&self, note: &str, limit: Duration) -> Reply {
        self.wait_while(limit, |v| v.running.is_some())
            .event_reply(note)
    }

    /// Waits up to `limit` for a change after `seen` (version, tray quits).
    pub fn wait_change(&self, seen: (u64, u64), limit: Duration) -> View {
        self.wait_while(limit, |v| (v.version, v.tray_quits) == seen)
    }

    pub fn add_subscriber(&self) {
        self.lock().subscribers += 1;
    }

    pub fn drop_subscriber(&self) {
        let mut v = self.lock();
        v.subscribers = v.subscribers.saturating_sub(1);
    }

    /// Asks the tray to quit (`WinPc::tray_stop`): its subscription gets
    /// `Update::Quit` at once, or its next one does when the tray waits to
    /// connect again (its 2 s retry). `WinPc::tray_stop` then waits for the
    /// process to end.
    pub fn tray_quit(&self) -> Result<(), String> {
        let mut v = self.lock();
        v.tray_quit_pending = true;
        v.tray_quits += 1;
        drop(v);
        self.changed.notify_all();
        Ok(())
    }

    /// Takes a quit no subscription has delivered yet (one delivers it).
    pub fn take_tray_quit(&self) -> bool {
        std::mem::take(&mut self.lock().tray_quit_pending)
    }

    /// Forgets a quit no tray took: a tray that starts must not quit at once.
    pub fn clear_tray_quit(&self) {
        self.lock().tray_quit_pending = false;
    }

    /// The session window's wait (design §5.4): true once the guard stopped
    /// what it stops at the end of the session.
    pub fn await_session_done(&self, limit: Duration) -> bool {
        self.wait_while(limit, |v| !v.session_done).session_done
    }

    /// Counts under the view's lock and wakes its waiters; not a change of
    /// the view (subscribers stay asleep).
    fn count(&self, f: impl FnOnce(&mut View)) {
        let mut v = self.lock();
        f(&mut v);
        drop(v);
        self.changed.notify_all();
    }

    /// The engine as the watch saw it last; not a change of the view (a
    /// status answer reads it, subscribers stay asleep: its counters move
    /// every second).
    pub fn set_engine(&self, engine: Option<EngineStatus>) {
        self.count(|v| v.engine = engine);
    }

    /// The daemon thread handed a reply to a pipe thread.
    pub fn reply_sent(&self) {
        self.count(|v| v.replies_sent += 1);
    }

    /// A pipe thread wrote a reply it was handed (or found its client gone).
    pub fn reply_done(&self) {
        self.count(|v| v.replies_done += 1);
    }

    /// Waits up to `limit` until every reply handed to the pipe was written:
    /// the guard's last reply (quit, a hand-over) before its process ends.
    pub fn await_replies(&self, limit: Duration) -> bool {
        let v = self.wait_while(limit, |v| v.replies_done < v.replies_sent);
        v.replies_done >= v.replies_sent
    }

    /// A switch into dev or live is about to end. Under the view's lock:
    /// false when "ide event" pre-empted it (it goes back to event instead);
    /// otherwise it stops counting as running, so a later "ide event" is
    /// queued behind its end, as when no switch runs, and never pre-empts a
    /// switch that no longer looks at the token.
    pub fn end_unless_preempted(&self) -> bool {
        let mut v = self.lock();
        if self.cancel.preempted() {
            return false;
        }
        v.running = None;
        true
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
    /// The request of the switch in progress.
    trial: bool,
    force: bool,
    build: Option<String>,
    /// The interlock's report when the stage was not quiet.
    activity: Option<String>,
    /// What the request being handled did (the reply's detail).
    report: Vec<String>,
    /// A status line (a dropped retry).
    note: Option<String>,
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
            force: false,
            build: None,
            activity: None,
            report: Vec::new(),
            note: None,
            subscriptions_note: None,
            lan_note: None,
            job_note: None,
            reaper_notice: false,
            crash: CrashLoop::default(),
            respawn_at: None,
            band_seen: false,
            last_drift: None,
            session_done: false,
            noticed: 0,
            seen: None,
            spawns: 0,
            last_exit: None,
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
            v.alarms = self.alarms.all().to_vec();
            v.status = status;
            v.engine = engine;
            extra(v);
        });
    }

    /// The running engine as `Reply.engine` shows it (design §7: HIL v1
    /// reads it through `iemmode status`); `None` while none runs.
    pub fn engine_status(&self) -> Option<EngineStatus> {
        self.seen
            .as_ref()
            .map(|seen| crate::effects::engine::engine_status(seen, self.spawns, self.last_exit))
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

    /// The switch is persisted as it begins and after every step (the
    /// steps done), so a restarted guard re-plans it to event.
    fn begin(&mut self, from: Mode, to: Mode, steps: &[Step]) {
        info!("switch {} → {}: {steps:?}", mode_name(from), mode_name(to));
        self.state.switching = Some(Switching {
            from,
            to,
            done: Vec::new(),
            started: self.now(),
        });
        self.store();
        let cancel = self.cancel.clone();
        self.publish(|v| {
            v.running = Some(to);
            v.epoch += 1;
            if to == Mode::Event {
                cancel.clear();
            }
        });
    }

    fn done(&mut self, pc: &mut dyn Pc, step: Step) {
        if let Some(s) = self.state.switching.as_mut() {
            s.done.push(step);
        }
        self.state.pids = pc.children();
        self.save();
    }

    fn finish(&mut self, pc: &mut dyn Pc, outcome: Outcome, mode: Mode) -> Outcome {
        let from = self.state.switching.take().map(|s| s.from);
        self.state.mode = mode;
        // A HIL job lives in dev only (an event plan without a runner has
        // no JobsCancel step).
        if mode != Mode::Dev {
            self.state.job = None;
        }
        self.trial = false;
        self.force = false;
        self.build = None;
        info!(
            "switch ended in {}: {}",
            mode_name(mode),
            outcome_text(Some(outcome))
        );
        self.store();
        self.publish(|v| {
            v.running = None;
            v.last = Some(outcome);
        });
        // Tuning drift after every switch that ran. After any other change
        // of the mode (a plan that stopped for the owner) the watch reads it
        // at its next tick: nothing follows a stopped plan's last step.
        if outcome == Outcome::Done {
            self.drift(pc, Instant::now());
        } else if from != Some(mode) {
            self.last_drift = None;
        }
        outcome
    }

    fn drift(&mut self, pc: &mut dyn Pc, at: Instant) {
        self.last_drift = Some(at);
        match pc.tuning_drift() {
            Ok(None) => {}
            Ok(Some(d)) => {
                self.raise(None, &format!("tuning drift: {d}"), false);
            }
            Err(e) => warn!("the tuning drift could not be read: {e}"),
        }
    }

    /// The answer to a request: the state, every kept alarm and what the
    /// request did.
    pub fn reply(&self, ok: bool, detail: &str) -> Reply {
        let mut text = detail.to_owned();
        for line in &self.report {
            text.push_str("; ");
            text.push_str(line);
        }
        Reply {
            ok,
            mode: self.state.mode,
            switching: self.state.switching.clone(),
            alarms: self.alarms.all().to_vec(),
            detail: cut(&text, DETAIL_CHARS),
            engine: self.engine_status(),
            guard_build: Some(proto::GUARD_BUILD.to_owned()),
        }
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

    #[cfg(test)]
    fn set_now(&self, t: u64) {
        if let Clock::Fixed(c) = &self.clock {
            c.store(t, Ordering::SeqCst);
        }
    }
}

/// `iemmode status`: one line.
pub fn status_text(g: &Guard) -> String {
    let mut parts = vec![format!("mode {}", mode_name(g.state.mode))];
    parts.push(match &g.state.pins.current {
        Some(sha) => format!("bundle {sha}"),
        None => "no bundle".to_owned(),
    });
    if let Some(run) = g.state.job {
        parts.push(format!("HIL job {run}"));
    }
    if let Some(r) = &g.state.interlock_retry {
        parts.push(format!(
            "the {} switch waits: {} interlock refusals, next try at {}",
            mode_name(r.target),
            r.refusals,
            r.next_at
        ));
    }
    if let Some(n) = &g.note {
        parts.push(n.clone());
    }
    if g.reaper_notice {
        parts.push(handover::NOTICE_REPORT.to_owned());
    }
    if let Some(n) = &g.state.pref_held {
        parts.push(n.clone());
    }
    if let Some(n) = &g.subscriptions_note {
        parts.push(n.clone());
    }
    if let Some(n) = &g.lan_note {
        parts.push(n.clone());
    }
    if let Some(n) = g.job_note {
        parts.push(n.to_owned());
    }
    let open = g.alarms.unacked();
    if open > 0 {
        parts.push(format!("{open} unacknowledged alarms"));
    }
    parts.join("; ")
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

// ---- the switch runner ----

/// Runs the plan from `from` to `to` with the error policy of design §5.2
/// (`plan::on_error`). "ide event" pre-empts a switch into dev/live within
/// 1 s of a waiting step, after a mutating one.
pub fn run_switch(pc: &mut dyn Pc, g: &mut Guard, from: Mode, to: Mode) -> Outcome {
    let facts = Facts {
        trial: g.trial,
        force: g.force,
        ..pc.facts()
    };
    let steps = plan(from, to, &facts);
    g.begin(from, to, &steps);
    let mut skip: Vec<Step> = Vec::new();
    for step in steps {
        if skip.contains(&step) {
            continue;
        }
        if to != Mode::Event && g.cancel.preempted() {
            return back_to_event(pc, g, "pre-empted by event");
        }
        info!("step {step:?}");
        match run_step(pc, g, step, to, &facts) {
            Ok(()) => g.done(pc, step),
            Err(StepError::Preempted) if to != Mode::Event => {
                return back_to_event(pc, g, "pre-empted by event");
            }
            Err(e) => {
                if let Some(report) = g.activity.take() {
                    return refused(pc, g, from, to, &report);
                }
                let (why, health, policy) = failure(pc, g, to, step, &e);
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
                    OnError::Skip(later) => {
                        g.alarm(step, &why, false);
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
    g.finish(pc, Outcome::Done, to)
}

/// Whether a switch into `to` may end as it is: an event plan always; one
/// into dev or live unless "ide event" pre-empted it after its last look at
/// the token (during a mutation that finishes first, or after the last
/// step). From then on "ide event" queues behind its end.
fn may_end(g: &Guard, to: Mode) -> bool {
    to == Mode::Event || g.shared.end_unless_preempted()
}

fn back_to_event(pc: &mut dyn Pc, g: &mut Guard, why: &str) -> Outcome {
    g.cancel.clear();
    g.info(format!("unwinding to event: {why}"));
    let now = g.state.mode;
    run_switch(pc, g, now, Mode::Event)
}

/// Why `step` failed, the engine's health after a failed release at "ide
/// event" (read over the supervisor pipe), and what that means.
fn failure(
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

/// The interlock heard the band: the entry waits for its retry in 15 min;
/// the fourth refusal sends the owner one notice, the eighth drops it.
fn refused(pc: &mut dyn Pc, g: &mut Guard, from: Mode, to: Mode, report: &str) -> Outcome {
    if !may_end(g, to) {
        return back_to_event(pc, g, "pre-empted by event");
    }
    let refusals = g
        .state
        .interlock_retry
        .as_ref()
        .filter(|r| r.target == to)
        .map_or(0, |r| r.refusals)
        + 1;
    g.info(format!(
        "the interlock heard the band ({report}); refusal {refusals}"
    ));
    if refusals >= RETRY_LAST {
        g.state.interlock_retry = None;
        g.note = Some(format!(
            "the {} switch was dropped after {refusals} interlock refusals",
            mode_name(to)
        ));
    } else {
        g.state.interlock_retry = Some(InterlockRetry {
            target: to,
            build: g.build.clone(),
            refusals,
            next_at: g.now() + RETRY_S,
        });
    }
    if refusals == RETRY_NOTICE_AT {
        g.raise(Some(Step::Interlock), RETRY_NOTICE, false);
    }
    g.finish(pc, Outcome::Refused, from)
}

/// One step, one `Pc` call (plus the verdicts of `handover`).
fn run_step(pc: &mut dyn Pc, g: &mut Guard, step: Step, to: Mode, facts: &Facts) -> R<()> {
    let c = g.cancel.clone();
    match step {
        Step::Precheck => {
            g.subscriptions_note = None;
            g.subscriptions_note = pc.precheck(to, facts.trial)?;
            if let Some(n) = g.subscriptions_note.clone() {
                g.info(n);
            }
            Ok(())
        }
        Step::Interlock => interlock(pc, g, facts, &c),
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
            let s = pc.engine_ready(READY_S, &c)?;
            g.info(format!(
                "engine ready: {} frames, {} callbacks, {} missed",
                s.frames, s.callbacks, s.missed
            ));
            pc.engine_arm()
        }
        Step::ServerStart => {
            let pid = pc.server_start(to)?;
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
                .pins
                .current
                .clone()
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
fn pref_step(pc: &mut dyn Pc, g: &mut Guard, to: Mode) -> R<()> {
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

/// 60 s on the stage inputs: REAPER's meters while REAPER runs, else
/// `iem-engine interlock`. Activity is not an error of the check: the
/// report goes to [`refused`].
fn interlock(pc: &mut dyn Pc, g: &mut Guard, facts: &Facts, c: &Cancel) -> R<()> {
    let (quiet, report) = if facts.reaper {
        let peaks = pc.reaper_meters(INTERLOCK_S, c)?;
        (
            stage_quiet(&peaks),
            format!("REAPER stage peaks {peaks:?} dBFS"),
        )
    } else {
        pc.engine_interlock(INTERLOCK_S, c)?
    };
    if quiet {
        g.info(format!("interlock quiet: {report}"));
        Ok(())
    } else {
        g.activity = Some(report.clone());
        Err(StepError::failed(format!("activity on stage: {report}")))
    }
}

// ---- requests ----

/// A request from the pipe with the switch generation it saw, and where
/// its reply goes.
#[derive(Debug)]
pub struct Job {
    pub req: Request,
    pub epoch: u64,
    pub reply: SyncSender<Reply>,
}

/// A request queued before a switch began is answered as during it.
fn stale(req: &Request, v: &View) -> Option<Reply> {
    match req {
        Request::Status | Request::Subscribe => None,
        Request::Event { dry_run: false } => Some(v.event_reply("a switch ran meanwhile")),
        _ => Some(v.reply(false, while_switching(req))),
    }
}

/// A dev or live entry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    to: Mode,
    build: Option<String>,
    force: bool,
    trial: bool,
    dry_run: bool,
    /// The guard's own retry after an interlock refusal.
    retry: bool,
}

/// Handles one request on the daemon thread.
pub fn handle(pc: &mut dyn Pc, g: &mut Guard, req: Request, epoch: u64) -> Reply {
    if epoch != g.shared.epoch()
        && let Some(reply) = stale(&req, &g.shared.view())
    {
        return reply;
    }
    g.report.clear();
    let (ok, detail) = match req {
        Request::Status => (true, status_text(g)),
        Request::Subscribe => (true, "subscriptions are served by the pipe".to_owned()),
        Request::Event { dry_run: true } => dry_event(pc, g),
        Request::Event { dry_run: false } => event_now(pc, g),
        Request::Dev {
            build,
            force,
            dry_run,
        } => entry(
            pc,
            g,
            Entry {
                to: Mode::Dev,
                build,
                force,
                trial: false,
                dry_run,
                retry: false,
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
                build: Some(build),
                force: false,
                trial,
                dry_run,
                retry: false,
            },
        ),
        Request::Install { zip } => install_bundle(g, Path::new(&zip)),
        Request::Activate { sha } => activate(pc, g, &sha),
        Request::TestSignal { input, dbfs, ttl_s } => test_signal(pc, g, &input, dbfs, ttl_s),
        Request::Report { sha, hil, detail } => report(g, &sha, &hil, &detail),
        Request::JobBegin { run } => job_begin(pc, g, run),
        Request::JobEnd { run } => job_end(g, run),
        Request::InstallSite { path } => install_site(pc, g, &path),
        Request::ForceReopen => match g.need_dev("force-reopen") {
            Ok(()) => outcome(pc.engine_force_reopen(), "the engine reopened the driver"),
            Err(why) => (false, why),
        },
        Request::InjectFault => inject_fault(pc, g),
        Request::InjectSeh => inject_seh(pc, g),
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

fn outcome(r: R<()>, done: &str) -> (bool, String) {
    match r {
        Ok(()) => (true, done.to_owned()),
        Err(e) => (false, e.to_string()),
    }
}

fn plan_text(steps: &[Step]) -> String {
    let names: Vec<String> = steps.iter().map(|s| format!("{s:?}")).collect();
    names.join(", ")
}

/// `event --dry-run`: the plan from the facts, nothing changed.
fn dry_event(pc: &mut dyn Pc, g: &mut Guard) -> (bool, String) {
    let facts = pc.facts();
    let steps = plan(g.state.mode, Mode::Event, &facts);
    (true, format!("dry run: {}", plan_text(&steps)))
}

/// "ide event": the event plan from the current mode (in `event` its
/// checks, plus a restart of what runs but does not serve).
fn event_now(pc: &mut dyn Pc, g: &mut Guard) -> (bool, String) {
    g.state.interlock_retry = None;
    g.note = None;
    let from = g.state.mode;
    let out = run_switch(pc, g, from, Mode::Event);
    (
        out == Outcome::Done && g.state.mode == Mode::Event,
        switch_text(Mode::Event, out, g.state.mode),
    )
}

/// Why the entry's build may not run: not installed, or (live) not a green
/// `main` bundle.
fn build_refusal(g: &Guard, e: &Entry) -> Option<String> {
    let sha = e.build.as_ref()?;
    let Some(rec) = g.state.bundles.get(sha) else {
        return Some(format!("bundle {sha} is not installed"));
    };
    if e.to == Mode::Live {
        bundle::may_go_live(rec).err()
    } else {
        None
    }
}

/// Ends the wait of a refused entry: it ran, unwound, was pre-empted, or
/// may no longer run. Otherwise the watch would enter again every second.
fn drop_retry(g: &mut Guard) {
    g.state.interlock_retry = None;
    g.note = None;
    g.save();
}

fn entry(pc: &mut dyn Pc, g: &mut Guard, e: Entry) -> (bool, String) {
    if !e.retry {
        g.state.interlock_retry = None;
        g.note = None;
    }
    if let Some(why) = build_refusal(g, &e) {
        if e.retry {
            drop_retry(g);
        }
        return (false, why);
    }
    if e.dry_run {
        return dry_entry(pc, g, &e);
    }
    if let Some(sha) = e.build.as_deref() {
        g.state.pins.promote(sha);
        pc.set_bundle(Some(sha));
    }
    g.trial = e.trial;
    g.force = e.force;
    g.build.clone_from(&e.build);
    let from = g.state.mode;
    let out = run_switch(pc, g, from, e.to);
    // Only a refusal waits for its retry (`refused` counted it).
    if out != Outcome::Refused {
        drop_retry(g);
    }
    (
        out == Outcome::Done && g.state.mode == e.to,
        switch_text(e.to, out, g.state.mode),
    )
}

/// `dev|live --dry-run`: the plan and the read-only checks (the precheck's
/// bundle, PWA notification subscriptions, foreign engine and app exe),
/// nothing changed.
fn dry_entry(pc: &mut dyn Pc, g: &mut Guard, e: &Entry) -> (bool, String) {
    let facts = Facts {
        trial: e.trial,
        force: e.force,
        ..pc.facts()
    };
    let steps = plan(g.state.mode, e.to, &facts);
    let bundle = e
        .build
        .clone()
        .or_else(|| g.state.pins.current.clone())
        .unwrap_or_else(|| "none".to_owned());
    let check = pc.precheck(e.to, e.trial);
    let verdict = match &check {
        Ok(None) => "ok".to_owned(),
        Ok(Some(note)) => format!("ok; {note}"),
        Err(why) => why.to_string(),
    };
    (
        check.is_ok(),
        format!(
            "dry run: {}; bundle {bundle}; precheck {verdict}",
            plan_text(&steps)
        ),
    )
}

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
/// very first bundle (no pin yet) its activation into `bin\`, so `iemmode`
/// exists from then on.
pub fn install_offline(g: &mut Guard, zip: &Path) -> (bool, String) {
    let (sha, what) = match install_record(g, zip) {
        Ok(done) => done,
        Err(why) => return (false, why),
    };
    let detail = format!("bundle {sha} {what}");
    if g.state.pins.current.is_some() {
        return (true, detail);
    }
    match activate_files(g, &sha) {
        Ok(_) => (true, format!("{detail}; activated into bin")),
        Err(why) => (false, format!("{detail}; activation failed: {why}")),
    }
}

/// Copies the bundle's guard and `iemmode` into `bin\` and pins it; true
/// when the guard's exe changed.
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
    g.state.pins.promote(sha);
    g.save();
    Ok(changed)
}

/// `plan::activation` on this guard's state and the process list.
fn activation_now(pc: &mut dyn Pc, g: &Guard) -> Activation {
    let busy = Busy {
        switching: g.state.switching.is_some(),
        job: g.state.job,
        retry: g.state.interlock_retry.as_ref().map(|r| r.target),
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
fn activate(pc: &mut dyn Pc, g: &mut Guard, sha: &str) -> (bool, String) {
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
/// `iemmode` into `bin\`, the pin, its Defender exclusions (a failure
/// alarms, the activation stands); with `restart_job` (dev, inside a HIL
/// job) the engine and the server then run the new bundle (HIL checks
/// their versions, design §7). The detail and whether the guard's exe
/// changed.
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
    // The other pin keeps its exclusions (a revert needs them).
    let keep: Vec<String> = g
        .state
        .pins
        .previous
        .iter()
        .filter(|p| p.as_str() != sha)
        .cloned()
        .collect();
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
/// state and the process list; then `activate_bundle` (the bins, the pin,
/// the exclusions through the elevated task as online; the state and any
/// alarm are saved). It starts no guard: the next `iemmode` call starts
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
        match run_step(pc, g, step, Mode::Dev, &f) {
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
                    switch_text(Mode::Event, out, g.state.mode)
                ));
            }
        }
    }
    Ok(())
}

/// The HIL test signal, card-masked to `[guard] hil_tx` (design §4), only
/// inside a begun HIL job (its band-quiet and stage checks, design §7).
fn test_signal(
    pc: &mut dyn Pc,
    g: &mut Guard,
    input: &str,
    dbfs: f64,
    ttl_s: f64,
) -> (bool, String) {
    if let Err(why) = g.need_dev("test-signal") {
        return (false, why);
    }
    if g.state.job.is_none() {
        return (
            false,
            "a test signal needs a begun HIL job (job-begin)".to_owned(),
        );
    }
    if g.site.hil_tx.is_empty() {
        return (
            false,
            "[guard] hil_tx is empty: no card output may carry a test signal".to_owned(),
        );
    }
    if dbfs.is_nan() || dbfs > HIL_MAX_DBFS {
        return (
            false,
            format!("{dbfs} dBFS is above the HIL ceiling of {HIL_MAX_DBFS} dBFS"),
        );
    }
    if !ttl_s.is_finite() || ttl_s <= 0.0 {
        return (false, format!("a TTL of {ttl_s} s is not a positive time"));
    }
    if ttl_s > HIL_MAX_TTL_S {
        return (
            false,
            format!("a TTL of {ttl_s} s is above the HIL limit of {HIL_MAX_TTL_S} s"),
        );
    }
    let tx = g.site.hil_tx.clone();
    outcome(
        pc.engine_hil_signal(input, dbfs, ttl_s, &tx),
        &format!("test signal on {input} at {dbfs} dBFS for {ttl_s} s on card outputs {tx:?}"),
    )
}

/// `report <sha> <green|red> <detail>`: the bundle's HIL result.
fn report(g: &mut Guard, sha: &str, hil: &str, detail: &str) -> (bool, String) {
    let result = match hil {
        "green" => Hil::Green,
        "red" => Hil::Red,
        other => return (false, format!("HIL result {other:?}: green or red")),
    };
    let Some(rec) = g.state.bundles.get_mut(sha) else {
        return (false, format!("bundle {sha} is not installed"));
    };
    rec.hil = result;
    g.save();
    (true, format!("bundle {sha}: HIL {hil} ({detail})"))
}

/// A HIL job may begin in dev, after 5 min of band quiet and a quiet
/// 60 s of stage peaks from the engine's meters (design §7).
fn job_begin(pc: &mut dyn Pc, g: &mut Guard, run: u64) -> (bool, String) {
    if let Err(why) = g.need_dev("a HIL job") {
        return (false, why);
    }
    if let Some(other) = g.state.job {
        return (false, format!("HIL job {other} has not ended"));
    }
    let quiet = match pc.band_quiet_for() {
        Ok(d) => d,
        Err(e) => return (false, format!("band activity unreadable: {e}")),
    };
    if quiet < JOB_QUIET {
        return (
            false,
            format!(
                "the band was quiet for {} s; a job needs {} s",
                quiet.as_secs(),
                JOB_QUIET.as_secs()
            ),
        );
    }
    let c = g.cancel.clone();
    let peaks = match pc.engine_stage_peaks(JOB_PEAKS_S, &c) {
        Ok(p) => p,
        Err(e) => return (false, format!("stage peaks: {e}")),
    };
    if !stage_quiet(&peaks) {
        return (false, format!("stage peaks {peaks:?} dBFS: not quiet"));
    }
    g.state.job = Some(run);
    g.save();
    (true, format!("HIL job {run} began"))
}

fn job_end(g: &mut Guard, run: u64) -> (bool, String) {
    match g.state.job {
        Some(r) if r == run => {
            g.state.job = None;
            g.save();
            (true, format!("HIL job {run} ended"))
        }
        Some(r) => (false, format!("HIL job {r} runs, not {run}")),
        None => (true, format!("no HIL job runs (job {run} has ended)")),
    }
}

/// F30: the new site checked and installed, then dev entered again (the
/// engine and the server restart with it). Inside a HIL job (HIL applies
/// and reverts a synthetic change, design §7) only the engine and the
/// server restart: a dev entry would stop the runner that runs the job.
fn install_site(pc: &mut dyn Pc, g: &mut Guard, path: &str) -> (bool, String) {
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
            switch_text(Mode::Dev, out, g.state.mode)
        ),
    )
}

/// HIL's RT panic (design §7): dev, inside a begun HIL job, forwarded to
/// the engine (started with its fault-injection flag for the job, which
/// refuses it otherwise); its exit 70 is the watch's, which starts it again
/// after the backoff (`crash::after_exit`, the fade-in on the new stream).
fn inject_fault(pc: &mut dyn Pc, g: &mut Guard) -> (bool, String) {
    if let Err(why) = g.need_dev("inject-fault") {
        return (false, why);
    }
    if g.state.job.is_none() {
        return (
            false,
            "a fault needs a begun HIL job (job-begin)".to_owned(),
        );
    }
    outcome(
        pc.engine_inject_fault(),
        "the engine faults its RT callback; the watch starts it again",
    )
}

/// The owner-approved SEH test (design §10): dev, inside a begun HIL job,
/// forwarded to the engine (started with its fault-injection flag for the
/// job). The engine raises a structured exception on its RT callback; the
/// SEH filter releases the driver within its bound or parks the stream, and
/// the watch starts the engine again.
fn inject_seh(pc: &mut dyn Pc, g: &mut Guard) -> (bool, String) {
    if let Err(why) = g.need_dev("inject-seh") {
        return (false, why);
    }
    if g.state.job.is_none() {
        return (
            false,
            "an SEH test needs a begun HIL job (job-begin)".to_owned(),
        );
    }
    outcome(
        pc.engine_inject_seh(),
        "the engine raises a structured exception on its RT callback; \
         the SEH filter releases the driver or parks, and the watch starts it again",
    )
}

/// Ctrl-Break to the idle runner (bootstrap check).
fn runner_stop(pc: &mut dyn Pc, g: &mut Guard) -> (bool, String) {
    if let Err(why) = g.need_dev("runner-stop") {
        return (false, why);
    }
    if let Some(run) = g.state.job {
        return (false, format!("HIL job {run} runs: the runner is not idle"));
    }
    let c = g.cancel.clone();
    outcome(pc.runner_stop(&c), "the runner stopped")
}

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
        let Err(e) = run_step(pc, g, step, Mode::Event, &f) else {
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
            OnError::StopAskOwner => {
                g.alarm(step, &format!("rehearsal: {why}; health {health:?}"), true);
                return (false, format!("rehearsal stopped at {step:?}: {why}"));
            }
            OnError::Unwind | OnError::Continue | OnError::Skip(_) => {
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
        format!("{verdict}; {}", switch_text(Mode::Dev, out, g.state.mode)),
    )
}

// ---- the watch ----

/// The once-a-second watch (P10: the process list only): exits of our
/// children, REAPER or the app appearing in dev/live, a due respawn or
/// interlock retry, hourly drift, the end of the session.
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
    if g.respawn_at.is_some_and(|due| due <= at) {
        g.respawn_at = None;
        respawn(pc, g);
    }
    retry_due(pc, g);
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
    let abnormal = !session && !matches!(code, Some(0 | 2 | 3));
    let looped = abnormal && g.crash.record(at);
    let mode = g.state.mode;
    let n = g.crash.in_window();
    match crash::after_exit(code, mode, g.site.prod, session, looped, n) {
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

fn retry_due(pc: &mut dyn Pc, g: &mut Guard) {
    let Some(r) = g.state.interlock_retry.clone() else {
        return;
    };
    if g.now() < r.next_at {
        return;
    }
    info!(
        "trying the {} switch again after {} interlock refusals",
        mode_name(r.target),
        r.refusals
    );
    let (_, detail) = entry(
        pc,
        g,
        Entry {
            to: r.target,
            build: r.build,
            force: false,
            trial: false,
            dry_run: false,
            retry: true,
        },
    );
    info!("{detail}");
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
/// unwinds to event (or resumes, when it was one). The outcome of an event
/// plan that ran.
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
        run_switch(pc, g, from, Mode::Event)
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
                let reply = handle(pc, g, job.req, job.epoch);
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
        dry_event(pc, g)
    } else {
        event_now(pc, g)
    };
    send_notices(pc, g);
    g.look(pc);
    g.reply(ok, &format!("direct: {detail}"))
}

#[cfg(test)]
mod tests;
