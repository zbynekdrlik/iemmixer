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
    InjectFault,
    Ping,
}

/// Every `op` tag, for telling an unknown command from a malformed one.
pub const OPS: [&str; 20] = [
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
    "ping",
];

impl Cmd {
    /// Commands an observer connection may send.
    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::GetState | Self::GetTopology | Self::Ping)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Control,
    Observe,
}

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
    DriverReleased {
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
mod tests {
    use super::*;
    use crate::state::EqBand;

    fn mix(s: &str) -> MixId {
        MixId::new(s)
    }

    fn every_cmd() -> Vec<Cmd> {
        let input = InputId::new("mic1");
        vec![
            Cmd::SetInput {
                input: input.clone(),
                trim_db: Some(1.0),
                muted: None,
                processing: Some(false),
            },
            Cmd::SetMix {
                mix: mix("member1"),
                volume_db: Some(-3.0),
                muted: None,
            },
            Cmd::SetLevel {
                mix: mix("member1"),
                source: Source::Input(input.clone()),
                gain_db: None,
                pan: Some(0.5),
                muted: Some(true),
            },
            Cmd::SetGroup {
                mix: mix("member1"),
                group: GroupId::new("stems"),
                gain_db: Some(-6.0),
                muted: None,
            },
            Cmd::SetEq {
                target: EqTarget::Input(input.clone()),
                eq: Eq::default(),
            },
            Cmd::SetLimiter {
                mix: mix("member1"),
                enabled: Some(true),
                limit_db: Some(-3.0),
            },
            Cmd::ResetLimiterStats {
                mix: mix("member1"),
            },
            Cmd::SetSolo {
                mix: mix("member1"),
                sources: vec![Source::Input(input.clone())],
            },
            Cmd::StartListen {
                mix: mix("member1"),
            },
            Cmd::StopListen {
                mix: mix("member1"),
            },
            Cmd::StartTestSignal {
                input,
                hz: 1000.0,
                dbfs: -30.0,
                ttl_s: 10.0,
            },
            Cmd::StopTestSignal,
            Cmd::Batch {
                ops: vec![Cmd::Ping],
            },
            Cmd::ImportState {
                state: MixState::default(),
                baseline: true,
            },
            Cmd::GetState,
            Cmd::GetTopology,
            Cmd::SaveNow,
            Cmd::Shutdown,
            Cmd::InjectFault,
            Cmd::Ping,
        ]
    }

    #[test]
    fn every_command_round_trips_and_its_op_is_listed() {
        let cmds = every_cmd();
        assert_eq!(cmds.len(), OPS.len());
        for (cmd, op) in cmds.iter().zip(OPS) {
            let v = serde_json::to_value(cmd).unwrap();
            assert_eq!(v["op"], op);
            assert_eq!(&serde_json::from_value::<Cmd>(v).unwrap(), cmd);
        }
    }

    #[test]
    fn json_shapes_are_stable() {
        let cmd = Cmd::SetLevel {
            mix: mix("member1"),
            source: Source::Mix(mix("member2")),
            gain_db: Some(-3.0),
            pan: None,
            muted: None,
        };
        assert_eq!(
            serde_json::to_string(&cmd).unwrap(),
            r#"{"op":"set_level","mix":"member1","source":{"mix":"member2"},"gain_db":-3.0,"pan":null,"muted":null}"#
        );
        let req = ClientMsg::Request {
            id: 7,
            origin: None,
            cmd: Cmd::Ping,
        };
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"type":"request","id":7,"origin":null,"cmd":{"op":"ping"}}"#
        );
        let hello = ClientMsg::Hello {
            proto: 1,
            role: Role::Observe,
            client: "t".into(),
        };
        assert_eq!(
            serde_json::to_string(&hello).unwrap(),
            r#"{"type":"hello","proto":1,"role":"observe","client":"t"}"#
        );
        let delta = EngineMsg::Delta {
            rev: 3,
            origin: Some(9),
            changes: vec![Change::LimiterStatsReset {
                mix: mix("engineer"),
            }],
        };
        assert_eq!(
            serde_json::to_string(&delta).unwrap(),
            r#"{"type":"delta","rev":3,"origin":9,"changes":[{"kind":"limiter_stats_reset","mix":"engineer"}]}"#
        );
        let level = Change::Level {
            mix: mix("member1"),
            source: Source::Input(InputId::new("mic1")),
            level: Level::default(),
        };
        assert_eq!(
            serde_json::to_string(&level).unwrap(),
            r#"{"kind":"level","mix":"member1","source":{"input":"mic1"},"level":{"gain_db":-150.0,"pan":0.0,"muted":false}}"#
        );
        assert_eq!(
            serde_json::to_string(&EngineMsg::Superseded).unwrap(),
            r#"{"type":"superseded"}"#
        );
        // Missing optional fields are None.
        let parsed: Cmd = serde_json::from_str(r#"{"op":"set_mix","mix":"member1"}"#).unwrap();
        assert_eq!(
            parsed,
            Cmd::SetMix {
                mix: mix("member1"),
                volume_db: None,
                muted: None
            }
        );
        // An older topology without groups or heard mixes still reads.
        let info: TopologyInfo = serde_json::from_str(
            r#"{"hash":"h","sample_rate":96000,"engineer":"e","inputs":[{"id":"mic","channels":1,"talkback":false}],"mixes":[{"id":"e","channels":2}]}"#,
        )
        .unwrap();
        assert!(info.groups.is_empty() && info.mixes[0].mixes.is_empty());
        assert_eq!(info.inputs[0].group, None);
    }

    #[test]
    fn engine_messages_round_trip() {
        let msgs = vec![
            EngineMsg::Hello(Hello {
                proto: 1,
                engine_build: "2.0.0".into(),
                topology_hash: "ab".into(),
                state_rev: 4,
                sample_rate: 96_000,
                block: 32,
                role: Role::Control,
            }),
            EngineMsg::Reply(Reply {
                id: 1,
                rev: 2,
                error: Some(ErrorBody {
                    code: ErrCode::NoSource,
                    msg: "x".into(),
                }),
            }),
            EngineMsg::Topology(TopologyInfo {
                hash: "h".into(),
                sample_rate: 96_000,
                engineer: mix("engineer"),
                inputs: vec![InputInfo {
                    id: InputId::new("drums"),
                    channels: 2,
                    talkback: false,
                    group: Some(GroupId::new("stems")),
                }],
                groups: vec![GroupInfo {
                    id: GroupId::new("stems"),
                    inputs: vec![InputId::new("drums")],
                }],
                mixes: vec![MixInfo {
                    id: mix("engineer"),
                    channels: 2,
                    mixes: vec![mix("member1")],
                }],
            }),
            EngineMsg::State {
                rev: 1,
                state: MixState::default(),
                transient: Transient::default(),
            },
            EngineMsg::Delta {
                rev: 2,
                origin: None,
                changes: vec![
                    Change::MixOut {
                        mix: mix("member1"),
                        out: MixOut::default(),
                    },
                    Change::Group {
                        mix: mix("member1"),
                        group: GroupId::new("stems"),
                        state: MixGroup::default(),
                    },
                    Change::Solo {
                        mix: mix("member1"),
                        sources: vec![Source::Mix(mix("member2"))],
                    },
                    Change::Listen {
                        listen: [Some(mix("engineer")), None],
                    },
                ],
            },
            EngineMsg::Meters(Meters {
                seq: 1,
                inputs: vec![[0.5, 0.25]],
                groups: vec![[0.125, 0.0]],
                ..Meters::default()
            }),
            EngineMsg::Status(Status::default()),
            EngineMsg::Saved {
                rev: 1,
                generation: 2,
            },
            EngineMsg::Alarm(Alarm {
                code: AlarmCode::StateLost,
                detail: "d".into(),
            }),
            EngineMsg::DriverReleased {
                reason: "shutdown".into(),
            },
            EngineMsg::Superseded,
        ];
        for m in msgs {
            let json = serde_json::to_string(&m).unwrap();
            assert_eq!(
                serde_json::from_str::<EngineMsg>(&json).unwrap(),
                m,
                "{json}"
            );
        }
        let band = EqBand::default();
        assert_eq!(band.gain_db, 0.0);
    }

    #[test]
    fn unknown_op_is_unsupported_with_its_id() {
        let (id, e) =
            parse_client(br#"{"type":"request","id":9,"cmd":{"op":"warp"}}"#).unwrap_err();
        assert_eq!(id, Some(9));
        assert_eq!(e.code, ErrCode::Unsupported);
    }

    #[test]
    fn a_malformed_known_op_is_a_bad_request_with_its_id() {
        let (id, e) =
            parse_client(br#"{"type":"request","id":4,"cmd":{"op":"set_mix"}}"#).unwrap_err();
        assert_eq!(id, Some(4));
        assert_eq!(e.code, ErrCode::BadRequest);
    }

    #[test]
    fn garbage_is_a_bad_request_without_id() {
        let cases: [&[u8]; 4] = [b"\xff", b"", b"[]", br#"{"type":"hello"}"#];
        for bytes in cases {
            let (id, e) = parse_client(bytes).unwrap_err();
            assert_eq!(id, None);
            assert_eq!(e.code, ErrCode::BadRequest);
        }
        // An unknown op outside a request is not "unsupported".
        let (_, e) = parse_client(br#"{"type":"other","id":1,"cmd":{"op":"warp"}}"#).unwrap_err();
        assert_eq!(e.code, ErrCode::BadRequest);
    }

    #[test]
    fn error_messages_are_bounded() {
        let long = format!(
            r#"{{"type":"request","id":1,"cmd":{{"op":"{}é"}}}}"#,
            "x".repeat(5000)
        );
        let (_, e) = parse_client(long.as_bytes()).unwrap_err();
        assert!(e.msg.len() <= MAX_MSG, "{}", e.msg.len());
        assert_eq!(short("é".repeat(150)).len(), 200);
        assert_eq!(short("abc".into()), "abc");
        // Byte 200 inside a character: cut before it. In a thread, so that a
        // cut that never finds a boundary fails instead of hanging.
        let odd = format!("a{}", "é".repeat(150));
        let (tx, rx) = std::sync::mpsc::channel();
        let text = odd.clone();
        let _ = std::thread::spawn(move || tx.send(short(text)));
        let cut = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("short() returns");
        assert_eq!(cut.len(), 199);
        assert_eq!(cut, odd[..199]);
    }

    #[test]
    fn requests_parse() {
        let msg =
            parse_client(br#"{"type":"request","id":2,"origin":5,"cmd":{"op":"ping"}}"#).unwrap();
        assert_eq!(
            msg,
            ClientMsg::Request {
                id: 2,
                origin: Some(5),
                cmd: Cmd::Ping
            }
        );
        assert_eq!(
            parse_client(br#"{"type":"hello","proto":1,"role":"control"}"#).unwrap(),
            ClientMsg::Hello {
                proto: 1,
                role: Role::Control,
                client: String::new()
            }
        );
    }

    #[test]
    fn negotiate_speaks_n_and_n_minus_1() {
        assert_eq!(negotiate(5, 5), Some(5));
        assert_eq!(negotiate(5, 6), Some(5));
        assert_eq!(negotiate(5, 9), Some(5));
        assert_eq!(negotiate(5, 4), Some(4));
        assert_eq!(negotiate(5, 3), None);
        assert_eq!(negotiate(5, 0), None);
        assert_eq!(negotiate(1, 0), None);
        assert_eq!(negotiate(PROTO, PROTO), Some(PROTO));
    }

    #[test]
    fn only_reads_are_read_only() {
        let reads: Vec<bool> = every_cmd().iter().map(Cmd::is_read_only).collect();
        assert_eq!(reads.iter().filter(|r| **r).count(), 3);
        assert!(Cmd::GetState.is_read_only());
        assert!(Cmd::GetTopology.is_read_only());
        assert!(Cmd::Ping.is_read_only());
        assert!(!Cmd::SaveNow.is_read_only());
    }
}
