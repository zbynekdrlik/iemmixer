//! The UI ↔ server WebSocket protocol, version 2 (S5 design note §5): the
//! mixer page's messages keyed by engine ids (an input or a heard mix; one
//! namespace) instead of REAPER track numbers. Client commands carry a `cmd`
//! tag, server events an `event` tag with their fields under `data`.
//!
//! Handshake (program spec §5.3): the page connects with `proto=UI_PROTO`;
//! the server's first message is [`ServerMsg::Hello`]; a page whose protocol
//! is outside `min_client_proto..=proto` reloads.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::Channel;
use crate::tunnel::TunnelStatusInfo;

/// The UI protocol this build speaks.
pub const UI_PROTO: u16 = 2;
/// The oldest UI protocol this server serves.
pub const MIN_CLIENT_PROTO: u16 = 2;

/// One EQ band in the UI's terms (F11): the engine's values, no REAPER
/// normalisation. `band_type` is "highpass", "lowshelf", "band" or "highshelf".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EqBand {
    pub band_type: String,
    pub freq_hz: f32,
    /// Gain in dB (±12 in the UI; the engine's off value means a notch).
    pub gain_db: f32,
    /// Bandwidth in octaves.
    pub bw: f32,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

/// Alert info for ActiveAlerts catch-up message
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertInfo {
    pub from_member: String,
    pub from_name: String,
}

/// One input on the engineer's console (F29).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConsoleInput {
    pub id: String,
    pub name: String,
    pub trim_db: f32,
    pub muted: bool,
    /// Trim and EQ run (Q3).
    pub processing: bool,
}

/// One mix's limiter counter on the engineer's console (F29, F32).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConsoleMix {
    pub id: String,
    pub name: String,
    pub active_seconds: f64,
}

/// A mixer page without a member, open to the engineer (e.g. the translator).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PageLink {
    pub id: String,
    pub name: String,
}

/// Login failures since the server started (`LoginGuard::stats`).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoginFailures {
    pub lan: u64,
    pub tunnel: u64,
    pub engineer_budget_trips: u64,
}

/// The engineer's console (F29).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ConsoleInfo {
    pub inputs: Vec<ConsoleInput>,
    pub limiters: Vec<ConsoleMix>,
    pub pages: Vec<PageLink>,
    pub login: LoginFailures,
}

/// Client → Server commands (sent via WebSocket)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "cmd")]
pub enum ClientMsg {
    /// Level of a channel in the page's mix (F5, F16); −60 dB is off
    SetLevel { id: String, level_db: f32 },
    /// Mute of a channel in the page's mix
    SetMute { id: String, muted: bool },
    /// Pan of a channel in the page's mix (0…1, 0.5 = centre)
    SetPan { id: String, pan: f32 },
    /// The page mix's volume (IEM VOL, F7)
    SetGlobalLevel { level_db: f32 },
    /// The page mix's mute
    SetGlobalMute { muted: bool },
    /// The page mix's stems strip level
    SetStemsLevel { level_db: f32 },
    /// The page mix's stems strip mute
    SetStemsMute { muted: bool },
    /// Pins and hides of the page's member (F8)
    UpdateCustomization {
        pinned: Vec<String>,
        hidden: Vec<String>,
    },
    /// The soloed channels of the page's mix (full replacement, F6)
    SetSolo { soloed: Vec<String> },
    /// Start listening (on `/ws/audio`, engineer): whose mix
    ListenStart { member_id: String },
    /// Stop listening (on `/ws/audio`)
    ListenStop,
    /// Request the EQ of a channel id, the page's mix id or its group id
    GetEqParams { target: String },
    /// Set one EQ band value: `param` is freq_hz, gain_db, bw_oct or enabled
    SetEqBand {
        target: String,
        band: u8,
        param: String,
        value: f32,
    },
    /// Band member calls engineer for help (F20)
    CallEngineer,
    /// Clear active alert (sent by engineer or member to dismiss)
    ClearAlert,
    /// Engineer requests the talkback lock (F18)
    TalkStart,
    /// Engineer releases the talkback lock
    TalkStop,
    /// Request the page mix's limiter (F12)
    GetLimiterParams,
    /// Set the page mix's limiter: `limit` (0…1 → −6…0 dB)
    SetLimiterParam { param: String, value: f32 },
    /// Enable or disable the page mix's limiter
    SetLimiterEnabled { enabled: bool },
    /// Reset the page mix's limiter active-seconds counter
    ResetLimiterActivity,
    /// Request the engineer's console (F29)
    GetConsole,
    /// F29: an input's trim, mute or processing (engineer)
    SetInput {
        input: String,
        #[serde(default)]
        trim_db: Option<f32>,
        #[serde(default)]
        muted: Option<bool>,
        #[serde(default)]
        processing: Option<bool>,
    },
    /// F29: reset any mix's limiter counter (engineer)
    ResetLimiterStats { mix: String },
}

/// Server → Client events (pushed via WebSocket)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event", content = "data")]
pub enum ServerMsg {
    /// First message of every connection (§5.3)
    Hello {
        proto: u16,
        build: String,
        min_client_proto: u16,
    },
    /// Full page state (on connect and after an engine resync)
    State {
        channels: Vec<Channel>,
        connected: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        global_level_db: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        global_muted: Option<bool>,
        /// The page's mix id (IEM VOL meter, EQ and limiter)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mix: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stems_level_db: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stems_muted: Option<bool>,
        /// The stems strip's group id (its meter and EQ)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<String>,
    },
    /// Peak meters by id (linear [left, right])
    Meters { meters: HashMap<String, [f32; 2]> },
    /// One channel changed
    ChannelUpdate {
        id: String,
        level_db: f32,
        muted: bool,
        pan: f32,
    },
    /// The page mix's volume changed
    GlobalVolumeUpdate { level_db: f32, muted: bool },
    /// The page mix's stems strip changed
    StemsVolumeUpdate { level_db: f32, muted: bool },
    /// The engine connection changed
    ConnectionChanged { connected: bool },
    /// Pins and hides (on connect and after changes)
    CustomizationUpdate {
        pinned: Vec<String>,
        hidden: Vec<String>,
    },
    /// Network mode indicator (local LAN vs remote internet)
    NetworkMode { mode: String },
    /// The soloed channels of the page's mix
    SoloUpdate { soloed: Vec<String> },
    /// Audio streaming status update
    AudioStatus {
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
    /// EQ of a target (answer to GetEqParams)
    EqParams {
        target: String,
        track_name: String,
        bands: Vec<EqBand>,
    },
    /// Alert: band member needs help (broadcast to engineer devices)
    EngineerAlert {
        from_member: String,
        from_name: String,
    },
    /// Alert cleared by engineer or member
    AlertCleared { member_id: String },
    /// Active alerts sent to engineer on WS connect (catch-up)
    ActiveAlerts { alerts: Vec<AlertInfo> },
    /// Talkback lock acquired: bind `/ws/talkback` with this id (X6)
    TalkAcquired { talk_id: String },
    /// Talkback lock denied — another engineer is talking
    TalkBusy { holder: String },
    /// Talkback lock released
    TalkReleased,
    /// Engineer is talking — broadcast to all band members for red overlay
    EngineerTalking { active: bool },
    /// The page mix's limiter (answer to GetLimiterParams)
    LimiterParams {
        mix: String,
        track_name: String,
        /// Limit in dB (−6 to 0)
        limit_db: f32,
        /// The limit as the slider position (0–1)
        limit_norm: f32,
        enabled: bool,
        /// Seconds of gain reduction below −1 dB since the last reset (X14)
        #[serde(default)]
        active_seconds: f64,
    },
    /// Internet access (Cloudflare tunnel) health — sent on connect and on change
    TunnelStatus(TunnelStatusInfo),
    /// The engineer's console (F29)
    Console(ConsoleInfo),
    /// One console input changed
    InputUpdate(ConsoleInput),
    /// Band activity while developing (§4.2): banner and switch on engineer pages
    BandActivity { active: bool, can_switch: bool },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_client(msg: ClientMsg, tag: &str) {
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(&format!("\"cmd\":\"{tag}\"")), "{json}");
        assert_eq!(serde_json::from_str::<ClientMsg>(&json).unwrap(), msg);
    }

    fn round_trip_server(msg: ServerMsg, tag: &str) -> String {
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(&format!("\"event\":\"{tag}\"")), "{json}");
        assert_eq!(serde_json::from_str::<ServerMsg>(&json).unwrap(), msg);
        json
    }

    #[test]
    fn every_client_command_round_trips_with_its_tag() {
        let id = || "mic1".to_string();
        for (msg, tag) in [
            (
                ClientMsg::SetLevel {
                    id: id(),
                    level_db: -6.0,
                },
                "SetLevel",
            ),
            (
                ClientMsg::SetMute {
                    id: id(),
                    muted: true,
                },
                "SetMute",
            ),
            (
                ClientMsg::SetPan {
                    id: id(),
                    pan: 0.25,
                },
                "SetPan",
            ),
            (
                ClientMsg::SetGlobalLevel { level_db: -3.0 },
                "SetGlobalLevel",
            ),
            (ClientMsg::SetGlobalMute { muted: true }, "SetGlobalMute"),
            (ClientMsg::SetStemsLevel { level_db: 1.0 }, "SetStemsLevel"),
            (ClientMsg::SetStemsMute { muted: false }, "SetStemsMute"),
            (
                ClientMsg::UpdateCustomization {
                    pinned: vec![id()],
                    hidden: vec!["keys".into()],
                },
                "UpdateCustomization",
            ),
            (ClientMsg::SetSolo { soloed: vec![id()] }, "SetSolo"),
            (
                ClientMsg::ListenStart {
                    member_id: "member3".into(),
                },
                "ListenStart",
            ),
            (ClientMsg::ListenStop, "ListenStop"),
            (ClientMsg::GetEqParams { target: id() }, "GetEqParams"),
            (
                ClientMsg::SetEqBand {
                    target: id(),
                    band: 2,
                    param: "gain_db".into(),
                    value: 3.0,
                },
                "SetEqBand",
            ),
            (ClientMsg::CallEngineer, "CallEngineer"),
            (ClientMsg::ClearAlert, "ClearAlert"),
            (ClientMsg::TalkStart, "TalkStart"),
            (ClientMsg::TalkStop, "TalkStop"),
            (ClientMsg::GetLimiterParams, "GetLimiterParams"),
            (
                ClientMsg::SetLimiterParam {
                    param: "limit".into(),
                    value: 0.5,
                },
                "SetLimiterParam",
            ),
            (
                ClientMsg::SetLimiterEnabled { enabled: false },
                "SetLimiterEnabled",
            ),
            (ClientMsg::ResetLimiterActivity, "ResetLimiterActivity"),
            (ClientMsg::GetConsole, "GetConsole"),
            (
                ClientMsg::SetInput {
                    input: id(),
                    trim_db: Some(3.0),
                    muted: None,
                    processing: Some(false),
                },
                "SetInput",
            ),
            (
                ClientMsg::ResetLimiterStats {
                    mix: "member1".into(),
                },
                "ResetLimiterStats",
            ),
        ] {
            round_trip_client(msg, tag);
        }
    }

    /// `msg` serialises to exactly `json`, and `json` reads back as `msg`.
    fn assert_shape<T>(msg: &T, json: &str)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        assert_eq!(serde_json::to_string(msg).unwrap(), json);
        assert_eq!(&serde_json::from_str::<T>(json).unwrap(), msg, "{json}");
    }

    /// Every variant name serde reads for `T`, from its refusal of an unknown
    /// `tag` value: the shape tables below must cover each one, so a new
    /// message fails them until it has a literal shape.
    fn variants<T>(tag: &str) -> std::collections::BTreeSet<String>
    where
        T: serde::de::DeserializeOwned + std::fmt::Debug,
    {
        let err = serde_json::from_str::<T>(&format!(r#"{{"{tag}":"NoSuchMessage"}}"#))
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("unknown variant `NoSuchMessage`, expected one of `"),
            "{err}"
        );
        err.split('`')
            .skip(3)
            .step_by(2)
            .map(str::to_owned)
            .collect()
    }

    /// The tag of a literal message (`cmd` or `event`).
    fn tag_of(json: &str, tag: &str) -> String {
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        v[tag].as_str().unwrap().to_owned()
    }

    /// Every command's JSON, field names included. The predecessor asserted
    /// them per message (`level_db`, `member_id`, `pinned`/`hidden`,
    /// `soloed`, …); the page and the E2E specs build these objects by name,
    /// so a serde rename must fail here, not in the browser.
    #[test]
    fn client_json_shapes_are_stable() {
        let cases = [
            (
                ClientMsg::SetLevel {
                    id: "mic1".into(),
                    level_db: -6.0,
                },
                r#"{"cmd":"SetLevel","id":"mic1","level_db":-6.0}"#,
            ),
            (
                ClientMsg::SetMute {
                    id: "mic3".into(),
                    muted: true,
                },
                r#"{"cmd":"SetMute","id":"mic3","muted":true}"#,
            ),
            (
                ClientMsg::SetPan {
                    id: "mic2".into(),
                    pan: 0.75,
                },
                r#"{"cmd":"SetPan","id":"mic2","pan":0.75}"#,
            ),
            (
                ClientMsg::SetGlobalLevel { level_db: -6.0 },
                r#"{"cmd":"SetGlobalLevel","level_db":-6.0}"#,
            ),
            (
                ClientMsg::SetGlobalMute { muted: true },
                r#"{"cmd":"SetGlobalMute","muted":true}"#,
            ),
            (
                ClientMsg::SetStemsLevel { level_db: -3.0 },
                r#"{"cmd":"SetStemsLevel","level_db":-3.0}"#,
            ),
            (
                ClientMsg::SetStemsMute { muted: false },
                r#"{"cmd":"SetStemsMute","muted":false}"#,
            ),
            (
                ClientMsg::UpdateCustomization {
                    pinned: vec!["mic1".into(), "mic5".into()],
                    hidden: vec!["click".into(), "keys".into()],
                },
                r#"{"cmd":"UpdateCustomization","pinned":["mic1","mic5"],"hidden":["click","keys"]}"#,
            ),
            (
                ClientMsg::SetSolo {
                    soloed: vec!["mic1".into(), "mic5".into()],
                },
                r#"{"cmd":"SetSolo","soloed":["mic1","mic5"]}"#,
            ),
            (
                ClientMsg::ListenStart {
                    member_id: "member3".into(),
                },
                r#"{"cmd":"ListenStart","member_id":"member3"}"#,
            ),
            // The engineer listens to their own mix by its id.
            (
                ClientMsg::ListenStart {
                    member_id: "engineer".into(),
                },
                r#"{"cmd":"ListenStart","member_id":"engineer"}"#,
            ),
            (ClientMsg::ListenStop, r#"{"cmd":"ListenStop"}"#),
            (
                ClientMsg::GetEqParams {
                    target: "mic3".into(),
                },
                r#"{"cmd":"GetEqParams","target":"mic3"}"#,
            ),
            (
                ClientMsg::SetEqBand {
                    target: "mic3".into(),
                    band: 2,
                    param: "gain_db".into(),
                    value: 3.5,
                },
                r#"{"cmd":"SetEqBand","target":"mic3","band":2,"param":"gain_db","value":3.5}"#,
            ),
            (ClientMsg::CallEngineer, r#"{"cmd":"CallEngineer"}"#),
            (ClientMsg::ClearAlert, r#"{"cmd":"ClearAlert"}"#),
            (ClientMsg::TalkStart, r#"{"cmd":"TalkStart"}"#),
            (ClientMsg::TalkStop, r#"{"cmd":"TalkStop"}"#),
            (ClientMsg::GetLimiterParams, r#"{"cmd":"GetLimiterParams"}"#),
            (
                ClientMsg::SetLimiterParam {
                    param: "limit".into(),
                    value: 0.6,
                },
                r#"{"cmd":"SetLimiterParam","param":"limit","value":0.6}"#,
            ),
            (
                ClientMsg::SetLimiterEnabled { enabled: false },
                r#"{"cmd":"SetLimiterEnabled","enabled":false}"#,
            ),
            (
                ClientMsg::ResetLimiterActivity,
                r#"{"cmd":"ResetLimiterActivity"}"#,
            ),
            (ClientMsg::GetConsole, r#"{"cmd":"GetConsole"}"#),
            (
                ClientMsg::SetInput {
                    input: "keys".into(),
                    trim_db: Some(3.0),
                    muted: None,
                    processing: Some(false),
                },
                r#"{"cmd":"SetInput","input":"keys","trim_db":3.0,"muted":null,"processing":false}"#,
            ),
            (
                ClientMsg::ResetLimiterStats {
                    mix: "member1".into(),
                },
                r#"{"cmd":"ResetLimiterStats","mix":"member1"}"#,
            ),
        ];
        let mut tags = std::collections::BTreeSet::new();
        for (msg, json) in &cases {
            assert_shape(msg, json);
            tags.insert(tag_of(json, "cmd"));
        }
        let all = variants::<ClientMsg>("cmd");
        assert_eq!(tags, all, "a literal shape for every command");
        let set_input: ClientMsg =
            serde_json::from_str(r#"{"cmd":"SetInput","input":"keys","muted":true}"#).unwrap();
        assert_eq!(
            set_input,
            ClientMsg::SetInput {
                input: "keys".into(),
                trim_db: None,
                muted: Some(true),
                processing: None
            }
        );
        // The REAPER-era shape is refused.
        assert!(
            serde_json::from_str::<ClientMsg>(r#"{"cmd":"SetLevel","track_index":1,"level_db":0}"#)
                .is_err()
        );
    }

    #[test]
    fn every_server_event_round_trips_with_its_tag() {
        let hello = round_trip_server(
            ServerMsg::Hello {
                proto: UI_PROTO,
                build: "2.0.0-dev.7".into(),
                min_client_proto: MIN_CLIENT_PROTO,
            },
            "Hello",
        );
        assert_eq!(
            hello,
            r#"{"event":"Hello","data":{"proto":2,"build":"2.0.0-dev.7","min_client_proto":2}}"#
        );
        round_trip_server(
            ServerMsg::Meters {
                meters: HashMap::from([("mic1".to_string(), [0.5, 0.25])]),
            },
            "Meters",
        );
        round_trip_server(
            ServerMsg::ChannelUpdate {
                id: "member2".into(),
                level_db: -60.0,
                muted: true,
                pan: 0.5,
            },
            "ChannelUpdate",
        );
        round_trip_server(
            ServerMsg::GlobalVolumeUpdate {
                level_db: 0.0,
                muted: false,
            },
            "GlobalVolumeUpdate",
        );
        round_trip_server(
            ServerMsg::StemsVolumeUpdate {
                level_db: -3.0,
                muted: true,
            },
            "StemsVolumeUpdate",
        );
        round_trip_server(
            ServerMsg::ConnectionChanged { connected: false },
            "ConnectionChanged",
        );
        round_trip_server(
            ServerMsg::CustomizationUpdate {
                pinned: vec!["mic1".into()],
                hidden: vec![],
            },
            "CustomizationUpdate",
        );
        round_trip_server(
            ServerMsg::NetworkMode {
                mode: "local".into(),
            },
            "NetworkMode",
        );
        round_trip_server(ServerMsg::SoloUpdate { soloed: vec![] }, "SoloUpdate");
        let status = round_trip_server(
            ServerMsg::AudioStatus {
                status: "no_source".into(),
                target: None,
            },
            "AudioStatus",
        );
        assert!(!status.contains("target"));
        // A listen stream names whose mix it plays (reaperiem
        // `test_server_msg_audio_status_with_target`).
        let listening = round_trip_server(
            ServerMsg::AudioStatus {
                status: "listening".into(),
                target: Some("member3".into()),
            },
            "AudioStatus",
        );
        assert!(
            listening.contains(r#""status":"listening""#)
                && listening.contains(r#""target":"member3""#),
            "{listening}"
        );
        round_trip_server(
            ServerMsg::EqParams {
                target: "mic3".into(),
                track_name: "MEMBER3 mic".into(),
                bands: vec![EqBand {
                    band_type: "lowshelf".into(),
                    freq_hz: 287.5,
                    gain_db: -2.7,
                    bw: 1.18,
                    enabled: true,
                }],
            },
            "EqParams",
        );
        round_trip_server(
            ServerMsg::EngineerAlert {
                from_member: "member1".into(),
                from_name: "Member1".into(),
            },
            "EngineerAlert",
        );
        round_trip_server(
            ServerMsg::AlertCleared {
                member_id: "member1".into(),
            },
            "AlertCleared",
        );
        round_trip_server(
            ServerMsg::ActiveAlerts {
                alerts: vec![AlertInfo {
                    from_member: "member1".into(),
                    from_name: "Member1".into(),
                }],
            },
            "ActiveAlerts",
        );
        round_trip_server(
            ServerMsg::TalkAcquired {
                talk_id: "ab".repeat(16),
            },
            "TalkAcquired",
        );
        round_trip_server(
            ServerMsg::TalkBusy {
                holder: "engineer".into(),
            },
            "TalkBusy",
        );
        round_trip_server(ServerMsg::TalkReleased, "TalkReleased");
        round_trip_server(
            ServerMsg::EngineerTalking { active: true },
            "EngineerTalking",
        );
        round_trip_server(
            ServerMsg::LimiterParams {
                mix: "member1".into(),
                track_name: "IEM VOL".into(),
                limit_db: -6.0,
                limit_norm: 0.0,
                enabled: true,
                active_seconds: 83.5,
            },
            "LimiterParams",
        );
        round_trip_server(
            ServerMsg::TunnelStatus(TunnelStatusInfo {
                state: crate::TunnelState::Down,
                ready_connections: 0,
                since_secs: 42,
                last_restart_secs_ago: None,
                last_restart_ok: None,
            }),
            "TunnelStatus",
        );
        round_trip_server(
            ServerMsg::Console(ConsoleInfo {
                inputs: vec![ConsoleInput {
                    id: "mic1".into(),
                    name: "MEMBER1 mic".into(),
                    trim_db: 0.0,
                    muted: false,
                    processing: true,
                }],
                limiters: vec![ConsoleMix {
                    id: "member1".into(),
                    name: "Member1".into(),
                    active_seconds: 1.5,
                }],
                pages: vec![PageLink {
                    id: "translator".into(),
                    name: "Translator".into(),
                }],
                login: LoginFailures {
                    lan: 1,
                    tunnel: 2,
                    engineer_budget_trips: 0,
                },
            }),
            "Console",
        );
        round_trip_server(
            ServerMsg::InputUpdate(ConsoleInput {
                id: "keys".into(),
                name: "KEYS".into(),
                trim_db: -3.0,
                muted: true,
                processing: false,
            }),
            "InputUpdate",
        );
        round_trip_server(
            ServerMsg::BandActivity {
                active: true,
                can_switch: true,
            },
            "BandActivity",
        );
    }

    /// Every event's JSON, field names included (W4 of the gen1 test parity
    /// report, iemmixer#25): the predecessor asserted `member_id`, `soloed`,
    /// `level_db`, `from_member`, `holder`, `mode`, `active_seconds`,
    /// `global_level_db` and the meters' `[L,R]` pairs per message; a round
    /// trip alone passes a serde rename that the page and the E2E specs, which
    /// read these objects by name, would not survive.
    #[test]
    fn server_json_shapes_are_stable() {
        let cases = [
            (
                ServerMsg::Hello {
                    proto: UI_PROTO,
                    build: "2.0.0-dev.7".into(),
                    min_client_proto: MIN_CLIENT_PROTO,
                },
                r#"{"event":"Hello","data":{"proto":2,"build":"2.0.0-dev.7","min_client_proto":2}}"#,
            ),
            (
                ServerMsg::State {
                    channels: vec![Channel {
                        id: "mic1".into(),
                        name: "MEMBER1 mic".into(),
                        level_db: -6.0,
                        pan: 0.5,
                        muted: false,
                        category: "mics".into(),
                        eq: true,
                        own: true,
                    }],
                    connected: true,
                    global_level_db: Some(-3.5),
                    global_muted: Some(false),
                    mix: Some("member1".into()),
                    stems_level_db: Some(-6.0),
                    stems_muted: Some(true),
                    group: Some("stems".into()),
                },
                concat!(
                    r#"{"event":"State","data":{"channels":[{"id":"mic1","name":"MEMBER1 mic","#,
                    r#""level_db":-6.0,"pan":0.5,"muted":false,"category":"mics","eq":true,"#,
                    r#""own":true}],"connected":true,"global_level_db":-3.5,"global_muted":false,"#,
                    r#""mix":"member1","stems_level_db":-6.0,"stems_muted":true,"group":"stems"}}"#,
                ),
            ),
            // Meters are linear [left, right] pairs by id.
            (
                ServerMsg::Meters {
                    meters: HashMap::from([("mic1".to_string(), [0.5, 0.3])]),
                },
                r#"{"event":"Meters","data":{"meters":{"mic1":[0.5,0.3]}}}"#,
            ),
            (
                ServerMsg::ChannelUpdate {
                    id: "member2".into(),
                    level_db: -12.0,
                    muted: true,
                    pan: 0.3,
                },
                r#"{"event":"ChannelUpdate","data":{"id":"member2","level_db":-12.0,"muted":true,"pan":0.3}}"#,
            ),
            (
                ServerMsg::GlobalVolumeUpdate {
                    level_db: -12.0,
                    muted: false,
                },
                r#"{"event":"GlobalVolumeUpdate","data":{"level_db":-12.0,"muted":false}}"#,
            ),
            (
                ServerMsg::StemsVolumeUpdate {
                    level_db: -6.0,
                    muted: true,
                },
                r#"{"event":"StemsVolumeUpdate","data":{"level_db":-6.0,"muted":true}}"#,
            ),
            (
                ServerMsg::ConnectionChanged { connected: false },
                r#"{"event":"ConnectionChanged","data":{"connected":false}}"#,
            ),
            (
                ServerMsg::CustomizationUpdate {
                    pinned: vec!["mic1".into(), "keys".into()],
                    hidden: vec!["click".into()],
                },
                r#"{"event":"CustomizationUpdate","data":{"pinned":["mic1","keys"],"hidden":["click"]}}"#,
            ),
            (
                ServerMsg::NetworkMode {
                    mode: "local".into(),
                },
                r#"{"event":"NetworkMode","data":{"mode":"local"}}"#,
            ),
            (
                ServerMsg::SoloUpdate {
                    soloed: vec!["mic1".into(), "mic5".into()],
                },
                r#"{"event":"SoloUpdate","data":{"soloed":["mic1","mic5"]}}"#,
            ),
            (
                ServerMsg::SoloUpdate { soloed: vec![] },
                r#"{"event":"SoloUpdate","data":{"soloed":[]}}"#,
            ),
            // Without a target the field is absent, and such a status reads.
            (
                ServerMsg::AudioStatus {
                    status: "no_source".into(),
                    target: None,
                },
                r#"{"event":"AudioStatus","data":{"status":"no_source"}}"#,
            ),
            (
                ServerMsg::AudioStatus {
                    status: "listening".into(),
                    target: Some("member3".into()),
                },
                r#"{"event":"AudioStatus","data":{"status":"listening","target":"member3"}}"#,
            ),
            (
                ServerMsg::EqParams {
                    target: "mic3".into(),
                    track_name: "MEMBER3 mic".into(),
                    bands: vec![EqBand {
                        band_type: "lowshelf".into(),
                        freq_hz: 287.5,
                        gain_db: -2.7,
                        bw: 1.18,
                        enabled: true,
                    }],
                },
                concat!(
                    r#"{"event":"EqParams","data":{"target":"mic3","track_name":"MEMBER3 mic","#,
                    r#""bands":[{"band_type":"lowshelf","freq_hz":287.5,"gain_db":-2.7,"#,
                    r#""bw":1.18,"enabled":true}]}}"#,
                ),
            ),
            (
                ServerMsg::EngineerAlert {
                    from_member: "member1".into(),
                    from_name: "Member1".into(),
                },
                r#"{"event":"EngineerAlert","data":{"from_member":"member1","from_name":"Member1"}}"#,
            ),
            (
                ServerMsg::AlertCleared {
                    member_id: "member1".into(),
                },
                r#"{"event":"AlertCleared","data":{"member_id":"member1"}}"#,
            ),
            (
                ServerMsg::ActiveAlerts {
                    alerts: vec![AlertInfo {
                        from_member: "member1".into(),
                        from_name: "Member1".into(),
                    }],
                },
                r#"{"event":"ActiveAlerts","data":{"alerts":[{"from_member":"member1","from_name":"Member1"}]}}"#,
            ),
            (
                ServerMsg::TalkAcquired {
                    talk_id: "talk-1".into(),
                },
                r#"{"event":"TalkAcquired","data":{"talk_id":"talk-1"}}"#,
            ),
            (
                ServerMsg::TalkBusy {
                    holder: "engineer".into(),
                },
                r#"{"event":"TalkBusy","data":{"holder":"engineer"}}"#,
            ),
            (ServerMsg::TalkReleased, r#"{"event":"TalkReleased"}"#),
            (
                ServerMsg::EngineerTalking { active: true },
                r#"{"event":"EngineerTalking","data":{"active":true}}"#,
            ),
            (
                ServerMsg::LimiterParams {
                    mix: "member1".into(),
                    track_name: "IEM VOL".into(),
                    limit_db: -6.0,
                    limit_norm: 0.0,
                    enabled: true,
                    active_seconds: 83.5,
                },
                concat!(
                    r#"{"event":"LimiterParams","data":{"mix":"member1","track_name":"IEM VOL","#,
                    r#""limit_db":-6.0,"limit_norm":0.0,"enabled":true,"active_seconds":83.5}}"#,
                ),
            ),
            (
                ServerMsg::TunnelStatus(TunnelStatusInfo {
                    state: crate::TunnelState::Down,
                    ready_connections: 0,
                    since_secs: 42,
                    last_restart_secs_ago: None,
                    last_restart_ok: None,
                }),
                concat!(
                    r#"{"event":"TunnelStatus","data":{"state":"Down","ready_connections":0,"#,
                    r#""since_secs":42,"last_restart_secs_ago":null,"last_restart_ok":null}}"#,
                ),
            ),
            (
                ServerMsg::TunnelStatus(TunnelStatusInfo {
                    state: crate::TunnelState::Restarting,
                    ready_connections: 2,
                    since_secs: 30,
                    last_restart_secs_ago: Some(7),
                    last_restart_ok: Some(false),
                }),
                concat!(
                    r#"{"event":"TunnelStatus","data":{"state":"Restarting","ready_connections":2,"#,
                    r#""since_secs":30,"last_restart_secs_ago":7,"last_restart_ok":false}}"#,
                ),
            ),
            (
                ServerMsg::Console(ConsoleInfo {
                    inputs: vec![ConsoleInput {
                        id: "mic1".into(),
                        name: "MEMBER1 mic".into(),
                        trim_db: 0.0,
                        muted: false,
                        processing: true,
                    }],
                    limiters: vec![ConsoleMix {
                        id: "member1".into(),
                        name: "Member1".into(),
                        active_seconds: 1.5,
                    }],
                    pages: vec![PageLink {
                        id: "translator".into(),
                        name: "Translator".into(),
                    }],
                    login: LoginFailures {
                        lan: 1,
                        tunnel: 2,
                        engineer_budget_trips: 3,
                    },
                }),
                concat!(
                    r#"{"event":"Console","data":{"inputs":[{"id":"mic1","name":"MEMBER1 mic","#,
                    r#""trim_db":0.0,"muted":false,"processing":true}],"limiters":[{"id":"member1","#,
                    r#""name":"Member1","active_seconds":1.5}],"pages":[{"id":"translator","#,
                    r#""name":"Translator"}],"login":{"lan":1,"tunnel":2,"engineer_budget_trips":3}}}"#,
                ),
            ),
            (
                ServerMsg::InputUpdate(ConsoleInput {
                    id: "keys".into(),
                    name: "KEYS".into(),
                    trim_db: -3.0,
                    muted: true,
                    processing: false,
                }),
                concat!(
                    r#"{"event":"InputUpdate","data":{"id":"keys","name":"KEYS","trim_db":-3.0,"#,
                    r#""muted":true,"processing":false}}"#,
                ),
            ),
            (
                ServerMsg::BandActivity {
                    active: true,
                    can_switch: false,
                },
                r#"{"event":"BandActivity","data":{"active":true,"can_switch":false}}"#,
            ),
        ];
        let mut tags = std::collections::BTreeSet::new();
        for (msg, json) in &cases {
            assert_shape(msg, json);
            tags.insert(tag_of(json, "event"));
        }
        let all = variants::<ServerMsg>("event");
        assert_eq!(tags, all, "a literal shape for every event");
        // Several meters read as one [L,R] pair per id (object order is free).
        let two = r#"{"event":"Meters","data":{"meters":{"mic1":[0.5,0.3],"member2":[0.0,1.0]}}}"#;
        assert_eq!(
            serde_json::from_str::<ServerMsg>(two).unwrap(),
            ServerMsg::Meters {
                meters: HashMap::from([
                    ("mic1".to_string(), [0.5, 0.3]),
                    ("member2".to_string(), [0.0, 1.0]),
                ]),
            }
        );
    }

    /// A `LimiterParams` without `active_seconds` (an older server) still
    /// reads, as 0 s (reaperiem `test_server_msg_limiter_params_active_seconds_default`).
    #[test]
    fn limiter_params_without_active_seconds_read_as_zero() {
        let json = r#"{"event":"LimiterParams","data":{"mix":"member1","track_name":"IEM VOL","limit_db":-6.0,"limit_norm":0.0,"enabled":true}}"#;
        assert_eq!(
            serde_json::from_str::<ServerMsg>(json).unwrap(),
            ServerMsg::LimiterParams {
                mix: "member1".into(),
                track_name: "IEM VOL".into(),
                limit_db: -6.0,
                limit_norm: 0.0,
                enabled: true,
                active_seconds: 0.0,
            }
        );
    }

    #[test]
    fn state_omits_absent_fields_and_reads_them_back_as_none() {
        let msg = ServerMsg::State {
            channels: vec![],
            connected: true,
            global_level_db: None,
            global_muted: None,
            mix: None,
            stems_level_db: None,
            stems_muted: None,
            group: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"event":"State","data":{"channels":[],"connected":true}}"#
        );
        assert_eq!(serde_json::from_str::<ServerMsg>(&json).unwrap(), msg);
        let full = ServerMsg::State {
            channels: vec![],
            connected: false,
            global_level_db: Some(-3.5),
            global_muted: Some(true),
            mix: Some("member1".into()),
            stems_level_db: Some(0.0),
            stems_muted: Some(false),
            group: Some("stems".into()),
        };
        let json = serde_json::to_string(&full).unwrap();
        assert!(json.contains(r#""mix":"member1""#) && json.contains(r#""group":"stems""#));
        assert_eq!(serde_json::from_str::<ServerMsg>(&json).unwrap(), full);
    }

    #[test]
    fn eq_bands_default_to_enabled() {
        let band: EqBand =
            serde_json::from_str(r#"{"band_type":"band","freq_hz":1000.0,"gain_db":0.0,"bw":1.0}"#)
                .unwrap();
        assert!(band.enabled);
        assert!(default_enabled());
    }
}
