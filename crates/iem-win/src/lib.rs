//! Safe Windows glue for the engine and the guard (S6 design note §3). Every
//! function has a portable signature; off Windows it returns `Unsupported`, so
//! callers keep their decisions testable on Linux. The only unsafe code of the
//! workspace besides `iem_audio_io::asio` lives in the `cfg(windows)` modules.
//!
//! No function here ends another process in any form: a process is asked to
//! stop (Ctrl-Break, a window command) and then watched ([`process::wait_gone`]).

#![cfg_attr(not(windows), forbid(unsafe_code))]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

pub mod console;
pub mod power;
pub mod process;
pub mod registry;
pub mod spawn;
pub mod token;
pub mod window;

#[cfg(windows)]
mod win;

#[cfg(not(windows))]
pub(crate) fn unsupported<T>() -> std::io::Result<T> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Windows only",
    ))
}

/// The error kind of a result (tests only: no `Debug` bound on `T`).
#[cfg(test)]
pub(crate) fn kind<T>(result: std::io::Result<T>) -> Option<std::io::ErrorKind> {
    result.err().map(|e| e.kind())
}
