//! `WinPc`: the [`Pc`] of the IEM PC (S6 plan Task 9, design §5).
//!
//! Built on `iem_win`'s safe wrappers, our scheduled tasks (`schtasks.exe`,
//! arguments only), Windows' own `curl.exe` for HTTPS and `ureq` for plain
//! HTTP on this PC. Settings come from the site's `[guard]` and `[card]`
//! tables and `%LOCALAPPDATA%\iemmixer\guard\pc.toml`.
//!
//! Nothing here ends a process: REAPER saves and quits by its own actions,
//! the predecessor app by its tray menu's Exit command, the engine by
//! `Shutdown`, the server and runner by Ctrl-Break on their own consoles,
//! the tray by `Quit`; every stop is then a bounded wait on a handle opened
//! before the request.
//!
//! Excluded from mutation testing (it is not compiled on Linux); its
//! decisions are the portable `crate::effects`, `crate::pc` and
//! `crate::handover`, which are mutated.

mod app;
mod card;
mod engine;
mod procs;
mod reaper;
mod tasks;
mod web;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use iem_win::spawn::{self, Placement};
use iem_win::window::{self, SessionEndWindow};
use tracing::{info, warn};

use crate::cancel::Cancel;
use crate::handover::{AppExit, ReaperFacts};
use crate::pc::{
    self, Audience, EngineSeen, Images, Kid, Pc, Ports, PrefSeen, Procs, R, Status, StepError,
};
use crate::plan::{Facts, Health, Mode};
use crate::site::{self, Settings};
use crate::state::Children;

/// Asks the tray to quit over the guard pipe (the daemon knows the route).
pub type TrayQuit = Box<dyn FnMut() -> Result<(), String> + Send>;

/// Plain HTTP on this PC answers in milliseconds; the bound keeps every
/// wait that polls it pre-emptible within 1 s.
const HTTP_TIMEOUT: Duration = Duration::from_millis(800);

pub struct WinPc {
    s: Settings,
    images: Images,
    /// The bundle every start runs from (the current pin).
    bundle: Option<String>,
    kids: procs::Kids,
    http: ureq::Agent,
    sup: Option<engine::Supervisor>,
    /// The engine pid whose control pipe's DACL was read, and the verdict.
    dacl: Option<(u32, bool)>,
    tray_quit: Option<TrayQuit>,
}

impl WinPc {
    pub fn new(s: Settings) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(HTTP_TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        Self {
            images: s.images(),
            s,
            bundle: None,
            kids: procs::Kids::default(),
            http: ureq::Agent::new_with_config(config),
            sup: None,
            dacl: None,
            tray_quit: None,
        }
    }

    /// The settings of `%LOCALAPPDATA%\iemmixer\guard\pc.toml` and its site.
    pub fn load() -> Result<Self, String> {
        let base = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
        Settings::load(&site::pc_toml_path(Path::new(&base))).map(Self::new)
    }

    pub fn settings(&self) -> &Settings {
        &self.s
    }

    /// How `tray_stop` asks the tray to quit (set by the daemon).
    pub fn set_tray_quit(&mut self, quit: TrayQuit) {
        self.tray_quit = Some(quit);
    }

    fn bundle_dir(&self) -> R<PathBuf> {
        self.bundle
            .as_deref()
            .map(|sha| self.s.bundle_dir(sha))
            .ok_or_else(|| StepError::failed("no bundle is active"))
    }
}

impl Pc for WinPc {
    fn procs(&mut self) -> Procs {
        let mut p = procs::list(self);
        p.exited = self.kids.reap();
        if p.exited.iter().any(|(kid, _)| *kid == Kid::Engine) {
            // A dead engine's stream is dropped at once: it only reads the
            // end, and the next look connects to the respawned engine and
            // reads its DACL again.
            self.sup = None;
            self.dacl = None;
        }
        p
    }

    fn facts(&mut self) -> Facts {
        let p = procs::list(self);
        let holders = match iem_win::process::module_holders(&self.s.card.module) {
            Ok(h) => Some(h),
            Err(e) => {
                warn!("the driver module's holders could not be read: {e}");
                None
            }
        };
        let ports = match web::ports() {
            Ok(ports) => Some(ports),
            Err(e) => {
                warn!("{e}");
                None
            }
        };
        pc::facts_from(&p, holders.as_deref(), ports)
    }

    fn adopt(&mut self, saved: &Children) -> Children {
        for kid in Kid::ALL {
            if let Some(child) = saved.of(kid)
                && !self.kids.adopt(kid, child)
            {
                warn!(
                    "the saved {} (pid {}) is not the guard's any more",
                    kid.id(),
                    child.pid
                );
            }
        }
        self.kids.records()
    }

    fn children(&mut self) -> Children {
        self.kids.records()
    }

    fn set_bundle(&mut self, sha: Option<&str>) {
        self.bundle = sha.map(str::to_owned);
    }

    fn precheck(&mut self, to: Mode, trial: bool) -> R<Option<String>> {
        app::precheck(self, to, trial)
    }

    fn reaper_save_quit(&mut self, c: &Cancel) -> R<()> {
        reaper::save_quit(self, c)
    }

    fn app_stop(&mut self, c: &Cancel) -> R<AppExit> {
        app::stop(self, c)
    }

    /// A mutation in the elevated task: it finishes first, the token waits.
    fn tuning(&mut self, verb: &str, _c: &Cancel) -> R<String> {
        tasks::tuning(self, verb)
    }

    fn tuning_drift(&mut self) -> R<Option<String>> {
        tasks::drift(self)
    }

    fn pref_check(&mut self) -> R<PrefSeen> {
        card::pref_check(self)
    }

    /// A started command finishes (it writes the guard's own data); "ide
    /// event" stops the refresh between two commands and after the last.
    fn data(&mut self, mode: Mode, c: &Cancel) -> R<String> {
        tasks::data(self, mode, c)
    }

    fn engine_start(&mut self, hold: bool, hil: bool) -> R<u32> {
        engine::start(self, hold, hil)
    }

    fn engine_ready(&mut self, secs: u32, c: &Cancel) -> R<Status> {
        engine::ready(self, secs, c)
    }

    fn engine_exit(&mut self) -> Option<Option<i32>> {
        self.kids.ended(Kid::Engine)
    }

    fn engine_arm(&mut self) -> R<()> {
        engine::arm(self)
    }

    fn engine_stop(&mut self, c: &Cancel) -> R<()> {
        engine::stop(self, c)
    }

    fn engine_health(&mut self) -> R<Health> {
        engine::health(self)
    }

    fn server_start(&mut self, mode: Mode) -> R<u32> {
        procs::server_start(self, mode)
    }

    fn server_stop(&mut self, c: &Cancel) -> R<()> {
        procs::server_stop(self, c)
    }

    fn tray_start(&mut self) -> R<()> {
        procs::tray_start(self)
    }

    fn tray_stop(&mut self, c: &Cancel) -> R<()> {
        procs::tray_stop(self, c)
    }

    fn identity(&mut self, sha: &str, c: &Cancel) -> R<Option<String>> {
        web::identity(self, sha, c)
    }

    fn runner_start(&mut self) -> R<()> {
        procs::runner_start(self)
    }

    fn runner_stop(&mut self, c: &Cancel) -> R<()> {
        procs::runner_stop(self, c)
    }

    fn holder_gone(&mut self, c: &Cancel) -> R<()> {
        card::holder_gone(self, c)
    }

    fn reaper_start(&mut self) -> R<()> {
        reaper::start(self)
    }

    fn reaper_facts(&mut self, c: &Cancel) -> R<ReaperFacts> {
        reaper::facts(self, c)
    }

    fn app_start(&mut self) -> R<()> {
        app::start(self)
    }

    fn app_answers(&mut self, c: &Cancel) -> R<()> {
        app::answers(self, c)
    }

    fn fingerprint(&mut self) -> R<()> {
        tasks::fingerprint(self)
    }

    fn probe_task(&mut self) -> R<()> {
        tasks::probe()
    }

    fn notify(&mut self, audience: Audience, title: &str, body: &str) -> R<()> {
        procs::notify(self, audience, title, body)
    }

    fn engine_hil_signal(&mut self, input: &str, dbfs: f64, ttl_s: f64, card_tx: &[u16]) -> R<()> {
        engine::hil_signal(self, input, dbfs, ttl_s, card_tx)
    }

    fn engine_force_reopen(&mut self) -> R<()> {
        engine::force_reopen(self)
    }

    fn engine_inject_fault(&mut self) -> R<()> {
        engine::inject_fault(self)
    }

    fn engine_inject_seh(&mut self) -> R<()> {
        engine::inject_seh(self)
    }

    fn engine_inject_park(&mut self) -> R<()> {
        engine::inject_park(self)
    }

    fn engine_seen(&mut self) -> Option<EngineSeen> {
        engine::seen(self)
    }

    fn install_site(&mut self, path: &str, c: &Cancel) -> R<String> {
        engine::install_site(self, path, c)
    }

    fn web_ports(&mut self) -> R<Ports> {
        web::ports()
    }

    fn exclude(&mut self, sha: &str, keep: &[String]) -> R<()> {
        tasks::exclude(self, sha, keep)
    }

    /// `logon.result.json` in the elevated root's `tasks\out`; no file is
    /// no run yet.
    fn logon(&mut self) -> Option<crate::effects::tuning::Logon> {
        use crate::effects::tuning;
        let path = self
            .s
            .results_dir()
            .join(tuning::result_name(tuning::LOGON));
        match std::fs::read_to_string(&path) {
            Ok(text) => tuning::logon_result(&text, &self.images.reaper),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => {
                warn!("{}: {e}", path.display());
                None
            }
        }
    }

    /// The job's limits go to the log with the placement they give.
    fn job(&mut self) -> Result<Placement, String> {
        let limits = spawn::job_limits().map_err(|e| e.to_string())?;
        let placed = spawn::placement(limits);
        info!("the guard's job: {limits:?}; its children: {placed:?}");
        Ok(placed)
    }
}

/// Starts the guard's task (`iemmode`, when nothing answers on the pipe).
pub fn start_guard_task() -> Result<(), String> {
    tasks::run_task(crate::effects::tasks::GUARD).map_err(|e| e.to_string())
}

/// How often the session window's thread looks at its messages.
const SESSION_PUMP: Duration = Duration::from_millis(50);

/// The hidden top-level window for the end of the Windows session (design
/// §5.4), on a thread of its own that pumps its messages. At the end of the
/// session it sets `ended` and runs `wait` (the daemon stops respawning,
/// gives the engine its time and stops the server and tray) while Windows
/// shows why the session waits.
pub fn watch_session_end(
    ended: Arc<AtomicBool>,
    wait: impl FnOnce() + Send + 'static,
) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("iemmixer-guard-session".to_owned())
        .spawn(move || {
            let window = match SessionEndWindow::create("iemmixer stops its processes", ended, wait)
            {
                Ok(w) => w,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            };
            let _ = tx.send(Ok(()));
            info!("the session window is {:#x}", window.hwnd());
            loop {
                match window::pump() {
                    Ok(false) => thread::sleep(SESSION_PUMP),
                    Ok(true) => break,
                    Err(e) => {
                        warn!("the session window's messages: {e}");
                        break;
                    }
                }
            }
            drop(window);
        })?;
    rx.recv()
        .map_err(|_| io::Error::other("the session window's thread ended"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::tests::settings;

    /// On the CI's Windows runner: a WinPc from the synthetic site reads the
    /// real process list and refuses what needs a bundle, without touching
    /// anything.
    #[test]
    fn a_win_pc_without_a_bundle_reads_and_refuses() {
        let mut pc = WinPc::new(settings());
        assert_eq!(pc.settings().guard.app_exit_id, 4242);
        let p = pc.procs();
        assert!(p.exited.is_empty());
        // The facts read the process list, the module's holders and the ports.
        let _ = pc.facts();
        assert_eq!(pc.children(), Children::default());
        let failed = |r: R<u32>| matches!(r, Err(StepError::Failed(_)));
        assert!(failed(pc.engine_start(true, false)));
        // No engine of ours runs: nothing to see, nothing connected.
        assert_eq!(pc.engine_seen(), None);
        assert!(failed(pc.server_start(Mode::Dev)));
        assert!(matches!(
            pc.notify(Audience::Alarm, "t", "b"),
            Err(StepError::Failed(_))
        ));
        pc.set_bundle(Some("0123456789abcdef0123456789abcdef01234567"));
        assert!(pc.bundle_dir().is_ok());
        pc.set_bundle(None);
        assert!(pc.bundle_dir().is_err());
    }
}
