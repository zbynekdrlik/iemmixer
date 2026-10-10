//! What the engine sends (`type`-tagged): replies and errors, its hello,
//! the topology, state changes (`kind`-tagged) and alarms.

use serde::{Deserialize, Serialize};

use super::client::Role;
use super::status::{Meters, Status};
use crate::ids::{GroupId, InputId, MixId, Source};
use crate::state::{InputState, Level, MixGroup, MixOut, MixState, TestSignal, Transient};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrCode {
    BadRequest,
    Unsupported,
    UnknownId,
    BadValue,
    Forbidden,
    NoSource,
    NotController,
    TooLarge,
    /// A supervisor command from another role (S6).
    NotSupervisor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrCode,
    pub msg: String,
}

/// The answer to one request: `rev` is the state revision after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    pub rev: u64,
    #[serde(default)]
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub proto: u16,
    pub engine_build: String,
    pub topology_hash: String,
    pub state_rev: u64,
    pub sample_rate: u32,
    pub block: u32,
    pub role: Role,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputInfo {
    pub id: InputId,
    pub channels: u8,
    pub talkback: bool,
    /// The group the input belongs to; mixes hear it through that group.
    #[serde(default)]
    pub group: Option<GroupId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupInfo {
    pub id: GroupId,
    pub inputs: Vec<InputId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MixInfo {
    pub id: MixId,
    /// TX channels: 2 (stereo) or 1 (the mono downmix, A10).
    pub channels: u8,
    /// The mixes this mix hears (the Mixes tab, F16), all declared before it.
    #[serde(default)]
    pub mixes: Vec<MixId>,
}

/// The compiled topology. Every mix has a level for every input, a strip for
/// every group and a level for each mix it hears. Meter frames list inputs,
/// then mixes, then every mix's group strips (mix-major), in this order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyInfo {
    pub hash: String,
    pub sample_rate: u32,
    /// The mix with the fixed listen tap (X3 slot 0).
    pub engineer: MixId,
    pub inputs: Vec<InputInfo>,
    #[serde(default)]
    pub groups: Vec<GroupInfo>,
    pub mixes: Vec<MixInfo>,
}

/// One changed entity, carried whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Change {
    Input {
        id: InputId,
        state: InputState,
    },
    MixOut {
        mix: MixId,
        out: MixOut,
    },
    Level {
        mix: MixId,
        source: Source,
        level: Level,
    },
    Group {
        mix: MixId,
        group: GroupId,
        state: MixGroup,
    },
    Solo {
        mix: MixId,
        sources: Vec<Source>,
    },
    Listen {
        listen: [Option<MixId>; 2],
    },
    TestSignal {
        signal: Option<TestSignal>,
    },
    LimiterStatsReset {
        mix: MixId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlarmCode {
    /// The state came from a generation or the baseline, not `current.json`.
    StateFallback,
    /// No state loaded: defaults with every mix muted.
    StateLost,
    /// A node's sanitiser tripped (X1).
    Sanitizer,
    /// The RT callback faulted; the driver is released.
    Fault,
    SaveFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alarm {
    pub code: AlarmCode,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineMsg {
    Hello(Hello),
    Reply(Reply),
    Topology(TopologyInfo),
    State {
        rev: u64,
        state: MixState,
        #[serde(default)]
        transient: Transient,
    },
    Delta {
        rev: u64,
        #[serde(default)]
        origin: Option<u64>,
        changes: Vec<Change>,
    },
    Meters(Meters),
    Status(Status),
    Saved {
        rev: u64,
        generation: u64,
    },
    Alarm(Alarm),
    /// The stream stopped and the card is released (the engine ends next).
    DriverReleased {
        reason: String,
    },
    /// The stream stopped parked (#35): a callback stayed in it, or the
    /// parked-engine test's hold kept the card, so nothing was released and
    /// the card is free only once the engine's process has ended. Sent
    /// instead of `DriverReleased`, at the same point.
    DriverParked {
        reason: String,
    },
    Superseded,
}
