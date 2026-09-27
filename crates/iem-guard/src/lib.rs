//! `iem-guard` (S6 design note `docs/superpowers/specs/2026-09-27-s6-asio-guard-hil-design.md` §5):
//! the guard switches the IEM PC between REAPER with the predecessor app
//! (`event`) and iemmixer (`dev`, `live`) on the owner's messages, installs
//! bundles and watches iemmixer's processes. Nothing here ends a process: every
//! stop is a request plus a bounded wait, then an alarm.

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
