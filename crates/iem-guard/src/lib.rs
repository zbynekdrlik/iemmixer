//! `iem-guard` (S6 design note `docs/superpowers/specs/2026-09-27-s6-asio-guard-hil-design.md` §5):
//! the guard switches the IEM PC between REAPER with the predecessor app
//! (`event`) and iemmixer (`dev`, `live`) on the owner's messages, installs
//! bundles and watches iemmixer's processes. Nothing here ends a process: every
//! stop is a request plus a bounded wait, then an alarm.
//!
//! The pure core, tested and mutated on Linux:
//!
//! - [`plan`]: the switch planner and the event error policy;
//! - [`crash`]: what an engine exit means (respawn, stay, crash loop);
//! - [`bundle`]: sums, manifest, records and pins of CI bundles;
//! - [`handover`]: the REAPER, meter-bridge and predecessor-exit verdicts;
//! - [`proto`]: the guard pipe's requests, replies and frames;
//! - [`state`]: the persistent state and the reboot rule;
//! - [`lifecycle`]: before the cutover, after it, or rolling back: the
//!   boot rule, the entry gates, the pin and the crash rules (S8);
//! - [`cutover`]: the cutover's refusals, steps and undo, the cutover
//!   task's request and the server config's `pin_changes` edit (S8);
//! - [`switch_log`]: the last switch's record, its steps timed, and its
//!   in-ear silence (S7);
//! - [`alarms`]: the kept alarms;
//! - [`cancel`]: the pre-emption token of every waiting step;
//! - [`view`]: what the tray shows (tooltip, alarm notices).
//!
//! The daemon and its clients (S6 plan Task 10):
//!
//! - [`daemon`]: the switch runner with its error policy and pre-emption,
//!   the requests, the watch, the start after a reboot, `event --direct`;
//! - [`pipe`]: the guard pipe's listener, connections and client;
//! - [`install`]: bundle install, `--verify-only` and the `bin\` copies;
//! - [`cli`]: the command lines of `iemmode` and `iemmixer-guard`.
//!
//! The PC's effects (S6 plan Task 9):
//!
//! - [`pc`]: the [`Pc`] trait every switch step goes through, and a fake for
//!   the daemon's tests;
//! - [`site`]: the guard's settings (`[guard]`, `[card]`, `pc.toml`);
//! - [`effects`]: the portable parsers and decisions of the effects;
//! - [`tls`]: the LAN 443 identity, a TLS client pinned to the server's own
//!   certificate (identity, not validity);
//! - `win`: `WinPc`, the effects on the PC (Windows only, built on
//!   `iem-win`; excluded from mutation testing, its decisions are above).

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

pub mod alarms;
pub mod bundle;
pub mod cancel;
pub mod cli;
pub mod crash;
pub mod cutover;
pub mod daemon;
pub mod effects;
pub mod handover;
pub mod install;
pub mod lifecycle;
pub mod pc;
pub mod pipe;
pub mod plan;
pub mod proto;
pub mod site;
pub mod state;
pub mod switch_log;
pub mod tls;
pub mod view;
#[cfg(windows)]
pub mod win;

pub use alarms::{Alarm, Alarms};
pub use cancel::{Cancel, Preempted};
pub use pc::{Audience, Pc, R, StepError};
pub use plan::{Facts, Health, Mode, OnError, PrefFail, Step};
pub use proto::{EngineStatus, Reply, Request, Update};
pub use site::Settings;
pub use state::GuardState;
