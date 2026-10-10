//! Commands, replies and events (program spec §2.3, I6; S3 design note §3.3,
//! §3.6; the model of the #20 design note §5). JSON shapes: client messages and engine messages carry a `type`
//! tag, commands an `op` tag, state changes a `kind` tag, all snake_case.
//!
//! Protocol N/N−1: the engine speaks `min(ours, theirs)` while the client is at
//! most one version behind ([`negotiate`]); newer fields are additive.
//!
//! The parts: `cmd` (the commands), `client` (what a client sends and how
//! it is parsed), `engine` (what the engine sends) and `status` (meter
//! frames and `Status`); every name is re-exported here.

mod client;
mod cmd;
mod engine;
mod status;

pub use self::client::{ClientMsg, Role, parse_client};
pub use self::cmd::{Cmd, OPS};
pub use self::engine::{
    Alarm, AlarmCode, Change, EngineMsg, ErrCode, ErrorBody, GroupInfo, Hello, InputInfo, MixInfo,
    Reply, TopologyInfo,
};
pub use self::status::{HilOut, Meters, Status};

/// The protocol version this build speaks.
pub const PROTO: u16 = 1;

/// The version both sides speak, or `None` when the client is more than one
/// version older than `ours`. A newer client speaks down to `ours`.
pub fn negotiate(ours: u16, theirs: u16) -> Option<u16> {
    if theirs >= ours {
        Some(ours)
    } else if theirs.checked_add(1) == Some(ours) && theirs > 0 {
        Some(theirs)
    } else {
        None
    }
}

#[cfg(test)]
mod tests;

/// S7 (#10): the listen probe's flag on `HilTestSignal`.
#[cfg(test)]
mod s7_tests;
