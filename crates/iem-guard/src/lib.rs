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
//! - [`alarms`]: the kept alarms;
//! - [`cancel`]: the pre-emption token of every waiting step.
//!
//! The PC's effects (S6 plan Task 9):
//!
//! - [`pc`]: the [`Pc`] trait every switch step goes through, and a fake for
//!   the daemon's tests;
//! - [`site`]: the guard's settings (`[guard]`, `[card]`, `pc.toml`);
//! - [`effects`]: the portable parsers and decisions of the effects.

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
pub mod crash;
pub mod effects;
pub mod handover;
pub mod pc;
pub mod plan;
pub mod proto;
pub mod site;
pub mod state;

pub use alarms::{Alarm, Alarms};
pub use cancel::{Cancel, Preempted};
pub use pc::{Audience, Pc, R, StepError};
pub use plan::{Facts, Health, Mode, OnError, PrefFail, Step};
pub use proto::{Reply, Request};
pub use site::Settings;
pub use state::GuardState;
