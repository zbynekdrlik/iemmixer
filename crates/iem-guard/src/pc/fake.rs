use std::collections::HashMap;
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::rollback::{Files, moves, placed};

/// Every `Pc` method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Call {
    Procs,
    Facts,
    Adopt,
    Children,
    SetBundle,
    Precheck,
    ReaperSaveQuit,
    AppStop,
    Tuning,
    TuningDrift,
    PrefCheck,
    /// The report-only shadow import of an entry from event (S8 lane 4).
    Shadow,
    Data,
    EngineStart,
    EngineReady,
    EngineArm,
    EngineStop,
    EngineHealth,
    ServerStart,
    ServerStop,
    TrayStart,
    TrayStop,
    Identity,
    RunnerStart,
    RunnerStop,
    HolderGone,
    ReaperStart,
    /// The handover's wait for a REAPER that is still ending (#10).
    ReaperAwaitEnd,
    ReaperFacts,
    AppStart,
    AppAnswers,
    Fingerprint,
    ProbeTask,
    Notify,
    HilSignal,
    ForceReopen,
    InjectFault,
    InjectSeh,
    InjectPark,
    InstallSite,
    Exclude,
    /// The cutover task's export and disable (S8 lane 2).
    AutostartsOff,
    /// The cutover task's re-enable.
    AutostartsOn,
    /// The cutover task's guard logon trigger.
    GuardLogon,
    ServerConfig,
    WriteServerConfig,
    MemberPage,
    /// The rollback's export of the band's data (S8 lane 3).
    ExportProject,
    /// The rollback's renames of the project files.
    SwapProject,
}

impl Call {
    /// Calls that change the PC (a dry run makes none of them).
    pub fn mutates(self) -> bool {
        matches!(
            self,
            Call::ReaperSaveQuit
                | Call::AppStop
                | Call::Tuning
                | Call::PrefCheck
                | Call::Shadow
                | Call::Data
                | Call::EngineStart
                | Call::EngineArm
                | Call::EngineStop
                | Call::ServerStart
                | Call::ServerStop
                | Call::TrayStart
                | Call::TrayStop
                | Call::RunnerStart
                | Call::RunnerStop
                | Call::ReaperStart
                | Call::ReaperFacts
                | Call::AppStart
                | Call::Fingerprint
                | Call::ProbeTask
                | Call::Notify
                | Call::HilSignal
                | Call::ForceReopen
                | Call::InjectFault
                | Call::InjectSeh
                | Call::InjectPark
                | Call::InstallSite
                | Call::Exclude
                | Call::AutostartsOff
                | Call::AutostartsOn
                | Call::GuardLogon
                | Call::WriteServerConfig
                | Call::ExportProject
                | Call::SwapProject
        )
    }
}

/// How long a blocked call waits for its token before it fails (a test
/// that never pre-empts must not hang).
pub const BLOCK_LIMIT: Duration = Duration::from_secs(10);

/// One HIL test signal as sent: input, dBFS, TTL, card outputs, listen.
pub type SentSignal = (String, f64, f64, Vec<u16>, bool);

/// A scripted PC. Every call is recorded with the instant it began.
/// Starts and stops change `facts` the way the real ones change the PC,
/// so a re-plan sees what an earlier step did; a failed call changes
/// nothing. There is no force verb to record: the trait has none.
#[derive(Debug)]
pub struct FakePc {
    pub facts: Facts,
    pub app_exit: AppExit,
    pub reaper: ReaperFacts,
    pub status: Status,
    /// The writes a restore takes; 0: the preference holds REAPER's
    /// original. A script: every check finds it so again.
    pub pref_attempts: u32,
    /// What the preference reads while it is not the original.
    pub pref_value: String,
    /// Every write the checks made (none under a holder).
    pub pref_writes: u32,
    pub drift: Option<String>,
    /// Handed out (and emptied) by the next `procs()`.
    pub exited: Vec<(Kid, Option<i32>)>,
    pub notices: Vec<(Audience, String, String)>,
    pub bundle: Option<String>,
    pub kids: Children,
    /// The windows `engine_ready` was asked for, in order.
    pub ready_secs: Vec<u32>,
    /// Every HIL test signal sent: (input, dBFS, TTL, card outputs,
    /// listen).
    pub hil_signals: Vec<SentSignal>,
    /// The site files installed.
    pub sites: Vec<String>,
    /// Every engine start: (hold, hil).
    pub engine_starts: Vec<(bool, bool)>,
    /// Engines that end before they are ready, in start order: the
    /// code each ends with (`None`: no code). While the running engine
    /// has one, `engine_ready` fails as `WinPc`'s does when its child
    /// ends.
    pub early_exits: Vec<Option<i32>>,
    /// How the engine started last will end before it is ready.
    ending: Option<Option<i32>>,
    /// How it ended, once it did (`engine_exit`).
    exit: Option<Option<i32>>,
    /// Every exclusion request: (bundle, the bundles kept).
    pub excluded: Vec<(String, Vec<String>)>,
    /// What `engine_seen` reports while an engine runs (a read of what
    /// the connection holds: not a recorded call).
    pub seen: EngineSeen,
    /// `false`: an engine runs, but its hello and first `Status` have
    /// not come yet, so `engine_seen` answers `None` (as `WinPc` does).
    pub engine_up: bool,
    /// The PWA notification subscriptions the precheck reads (`None`:
    /// unreadable).
    pub subscriptions: Option<u32>,
    /// `[guard] pc_tests_passed` as the precheck reads it (default:
    /// passed, so a live trial goes as far as the other facts let it).
    pub pc_tests_passed: bool,
    /// What `job` reads (a fixed fact of the process: not a recorded
    /// call).
    pub job: Result<Placement, String>,
    /// What `logon` reads (a file read: not a recorded call).
    pub logon: Option<Logon>,
    /// What a passing `identity` names about the LAN certificate (its
    /// validity; #9 2026-09-28).
    pub lan_note: Option<String>,
    /// What `web_ports` reads (a read: not a recorded call).
    pub ports: Result<Ports, String>,
    /// The REAPER that runs is still ending (#10: its crash on quit held
    /// by Windows Error Reporting, or a quit the guard asked for):
    /// `reaper_procs` names it ending until `reaper_await_end` sees it
    /// gone.
    pub reaper_ending: bool,
    /// The ending REAPER outlasts the handover's wait.
    pub reaper_held: bool,
    /// REAPER's process ends by itself as this call begins (a crash on
    /// quit that Windows let go after the plan read its facts); once.
    pub reaper_ends_at: Option<Call>,
    /// A started REAPER's process shows only later: `reaper_start`
    /// leaves `facts.reaper` as it was.
    pub reaper_shows_late: bool,
    /// `reaper_procs` fails with this (an unreadable process list).
    pub reaper_procs_fail: Option<String>,
    /// The server's config (`server_config`, `write_server_config`); a
    /// server start applies `effects::web::pin_policy` to it as `WinPc`
    /// does.
    pub server_config: String,
    /// `write_server_config` answers Ok but the file keeps its bytes (the
    /// read-back must see it).
    pub config_sticks: bool,
    /// The guard task's logon trigger (`guard_logon`).
    pub guard_at_logon: bool,
    /// The export the predecessor's autostarts were disabled into
    /// (`autostarts_off`); none while they are enabled.
    pub autostarts_in: Option<String>,
    /// The project files the rollback exports and renames (`project` the
    /// band's project, `export` its export under its own name, `kept` the
    /// original kept beside it).
    pub project: Files,
    /// REAPER cannot open the export: its handover's facts name no track
    /// while the project's path holds it.
    pub export_unloadable: bool,
    /// `pc.toml` names a shadow command (S8 lane 4; default: none, so no
    /// entry plans the step). A read of the settings: not a recorded call.
    pub shadows: bool,
    /// Every call that takes a token returns `Preempted` when its token is
    /// pre-empted as it begins, as `WinPc`'s waits do within 1 s.
    pub waits_see_preemption: bool,
    /// "ide event" pre-empts this token as this call begins (a test of what
    /// comes after a step during which it came); once.
    pub preempt_at: Option<(Call, Cancel)>,
    calls: Vec<(Call, Instant)>,
    fails: HashMap<Call, String>,
    blocked: Vec<Call>,
    delays: HashMap<Call, Duration>,
    health: Health,
    next_pid: u32,
}

impl FakePc {
    pub fn new(facts: Facts) -> Self {
        Self {
            facts,
            app_exit: AppExit {
                exit_code: Some(0),
                ports_free: true,
                newer_temp: false,
                logged: true,
            },
            reaper: ReaperFacts {
                tracks: Some(40),
                expected_tracks: 40,
                dialogs: Vec::new(),
                heartbeat_advanced: true,
                holds_module: true,
                peaks: vec![-40.0],
            },
            status: Status {
                build: "2.0.0+test".into(),
                frames: 32,
                callbacks: 30_000,
                missed: 0,
                resets: 0,
                faulted: false,
                parked: false,
                hil: Vec::new(),
                loopback_samples: 0,
                late: 0,
                overruns: 0,
                process_max_us: 0.0,
                hist_top_us: 0,
                interval_hist: Vec::new(),
                process_hist: Vec::new(),
                last_reopen_us: 0,
                fault_callback_us: 0.0,
            },
            pref_attempts: 0,
            pref_value: "32".into(),
            pref_writes: 0,
            drift: None,
            exited: Vec::new(),
            notices: Vec::new(),
            bundle: None,
            kids: Children::default(),
            ready_secs: Vec::new(),
            hil_signals: Vec::new(),
            sites: Vec::new(),
            engine_starts: Vec::new(),
            early_exits: Vec::new(),
            ending: None,
            exit: None,
            excluded: Vec::new(),
            seen: EngineSeen {
                status: Status {
                    build: "2.0.0-dev.9+0123456789abcdef0123456789abcdef01234567".into(),
                    frames: 32,
                    callbacks: 30_000,
                    ..Status::default()
                },
                pipe_private: true,
                pipe_server_pid: None,
                last_fault_us: None,
            },
            engine_up: true,
            subscriptions: Some(1),
            pc_tests_passed: true,
            job: Ok(Placement::NoJob),
            logon: None,
            lan_note: None,
            ports: Ok((None, None)),
            reaper_ending: false,
            reaper_held: false,
            reaper_ends_at: None,
            reaper_shows_late: false,
            reaper_procs_fail: None,
            server_config: "port = 80\npin_changes = false\n".to_owned(),
            config_sticks: false,
            guard_at_logon: false,
            autostarts_in: None,
            project: Files {
                project: true,
                ..Files::default()
            },
            export_unloadable: false,
            shadows: false,
            waits_see_preemption: false,
            preempt_at: None,
            calls: Vec::new(),
            fails: HashMap::new(),
            blocked: Vec::new(),
            delays: HashMap::new(),
            health: Health::Dead,
            next_pid: 1000,
        }
    }

    /// `call` fails with `why` (and changes nothing).
    pub fn fail(&mut self, call: Call, why: &str) {
        self.fails.insert(call, why.to_owned());
    }

    /// `call` succeeds again (a failure that went away).
    pub fn heal(&mut self, call: Call) {
        self.fails.remove(&call);
    }

    /// `call` waits until its token is pre-empted, as `WinPc` waits, and
    /// then returns `Preempted`. Only calls that take a token can block.
    pub fn block_until_cancel(&mut self, call: Call) {
        self.blocked.push(call);
    }

    /// `call` takes `d` and ignores the token: a mutation finishes first.
    pub fn delay(&mut self, call: Call, d: Duration) {
        self.delays.insert(call, d);
    }

    /// What `engine_health` reads (default: dead).
    pub fn health(&mut self, h: Health) {
        self.health = h;
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.iter().map(|(c, _)| *c).collect()
    }

    pub fn called(&self, call: Call) -> bool {
        self.calls.iter().any(|(c, _)| *c == call)
    }

    pub fn count(&self, call: Call) -> usize {
        self.calls.iter().filter(|(c, _)| *c == call).count()
    }

    /// The position of the first `call`; panics when it was never made.
    pub fn index(&self, call: Call) -> usize {
        self.calls
            .iter()
            .position(|(c, _)| *c == call)
            .unwrap_or_else(|| panic!("{call:?} was never called: {:?}", self.calls()))
    }

    /// The first call that began at or after `at`.
    pub fn first_after(&self, at: Instant) -> Option<(Call, Instant)> {
        self.calls.iter().find(|(_, t)| *t >= at).copied()
    }

    /// The calls after the last `call`; panics when it was never made.
    pub fn calls_after(&self, call: Call) -> Vec<Call> {
        let last = self
            .calls
            .iter()
            .rposition(|(c, _)| *c == call)
            .unwrap_or_else(|| panic!("{call:?} was never called: {:?}", self.calls()));
        self.calls
            .get(last + 1..)
            .unwrap_or_default()
            .iter()
            .map(|(c, _)| *c)
            .collect()
    }

    /// The recorded calls that change the PC.
    pub fn mutating_calls(&self) -> Vec<Call> {
        self.calls().into_iter().filter(|c| c.mutates()).collect()
    }

    fn record(&mut self, call: Call) {
        self.calls.push((call, Instant::now()));
        if self.preempt_at.as_ref().is_some_and(|(at, _)| *at == call)
            && let Some((_, token)) = self.preempt_at.take()
        {
            token.preempt();
        }
        if self.reaper_ends_at == Some(call) {
            self.reaper_ends_at = None;
            self.reaper_ending = false;
            self.facts.reaper = false;
            self.facts.reaper_holds_module = false;
        }
    }

    fn pid(&mut self) -> u32 {
        self.next_pid += 1;
        self.next_pid
    }

    /// Records `call`, then plays its script: blocked, delayed, failed.
    fn enter(&mut self, call: Call, c: Option<&Cancel>) -> R<()> {
        self.record(call);
        if self.waits_see_preemption && c.is_some_and(Cancel::preempted) {
            return Err(StepError::Preempted);
        }
        if self.blocked.contains(&call) {
            let Some(c) = c else {
                return Err(StepError::failed(format!(
                    "{call:?} takes no token and cannot block"
                )));
            };
            let start = Instant::now();
            while start.elapsed() < BLOCK_LIMIT {
                c.sleep(Cancel::SLICE)?;
            }
            return Err(StepError::failed(format!(
                "{call:?} blocked {BLOCK_LIMIT:?} and was never pre-empted"
            )));
        }
        if let Some(d) = self.delays.get(&call) {
            thread::sleep(*d);
        }
        match self.fails.get(&call) {
            Some(why) => Err(StepError::Failed(why.clone())),
            None => Ok(()),
        }
    }
}

impl Pc for FakePc {
    fn procs(&mut self) -> Procs {
        self.record(Call::Procs);
        let one = |running: bool| if running { vec![1] } else { Vec::new() };
        let f = self.facts;
        Procs {
            reaper: one(f.reaper),
            app: one(f.app),
            engine: one(f.engine),
            server: one(f.server),
            tray: one(f.tray),
            runner: one(f.runner),
            exited: std::mem::take(&mut self.exited),
        }
    }

    fn facts(&mut self) -> Facts {
        self.record(Call::Facts);
        self.facts
    }

    fn adopt(&mut self, saved: &Children) -> Children {
        self.record(Call::Adopt);
        self.kids = saved.clone();
        self.kids.clone()
    }

    fn children(&mut self) -> Children {
        self.record(Call::Children);
        self.kids.clone()
    }

    fn set_bundle(&mut self, sha: Option<&str>) {
        self.record(Call::SetBundle);
        self.bundle = sha.map(str::to_owned);
    }

    fn precheck(&mut self, to: Mode, trial: bool) -> R<Option<String>> {
        self.enter(Call::Precheck, None)?;
        // The real verdict over `subscriptions` and `pc_tests_passed`;
        // every other fact passes.
        precheck(&PrecheckFacts {
            to,
            trial,
            bundle: true,
            pc_tests_passed: self.pc_tests_passed,
            subscriptions: self.subscriptions,
            foreign_engine: false,
            app_binary: Ok(()),
        })
    }

    fn reaper_save_quit(&mut self, c: &Cancel) -> R<()> {
        self.enter(Call::ReaperSaveQuit, Some(c))?;
        self.facts.reaper = false;
        self.facts.reaper_holds_module = false;
        Ok(())
    }

    fn app_stop(&mut self, c: &Cancel) -> R<AppExit> {
        self.enter(Call::AppStop, Some(c))?;
        if self.app_exit.exit_code.is_some() {
            self.facts.app = false;
            self.facts.app_serves = false;
        }
        Ok(self.app_exit)
    }

    fn tuning(&mut self, verb: &str, c: &Cancel) -> R<String> {
        self.enter(Call::Tuning, Some(c))?;
        Ok(format!("{verb}: ok"))
    }

    fn tuning_drift(&mut self) -> R<Option<String>> {
        self.enter(Call::TuningDrift, None)?;
        Ok(self.drift.clone())
    }

    /// The PC's decision over the facts: REAPER (pid 1), a foreign
    /// holder (99) or an engine (2) holds the driver module.
    fn pref_check(&mut self) -> R<PrefSeen> {
        self.enter(Call::PrefCheck, None)?;
        if self.pref_attempts == 0 {
            return Ok(PrefSeen::Original(0));
        }
        let f = self.facts;
        let mut holders = Vec::new();
        if f.reaper_holds_module {
            holders.push((1, "reaper.exe".to_owned()));
        }
        if f.other_module_holder {
            holders.push((99, "spike.exe".to_owned()));
        }
        if f.engine {
            holders.push((2, "iem-engine.exe".to_owned()));
        }
        let reaper: Vec<u32> = if f.reaper { vec![1] } else { Vec::new() };
        Ok(match card_holders(Some(holders.as_slice()), &reaper) {
            Some(by) => PrefSeen::Held(PrefHeld {
                value: Some(self.pref_value.clone()),
                by,
            }),
            None => {
                self.pref_writes += self.pref_attempts;
                PrefSeen::Original(self.pref_attempts)
            }
        })
    }

    fn data(&mut self, mode: Mode, c: &Cancel) -> R<String> {
        self.enter(Call::Data, Some(c))?;
        Ok(format!("{mode:?} data refreshed"))
    }

    fn shadows(&self) -> bool {
        self.shadows
    }

    /// A wait: a blocked or failing script plays as for any call; else the
    /// line is taken as recorded.
    fn shadow(&mut self, to: Mode, c: &Cancel) -> R<String> {
        self.enter(Call::Shadow, Some(c))?;
        Ok(format!(
            "shadow import ({}): import writes, 0 site and 0 state difference(s)",
            crate::shadow::entry(to)
        ))
    }

    fn engine_start(&mut self, hold: bool, hil: bool) -> R<u32> {
        self.enter(Call::EngineStart, None)?;
        self.engine_starts.push((hold, hil));
        self.facts.engine = true;
        self.ending = (!self.early_exits.is_empty()).then(|| self.early_exits.remove(0));
        self.exit = None;
        Ok(self.pid())
    }

    fn engine_ready(&mut self, secs: u32, c: &Cancel) -> R<Status> {
        self.ready_secs.push(secs);
        self.enter(Call::EngineReady, Some(c))?;
        if let Some(code) = self.ending.take() {
            self.facts.engine = false;
            self.exit = Some(code);
            return Err(StepError::failed(format!(
                "the engine ended ({code:?}) before it was ready"
            )));
        }
        Ok(self.status.clone())
    }

    /// A read of the watched engine: not a recorded call.
    fn engine_exit(&mut self) -> Option<Option<i32>> {
        self.exit
    }

    fn engine_arm(&mut self) -> R<()> {
        self.enter(Call::EngineArm, None)
    }

    fn engine_stop(&mut self, c: &Cancel) -> R<()> {
        self.enter(Call::EngineStop, Some(c))?;
        self.facts.engine = false;
        Ok(())
    }

    fn engine_health(&mut self) -> R<Health> {
        self.enter(Call::EngineHealth, None)?;
        Ok(self.health)
    }

    /// The PIN rule as `WinPc` reads it from the config (S8).
    fn server_start(&mut self, _mode: Mode, prod: bool) -> R<u32> {
        self.enter(Call::ServerStart, None)?;
        crate::effects::web::pin_policy(&self.server_config, prod).map_err(StepError::Failed)?;
        self.facts.server = true;
        Ok(self.pid())
    }

    fn server_stop(&mut self, c: &Cancel) -> R<()> {
        self.enter(Call::ServerStop, Some(c))?;
        self.facts.server = false;
        Ok(())
    }

    fn tray_start(&mut self) -> R<()> {
        self.enter(Call::TrayStart, None)?;
        self.facts.tray = true;
        Ok(())
    }

    fn tray_stop(&mut self, c: &Cancel) -> R<()> {
        self.enter(Call::TrayStop, Some(c))?;
        self.facts.tray = false;
        Ok(())
    }

    fn identity(&mut self, _sha: &str, c: &Cancel) -> R<Option<String>> {
        self.enter(Call::Identity, Some(c))?;
        Ok(self.lan_note.clone())
    }

    fn runner_start(&mut self) -> R<()> {
        self.enter(Call::RunnerStart, None)?;
        self.facts.runner = true;
        Ok(())
    }

    fn runner_stop(&mut self, c: &Cancel) -> R<()> {
        self.enter(Call::RunnerStop, Some(c))?;
        self.facts.runner = false;
        Ok(())
    }

    fn holder_gone(&mut self, c: &Cancel) -> R<()> {
        self.enter(Call::HolderGone, Some(c))?;
        self.facts.other_module_holder = false;
        Ok(())
    }

    fn reaper_start(&mut self) -> R<()> {
        self.enter(Call::ReaperStart, None)?;
        if !self.reaper_shows_late {
            self.facts.reaper = true;
            self.facts.reaper_holds_module = true;
        }
        Ok(())
    }

    /// A read of REAPER's processes (#10): not a recorded call.
    fn reaper_procs(&mut self) -> R<ReaperProcs> {
        if let Some(why) = &self.reaper_procs_fail {
            return Err(StepError::Failed(why.clone()));
        }
        let up = u32::from(self.facts.reaper);
        Ok(if self.reaper_ending {
            ReaperProcs {
                running: 0,
                ending: up,
            }
        } else {
            ReaperProcs {
                running: up,
                ending: 0,
            }
        })
    }

    fn reaper_await_end(&mut self, c: &Cancel) -> R<()> {
        self.enter(Call::ReaperAwaitEnd, Some(c))?;
        if self.reaper_ending && !self.reaper_held {
            self.reaper_ending = false;
            self.facts.reaper = false;
            self.facts.reaper_holds_module = false;
        }
        Ok(())
    }

    fn reaper_facts(&mut self, c: &Cancel) -> R<ReaperFacts> {
        self.enter(Call::ReaperFacts, Some(c))?;
        let on_export = placed(Want::Export, self.project) == Some(Placed::Export);
        if self.export_unloadable && on_export {
            return Ok(ReaperFacts {
                tracks: None,
                ..self.reaper.clone()
            });
        }
        Ok(self.reaper.clone())
    }

    fn app_start(&mut self) -> R<()> {
        self.enter(Call::AppStart, None)?;
        self.facts.app = true;
        self.facts.app_serves = true;
        Ok(())
    }

    fn app_answers(&mut self, c: &Cancel) -> R<()> {
        self.enter(Call::AppAnswers, Some(c))
    }

    fn fingerprint(&mut self) -> R<()> {
        self.enter(Call::Fingerprint, None)
    }

    fn probe_task(&mut self) -> R<()> {
        self.enter(Call::ProbeTask, None)
    }

    fn notify(&mut self, audience: Audience, title: &str, body: &str) -> R<()> {
        self.enter(Call::Notify, None)?;
        self.notices
            .push((audience, title.to_owned(), body.to_owned()));
        Ok(())
    }

    fn engine_hil_signal(
        &mut self,
        input: &str,
        dbfs: f64,
        ttl_s: f64,
        card_tx: &[u16],
        listen: bool,
    ) -> R<()> {
        self.enter(Call::HilSignal, None)?;
        self.hil_signals
            .push((input.to_owned(), dbfs, ttl_s, card_tx.to_vec(), listen));
        Ok(())
    }

    fn engine_force_reopen(&mut self) -> R<()> {
        self.enter(Call::ForceReopen, None)
    }

    fn engine_inject_fault(&mut self) -> R<()> {
        self.enter(Call::InjectFault, None)
    }

    fn engine_inject_seh(&mut self) -> R<()> {
        self.enter(Call::InjectSeh, None)
    }

    fn engine_inject_park(&mut self) -> R<()> {
        self.enter(Call::InjectPark, None)
    }

    fn engine_seen(&mut self) -> Option<EngineSeen> {
        (self.facts.engine && self.engine_up).then(|| self.seen.clone())
    }

    fn install_site(&mut self, path: &str, c: &Cancel) -> R<String> {
        self.enter(Call::InstallSite, Some(c))?;
        self.sites.push(path.to_owned());
        Ok(format!("{path}: checked and installed"))
    }

    fn exclude(&mut self, sha: &str, keep: &[String]) -> R<()> {
        self.enter(Call::Exclude, None)?;
        self.excluded.push((sha.to_owned(), keep.to_vec()));
        Ok(())
    }

    fn job(&mut self) -> Result<Placement, String> {
        self.job.clone()
    }

    fn web_ports(&mut self) -> R<Ports> {
        self.ports.clone().map_err(StepError::Failed)
    }

    fn logon(&mut self) -> Option<Logon> {
        self.logon.clone()
    }

    fn autostarts_off(&mut self, export: &str) -> R<String> {
        self.enter(Call::AutostartsOff, None)?;
        self.autostarts_in = Some(export.to_owned());
        Ok(format!("exported to {export}; 2 disabled"))
    }

    /// Re-enables what `export` saved; any other export saved nothing.
    fn autostarts_on(&mut self, export: &str) -> R<String> {
        self.enter(Call::AutostartsOn, None)?;
        if self.autostarts_in.as_deref() == Some(export) {
            self.autostarts_in = None;
            Ok(format!("2 re-enabled from {export}"))
        } else {
            Ok(format!("no export {export}: nothing to re-enable"))
        }
    }

    fn guard_logon(&mut self, on: bool) -> R<()> {
        self.enter(Call::GuardLogon, None)?;
        self.guard_at_logon = on;
        Ok(())
    }

    fn server_config(&mut self) -> R<String> {
        self.enter(Call::ServerConfig, None)?;
        Ok(self.server_config.clone())
    }

    fn write_server_config(&mut self, text: &str) -> R<()> {
        self.enter(Call::WriteServerConfig, None)?;
        if !self.config_sticks {
            self.server_config = text.to_owned();
        }
        Ok(())
    }

    fn member_page(&mut self) -> R<()> {
        self.enter(Call::MemberPage, None)
    }

    fn export_project(&mut self, _at: u64) -> R<String> {
        self.enter(Call::ExportProject, None)?;
        if self.project.export {
            return Err(StepError::failed(
                "the export exists: an export never overwrites a file",
            ));
        }
        self.project.export = true;
        Ok("export: self-check passed".to_owned())
    }

    fn swap_project(&mut self, want: Want, _at: u64) -> R<Placed> {
        self.enter(Call::SwapProject, None)?;
        for m in moves(want, self.project).map_err(StepError::Failed)? {
            self.project = m.apply(self.project);
        }
        placed(want, self.project)
            .ok_or_else(|| StepError::failed("the project files do not read back"))
    }
}
