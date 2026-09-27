//! The engine seen from the server (S5 design note §4): the control pipe
//! client and its mirror of the engine's state, the pipes' framing, and (with
//! `audio`) the media pipe with Opus.

pub mod client;
#[cfg(feature = "audio")]
pub mod media;
pub mod mirror;
// Split cfgs: cargo-mutants skips a module only under a plain `#[cfg(test)]`.
#[cfg(test)]
#[cfg(unix)]
pub mod testkit;
pub mod wire;

pub use client::EngineClient;
