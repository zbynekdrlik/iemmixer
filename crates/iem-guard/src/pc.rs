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
use crate::handover::{AppExit, ReaperFacts, ReaperProcs};
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
        Some(ports) => app_serves(&p.app, ports),
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

/// The predecessor app serves the band: its one process owns both ports 80
/// and 443. The plan's facts read it, and the app handover requires it
/// (#10: an iem-server that did not stop keeps the ports and answers the
/// app's HTTP checks itself).
pub fn app_serves(app: &[u32], (http, https): Ports) -> bool {
    match app {
        [pid] => http == Some(*pid) && https == Some(*pid),
        _ => false,
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
    // S7, from the engine's `Status` (design note §3); an older engine's are
    // 0 and empty.
    /// Callback intervals above 1.5 periods (information; the soak judges the
    /// histogram).
    pub late: u64,
    /// Callbacks longer than one period.
    pub overruns: u64,
    /// The longest callback since the start, in µs.
    pub process_max_us: f64,
    /// The histograms' overflow bucket: two periods in µs, rounded up.
    pub hist_top_us: u32,
    /// The callback interval and the callback's own time, sparse
    /// `(bucket µs, count)` pairs, ascending.
    pub interval_hist: Vec<(u32, u64)>,
    pub process_hist: Vec<(u32, u64)>,
    // S7 HIL v2 (#10), from the engine's `Status`; an older engine's are 0.
    /// The last driver reopen, from the old stream's stop to the new one's
    /// measured period, in µs; 0 before any.
    pub last_reopen_us: u64,
    /// The faulting callback's own time in µs; 0 while not faulted.
    pub fault_callback_us: f64,
}

/// The running engine as the guard's supervisor connection saw it last
/// (the guard's `Reply.engine`, design §7): the hello's build in
/// `status.build`, the newest status, whether the engine's control pipe's
/// DACL reads back private (the user and SYSTEM only), the process serving
/// the supervisor connection's pipe, and the last fault the guard kept.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineSeen {
    pub status: Status,
    pub pipe_private: bool,
    /// `GetNamedPipeServerProcessId` on the supervisor connection, read once
    /// per engine process like the DACL (S7 HIL v2); none while unread.
    pub pipe_server_pid: Option<u32>,
    /// The faulting callback's time of the last faulted `Status` this guard
    /// read (`effects::engine::fault_time`), kept across the respawn (S7 HIL
    /// v2); none before any.
    pub last_fault_us: Option<f64>,
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
    /// evaluation notice (`handover::dialogs`); 40004; gone ≤ 30 s, or, when
    /// Windows Error Reporting reports its crash on quit, gone within
    /// `effects::reaper::CRASH_HOLD` more (#10); driver module unheld. A
    /// REAPER that has not ended when the step fails or is pre-empted stays
    /// known as ending to [`Pc::reaper_procs`]. A REAPER already ending gets
    /// no save and no quit, only the wait; the one the guard asked to quit
    /// that has ended is a quit done (`effects::reaper::quit_step`).
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
    /// `Shutdown`, `DriverReleased` (or `DriverParked`, #35: nothing was
    /// released) ≤ 10 s, gone ≤ 5 s; a refused `Shutdown` fails at once.
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
    /// REAPER's processes for the handover's first part (#10): each one
    /// runs, or is still ending (Windows Error Reporting reports its crash,
    /// or the guard asked it to quit and it has not ended). Never waits; an
    /// unreadable process list fails (it never reads as "no REAPER").
    fn reaper_procs(&mut self) -> R<ReaperProcs>;
    /// Waits up to `effects::reaper::CRASH_HOLD` for every REAPER that is
    /// still ending to be gone ("ide event" ends the wait), and logs how
    /// long it took (#10). After it the guard's own quit request no longer
    /// marks a REAPER as ending; a crash Windows Error Reporting still
    /// reports does.
    fn reaper_await_end(&mut self, c: &Cancel) -> R<()>;
    /// ≤ 60 s for the track count (S7, #10: the whole step measured
    /// 6.0–6.3 s), ended at once when REAPER's process has ended (#10); the
    /// meter bridge at most once; the titles of REAPER's visible dialogs.
    fn reaper_facts(&mut self, c: &Cancel) -> R<ReaperFacts>;
    fn app_start(&mut self) -> R<()>;
    /// `/api/version`, the member count and the public host, and the app's
    /// own process owns ports 80/443 ([`app_serves`], #10).
    fn app_answers(&mut self, c: &Cancel) -> R<()>;
    /// S1c's REAPER-mode fingerprint through the tuning task (`state`).
    fn fingerprint(&mut self) -> R<()>;
    /// `\iemmixer\iemmixer-probe` started by the guard (design §5.1).
    fn probe_task(&mut self) -> R<()>;
    fn notify(&mut self, audience: Audience, title: &str, body: &str) -> R<()>;
    /// The HIL test signal (design §4, §7): `HilTestSignal` over the
    /// supervisor pipe, encoded only on `card_tx` (`[guard] hil_tx`); with
    /// `listen` also the listen probe (S7, #10).
    fn engine_hil_signal(
        &mut self,
        input: &str,
        dbfs: f64,
        ttl_s: f64,
        card_tx: &[u16],
        listen: bool,
    ) -> R<()>;
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
    }
}

#[cfg(test)]
mod fake_tests;
#[cfg(test)]
mod precheck_tests;
#[cfg(test)]
mod pref_tests;
#[cfg(test)]
mod tests;
