//! Commands, replies and events (program spec §2.3, I6; S3 design note §3.3,
//! §3.6; the model of the #20 design note §5). JSON shapes: client messages and engine messages carry a `type`
//! tag, commands an `op` tag, state changes a `kind` tag, all snake_case.
//!
//! Protocol N/N−1: the engine speaks `min(ours, theirs)` while the client is at
//! most one version behind ([`negotiate`]); newer fields are additive.

use serde::{Deserialize, Serialize};

use crate::ids::{EqTarget, GroupId, InputId, MixId, Source};
use crate::state::{Eq, InputState, Level, MixGroup, MixOut, MixState, TestSignal, Transient};

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Cmd {
    /// F29: trim, mute and processing of an input (its EQ: `SetEq`).
    SetInput {
        input: InputId,
        trim_db: Option<f64>,
        muted: Option<bool>,
        processing: Option<bool>,
    },
    /// F7: a mix's volume and mute (its EQ: `SetEq`, its limiter: `SetLimiter`).
    SetMix {
        mix: MixId,
        volume_db: Option<f64>,
        muted: Option<bool>,
    },
    /// F5, F16: the level of an input or a heard mix in a mix.
    SetLevel {
        mix: MixId,
        source: Source,
        gain_db: Option<f64>,
        pan: Option<f64>,
        muted: Option<bool>,
    },
    /// F7: a group's strip in a mix (its EQ: `SetEq`).
    SetGroup {
        mix: MixId,
        group: GroupId,
        gain_db: Option<f64>,
        muted: Option<bool>,
    },
    SetEq {
        target: EqTarget,
        eq: Eq,
    },
    SetLimiter {
        mix: MixId,
        enabled: Option<bool>,
        limit_db: Option<f64>,
    },
    ResetLimiterStats {
        mix: MixId,
    },
    SetSolo {
        mix: MixId,
        sources: Vec<Source>,
    },
    StartListen {
        mix: MixId,
    },
    StopListen {
        mix: MixId,
    },
    StartTestSignal {
        input: InputId,
        hz: f64,
        dbfs: f64,
        ttl_s: f64,
    },
    StopTestSignal,
    Batch {
        ops: Vec<Cmd>,
    },
    ImportState {
        state: MixState,
        #[serde(default)]
        baseline: bool,
    },
    GetState,
    GetTopology,
    SaveNow,
    Shutdown,
    /// A panic on the RT thread (HIL's panic check), under the
    /// fault-injection flag. Supervisor only: an engine keeps the flag until
    /// it restarts, after its HIL job too, and only the guard allows an
    /// injection, inside a job (review of PR #40, #35).
    InjectFault,
    /// Owner-approved SEH test (design §10): a structured exception on the
    /// RT thread, under the fault-injection flag. Supervisor only, like
    /// `InjectFault`.
    InjectSeh,
    /// The parked-engine test (design §10 test #2, #35): the SEH test's
    /// exception under a test hold, so the backend keeps the driver and the
    /// SEH filter parks the RT thread; the engine keeps running and reports
    /// `parked`. Under the fault-injection flag, supervisor only, like
    /// `InjectSeh`.
    InjectPark,
    Ping,
    /// The supervisor lets an engine started with `--hold` sound (S6 design
    /// note §4): the outputs fade in over 500 ms.
    Arm,
    /// The HIL test signal (S6 design note §4, §7): the test signal on
    /// `input` (under the test-signal flag, capped like `StartTestSignal`;
    /// `dbfs` above the cap is refused), whose sine, while it runs, sounds
    /// only on the card outputs `card_tx`: spare outputs no mix uses (the
    /// site's `[guard] hil_tx`, which the engine opens under the flag); a
    /// mix's TX is refused, so it never reaches a band member. Every mix's
    /// TX stays zero meanwhile; the mixes render as usual, so their meters
    /// show the routing. Supervisor only.
    HilTestSignal {
        input: InputId,
        hz: f64,
        dbfs: f64,
        ttl_s: f64,
        card_tx: Vec<u16>,
        /// S7, additive (design note §6): the listen probe. While the signal
        /// runs, the listen taps keep their silent frames and the probe streams
        /// (`media::stream::ENGINEER_PROBE`, `MEMBER_PROBE`) carry the spare
        /// outputs' sine, for the server's `&hil=1` listeners only. Every mix's
        /// TX stays zero. An older engine ignores it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        listen: bool,
    },
    /// HIL's forced reopen of the driver (S6 design note §7): the backend
    /// stops, releases and opens the card again (its reset budget applies;
    /// the outputs fade in again). Supervisor only, under the
    /// fault-injection flag.
    ForceReopen,
}

/// Every `op` tag, for telling an unknown command from a malformed one.
pub const OPS: [&str; 25] = [
    "set_input",
    "set_mix",
    "set_level",
    "set_group",
    "set_eq",
    "set_limiter",
    "reset_limiter_stats",
    "set_solo",
    "start_listen",
    "stop_listen",
    "start_test_signal",
    "stop_test_signal",
    "batch",
    "import_state",
    "get_state",
    "get_topology",
    "save_now",
    "shutdown",
    "inject_fault",
    "inject_seh",
    "inject_park",
    "ping",
    "arm",
    "hil_test_signal",
    "force_reopen",
];

impl Cmd {
    /// Commands an observer connection may send.
    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::GetState | Self::GetTopology | Self::Ping)
    }

    /// Commands only the supervisor may send (S6 design note §4): `Arm`,
    /// the HIL signal, the forced reopen and the fault injections (review
    /// of PR #40, #35); anyone else gets `NotSupervisor`.
    pub fn is_supervisor(&self) -> bool {
        matches!(
            self,
            Self::Arm
                | Self::HilTestSignal { .. }
                | Self::ForceReopen
                | Self::InjectFault
                | Self::InjectSeh
                | Self::InjectPark
        )
    }

    /// Commands a supervisor connection may send: reads, its own commands
    /// (fault injection among them), `Shutdown`, `SaveNow` and the test
    /// signal (their launch flags still apply); never a change of the mix.
    pub fn supervisor_may(&self) -> bool {
        self.is_read_only()
            || self.is_supervisor()
            || matches!(
                self,
                Self::Shutdown
                    | Self::SaveNow
                    | Self::StartTestSignal { .. }
                    | Self::StopTestSignal
            )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Control,
    Observe,
    /// The guard's connection (S6 design note §4): `Shutdown`, `SaveNow`,
    /// `Arm`, the test signals and fault injection (under their launch
    /// flags) and reads; never a mix change. One at a time, beside the
    /// controller: a new supervisor supersedes the old one only.
    Supervisor,
}

// One message at a time, on the control thread only (never the RT thread):
// its size does not matter, and boxing the command would only add an
// allocation per request.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        proto: u16,
        role: Role,
        #[serde(default)]
        client: String,
    },
    Request {
        id: u64,
        /// The sender's session tag, echoed in the resulting `Delta` (echo suppression).
        #[serde(default)]
        origin: Option<u64>,
        cmd: Cmd,
    },
}

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

/// Peaks since the previous frame (linear): inputs (post-mute), mixes (post
/// volume and mute) and group strips (mix-major: mix `m`, group `g` at
/// `m · groups + g`); limiter GR (dB, 0 while disabled) and X14 active
/// seconds per mix; all in topology order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Meters {
    pub seq: u64,
    pub inputs: Vec<[f32; 2]>,
    pub mixes: Vec<[f32; 2]>,
    pub groups: Vec<[f32; 2]>,
    pub gr_db: Vec<f32>,
    pub limiter_active_s: Vec<f64>,
    pub trips: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    pub callbacks: u64,
    pub late: u64,
    pub faulted: bool,
    pub process_max_us: f64,
    pub trips: u64,
    pub tap_overruns: u64,
    pub talkback_dropped: u64,
    pub cmd_backlog: u64,
    // S6, additive: the stream's own figures (the ASIO backend; NullRt
    // reports its block and zeros).
    /// The period the driver delivers, measured from its sample positions.
    pub frames: u32,
    /// Callback intervals of two periods or more.
    pub missed: u64,
    /// Callbacks longer than one period.
    pub overruns: u64,
    /// Driver reopens after a reset request or a stall.
    pub resets: u64,
    /// A callback outlived the stop wait: the stream stays allocated (the
    /// guard alarms).
    pub parked: bool,
    /// Started with `--hold` and not armed yet: every output is silent.
    pub held: bool,
    /// Locking the real-time memory failed (logged, never fatal).
    pub lock_failed: bool,
    /// S6, additive: HIL's spare card outputs (the engine opens the site's
    /// `[guard] hil_tx` under the test-signal flag), in that order; empty
    /// otherwise. HIL v1 reads them to prove its test signal reached them,
    /// at its level, and left them.
    pub hil: Vec<HilOut>,
    /// S6 test 5, additive: the D5(b) loopback round-trip, in samples, once
    /// measured (the HIL signal on a spare output, looped back to the matching
    /// spare input in Dante); 0 while none. `loopback_ms` derives the time.
    pub loopback_samples: u64,
    /// S7, additive (design note §3): the callback interval since the stream
    /// opened, 1 µs buckets `[b, b + 1)` below two periods and the overflow
    /// bucket `hist_top_us` (two periods or more: the card's `missed`), as
    /// `[[bucket, count], …]`, non-empty buckets ascending. Absent without a
    /// stream and from an older engine.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub interval_hist: Vec<(u32, u64)>,
    /// S7, additive: the callback's own time, the span `process_max_us`
    /// measures (decode, `process()` and encode on the card; `process()` on
    /// NullRt), in the buckets of `interval_hist`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub process_hist: Vec<(u32, u64)>,
    /// S7, additive: the overflow bucket's index, two periods in µs rounded
    /// up (667 at 32 samples, 96 kHz; at most 1000); 0 without histograms.
    pub hist_top_us: u32,
    /// S7 HIL v2, additive: the last driver reopen, from the old stream's
    /// stop to the new one's measured period, µs; 0 before any (NullRt: 0).
    pub last_reopen_us: u64,
    /// S7 HIL v2, additive: the faulting callback's own time, µs (its entry
    /// to its return, the caught panic included); 0 while not faulted. A
    /// fault sends one last `Status` carrying it before its alarm and
    /// `DriverReleased`.
    pub fault_callback_us: f64,
}

/// One of HIL's spare card outputs in a [`Status`] (S6): its card channel
/// and its peak since the previous `Status` (linear).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HilOut {
    pub tx: u16,
    pub peak: f32,
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

/// Longest error text echoed back (a message may quote the offending input).
const MAX_MSG: usize = 200;

fn short(text: String) -> String {
    if text.len() <= MAX_MSG {
        return text;
    }
    let mut end = MAX_MSG;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default().to_owned()
}

fn error(code: ErrCode, msg: String) -> ErrorBody {
    ErrorBody {
        code,
        msg: short(msg),
    }
}

#[derive(Deserialize)]
struct Probe {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<u64>,
    cmd: Option<serde_json::Value>,
}

/// Parses one client message. A request the engine cannot read is answered
/// with its id when the id is readable: `Unsupported` for an unknown `op`,
/// `BadRequest` for anything else malformed.
pub fn parse_client(bytes: &[u8]) -> Result<ClientMsg, (Option<u64>, ErrorBody)> {
    let err = match serde_json::from_slice::<ClientMsg>(bytes) {
        Ok(msg) => return Ok(msg),
        Err(e) => e.to_string(),
    };
    let Ok(probe) = serde_json::from_slice::<Probe>(bytes) else {
        return Err((None, error(ErrCode::BadRequest, err)));
    };
    let op = probe
        .cmd
        .as_ref()
        .and_then(|c| c.get("op"))
        .and_then(|op| op.as_str());
    let code = match op {
        Some(op) if probe.kind.as_deref() == Some("request") && !OPS.contains(&op) => {
            ErrCode::Unsupported
        }
        _ => ErrCode::BadRequest,
    };
    Err((probe.id, error(code, err)))
}

#[cfg(test)]
mod tests;

/// S7 (#10): the listen probe's flag on `HilTestSignal`.
#[cfg(test)]
mod s7_tests;
