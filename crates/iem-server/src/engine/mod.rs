//! The engine seen from the server (S5 design note §4): the control pipe
//! client and its mirror of the engine's state, the pipes' framing, and (with
//! `audio`) the media pipe with Opus.

pub mod client;
#[cfg(feature = "audio")]
pub mod media;
pub mod mirror;
#[cfg(all(test, unix))]
pub mod testkit;
pub mod wire;

pub use client::EngineClient;
