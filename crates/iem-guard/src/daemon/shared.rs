//! What the guard pipe's threads share with the daemon thread (design
//! §5.1): the view they answer from while a switch runs, the switch
//! generation a request saw, and where the pipe sends a request.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use super::reply::outcome_text;
use super::{DETAIL_CHARS, Outcome, cut, mode_name};
use crate::alarms::Alarm;
use crate::cancel::Cancel;
use crate::plan::Mode;
use crate::proto::{self, EngineStatus, Reply, Request};
use crate::state::Switching;
use crate::switch_log::{LastSwitch, needs_owner_text};

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
    /// The steps of the last switch that asked the owner while its plan
    /// went on (#10), each "<Step> failed: <why>".
    pub last_owner: Vec<String>,
    /// Counts the switches begun: a request queued before one began is
    /// answered as during it (a dev or live entry by `fence`).
    pub epoch: u64,
    /// Counts what a dev or live entry queued before it must not follow
    /// (#42): every switch begun but the start's checks, and every "ide
    /// event" the pipe routes.
    pub fence: u64,
    /// The switch begun last is the start's checks (event → event, #42): a
    /// dev or live entry routed while it runs is queued behind it.
    pub start_checks: bool,
    /// The switch begun last (from, to): a fenced entry's reply names it.
    pub began: Option<(Mode, Mode)>,
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
    /// `GuardState.last_switch` (`Reply.last_switch`, S7).
    pub last_switch: Option<LastSwitch>,
    /// A plain `iemmode event` (the engineer's button) is a rollback
    /// (`rollback::button_rolls_back`, S8 lane 3).
    pub rolls_back: bool,
    /// The lifecycle is prod (S8 lane 5): "ide event" waits for a switch to
    /// live instead of pre-empting it, and live counts as its end.
    pub prod: bool,
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
            last_switch: self.last_switch.clone(),
        }
    }

    /// The generation a request routed now sees.
    pub fn generation(&self) -> Generation {
        Generation {
            epoch: self.epoch,
            fence: self.fence,
        }
    }

    /// "ide event" once no switch runs: done when the last switch ended in
    /// `event`, or in prod in `live` (S8 lane 5: after the cutover iemmixer
    /// serves the band at an event, so a switch to live is what "ide event"
    /// waits for, never what it cancels).
    pub fn event_reply(&self, note: &str) -> Reply {
        let target = if self.prod && self.mode == Mode::Live {
            Mode::Live
        } else {
            Mode::Event
        };
        let ok = self.running.is_none() && self.mode == target && self.last == Some(Outcome::Done);
        let how = if self.last == Some(Outcome::NeedsOwner) && !self.last_owner.is_empty() {
            needs_owner_text("event", mode_name(self.mode), &self.last_owner)
        } else {
            format!("{}: {}", mode_name(target), outcome_text(self.last))
        };
        self.reply(ok, &format!("{note}; {how}"))
    }
}

/// Why a request is refused while a switch runs.
pub fn while_switching(req: &Request) -> &'static str {
    match req {
        Request::Dev { .. }
        | Request::Live { .. }
        | Request::Cutover { .. }
        | Request::Rollback { .. } => "busy",
        _ => "switching",
    }
}

/// The switch generation a request saw when the pipe routed it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Generation {
    /// [`View::epoch`].
    pub epoch: u64,
    /// [`View::fence`].
    pub fence: u64,
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
    Queue(Generation),
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

    pub fn generation(&self) -> Generation {
        self.lock().generation()
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
    /// begins. "Ide event" moves the fence, so a dev or live entry queued
    /// before it never runs after it; one routed while the start's checks
    /// run is queued behind them (#42).
    pub fn route(&self, req: &Request) -> Route {
        let mut v = self.lock();
        if matches!(req, Request::Event { dry_run: false, .. }) {
            v.fence += 1;
        }
        match (req, v.running) {
            (Request::Subscribe, _) => Route::Subscribe,
            (Request::Status, _) => Route::Now(v.reply(true, &v.status)),
            // In prod the button is the rollback (S8 lane 3): it runs after
            // the switch in progress, never answered as its end; it pre-empts
            // a dev or live entry, never an event plan (whose waits would
            // fail on the token).
            (
                Request::Event {
                    dry_run: false,
                    signal: false,
                },
                Some(running),
            ) if v.rolls_back => {
                if running != Mode::Event {
                    self.cancel.preempt();
                }
                Route::Queue(v.generation())
            }
            (Request::Event { dry_run: false, .. }, Some(Mode::Event)) => {
                Route::AwaitEnd("already switching to event")
            }
            // In prod "ide event" waits for a switch to live (S8 lane 5): it
            // is the band's system, which an event keeps (`rollback::on_event`).
            (
                Request::Event {
                    dry_run: false,
                    signal: true,
                },
                Some(Mode::Live),
            ) if v.prod => Route::AwaitEnd("waited for the switch to live in prod"),
            (Request::Event { dry_run: false, .. }, Some(_)) => {
                self.cancel.preempt();
                Route::AwaitEnd("pre-empted the switch in progress")
            }
            (Request::Event { dry_run: false, .. }, None) => {
                // A request queued before it pre-empts at its start.
                self.cancel.preempt();
                Route::Queue(v.generation())
            }
            (
                Request::Dev { .. }
                | Request::Live { .. }
                | Request::Cutover { .. }
                | Request::Rollback { .. },
                Some(_),
            ) if v.start_checks => Route::Queue(v.generation()),
            (_, Some(_)) => Route::Now(v.reply(false, while_switching(req))),
            (_, None) => Route::Queue(v.generation()),
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

    /// "Ide event"'s own switch to live in prod (S8 lane 5,
    /// `rollback::OnEvent::Live`), claimed under the view's lock before
    /// anything is read for it: the token is cleared (a pre-emption routed
    /// before this, another "ide event" or the button, is answered by the
    /// queue as the switch's end or runs after it) and the view shows the
    /// switch to live, so an "ide event" routed from now on waits for it and
    /// never pre-empts it. Its `begin` (or the event plan when the pin may
    /// not go live) follows.
    pub fn claim_live(&self) {
        let mut v = self.lock();
        self.cancel.clear();
        v.running = Some(Mode::Live);
        v.version += 1;
        drop(v);
        self.changed.notify_all();
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
