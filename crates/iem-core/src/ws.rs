//! WebSocket message types for real-time mixer communication
//! Includes stems group bus control (v1.91.0)
//! Includes shared EQ control (v1.102.0)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::Channel;
use crate::tunnel::TunnelStatusInfo;

/// EQ band data for parametric EQ (ReaEQ)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EqBand {
    /// Band type name: "band", "lowshelf", "highshelf", "highpass", "lowpass", "notch"
    pub band_type: String,
    /// Center/corner frequency in Hz (20-20000)
    pub freq_hz: f32,
    /// Gain in dB (ReaEQ range: -inf to +12, 0 = flat, norm 0.25 = 0dB)
    pub gain_db: f32,
    /// Bandwidth/Q factor in octaves (0.1-4.0)
    pub bw: f32,
    /// Normalized frequency value for ReaEQ (0-1)
    pub freq_norm: f32,
    /// Normalized gain value for ReaEQ (0-1, 0.25 = 0dB)
    pub gain_norm: f32,
    /// Normalized bandwidth value for ReaEQ (0-1)
    pub bw_norm: f32,
    /// Minimum dB this band's gain can produce (norm=0.0 endpoint, REAPER-sampled)
    #[serde(default = "default_gain_db_min")]
    pub gain_db_min: f32,
    /// Maximum dB this band's gain can produce (norm=1.0 endpoint, REAPER-sampled)
    #[serde(default = "default_gain_db_max")]
    pub gain_db_max: f32,
    /// Whether this band is enabled in ReaEQ (BANDENABLED config param)
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

fn default_gain_db_min() -> f32 {
    -12.0
}

fn default_gain_db_max() -> f32 {
    12.0
}

/// Alert info for ActiveAlerts catch-up message
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertInfo {
    pub from_member: String,
    pub from_name: String,
}

/// Client → Server commands (sent via WebSocket)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "cmd")]
pub enum ClientMsg {
    /// Set send level for a track
    SetLevel { track_index: usize, level_db: f32 },
    /// Set send mute for a track
    SetMute { track_index: usize, muted: bool },
    /// Set send pan for a track
    SetPan { track_index: usize, pan: f32 },
    /// Set global IEM output volume for this member
    SetGlobalLevel { level_db: f32 },
    /// Set global IEM output mute for this member
    SetGlobalMute { muted: bool },
    /// Set stems group bus volume for this member
    SetStemsLevel { level_db: f32 },
    /// Set stems group bus mute for this member
    SetStemsMute { muted: bool },
    /// Update channel customization (pin/hide preferences)
    UpdateCustomization {
        pinned: Vec<usize>,
        hidden: Vec<usize>,
    },
    /// Set solo state for this member (full replacement, syncs across devices)
    SetSolo { soloed: Vec<usize> },
    /// Start listening to audio stream (engineer only)
    /// member_id specifies whose mix to listen to (e.g., "oldmember1", "engineer")
    ListenStart { member_id: String },
    /// Stop listening to audio stream
    ListenStop,
    /// Request EQ parameters for a track (loads from REAPER on-demand)
    GetEqParams { track_index: usize },
    /// Set a single EQ band parameter (normalized values for ReaEQ)
    SetEqBand {
        track_index: usize,
        band: u8,
        param: String,
        value: f32,
    },
    /// Request EQ parameters for multiple tracks (used during preset save)
    GetEqParamsMulti { track_indices: Vec<usize> },
    /// Band member calls engineer for help (reaperiem#125)
    CallEngineer,
    /// Clear active alert (sent by engineer or member to dismiss)
    ClearAlert,
    /// Engineer requests talkback lock (reaperiem#123)
    TalkStart,
    /// Engineer releases talkback lock (reaperiem#123)
    TalkStop,
    /// Request limiter parameters for a track (loads from REAPER on-demand) (reaperiem#72)
    GetLimiterParams { track_index: usize },
    /// Set limiter max level (normalized 0-1, maps to -6 to 0 dB) (reaperiem#72)
    SetLimiterParam {
        track_index: usize,
        param: String,
        value: f32,
    },
    /// Enable/disable limiter (FX bypass toggle) (reaperiem#72)
    SetLimiterEnabled { track_index: usize, enabled: bool },
    /// Reset the activity counter for one limiter track (reaperiem#145).
    /// Server zeros AppState.limiter_activity[track] AND writes
    /// EXTSTATE REAPERIEM_LIMITER_ACTIVITY/reset = "<track_index>"
    /// so meter_bridge.lua zeros its local accumulator (otherwise the
    /// next poller cycle would re-overwrite the server's zero).
    ResetLimiterActivity { track_index: usize },
}

/// Server → Client events (pushed via WebSocket)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event", content = "data")]
pub enum ServerMsg {
    /// Full mixer state (sent on connect and periodically)
    State {
        channels: Vec<Channel>,
        connected: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        global_level_db: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        global_muted: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_track_index: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stems_level_db: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stems_muted: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stems_bus_index: Option<usize>,
    },
    /// Meter levels (sent every ~150ms) — stereo [left, right] peaks
    Meters { meters: HashMap<usize, [f32; 2]> },
    /// Single channel changed (delta update)
    ChannelUpdate {
        track_index: usize,
        level_db: f32,
        muted: bool,
        pan: f32,
    },
    /// Global IEM output volume changed
    GlobalVolumeUpdate { level_db: f32, muted: bool },
    /// Stems group bus volume changed
    StemsVolumeUpdate { level_db: f32, muted: bool },
    /// REAPER connection status changed
    ConnectionChanged { connected: bool },
    /// Channel customization update (sent on connect and after changes)
    CustomizationUpdate {
        pinned: Vec<usize>,
        hidden: Vec<usize>,
    },
    /// Network mode indicator (local LAN vs remote internet)
    NetworkMode { mode: String },
    /// Solo state update (sent on connect and after changes)
    SoloUpdate { soloed: Vec<usize> },
    /// Audio streaming status update
    AudioStatus {
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
    /// EQ parameters for a track (sent on-demand in response to GetEqParams)
    EqParams {
        track_index: usize,
        track_name: String,
        bands: Vec<EqBand>,
    },
    /// EQ parameters for multiple tracks (response to GetEqParamsMulti)
    EqParamsMulti { bands: HashMap<usize, Vec<EqBand>> },
    /// Alert: band member needs help (broadcast to engineer devices) (reaperiem#125)
    EngineerAlert {
        from_member: String,
        from_name: String,
    },
    /// Alert cleared by engineer or member
    AlertCleared { member_id: String },
    /// Active alerts sent to engineer on WS connect (catch-up)
    ActiveAlerts { alerts: Vec<AlertInfo> },
    /// Talkback lock acquired — engineer may start sending audio (reaperiem#123)
    TalkAcquired,
    /// Talkback lock denied — another engineer is talking (reaperiem#123)
    TalkBusy { holder: String },
    /// Talkback lock released (reaperiem#123)
    TalkReleased,
    /// Engineer is talking — broadcast to all band members for red overlay (reaperiem#123)
    EngineerTalking { active: bool },
    /// Limiter parameters for a track — single "limit" control (reaperiem#72)
    LimiterParams {
        track_index: usize,
        track_name: String,
        /// Max output level in dB (-6 to 0)
        limit_db: f32,
        /// Normalized slider position (0-1)
        limit_norm: f32,
        enabled: bool,
        /// Cumulative seconds the limiter has been actively reducing gain
        /// (GR < -1 dB) since the last reset or app restart (reaperiem#145).
        /// Default 0.0 for backwards compatibility with older servers.
        #[serde(default)]
        active_seconds: f64,
    },
    /// Internet access (Cloudflare tunnel) health — sent on connect and
    /// broadcast to every client whenever it changes (reaperiem#202). Wire format:
    /// `{"event":"TunnelStatus","data":{"state":"Ok","ready_connections":4,...}}`.
    TunnelStatus(TunnelStatusInfo),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_msg_set_level_serialization() {
        let msg = ClientMsg::SetLevel {
            track_index: 1,
            level_db: -6.0,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"SetLevel\""));
        assert!(json.contains("\"track_index\":1"));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_set_mute_serialization() {
        let msg = ClientMsg::SetMute {
            track_index: 3,
            muted: true,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"SetMute\""));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_set_pan_serialization() {
        let msg = ClientMsg::SetPan {
            track_index: 2,
            pan: 0.75,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"SetPan\""));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_state_serialization() {
        let msg = ServerMsg::State {
            channels: vec![Channel {
                track_index: 1,
                name: "MEMBER3 mic".to_string(),
                level_db: 0.0,
                pan: 0.5,
                muted: false,
                category: "mics".to_string(),
                stereo_pair: None,
                stereo_side: None,
            }],
            connected: true,
            global_level_db: None,
            global_muted: None,
            output_track_index: None,
            stems_level_db: None,
            stems_muted: None,
            stems_bus_index: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"State\""));
        // Optional fields should NOT appear when None
        assert!(!json.contains("global_level_db"));
        assert!(!json.contains("global_muted"));
        assert!(!json.contains("output_track_index"));
        assert!(!json.contains("stems_level_db"));
        assert!(!json.contains("stems_muted"));
        assert!(!json.contains("stems_bus_index"));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_state_with_global_volume() {
        let msg = ServerMsg::State {
            channels: vec![],
            connected: true,
            global_level_db: Some(-3.5),
            global_muted: Some(false),
            output_track_index: Some(23),
            stems_level_db: None,
            stems_muted: None,
            stems_bus_index: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"global_level_db\":-3.5"));
        assert!(json.contains("\"global_muted\":false"));
        assert!(json.contains("\"output_track_index\":23"));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_state_backwards_compat() {
        // Old State messages without global/output fields should still deserialize
        let json = r#"{"event":"State","data":{"channels":[],"connected":true}}"#;
        let decoded: ServerMsg = serde_json::from_str(json).unwrap();
        match decoded {
            ServerMsg::State {
                global_level_db,
                global_muted,
                output_track_index,
                stems_level_db,
                stems_muted,
                stems_bus_index,
                ..
            } => {
                assert_eq!(global_level_db, None);
                assert_eq!(global_muted, None);
                assert_eq!(output_track_index, None);
                assert_eq!(stems_level_db, None);
                assert_eq!(stems_muted, None);
                assert_eq!(stems_bus_index, None);
            }
            _ => panic!("Expected State variant"),
        }
    }

    #[test]
    fn test_server_msg_meters_serialization() {
        let mut meters = HashMap::new();
        meters.insert(1, [0.5, 0.4]);
        meters.insert(2, [0.3, 0.25]);
        let msg = ServerMsg::Meters { meters };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"Meters\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_meters_stereo_wire_format() {
        let mut meters = HashMap::new();
        meters.insert(1, [0.5, 0.3]);
        let msg = ServerMsg::Meters { meters };
        let json = serde_json::to_string(&msg).unwrap();
        // Verify stereo pair format: key maps to [left, right] array
        assert!(
            json.contains("[0.5,0.3]"),
            "Stereo meters should serialize as [L,R] arrays, got: {}",
            json
        );
    }

    #[test]
    fn test_server_msg_channel_update_serialization() {
        let msg = ServerMsg::ChannelUpdate {
            track_index: 5,
            level_db: -12.0,
            muted: true,
            pan: 0.3,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"ChannelUpdate\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_connection_changed_serialization() {
        let msg = ServerMsg::ConnectionChanged { connected: false };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"ConnectionChanged\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_tunnel_status_serialization() {
        let msg = ServerMsg::TunnelStatus(TunnelStatusInfo {
            state: crate::TunnelState::Down,
            ready_connections: 0,
            since_secs: 42,
            last_restart_secs_ago: None,
            last_restart_ok: None,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"TunnelStatus\""));
        assert!(json.contains("\"state\":\"Down\""));
        assert!(json.contains("\"since_secs\":42"));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_set_global_level_serialization() {
        let msg = ClientMsg::SetGlobalLevel { level_db: -6.0 };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"SetGlobalLevel\""));
        assert!(json.contains("\"level_db\":-6.0"));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_set_global_mute_serialization() {
        let msg = ClientMsg::SetGlobalMute { muted: true };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"SetGlobalMute\""));
        assert!(json.contains("\"muted\":true"));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_global_volume_update_serialization() {
        let msg = ServerMsg::GlobalVolumeUpdate {
            level_db: -12.0,
            muted: false,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"GlobalVolumeUpdate\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_update_customization_serialization() {
        let msg = ClientMsg::UpdateCustomization {
            pinned: vec![1, 5],
            hidden: vec![3, 7],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"UpdateCustomization\""));
        assert!(json.contains("\"pinned\":[1,5]"));
        assert!(json.contains("\"hidden\":[3,7]"));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_customization_update_serialization() {
        let msg = ServerMsg::CustomizationUpdate {
            pinned: vec![1, 5, 8],
            hidden: vec![3],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"CustomizationUpdate\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_listen_start_serialization() {
        let msg = ClientMsg::ListenStart {
            member_id: "oldmember1".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"ListenStart\""));
        assert!(json.contains("\"member_id\":\"oldmember1\""));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_listen_start_engineer_own_mix() {
        let msg = ClientMsg::ListenStart {
            member_id: "engineer".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"member_id\":\"engineer\""));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_listen_stop_serialization() {
        let msg = ClientMsg::ListenStop;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"ListenStop\""));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_audio_status_serialization() {
        let msg = ServerMsg::AudioStatus {
            status: "no_source".to_string(),
            target: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"AudioStatus\""));
        assert!(json.contains("\"status\":\"no_source\""));
        // target should be omitted when None
        assert!(!json.contains("target"));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_audio_status_with_target() {
        let msg = ServerMsg::AudioStatus {
            status: "listening".to_string(),
            target: Some("oldmember1".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"status\":\"listening\""));
        assert!(json.contains("\"target\":\"oldmember1\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_audio_status_backwards_compat() {
        // Old AudioStatus without target field should still deserialize
        let json = r#"{"event":"AudioStatus","data":{"status":"stopped"}}"#;
        let decoded: ServerMsg = serde_json::from_str(json).unwrap();
        match decoded {
            ServerMsg::AudioStatus { status, target } => {
                assert_eq!(status, "stopped");
                assert_eq!(target, None);
            }
            _ => panic!("Expected AudioStatus variant"),
        }
    }

    #[test]
    fn test_server_msg_network_mode_serialization() {
        let msg = ServerMsg::NetworkMode {
            mode: "local".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"NetworkMode\""));
        assert!(json.contains("\"mode\":\"local\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_set_solo_serialization() {
        let msg = ClientMsg::SetSolo { soloed: vec![1, 5] };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"SetSolo\""));
        assert!(json.contains("\"soloed\":[1,5]"));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_solo_update_serialization() {
        let msg = ServerMsg::SoloUpdate { soloed: vec![1, 5] };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"SoloUpdate\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_set_stems_level_serialization() {
        let msg = ClientMsg::SetStemsLevel { level_db: -3.0 };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"SetStemsLevel\""));
        assert!(json.contains("\"level_db\":-3.0"));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_set_stems_mute_serialization() {
        let msg = ClientMsg::SetStemsMute { muted: true };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"SetStemsMute\""));
        assert!(json.contains("\"muted\":true"));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_stems_volume_update_serialization() {
        let msg = ServerMsg::StemsVolumeUpdate {
            level_db: -6.0,
            muted: false,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"StemsVolumeUpdate\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_state_with_stems_fields() {
        let msg = ServerMsg::State {
            channels: vec![],
            connected: true,
            global_level_db: Some(-3.5),
            global_muted: Some(false),
            output_track_index: Some(23),
            stems_level_db: Some(-6.0),
            stems_muted: Some(false),
            stems_bus_index: Some(33),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"stems_level_db\":-6.0"));
        assert!(json.contains("\"stems_muted\":false"));
        assert!(json.contains("\"stems_bus_index\":33"));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_state_stems_backwards_compat() {
        // Old State messages without stems fields should still deserialize
        let json =
            r#"{"event":"State","data":{"channels":[],"connected":true,"global_level_db":-3.5}}"#;
        let decoded: ServerMsg = serde_json::from_str(json).unwrap();
        match decoded {
            ServerMsg::State {
                stems_level_db,
                stems_muted,
                stems_bus_index,
                ..
            } => {
                assert_eq!(stems_level_db, None);
                assert_eq!(stems_muted, None);
                assert_eq!(stems_bus_index, None);
            }
            _ => panic!("Expected State variant"),
        }
    }

    #[test]
    fn test_server_msg_solo_update_empty() {
        let msg = ServerMsg::SoloUpdate { soloed: vec![] };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_get_eq_params_serialization() {
        let msg = ClientMsg::GetEqParams { track_index: 3 };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"GetEqParams\""));
        assert!(json.contains("\"track_index\":3"));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_set_eq_band_serialization() {
        let msg = ClientMsg::SetEqBand {
            track_index: 3,
            band: 0,
            param: "gain".to_string(),
            value: 0.3,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"SetEqBand\""));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_eq_params_serialization() {
        let msg = ServerMsg::EqParams {
            track_index: 3,
            track_name: "MEMBER3 mic".to_string(),
            bands: vec![EqBand {
                band_type: "lowshelf".to_string(),
                freq_hz: 287.5,
                gain_db: -2.7,
                bw: 1.18,
                freq_norm: 0.283,
                gain_norm: 0.184,
                bw_norm: 0.295,
                gain_db_min: -12.0,
                gain_db_max: 12.0,
                enabled: true,
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"EqParams\""));
        assert!(json.contains("\"track_name\":\"MEMBER3 mic\""));
        assert!(json.contains("\"band_type\":\"lowshelf\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_get_eq_params_multi_serialization() {
        let msg = ClientMsg::GetEqParamsMulti {
            track_indices: vec![1, 3, 5],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"GetEqParamsMulti\""));
        assert!(json.contains("\"track_indices\":[1,3,5]"));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_eq_params_multi_serialization() {
        let mut bands = HashMap::new();
        bands.insert(
            1,
            vec![EqBand {
                band_type: "band".to_string(),
                freq_hz: 1000.0,
                gain_db: 3.0,
                bw: 1.5,
                freq_norm: 0.5,
                gain_norm: 0.3,
                bw_norm: 0.4,
                gain_db_min: -12.0,
                gain_db_max: 12.0,
                enabled: true,
            }],
        );
        bands.insert(3, vec![]);
        let msg = ServerMsg::EqParamsMulti { bands };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"EqParamsMulti\""));
        assert!(json.contains("\"band_type\":\"band\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_call_engineer_serialization() {
        let msg = ClientMsg::CallEngineer;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"CallEngineer\""));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_engineer_alert_serialization() {
        let msg = ServerMsg::EngineerAlert {
            from_member: "oldmember1".to_string(),
            from_name: "Oldmember1".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"EngineerAlert\""));
        assert!(json.contains("\"from_member\":\"oldmember1\""));
        assert!(json.contains("\"from_name\":\"Oldmember1\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_clear_alert_serialization() {
        let msg = ClientMsg::ClearAlert;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"ClearAlert\""));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_alert_cleared_serialization() {
        let msg = ServerMsg::AlertCleared {
            member_id: "oldmember1".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"AlertCleared\""));
        assert!(json.contains("\"member_id\":\"oldmember1\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_active_alerts_serialization() {
        let msg = ServerMsg::ActiveAlerts {
            alerts: vec![AlertInfo {
                from_member: "oldmember1".to_string(),
                from_name: "Oldmember1".to_string(),
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"ActiveAlerts\""));
        assert!(json.contains("\"from_member\":\"oldmember1\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_talk_start_serialization() {
        let msg = ClientMsg::TalkStart;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"TalkStart\""));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_talk_stop_serialization() {
        let msg = ClientMsg::TalkStop;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"TalkStop\""));
        let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_talk_acquired_serialization() {
        let msg = ServerMsg::TalkAcquired;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"TalkAcquired\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_talk_busy_serialization() {
        let msg = ServerMsg::TalkBusy {
            holder: "engineer".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"TalkBusy\""));
        assert!(json.contains("\"holder\":\"engineer\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_server_msg_talk_released_serialization() {
        let msg = ServerMsg::TalkReleased;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"event\":\"TalkReleased\""));
        let decoded: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_client_msg_get_limiter_params_serialization() {
        let msg = ClientMsg::GetLimiterParams { track_index: 23 };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("GetLimiterParams"));
        assert!(json.contains("23"));
        let back: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn test_client_msg_set_limiter_param_serialization() {
        let msg = ClientMsg::SetLimiterParam {
            track_index: 23,
            param: "limit".to_string(),
            value: 0.6,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("SetLimiterParam"));
        assert!(json.contains("limit"));
        let back: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn test_client_msg_set_limiter_enabled_serialization() {
        let msg = ClientMsg::SetLimiterEnabled {
            track_index: 23,
            enabled: false,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("SetLimiterEnabled"));
        let back: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn test_server_msg_limiter_params_serialization() {
        let msg = ServerMsg::LimiterParams {
            track_index: 23,
            track_name: "MEMBER1 inear".to_string(),
            limit_db: -6.0,
            limit_norm: 0.0,
            enabled: true,
            active_seconds: 0.0,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("LimiterParams"));
        assert!(json.contains("MEMBER1 inear"));
        assert!(json.contains("-6"));
        let back: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn test_server_msg_limiter_params_active_seconds_default() {
        // Older server may emit LimiterParams without active_seconds; new client
        // must still deserialize it with active_seconds = 0.0.
        let json = r#"{"event":"LimiterParams","data":{"track_index":23,"track_name":"MEMBER1 inear","limit_db":-6.0,"limit_norm":0.0,"enabled":true}}"#;
        let decoded: ServerMsg = serde_json::from_str(json).unwrap();
        match decoded {
            ServerMsg::LimiterParams { active_seconds, .. } => {
                assert_eq!(active_seconds, 0.0);
            }
            _ => panic!("Expected LimiterParams variant"),
        }
    }

    #[test]
    fn test_server_msg_limiter_params_with_active_seconds() {
        let msg = ServerMsg::LimiterParams {
            track_index: 23,
            track_name: "MEMBER1 inear".to_string(),
            limit_db: -6.0,
            limit_norm: 0.0,
            enabled: true,
            active_seconds: 83.5,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"active_seconds\":83.5"));
        let back: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn test_client_msg_reset_limiter_activity_serialization() {
        let msg = ClientMsg::ResetLimiterActivity { track_index: 23 };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"cmd\":\"ResetLimiterActivity\""));
        assert!(json.contains("\"track_index\":23"));
        let back: ClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn test_default_gain_db_min_is_negative_twelve() {
        // Mutation killer: prevents -12.0 → 0.0, 1.0, -1.0, or 12.0 (delete -)
        assert_eq!(super::default_gain_db_min(), -12.0);
    }

    #[test]
    fn test_default_gain_db_max_is_positive_twelve() {
        // Mutation killer: prevents 12.0 → 0.0, 1.0, or -1.0
        assert_eq!(super::default_gain_db_max(), 12.0);
    }

    #[test]
    fn test_eq_band_serde_default_for_missing_db_bounds() {
        // Mutation + integration: when an old snapshot JSON lacks the gain_db_min/max
        // fields, serde must inject -12.0 / +12.0 from the default helpers.
        let json = r#"{
            "band_type": "band",
            "freq_hz": 1000.0,
            "gain_db": 0.0,
            "bw": 1.0,
            "freq_norm": 0.5,
            "gain_norm": 0.25,
            "bw_norm": 0.5
        }"#;
        let band: EqBand = serde_json::from_str(json).unwrap();
        assert_eq!(band.gain_db_min, -12.0);
        assert_eq!(band.gain_db_max, 12.0);
        assert_eq!(band.enabled, true); // also tests existing default_enabled
    }
}
