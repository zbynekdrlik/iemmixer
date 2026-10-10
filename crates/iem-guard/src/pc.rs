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
//!
//! Its parts: `procs.rs` (the process list, the facts of a plan, the
//! module's holders, adoption), `pref.rs` (what `PrefCheck` found and who
//! holds the driver), `precheck.rs` (the dev/live entry's precheck) and
//! `fake.rs` (the scripted PC of the tests).

use std::fmt;

use iem_win::spawn::Placement;

use crate::cancel::{Cancel, Preempted};
use crate::effects::tuning::Logon;
use crate::handover::{AppExit, ReaperFacts, ReaperProcs};
use crate::plan::{Facts, Health, Mode};
use crate::proto::HilOut;
use crate::rollback::{Placed, Want};
use crate::state::Children;

mod precheck;
mod pref;
mod procs;

pub use self::precheck::{PrecheckFacts, precheck};
pub use self::pref::{CardHolders, PREF_ATTEMPTS, PrefHeld, PrefSeen, card_holders};
pub use self::procs::{
    Images, Ports, Procs, adoptable, app_serves, facts_from, foreign_engine, foreign_holders,
};

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
    /// With a config that freezes PIN changes before the cutover; in prod
    /// (`prod`) `pin_changes = true` is allowed (`effects::web::pin_policy`,
    /// S8).
    fn server_start(&mut self, mode: Mode, prod: bool) -> R<u32>;
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
    /// verified bundle `sha` (design §5.1); `keep`'s (`lifecycle::kept`) stay,
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
    /// The cutover task (`\iemmixer\iemmixer-cutover`, S8 lane 2;
    /// `IemCutover.psm1`): the predecessor's autostarts it was installed
    /// with exported to `<elevated root>\cutover\<export>` and then
    /// disabled, each read back. What it did. A mutation: it finishes.
    fn autostarts_off(&mut self, export: &str) -> R<String>;
    /// The cutover task: every autostart `<export>` saved, back exactly as
    /// it was, read back; no such export: nothing to do. What it did.
    fn autostarts_on(&mut self, export: &str) -> R<String>;
    /// The cutover task: the guard task's logon trigger on or off, read
    /// back (after the cutover the guard starts at the user's logon).
    fn guard_logon(&mut self, on: bool) -> R<()>;
    /// The server's config file (`pc.toml` `server_config`) as text.
    fn server_config(&mut self) -> R<String>;
    /// Replaces the server's config whole (a temp file renamed over it), read
    /// back byte for byte.
    fn write_server_config(&mut self, text: &str) -> R<()>;
    /// LAN 80 serves the mixer's page and `/api/members` lists the band
    /// (`effects::web::member_page_problem`). Never waits beyond two local
    /// requests.
    fn member_page(&mut self) -> R<()>;
    /// The rollback's export (S8 lane 3, design note §3.3): `pc.toml`'s
    /// `rollback_export` (`iem-migrate export`, self-checked) writes the
    /// band's data from the engine's saved state into a new project,
    /// `rollback::export_path` of `[guard] reaper_project` and `at`; it
    /// creates that file (never one that exists) and only reads the
    /// original. What it reported. A mutation: it finishes.
    fn export_project(&mut self, at: u64) -> R<String>;
    /// The project's path made to hold `want` by renames only
    /// (`rollback::moves`: a target never exists; the original kept as
    /// `rollback::kept_path` of `at`), read back: what it holds now.
    fn swap_project(&mut self, want: Want, at: u64) -> R<Placed>;
}

/// A scripted PC for the daemon's tests.
#[cfg(test)]
pub mod fake;

#[cfg(test)]
mod fake_tests;
#[cfg(test)]
mod precheck_tests;
#[cfg(test)]
mod pref_tests;
#[cfg(test)]
mod tests;
