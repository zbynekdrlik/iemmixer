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

    #[test]
    fn client_json_shapes_are_stable() {
        assert_eq!(
            serde_json::to_string(&ClientMsg::SetLevel {
                id: "mic1".into(),
                level_db: -6.0
            })
            .unwrap(),
            r#"{"cmd":"SetLevel","id":"mic1","level_db":-6.0}"#
        );
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
