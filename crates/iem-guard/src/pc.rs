//! The PC's effects behind one trait (S6 design note §5.1, §5.2; plan Task 9).
//!
//! The daemon drives every switch step through [`Pc`]: `win::WinPc` on the
//! PC, `fake::FakePc` in tests. Every method is bounded in time and none
//! ends a process: a stop is a request (a pipe command, the tray's menu
//! command, Ctrl-Break, a web-control action) and then a bounded wait. Every
//! method that waits takes the pre-emption token and returns
//! [`StepError::Preempted`] within 1 s of "ide event"; a mutating call (a
//! save, a quit command, a registry write, a data refresh, a tuning run)
//! finishes its mutation first.
//!
//! The decisions of the effects live here and in [`crate::effects`],
//! portable and mutation-tested; `win` only reads and acts.

use std::fmt;

use iem_win::prefwin::Checked;
use iem_win::spawn::Placement;

use crate::cancel::{Cancel, Preempted};
use crate::effects::app::holders_text;
use crate::effects::tuning::Logon;
use crate::handover::{AppExit, ReaperFacts};
use crate::plan::{Facts, Health, Mode};
use crate::proto::HilOut;
use crate::state::{Child, Children};

pub type R<T> = Result<T, StepError>;

/// Why a step did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepError {
    /// "ide event" ended a wait.
    Preempted,
    Failed(String),
}

impl StepError {
    pub fn failed(why: impl Into<String>) -> Self {
        Self::Failed(why.into())
    }
}

impl From<Preempted> for StepError {
    fn from(_: Preempted) -> Self {
        Self::Preempted
    }
}

impl fmt::Display for StepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preempted => f.write_str("pre-empted by event"),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for StepError {}

/// What a notice is (`iem-server notify --to`, design §5.4). It goes to the
/// engineer's devices: the mixer app's (the PWA's) notification
/// subscriptions, as the predecessor's alerts did (#9 2026-09-28).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// A technical alarm of the guard.
    Alarm,
}

impl Audience {
    /// The `--to` argument of `iem-server notify`.
    pub fn arg(self) -> &'static str {
        match self {
            Self::Alarm => "alarm",
        }
    }
}

/// The processes the guard starts and watches (design §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kid {
    Engine,
    Server,
    Tray,
    Runner,
}

impl Kid {
    pub const ALL: [Kid; 4] = [Kid::Engine, Kid::Server, Kid::Tray, Kid::Runner];

    /// A short name for messages and log files.
    pub fn id(self) -> &'static str {
        match self {
            Self::Engine => "engine",
            Self::Server => "server",
            Self::Tray => "tray",
            Self::Runner => "runner",
        }
    }
}

impl Children {
    /// The record of `kid`, if the guard started or adopted one.
    pub fn of(&self, kid: Kid) -> Option<&Child> {
        match kid {
            Kid::Engine => self.engine.as_ref(),
            Kid::Server => self.server.as_ref(),
            Kid::Tray => self.tray.as_ref(),
            Kid::Runner => self.runner.as_ref(),
        }
    }
}

/// The image names the process list is read for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Images {
    pub reaper: String,
    pub app: String,
    pub engine: String,
    pub server: String,
    pub tray: String,
    pub runner: String,
}

/// The once-a-second look (P10): the pids of each image, and the children of
/// ours that ended since the last look.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Procs {
    pub reaper: Vec<u32>,
    pub app: Vec<u32>,
    pub engine: Vec<u32>,
    pub server: Vec<u32>,
    pub tray: Vec<u32>,
    pub runner: Vec<u32>,
    /// Our children that ended, with their exit codes (`None`: no code could
    /// be read); the daemon applies `crash::after_exit` to the engine's.
    pub exited: Vec<(Kid, Option<i32>)>,
}

impl Procs {
    /// Picks the images out of a process list (names compare without ASCII
    /// case, as Windows does).
    pub fn from_list(list: &[(u32, String)], images: &Images) -> Self {
        let pick = |image: &str| -> Vec<u32> {
            list.iter()
                .filter(|(_, name)| name.eq_ignore_ascii_case(image))
                .map(|(pid, _)| *pid)
                .collect()
        };
        Self {
            reaper: pick(&images.reaper),
            app: pick(&images.app),
            engine: pick(&images.engine),
            server: pick(&images.server),
            tray: pick(&images.tray),
            runner: pick(&images.runner),
            exited: Vec::new(),
        }
    }

    /// REAPER or the predecessor app runs: the band's system is up (design
    /// §5.2, the reboot rule).
    pub fn band_up(&self) -> bool {
        !self.reaper.is_empty() || !self.app.is_empty()
    }

    /// The pids of one of our children's images.
    pub fn of(&self, kid: Kid) -> &[u32] {
        match kid {
            Kid::Engine => &self.engine,
            Kid::Server => &self.server,
            Kid::Tray => &self.tray,
            Kid::Runner => &self.runner,
        }
    }
}

/// The listening pids of ports 80 and 443 (`None`: free).
pub type Ports = (Option<u32>, Option<u32>);

/// The facts of a plan (design §5.1) from the process list, the driver
/// module's holders and the owners of ports 80/443.
///
/// An unreadable holder list assumes a running REAPER holds the card (the
/// handover checks it) and no foreign holder (`reaper_start` reads again and
/// refuses on one); unreadable ports assume a running app serves (the app
/// handover checks it). So a failed read never restarts a REAPER or an app
/// that serves the band.
pub fn facts_from(p: &Procs, holders: Option<&[(u32, String)]>, ports: Option<Ports>) -> Facts {
    let reaper = !p.reaper.is_empty();
    let app = !p.app.is_empty();
    let (reaper_holds_module, other_module_holder) = match holders {
        Some(h) => (
            h.iter().any(|(pid, _)| p.reaper.contains(pid)),
            h.iter()
                .any(|(pid, _)| !p.reaper.contains(pid) && !p.engine.contains(pid)),
        ),
        None => (reaper, false),
    };
    let app_serves = match ports {
        Some((http, https)) => match p.app.as_slice() {
            [pid] => http == Some(*pid) && https == Some(*pid),
            _ => false,
        },
        None => app,
    };
    Facts {
        reaper,
        app,
        engine: !p.engine.is_empty(),
        server: !p.server.is_empty(),
        tray: !p.tray.is_empty(),
        runner: !p.runner.is_empty(),
        reaper_holds_module,
        app_serves,
        other_module_holder,
    }
}

/// The driver module's holders other than REAPER: they must leave before
/// REAPER starts (design §5.2 "back to event" step 4, I3).
pub fn foreign_holders(holders: &[(u32, String)], reaper: &[u32]) -> Vec<(u32, String)> {
    holders
        .iter()
        .filter(|(pid, _)| !reaper.contains(pid))
        .cloned()
        .collect()
}

/// `PrefCheck`'s restore: up to this many writes, each read back.
pub const PREF_ATTEMPTS: u32 = 3;

/// Who holds the driver module when `PrefCheck` finds something other than
/// REAPER's original (#9 2026-09-28).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardHolders {
    /// REAPER is one of them.
    pub reaper: bool,
    /// Every holder: image and pid.
    pub names: String,
}

/// The holders `PrefCheck` must not write under; `None` while nothing holds
/// the driver module (the restore may write). Anything counts, our engine
/// too. An unreadable list assumes that a running REAPER holds it (as
/// [`facts_from`] does), so a failed read never writes under a REAPER that
/// may hold the card.
pub fn card_holders(holders: Option<&[(u32, String)]>, reaper: &[u32]) -> Option<CardHolders> {
    let assumed: Vec<(u32, String)> = match holders {
        Some(_) => Vec::new(),
        None => reaper
            .iter()
            .map(|pid| (*pid, "REAPER".to_owned()))
            .collect(),
    };
    let list = holders.unwrap_or(&assumed);
    (!list.is_empty()).then(|| CardHolders {
        reaper: list.iter().any(|(pid, _)| reaper.contains(pid)),
        names: holders_text(list),
    })
}

/// A preference that is not REAPER's original while the driver module is
/// held: `PrefCheck` wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefHeld {
    /// What the preference reads (`None`: unreadable).
    pub value: Option<String>,
    pub by: CardHolders,
}

impl PrefHeld {
    /// The alarm, the report line and the status line.
    pub fn text(&self) -> String {
        let at = self
            .value
            .as_ref()
            .map_or_else(|| "unreadable".to_owned(), |v| format!("at {v}"));
        if self.by.reaper {
            format!(
                "REAPER runs with the preferred buffer {at}; it is restored at REAPER's next start"
            )
        } else {
            format!(
                "the driver module is held by {} with the preferred buffer {at}; nothing was \
                 written",
                self.by.names
            )
        }
    }
}

/// What `PrefCheck` found (design §5.2; #9 2026-09-28).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefSeen {
    /// REAPER's original is there, after this many writes (each read back;
    /// 0: it already was).
    Original(u32),
    /// Not the original while the driver module is held: nothing written.
    Held(PrefHeld),
}

impl From<Checked<CardHolders>> for PrefSeen {
    fn from(c: Checked<CardHolders>) -> Self {
        match c {
            Checked::Original(writes) => Self::Original(writes),
            Checked::Open { found, by } => Self::Held(PrefHeld {
                value: found.map(|p| p.raw),
                by,
            }),
        }
    }
}

/// Whether an engine runs that is not the guard's own child (`ours`).
pub fn foreign_engine(running: &[u32], ours: Option<u32>) -> bool {
    running.iter().any(|pid| Some(*pid) != ours)
}

/// Whether a running process is the child a previous guard started: the
/// same image path (without ASCII case) and the same start time, so a
/// recycled pid never passes for it (design §5.1, adoption).
pub fn adoptable(saved: &Child, image_path: &str, start_time: u64) -> bool {
    saved.image.eq_ignore_ascii_case(image_path) && saved.start_time == start_time
}

/// What the precheck of a dev/live entry reads (design §5.2 step 1). The
/// bundle's record and HIL result are the daemon's (`bundle::may_go_live`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecheckFacts {
    pub to: Mode,
    pub trial: bool,
    /// The current bundle's engine is installed.
    pub bundle: bool,
    /// `[guard] pc_tests_passed` (design §10).
    pub pc_tests_passed: bool,
    /// The PWA notification subscriptions an alarm goes to (`iem-server
    /// notify --count alarm`); `None`: unreadable.
    pub subscriptions: Option<u32>,
    /// An engine the guard did not start runs (ours is stopped by the plan).
    pub foreign_engine: bool,
    /// `handover::app_binary` of the predecessor's exe.
    pub app_binary: Result<(), String>,
}

/// Why the guard's alarms would reach no phone (design §5.4); `None`: at
/// least one PWA notification subscription.
fn no_subscription(subscriptions: Option<u32>) -> Option<&'static str> {
    match subscriptions {
        None => Some("the PWA notification subscriptions cannot be read"),
        Some(0) => {
            Some("no PWA notification subscription: no engineer device allowed notifications")
        }
        Some(_) => None,
    }
}

/// How a dev entry's note goes on after [`no_subscription`].
const DEV_WITHOUT_SUBSCRIPTION: &str =
    " (not needed for dev: the alarms stay in the guard's alarm file)";

/// The precheck's verdict: `Err` refuses the entry with every problem;
/// `Ok(Some(note))` lets it go on and names what it found. A PWA
/// notification subscription (the engineer's, where the alarms go, #9
/// 2026-09-28) is required for live and live trials only: the predecessor's
/// arrive with the band import, a later step of the entry, and a new one
/// only through iem-server, which runs only in dev and live (in event the
/// predecessor holds the band's address). Dev without one names it, and
/// the alarms stay in the guard's alarm file.
pub fn precheck(f: &PrecheckFacts) -> R<Option<String>> {
    let mut bad = Vec::new();
    let mut note = None;
    if f.to == Mode::Event {
        bad.push("the precheck is for dev and live".to_owned());
    }
    if !f.bundle {
        bad.push("no installed bundle is active".to_owned());
    }
    if f.trial && f.to != Mode::Live {
        bad.push("a trial is a live switch".to_owned());
    }
    if f.to == Mode::Live && f.trial && !f.pc_tests_passed {
        bad.push(
            "[guard] pc_tests_passed is false: no trial before the owner-approved PC tests"
                .to_owned(),
        );
    }
    if let Some(why) = no_subscription(f.subscriptions) {
        if f.to == Mode::Live || f.trial {
            bad.push(why.to_owned());
        } else {
            note = Some(format!("{why}{DEV_WITHOUT_SUBSCRIPTION}"));
        }
    }
    if f.foreign_engine {
        bad.push("an engine the guard did not start runs".to_owned());
    }
    if let Err(e) = &f.app_binary {
        bad.push(e.clone());
    }
    if bad.is_empty() {
        Ok(note)
    } else {
        Err(StepError::Failed(bad.join("; ")))
    }
}

/// `iemmode status` names a guard whose children stay in its task's job
/// (#9 2026-09-28: the PC's task job allows no breakaway). A restart of the
/// guard still leaves them running: the job lives on while any of them
/// runs.
pub const JOB_NOTE: &str = "children stay in the guard task's job (no breakaway)";

/// The status note of the guard's job as [`Pc::job`] read it: only
/// [`Placement::InJob`] names one. A job that ends its processes when it
/// closes refuses each start instead (that step's error, then the error
/// policy); a job that cannot be read is logged, and every start reads it
/// again.
pub fn job_note(job: &Result<Placement, String>) -> Option<&'static str> {
    matches!(job, Ok(Placement::InJob)).then_some(JOB_NOTE)
}

/// The engine as its supervisor pipe reports it (`Hello` and `Status`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// `Hello.engine_build`: `<version>+<commit>`.
    pub build: String,
    /// The measured period in frames (design §3).
    pub frames: u32,
    pub callbacks: u64,
    /// Missed periods since the start.
    pub missed: u64,
    /// Driver reopens since the start (a reset request, a stall, a forced
    /// reopen).
    pub resets: u64,
    pub faulted: bool,
    /// A stream is parked (a callback still in flight after a stop, R6).
    pub parked: bool,
    /// HIL's spare outputs and their peaks since the previous `Status`.
    pub hil: Vec<HilOut>,
    /// The D5(b) loopback round-trip in samples, once measured (S6 test 5); 0
    /// while none.
    pub loopback_samples: u64,
}

/// The running engine as the guard's supervisor connection saw it last
/// (the guard's `Reply.engine`, design §7): the hello's build in
/// `status.build`, the newest status, and whether the engine's control
/// pipe's DACL reads back private (the user and SYSTEM only).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineSeen {
    pub status: Status,
    pub pipe_private: bool,
}

/// The PC as the guard sees and changes it. No method ends a process.
pub trait Pc {
    /// Once a second: the process list only (P10) — the pids of REAPER, the
    /// app, engine, server, tray and runner — and our children that ended.
    fn procs(&mut self) -> Procs;
    /// Once per plan: processes, driver-module holders, port owners (design
    /// §5.1).
    fn facts(&mut self) -> Facts;
    /// Re-adopts the children a previous guard started (pid, image path and
    /// start time must match); returns the adopted ones.
    fn adopt(&mut self, saved: &Children) -> Children;
    /// The children the guard started or adopted and that still run (the
    /// daemon persists them in `GuardState::pids`).
    fn children(&mut self) -> Children;
    /// The bundle every start runs from (the current pin); `None`: none, so
    /// every start and the precheck refuse.
    fn set_bundle(&mut self, sha: Option<&str>);
    /// Bundle installed, `pc_tests_passed` (trial), ≥ 1 PWA notification
    /// subscription (live and trials; dev only names a missing one), no
    /// foreign engine, the app's exe hash. `Some`: what it names without
    /// refusing.
    fn precheck(&mut self, to: Mode, trial: bool) -> R<Option<String>>;
    /// 40026; project mtime changed ≤ 15 s; no dialog but REAPER's
    /// evaluation notice (`handover::dialogs`); 40004; gone ≤ 30 s; driver
    /// module unheld.
    fn reaper_save_quit(&mut self, c: &Cancel) -> R<()>;
    /// A handle first, then the tray's Exit command, then observe (design
    /// §5.3); the verdict is `handover::app_exit`.
    fn app_stop(&mut self, c: &Cancel) -> R<AppExit>;
    /// The elevated tuning task with one verb; "absent" when the bundle has
    /// no tuning module.
    fn tuning(&mut self, verb: &str, c: &Cancel) -> R<String>;
    /// Native reads (power plan, service start types) against the tuning
    /// module's record; `Some` describes a drift. On mode changes and hourly.
    fn tuning_drift(&mut self) -> R<Option<String>>;
    /// `prefwin::check(.., PREF_ATTEMPTS, ..)` with [`card_holders`]:
    /// REAPER's original, or restored (the writes it took, each read back)
    /// while nothing holds the driver module; never a write while something
    /// does ([`PrefSeen::Held`]).
    fn pref_check(&mut self) -> R<PrefSeen>;
    /// The band's data refresh of an entry (`iem-migrate band`, …): a
    /// started command finishes (a mutation); "ide event" stops the refresh
    /// between two commands and after the last.
    fn data(&mut self, mode: Mode, c: &Cancel) -> R<String>;
    /// `hold`: silent until `Arm`. `hil`: with the engine's test-signal and
    /// fault-injection flags, only in dev while a HIL job runs (design §7,
    /// never in live).
    fn engine_start(&mut self, hold: bool, hil: bool) -> R<u32>;
    /// Hello names the bundle; measured frames 32, callbacks advancing, no
    /// missed period for `secs` (one warm-up miss restarts the window once).
    fn engine_ready(&mut self, secs: u32, c: &Cancel) -> R<Status>;
    /// Whether the engine this guard started (or adopted) has ended, and
    /// its exit code (`Some(None)`: none): `None` while it runs or none is
    /// watched. A look only: the crash watch still sees the exit unless a
    /// new start replaces the engine (#32 F3-r4 4: the plan's ready wait
    /// starts one that ended with exit 75 again).
    fn engine_exit(&mut self) -> Option<Option<i32>>;
    fn engine_arm(&mut self) -> R<()>;
    /// `Shutdown`, `DriverReleased` ≤ 10 s, gone ≤ 5 s; a refused
    /// `Shutdown` fails at once.
    fn engine_stop(&mut self, c: &Cancel) -> R<()>;
    /// Two statuses about 1 s apart: callbacks advancing, not faulted, not
    /// parked.
    fn engine_health(&mut self) -> R<Health>;
    /// With a config that freezes PIN changes before cutover.
    fn server_start(&mut self, mode: Mode) -> R<u32>;
    /// Ctrl-Break on its own console, gone ≤ 10 s, ports free.
    fn server_stop(&mut self, c: &Cancel) -> R<()>;
    fn tray_start(&mut self) -> R<()>;
    /// `Quit` over the guard pipe, gone ≤ 10 s.
    fn tray_stop(&mut self, c: &Cancel) -> R<()>;
    /// LAN 80 and the public host answer `/api/version` with `sha`, LAN 443
    /// too while serving the server's own certificate (`tls::check`:
    /// identity, not validity); the tunnel has a ready connection. `Some`:
    /// what it names about the LAN certificate without failing (outside its
    /// validity; #9 2026-09-28).
    fn identity(&mut self, sha: &str, c: &Cancel) -> R<Option<String>>;
    fn runner_start(&mut self) -> R<()>;
    /// Ctrl-Break on its own console (the daemon stops only an idle runner).
    fn runner_stop(&mut self, c: &Cancel) -> R<()>;
    /// ≤ 30 s for every holder of the driver module but REAPER to leave.
    fn holder_gone(&mut self, c: &Cancel) -> R<()>;
    /// Our task (or the direct start, `[guard] start_direct`); refuses with
    /// an engine or a driver-module holder.
    fn reaper_start(&mut self) -> R<()>;
    /// ≤ 120 s for the track count; the meter bridge at most once; the
    /// titles of REAPER's visible dialogs.
    fn reaper_facts(&mut self, c: &Cancel) -> R<ReaperFacts>;
    fn app_start(&mut self) -> R<()>;
    /// `/api/version`, the member count and the public host.
    fn app_answers(&mut self, c: &Cancel) -> R<()>;
    /// S1c's REAPER-mode fingerprint through the tuning task (`state`).
    fn fingerprint(&mut self) -> R<()>;
    /// `\iemmixer\iemmixer-probe` started by the guard (design §5.1).
    fn probe_task(&mut self) -> R<()>;
    fn notify(&mut self, audience: Audience, title: &str, body: &str) -> R<()>;
    /// The HIL test signal (design §4, §7): `HilTestSignal` over the
    /// supervisor pipe, encoded only on `card_tx` (`[guard] hil_tx`).
    fn engine_hil_signal(&mut self, input: &str, dbfs: f64, ttl_s: f64, card_tx: &[u16]) -> R<()>;
    /// A forced reopen of the driver (HIL, design §7); the engine's reset
    /// budget applies.
    fn engine_force_reopen(&mut self) -> R<()>;
    /// `InjectFault` over the supervisor pipe (HIL, design §7): the engine,
    /// started with its fault-injection flag, faults its RT callback and
    /// exits 70; the engine refuses it without the flag.
    fn engine_inject_fault(&mut self) -> R<()>;
    /// `InjectSeh` over the supervisor pipe (the owner-approved SEH test,
    /// design §10): the engine raises a structured exception on its RT
    /// callback; the SEH filter releases the driver or parks, and the watch
    /// starts it again. Under the fault-injection flag, like the fault.
    fn engine_inject_seh(&mut self) -> R<()>;
    /// `InjectPark` over the supervisor pipe (the parked-engine test, design
    /// §10 test #2, #35): the SEH test's exception under the backend's test
    /// hold, so the driver is kept, the SEH filter parks the RT thread and
    /// the engine keeps running with its stream parked (`Status.parked`).
    /// Under the fault-injection flag, like the SEH test.
    fn engine_inject_park(&mut self) -> R<()>;
    /// What the supervisor connection holds of the running engine; `None`
    /// while no engine of ours runs. Never waits: at most one attempt to
    /// connect, and the pipe's DACL is read once per engine process.
    fn engine_seen(&mut self) -> Option<EngineSeen>;
    /// F30 (design §7): `iem-engine check-site` of the new site file and
    /// the guard's own tables, then it replaces the site; returns the old
    /// and the new check report. The checks are waits: "ide event" ends
    /// them through `c`.
    fn install_site(&mut self, path: &str, c: &Cancel) -> R<String>;
    /// `\iemmixer\iemmixer-exclude`: Defender process exclusions for the
    /// verified bundle `sha` (design §5.1); `keep`'s (the other pin) stay,
    /// every other bundle's go.
    fn exclude(&mut self, sha: &str, keep: &[String]) -> R<()>;
    /// Where this process's long-lived children start (design §5.1, I9):
    /// `iem_win::spawn::placement` of the job it runs in, read once at the
    /// guard's start for its log and status ([`job_note`]); every start
    /// reads the job again. Never waits.
    fn job(&mut self) -> Result<Placement, String>;
    /// The elevated logon task's last result (`<elevated root>\tasks\out\
    /// logon.result.json`, G1): what it found of the preference
    /// (`effects::tuning::logon_result`); `None`: no result or an unreadable
    /// one. Never waits.
    fn logon(&mut self) -> Option<Logon>;
    /// The listening pids of ports 80 and 443: the rehearsal's check that
    /// the predecessor could bind them after the teardown. Never waits.
    fn web_ports(&mut self) -> R<Ports>;
}

/// A scripted PC for the daemon's tests.
#[cfg(test)]
pub mod fake {
    use std::collections::HashMap;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

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
            )
        }
    }

    /// How long a blocked call waits for its token before it fails (a test
    /// that never pre-empts must not hang).
    pub const BLOCK_LIMIT: Duration = Duration::from_secs(10);

    /// One HIL test signal as sent: input, dBFS, TTL, card outputs.
    pub type SentSignal = (String, f64, f64, Vec<u16>);

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
        /// Every HIL test signal sent: (input, dBFS, TTL, card outputs).
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
                },
                engine_up: true,
                subscriptions: Some(1),
                job: Ok(Placement::NoJob),
                logon: None,
                lan_note: None,
                ports: Ok((None, None)),
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
        }

        fn pid(&mut self) -> u32 {
            self.next_pid += 1;
            self.next_pid
        }

        /// Records `call`, then plays its script: blocked, delayed, failed.
        fn enter(&mut self, call: Call, c: Option<&Cancel>) -> R<()> {
            self.record(call);
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
            // The real verdict over `subscriptions`; every other fact passes.
            precheck(&PrecheckFacts {
                to,
                trial,
                bundle: true,
                pc_tests_passed: true,
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

        fn server_start(&mut self, _mode: Mode) -> R<u32> {
            self.enter(Call::ServerStart, None)?;
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
            self.facts.reaper = true;
            self.facts.reaper_holds_module = true;
            Ok(())
        }

        fn reaper_facts(&mut self, c: &Cancel) -> R<ReaperFacts> {
            self.enter(Call::ReaperFacts, Some(c))?;
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
        ) -> R<()> {
            self.enter(Call::HilSignal, None)?;
            self.hil_signals
                .push((input.to_owned(), dbfs, ttl_s, card_tx.to_vec()));
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
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::fake::{Call, FakePc};
    use super::*;

    fn images() -> Images {
        Images {
            reaper: "reaper.exe".into(),
            app: "app.exe".into(),
            engine: "iem-engine.exe".into(),
            server: "iem-server.exe".into(),
            tray: "iem-tray.exe".into(),
            runner: "Runner.Listener.exe".into(),
        }
    }

    /// No holder of the driver module.
    const NO_HOLDER: &[(u32, String)] = &[];

    fn list() -> Vec<(u32, String)> {
        vec![
            (4, "System".into()),
            (11, "REAPER.EXE".into()),
            (12, "app.exe".into()),
            (13, "iem-engine.exe".into()),
            (14, "iem-server.exe".into()),
            (15, "iem-tray.exe".into()),
            (16, "runner.listener.exe".into()),
            (17, "app.exe".into()),
            (18, "notepad.exe".into()),
        ]
    }

    #[test]
    fn step_errors_read_as_sentences_and_come_from_a_preemption() {
        assert_eq!(StepError::from(Preempted), StepError::Preempted);
        assert_eq!(StepError::Preempted.to_string(), "pre-empted by event");
        assert_eq!(
            StepError::failed("no DriverReleased within 10 s").to_string(),
            "no DriverReleased within 10 s"
        );
        assert_eq!(
            StepError::failed(String::from("x")),
            StepError::Failed("x".into())
        );
        let c = Cancel::default();
        c.preempt();
        let r: R<()> = c.sleep(Duration::from_secs(1)).map_err(StepError::from);
        assert_eq!(r, Err(StepError::Preempted));
    }

    #[test]
    fn audiences_and_kids_have_their_names() {
        assert_eq!(Audience::Alarm.arg(), "alarm");
        let ids: Vec<&str> = Kid::ALL.iter().map(|k| k.id()).collect();
        assert_eq!(ids, ["engine", "server", "tray", "runner"]);
    }

    #[test]
    fn children_are_found_by_kid() {
        let child = |pid: u32| Child {
            pid,
            start_time: u64::from(pid) * 10,
            image: format!("C:\\IEM\\{pid}.exe"),
        };
        let kids = Children {
            engine: Some(child(1)),
            server: Some(child(2)),
            tray: Some(child(3)),
            runner: Some(child(4)),
        };
        let pids: Vec<u32> = Kid::ALL
            .iter()
            .map(|k| kids.of(*k).map_or(0, |c| c.pid))
            .collect();
        assert_eq!(pids, [1, 2, 3, 4]);
        assert_eq!(Children::default().of(Kid::Tray), None);
    }

    #[test]
    fn the_process_list_is_read_by_image_without_case() {
        let p = Procs::from_list(&list(), &images());
        assert_eq!(p.reaper, [11]);
        assert_eq!(p.app, [12, 17]);
        assert_eq!(p.engine, [13]);
        assert_eq!(p.server, [14]);
        assert_eq!(p.tray, [15]);
        assert_eq!(p.runner, [16]);
        assert!(p.exited.is_empty());
        assert_eq!(p.of(Kid::Engine), [13]);
        assert_eq!(p.of(Kid::Server), [14]);
        assert_eq!(p.of(Kid::Tray), [15]);
        assert_eq!(p.of(Kid::Runner), [16]);
        assert_eq!(Procs::from_list(&[], &images()), Procs::default());
    }

    #[test]
    fn the_band_is_up_with_reaper_or_the_app() {
        let with = |reaper: Vec<u32>, app: Vec<u32>| Procs {
            reaper,
            app,
            ..Procs::default()
        };
        assert!(with(vec![1], vec![]).band_up());
        assert!(with(vec![], vec![2]).band_up());
        assert!(with(vec![1], vec![2]).band_up());
        assert!(!with(vec![], vec![]).band_up());
        let engine_only = Procs {
            engine: vec![3],
            ..Procs::default()
        };
        assert!(!engine_only.band_up());
    }

    fn band() -> Procs {
        Procs {
            reaper: vec![11],
            app: vec![12],
            engine: vec![13],
            server: vec![14],
            tray: vec![15],
            runner: vec![16],
            exited: Vec::new(),
        }
    }

    #[test]
    fn facts_name_what_runs() {
        let f = facts_from(&band(), Some(NO_HOLDER), Some((None, None)));
        assert!(f.reaper && f.app && f.engine && f.server && f.tray && f.runner);
        let nothing = facts_from(&Procs::default(), Some(NO_HOLDER), Some((None, None)));
        assert_eq!(nothing, Facts::default());
        for (p, want) in [
            (
                Procs {
                    server: vec![1],
                    ..Procs::default()
                },
                Facts {
                    server: true,
                    ..Facts::default()
                },
            ),
            (
                Procs {
                    tray: vec![1],
                    ..Procs::default()
                },
                Facts {
                    tray: true,
                    ..Facts::default()
                },
            ),
            (
                Procs {
                    runner: vec![1],
                    ..Procs::default()
                },
                Facts {
                    runner: true,
                    ..Facts::default()
                },
            ),
            (
                Procs {
                    engine: vec![1],
                    ..Procs::default()
                },
                Facts {
                    engine: true,
                    ..Facts::default()
                },
            ),
        ] {
            assert_eq!(facts_from(&p, Some(NO_HOLDER), Some((None, None))), want);
        }
    }

    #[test]
    fn module_holders_split_into_reaper_and_foreign() {
        let holders = |pids: &[u32]| -> Vec<(u32, String)> {
            pids.iter().map(|p| (*p, format!("{p}.exe"))).collect()
        };
        let facts = |pids: &[u32]| {
            let h = holders(pids);
            facts_from(&band(), Some(h.as_slice()), Some((None, None)))
        };
        let f = facts(&[11]);
        assert!(f.reaper_holds_module && !f.other_module_holder);
        // Our engine is neither REAPER nor foreign.
        let f = facts(&[13]);
        assert!(!f.reaper_holds_module && !f.other_module_holder);
        let f = facts(&[99]);
        assert!(!f.reaper_holds_module && f.other_module_holder);
        let f = facts(&[11, 99]);
        assert!(f.reaper_holds_module && f.other_module_holder);
        let f = facts(&[]);
        assert!(!f.reaper_holds_module && !f.other_module_holder);
    }

    #[test]
    fn unreadable_holders_never_restart_a_running_reaper() {
        let f = facts_from(&band(), None, Some((None, None)));
        assert!(f.reaper_holds_module && !f.other_module_holder);
        let without = facts_from(&Procs::default(), None, Some((None, None)));
        assert!(!without.reaper_holds_module && !without.other_module_holder);
    }

    #[test]
    fn the_app_serves_when_it_owns_both_ports() {
        let app = |pids: Vec<u32>| Procs {
            app: pids,
            ..Procs::default()
        };
        let serves =
            |p: &Procs, ports: Option<Ports>| facts_from(p, Some(NO_HOLDER), ports).app_serves;
        assert!(serves(&app(vec![12]), Some((Some(12), Some(12)))));
        assert!(!serves(&app(vec![12]), Some((Some(12), None))));
        assert!(!serves(&app(vec![12]), Some((None, Some(12)))));
        assert!(!serves(&app(vec![12]), Some((Some(14), Some(12)))));
        assert!(!serves(&app(vec![12]), Some((Some(12), Some(14)))));
        assert!(!serves(&app(vec![12]), Some((None, None))));
        // Two instances: neither is known to serve.
        assert!(!serves(&app(vec![12, 17]), Some((Some(12), Some(12)))));
        assert!(!serves(&app(vec![]), Some((Some(12), Some(12)))));
        // Unreadable ports: a running app is assumed to serve.
        assert!(serves(&app(vec![12]), None));
        assert!(!serves(&app(vec![]), None));
    }

    #[test]
    fn foreign_holders_are_all_but_reaper() {
        let h = vec![
            (11, "reaper.exe".to_owned()),
            (99, "spike.exe".to_owned()),
            (13, "iem-engine.exe".to_owned()),
        ];
        assert_eq!(
            foreign_holders(&h, &[11]),
            [
                (99, "spike.exe".to_owned()),
                (13, "iem-engine.exe".to_owned())
            ]
        );
        assert_eq!(
            foreign_holders(&h, &[11, 99, 13]),
            Vec::<(u32, String)>::new()
        );
        assert_eq!(foreign_holders(&h, &[]), h);
    }

    #[test]
    fn an_engine_is_foreign_unless_it_is_our_child() {
        assert!(!foreign_engine(&[], None));
        assert!(!foreign_engine(&[], Some(13)));
        assert!(!foreign_engine(&[13], Some(13)));
        assert!(foreign_engine(&[13], None));
        assert!(foreign_engine(&[14], Some(13)));
        assert!(foreign_engine(&[13, 14], Some(13)));
    }

    #[test]
    fn adoption_needs_the_same_image_and_start_time() {
        let saved = Child {
            pid: 13,
            start_time: 133_000_000_000,
            image: "C:\\IEM\\bundles\\a\\iem-engine.exe".into(),
        };
        assert!(adoptable(
            &saved,
            "C:\\IEM\\bundles\\a\\iem-engine.exe",
            133_000_000_000
        ));
        assert!(adoptable(
            &saved,
            "c:\\iem\\BUNDLES\\a\\IEM-ENGINE.EXE",
            133_000_000_000
        ));
        assert!(!adoptable(
            &saved,
            "C:\\IEM\\bundles\\a\\iem-engine.exe",
            133_000_000_001
        ));
        assert!(!adoptable(
            &saved,
            "C:\\IEM\\bundles\\a\\iem-engine.exe",
            132_999_999_999
        ));
        assert!(!adoptable(
            &saved,
            "C:\\Other\\iem-engine.exe",
            133_000_000_000
        ));
    }

    fn ready() -> PrecheckFacts {
        PrecheckFacts {
            to: Mode::Dev,
            trial: false,
            bundle: true,
            pc_tests_passed: false,
            subscriptions: Some(1),
            foreign_engine: false,
            app_binary: Ok(()),
        }
    }

    #[test]
    fn a_complete_precheck_passes() {
        assert_eq!(precheck(&ready()), Ok(None));
        // A live entry that is not a trial needs no PC tests.
        let live = PrecheckFacts {
            to: Mode::Live,
            ..ready()
        };
        assert_eq!(precheck(&live), Ok(None));
        let trial = PrecheckFacts {
            to: Mode::Live,
            trial: true,
            pc_tests_passed: true,
            ..ready()
        };
        assert_eq!(precheck(&trial), Ok(None));
        let many = PrecheckFacts {
            subscriptions: Some(3),
            ..ready()
        };
        assert_eq!(precheck(&many), Ok(None));
    }

    /// The precheck's texts for the PWA notification subscriptions.
    const NO_SUBSCRIPTION: &str =
        "no PWA notification subscription: no engineer device allowed notifications";
    const SUBSCRIPTIONS_UNREADABLE: &str = "the PWA notification subscriptions cannot be read";

    /// The alarms go to the engineer's PWA notification subscriptions (#9
    /// 2026-09-28). The predecessor's arrive with the band import, a later
    /// step of the entry, and a new one only through iem-server, which runs
    /// only in dev and live: one is required for live and live trials; dev
    /// goes on and names what is missing, the alarms stay in the guard's
    /// alarm file.
    #[test]
    fn a_pwa_subscription_is_required_for_live_and_named_for_dev() {
        let dev = |subscriptions| PrecheckFacts {
            subscriptions,
            ..ready()
        };
        assert_eq!(
            precheck(&dev(Some(0))),
            Ok(Some(format!(
                "{NO_SUBSCRIPTION} (not needed for dev: the alarms stay in the guard's alarm file)"
            )))
        );
        assert_eq!(
            precheck(&dev(None)),
            Ok(Some(format!(
                "{SUBSCRIPTIONS_UNREADABLE} \
                 (not needed for dev: the alarms stay in the guard's alarm file)"
            )))
        );
        assert_eq!(precheck(&dev(Some(1))), Ok(None));
        // Another refusal of a dev entry takes precedence over the note.
        let unbundled = PrecheckFacts {
            bundle: false,
            ..dev(Some(0))
        };
        assert_eq!(
            precheck(&unbundled),
            Err(StepError::Failed("no installed bundle is active".into()))
        );
        // Live and live trials refuse as before.
        let live = |subscriptions| PrecheckFacts {
            to: Mode::Live,
            subscriptions,
            ..ready()
        };
        let trial = |subscriptions| PrecheckFacts {
            trial: true,
            pc_tests_passed: true,
            ..live(subscriptions)
        };
        for f in [live(Some(0)), trial(Some(0))] {
            assert_eq!(
                precheck(&f),
                Err(StepError::Failed(NO_SUBSCRIPTION.into())),
                "{f:?}"
            );
        }
        for f in [live(None), trial(None)] {
            assert_eq!(
                precheck(&f),
                Err(StepError::Failed(SUBSCRIPTIONS_UNREADABLE.into())),
                "{f:?}"
            );
        }
    }

    #[test]
    fn every_precheck_problem_is_named() {
        let cases = [
            (
                PrecheckFacts {
                    to: Mode::Event,
                    ..ready()
                },
                "the precheck is for dev and live",
            ),
            (
                PrecheckFacts {
                    bundle: false,
                    ..ready()
                },
                "no installed bundle is active",
            ),
            (
                PrecheckFacts {
                    trial: true,
                    pc_tests_passed: true,
                    ..ready()
                },
                "a trial is a live switch",
            ),
            (
                PrecheckFacts {
                    to: Mode::Live,
                    trial: true,
                    ..ready()
                },
                "[guard] pc_tests_passed is false: no trial before the owner-approved PC tests",
            ),
            (
                PrecheckFacts {
                    to: Mode::Live,
                    subscriptions: None,
                    ..ready()
                },
                SUBSCRIPTIONS_UNREADABLE,
            ),
            (
                PrecheckFacts {
                    to: Mode::Live,
                    subscriptions: Some(0),
                    ..ready()
                },
                NO_SUBSCRIPTION,
            ),
            (
                PrecheckFacts {
                    foreign_engine: true,
                    ..ready()
                },
                "an engine the guard did not start runs",
            ),
            (
                PrecheckFacts {
                    app_binary: Err("predecessor exe changed".into()),
                    ..ready()
                },
                "predecessor exe changed",
            ),
        ];
        for (f, want) in cases {
            assert_eq!(precheck(&f), Err(StepError::Failed(want.into())), "{want}");
        }
        let all = PrecheckFacts {
            to: Mode::Dev,
            trial: true,
            bundle: false,
            pc_tests_passed: false,
            subscriptions: Some(0),
            foreign_engine: true,
            app_binary: Err("changed".into()),
        };
        assert_eq!(
            precheck(&all),
            Err(StepError::Failed(format!(
                "no installed bundle is active; a trial is a live switch; {NO_SUBSCRIPTION}; \
                 an engine the guard did not start runs; changed"
            )))
        );
    }

    fn up() -> Facts {
        Facts {
            reaper: true,
            app: true,
            reaper_holds_module: true,
            app_serves: true,
            ..Facts::default()
        }
    }

    #[test]
    fn the_fake_reports_its_facts_and_processes() {
        let mut pc = FakePc::new(up());
        assert_eq!(pc.facts(), up());
        pc.exited.push((Kid::Engine, Some(70)));
        let p = pc.procs();
        assert_eq!((p.reaper, p.app), (vec![1], vec![1]));
        assert!(p.engine.is_empty() && p.server.is_empty());
        assert!(p.tray.is_empty() && p.runner.is_empty());
        assert_eq!(p.exited, [(Kid::Engine, Some(70))]);
        assert!(pc.procs().exited.is_empty());
        assert_eq!(pc.calls(), [Call::Facts, Call::Procs, Call::Procs]);
        assert_eq!(pc.count(Call::Procs), 2);
        assert!(pc.mutating_calls().is_empty());
    }

    #[test]
    fn the_fake_starts_and_stops_like_the_pc() {
        let c = Cancel::default();
        let mut pc = FakePc::new(up());
        pc.app_stop(&c).unwrap();
        pc.reaper_save_quit(&c).unwrap();
        assert_eq!(pc.facts, Facts::default());
        assert!(pc.tuning("enter", &c).unwrap().starts_with("enter"));
        assert!(pc.data(Mode::Dev, &c).unwrap().starts_with("Dev"));
        let engine = pc.engine_start(true, false).unwrap();
        pc.engine_ready(10, &c).unwrap();
        pc.engine_arm().unwrap();
        let server = pc.server_start(Mode::Dev).unwrap();
        assert!(server > engine);
        pc.tray_start().unwrap();
        assert_eq!(pc.identity("a", &c), Ok(None));
        pc.runner_start().unwrap();
        let f = pc.facts;
        assert!(f.engine && f.server && f.tray && f.runner && !f.reaper && !f.app);

        pc.runner_stop(&c).unwrap();
        pc.engine_stop(&c).unwrap();
        pc.server_stop(&c).unwrap();
        pc.tray_stop(&c).unwrap();
        pc.facts.other_module_holder = true;
        pc.holder_gone(&c).unwrap();
        pc.reaper_start().unwrap();
        pc.reaper_facts(&c).unwrap();
        pc.app_start().unwrap();
        pc.app_answers(&c).unwrap();
        pc.fingerprint().unwrap();
        assert_eq!(pc.facts, up());
        assert_eq!(pc.index(Call::AppStop), 0);
        assert_eq!(pc.index(Call::ReaperSaveQuit), 1);
        assert!(pc.index(Call::ReaperStart) > pc.index(Call::EngineStop));
        assert_eq!(
            pc.calls_after(Call::AppStart),
            [Call::AppAnswers, Call::Fingerprint]
        );
        assert_eq!(pc.calls_after(Call::Fingerprint), Vec::<Call>::new());
        assert!(pc.mutating_calls().contains(&Call::EngineStart));
        assert!(!pc.mutating_calls().contains(&Call::EngineReady));
        assert!(!pc.mutating_calls().contains(&Call::Identity));
    }

    #[test]
    fn the_fake_answers_the_reads() {
        let mut pc = FakePc::new(Facts::default());
        assert_eq!(pc.pref_check().unwrap(), PrefSeen::Original(0));
        assert_eq!(pc.tuning_drift().unwrap(), None);
        assert_eq!(pc.engine_health().unwrap(), Health::Dead);
        pc.health(Health::Healthy);
        assert_eq!(pc.engine_health().unwrap(), Health::Healthy);
        pc.precheck(Mode::Dev, false).unwrap();
        pc.probe_task().unwrap();
        pc.notify(Audience::Alarm, "t", "b").unwrap();
        assert_eq!(
            pc.notices,
            [(Audience::Alarm, "t".to_owned(), "b".to_owned())]
        );
        pc.set_bundle(Some("abc"));
        assert_eq!(pc.bundle.as_deref(), Some("abc"));
        let saved = Children {
            engine: Some(Child {
                pid: 5,
                start_time: 6,
                image: "e.exe".into(),
            }),
            ..Children::default()
        };
        assert_eq!(pc.adopt(&saved), saved);
        assert_eq!(pc.children(), saved);
        assert!(pc.called(Call::Notify));
        assert!(!pc.called(Call::AppStop));
        pc.engine_ready(10, &Cancel::default()).unwrap();
        assert_eq!(pc.ready_secs, [10]);
        pc.engine_hil_signal("mic1", -30.0, 5.0, &[94]).unwrap();
        assert_eq!(pc.hil_signals, [("mic1".to_owned(), -30.0, 5.0, vec![94])]);
        pc.engine_force_reopen().unwrap();
        pc.engine_inject_fault().unwrap();
        pc.engine_inject_seh().unwrap();
        pc.engine_inject_park().unwrap();
        assert_eq!(
            pc.install_site("site.toml", &Cancel::default()).unwrap(),
            "site.toml: checked and installed"
        );
        assert_eq!(pc.sites, ["site.toml"]);
        pc.exclude("a", &["b".to_owned()]).unwrap();
        assert_eq!(pc.excluded, [("a".to_owned(), vec!["b".to_owned()])]);
        for c in [
            Call::HilSignal,
            Call::ForceReopen,
            Call::InjectFault,
            Call::InjectSeh,
            Call::InjectPark,
            Call::InstallSite,
            Call::Exclude,
        ] {
            assert!(pc.called(c) && c.mutates(), "{c:?}");
        }
        // The engine is seen only while one runs; the look is no call.
        let calls = pc.calls().len();
        assert_eq!(pc.engine_seen(), None);
        pc.facts.engine = true;
        assert_eq!(pc.engine_seen(), Some(pc.seen.clone()));
        assert!(pc.seen.pipe_private);
        assert_eq!(pc.seen.status.frames, 32);
        // An engine coming up (no hello and Status yet) is not seen.
        pc.engine_up = false;
        assert_eq!(pc.engine_seen(), None);
        pc.engine_up = true;
        assert_eq!(pc.calls().len(), calls);
        pc.fail(Call::InstallSite, "check-site exit 2");
        assert!(pc.install_site("bad.toml", &Cancel::default()).is_err());
        assert_eq!(pc.sites, ["site.toml"]);
        pc.fail(Call::HilSignal, "refused");
        assert!(pc.engine_hil_signal("mic2", -30.0, 5.0, &[94]).is_err());
        assert_eq!(pc.hil_signals.len(), 1);
    }

    fn holders(list: &[(u32, &str)]) -> Vec<(u32, String)> {
        list.iter()
            .map(|(pid, n)| (*pid, (*n).to_owned()))
            .collect()
    }

    fn by(reaper: bool, names: &str) -> CardHolders {
        CardHolders {
            reaper,
            names: names.to_owned(),
        }
    }

    /// `PrefCheck` never writes while a process holds the driver module
    /// (#9 2026-09-28): who holds it, as read; anything counts, REAPER is
    /// named. An unreadable list assumes that a running REAPER holds it (as
    /// `facts_from` does), so a failed read never writes under a REAPER that
    /// may hold the card.
    #[test]
    fn pref_check_names_whatever_holds_the_driver() {
        let reaper = [11];
        assert_eq!(card_holders(Some(NO_HOLDER), &reaper), None);
        assert_eq!(card_holders(Some(NO_HOLDER), &[]), None);
        let one = holders(&[(11, "reaper.exe")]);
        assert_eq!(
            card_holders(Some(one.as_slice()), &reaper),
            Some(by(true, "reaper.exe (11)"))
        );
        let spike = holders(&[(99, "spike.exe")]);
        assert_eq!(
            card_holders(Some(spike.as_slice()), &reaper),
            Some(by(false, "spike.exe (99)"))
        );
        let both = holders(&[(99, "spike.exe"), (11, "reaper.exe")]);
        assert_eq!(
            card_holders(Some(both.as_slice()), &reaper),
            Some(by(true, "spike.exe (99), reaper.exe (11)"))
        );
        // An engine holds it too.
        let engine = holders(&[(13, "iem-engine.exe")]);
        assert_eq!(
            card_holders(Some(engine.as_slice()), &[]),
            Some(by(false, "iem-engine.exe (13)"))
        );
        // Unreadable: a running REAPER is assumed to hold it, nobody else.
        assert_eq!(card_holders(None, &reaper), Some(by(true, "REAPER (11)")));
        assert_eq!(card_holders(None, &[]), None);
    }

    #[test]
    fn a_held_preference_reads_as_a_sentence() {
        let held = |value: Option<&str>, by: CardHolders| PrefHeld {
            value: value.map(str::to_owned),
            by,
        };
        assert_eq!(
            held(Some("32"), by(true, "reaper.exe (11)")).text(),
            "REAPER runs with the preferred buffer at 32; it is restored at REAPER's next start"
        );
        assert_eq!(
            held(None, by(true, "REAPER (11)")).text(),
            "REAPER runs with the preferred buffer unreadable; it is restored at REAPER's next \
             start"
        );
        assert_eq!(
            held(Some("128"), by(false, "spike.exe (99)")).text(),
            "the driver module is held by spike.exe (99) with the preferred buffer at 128; \
             nothing was written"
        );
        assert_eq!(
            held(None, by(false, "spike.exe (99), iem-engine.exe (13)")).text(),
            "the driver module is held by spike.exe (99), iem-engine.exe (13) with the preferred \
             buffer unreadable; nothing was written"
        );
    }

    #[test]
    fn a_check_becomes_what_pref_check_reports() {
        use iem_win::prefwin::{Checked, Kind, Pref};
        assert_eq!(PrefSeen::from(Checked::Original(0)), PrefSeen::Original(0));
        assert_eq!(PrefSeen::from(Checked::Original(2)), PrefSeen::Original(2));
        let found = Pref {
            kind: Kind::Dword,
            raw: "32".into(),
        };
        assert_eq!(
            PrefSeen::from(Checked::Open {
                found: Some(found),
                by: by(true, "reaper.exe (11)")
            }),
            PrefSeen::Held(PrefHeld {
                value: Some("32".into()),
                by: by(true, "reaper.exe (11)")
            })
        );
        assert_eq!(
            PrefSeen::from(Checked::Open {
                found: None,
                by: by(false, "spike.exe (99)")
            }),
            PrefSeen::Held(PrefHeld {
                value: None,
                by: by(false, "spike.exe (99)")
            })
        );
        assert_eq!(PREF_ATTEMPTS, 3);
    }

    /// The fake decides with `card_holders` over its facts: REAPER, a
    /// foreign holder or an engine holds the driver.
    #[test]
    fn the_fake_never_writes_the_preference_under_a_holder() {
        let mut pc = FakePc::new(Facts::default());
        assert_eq!(pc.pref_value, "32");
        pc.pref_attempts = 2;
        assert_eq!(pc.pref_check().unwrap(), PrefSeen::Original(2));
        assert_eq!(pc.pref_writes, 2);
        for (f, holder) in [
            (up(), by(true, "reaper.exe (1)")),
            (
                Facts {
                    other_module_holder: true,
                    ..Facts::default()
                },
                by(false, "spike.exe (99)"),
            ),
            (
                Facts {
                    engine: true,
                    ..Facts::default()
                },
                by(false, "iem-engine.exe (2)"),
            ),
        ] {
            let mut pc = FakePc::new(f);
            pc.pref_attempts = 1;
            assert_eq!(
                pc.pref_check().unwrap(),
                PrefSeen::Held(PrefHeld {
                    value: Some("32".into()),
                    by: holder
                }),
                "{f:?}"
            );
            assert_eq!(pc.pref_writes, 0, "{f:?}");
            // The original is there: nothing to hold back.
            pc.pref_attempts = 0;
            assert_eq!(pc.pref_check().unwrap(), PrefSeen::Original(0));
        }
    }

    #[test]
    fn a_failed_fake_call_changes_nothing() {
        let c = Cancel::default();
        let mut pc = FakePc::new(Facts {
            engine: true,
            ..Facts::default()
        });
        pc.fail(Call::EngineStop, "no DriverReleased within 10 s");
        assert_eq!(
            pc.engine_stop(&c),
            Err(StepError::Failed("no DriverReleased within 10 s".into()))
        );
        assert!(pc.facts.engine);
        assert!(pc.called(Call::EngineStop));
    }

    #[test]
    fn a_blocked_fake_call_ends_within_a_slice_of_the_preemption() {
        let mut pc = FakePc::new(up());
        pc.block_until_cancel(Call::Data);
        let c = Cancel::default();
        let other = c.clone();
        let fired = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            other.preempt();
            Instant::now()
        });
        assert_eq!(pc.data(Mode::Dev, &c), Err(StepError::Preempted));
        let back = Instant::now();
        let at = fired.join().unwrap();
        assert!(back.duration_since(at) < Duration::from_millis(600));
        assert_eq!(pc.first_after(at), None);
        let before = back.checked_sub(Duration::from_secs(5)).unwrap();
        let (first, t) = pc.first_after(before).unwrap();
        assert_eq!(first, Call::Data);
        assert!(t < at);
        // A call without a token cannot block.
        pc.block_until_cancel(Call::EngineStart);
        assert!(matches!(
            pc.engine_start(false, false),
            Err(StepError::Failed(_))
        ));
        assert!(!pc.facts.engine);
    }

    #[test]
    fn a_delayed_fake_call_finishes_even_when_preempted() {
        let mut pc = FakePc::new(Facts::default());
        pc.delay(Call::EngineStart, Duration::from_millis(200));
        let c = Cancel::default();
        c.preempt();
        let t = Instant::now();
        assert!(pc.engine_start(false, true).is_ok());
        assert!(t.elapsed() >= Duration::from_millis(200));
        assert!(pc.facts.engine);
        assert!(c.preempted());
    }

    /// The PC's task job allows no breakaway (#9 2026-09-28). Children that
    /// stay in a job that does not end its processes when it closes are
    /// named in `iemmode status`; every other reading names nothing (a
    /// refusal is each start's step error, an unreadable job is logged).
    #[test]
    fn only_children_that_stay_in_the_guards_job_are_named() {
        assert_eq!(
            JOB_NOTE,
            "children stay in the guard task's job (no breakaway)"
        );
        assert_eq!(job_note(&Ok(Placement::InJob)), Some(JOB_NOTE));
        for other in [
            Ok(Placement::Breakaway),
            Ok(Placement::NoJob),
            Ok(Placement::Refuse("the job ends its processes")),
            Err("the job could not be read".to_owned()),
        ] {
            assert_eq!(job_note(&other), None, "{other:?}");
        }
        // The fake reads no job unless a test sets one, and never records
        // the read.
        let mut pc = FakePc::new(Facts::default());
        assert_eq!(pc.job(), Ok(Placement::NoJob));
        pc.job = Ok(Placement::InJob);
        assert_eq!(pc.job(), Ok(Placement::InJob));
        assert!(pc.calls().is_empty());
    }
}
