//! Preset types for saving and restoring mix configurations
//!
//! Presets store volume in dB (matching frontend convention), unlike snapshots
//! which store REAPER linear scale values.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Maximum number of presets per member
pub const MAX_PRESETS: usize = 20;

/// A single channel's preset settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelPreset {
    /// Volume level in dB
    pub vol: f32,
    /// Muted state
    pub mute: bool,
    /// Pan position in UI range (0.0 left, 0.5 center, 1.0 right) — stored as
    /// the UI sends it; restore converts it back to REAPER's -1.0..1.0. (reaperiem#203)
    pub pan: f32,
}

/// A named preset containing all channel states
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetEntry {
    /// Preset name (unique per member)
    pub name: String,
    /// Channel states keyed by track index
    pub channels: HashMap<usize, ChannelPreset>,
    /// Unix timestamp (seconds) when preset was created
    pub created_at: i64,
    /// Unix timestamp (seconds) when preset was last updated
    pub updated_at: i64,
    /// Stems bus volume in dB (None for presets saved before stems-bus feature)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stems_level_db: Option<f32>,
    /// EQ band data per track (None for presets saved before EQ feature)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq_bands: Option<HashMap<usize, Vec<crate::EqBand>>>,
}

impl PresetEntry {
    /// Create a new preset with the current timestamp
    pub fn new(name: String, channels: HashMap<usize, ChannelPreset>) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            name,
            channels,
            created_at: now,
            updated_at: now,
            stems_level_db: None,
            eq_bands: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_preset_serde_roundtrip() {
        let channel = ChannelPreset {
            vol: -6.0,
            mute: false,
            pan: 0.5,
        };

        let json = serde_json::to_string(&channel).unwrap();
        let parsed: ChannelPreset = serde_json::from_str(&json).unwrap();
        assert_eq!(channel, parsed);
    }

    #[test]
    fn test_preset_entry_serde_roundtrip() {
        let mut channels = HashMap::new();
        channels.insert(
            1,
            ChannelPreset {
                vol: -12.0,
                mute: true,
                pan: -0.5,
            },
        );
        channels.insert(
            5,
            ChannelPreset {
                vol: 0.0,
                mute: false,
                pan: 0.0,
            },
        );

        let preset = PresetEntry {
            name: "rehearsal".to_string(),
            channels,
            created_at: 1709582400,
            updated_at: 1709582500,
            stems_level_db: None,
            eq_bands: None,
        };

        let json = serde_json::to_string(&preset).unwrap();
        let parsed: PresetEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(preset.name, parsed.name);
        assert_eq!(preset.created_at, parsed.created_at);
        assert_eq!(preset.updated_at, parsed.updated_at);
        assert_eq!(preset.channels.len(), parsed.channels.len());
        assert_eq!(preset.channels[&1], parsed.channels[&1]);
        assert_eq!(preset.channels[&5], parsed.channels[&5]);
    }

    #[test]
    fn test_new_preset() {
        let channels = HashMap::new();
        let preset = PresetEntry::new("test".to_string(), channels);

        assert_eq!(preset.name, "test");
        assert!(preset.created_at > 0);
        assert_eq!(preset.created_at, preset.updated_at);
    }

    #[test]
    fn test_preset_entry_backwards_compat_without_stems() {
        // Old presets without stems_level_db should still deserialize
        let json = r#"{"name":"old","channels":{},"created_at":100,"updated_at":200}"#;
        let parsed: PresetEntry = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.stems_level_db, None);
    }

    #[test]
    fn test_preset_entry_with_stems_level() {
        let mut channels = HashMap::new();
        channels.insert(
            1,
            ChannelPreset {
                vol: -6.0,
                mute: false,
                pan: 0.0,
            },
        );
        let preset = PresetEntry {
            name: "with_stems".to_string(),
            channels,
            created_at: 100,
            updated_at: 200,
            stems_level_db: Some(-3.0),
            eq_bands: None,
        };
        let json = serde_json::to_string(&preset).unwrap();
        assert!(json.contains("\"stems_level_db\":-3.0"));
        let parsed: PresetEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.stems_level_db, Some(-3.0));
    }

    #[test]
    fn test_max_presets_constant() {
        assert_eq!(MAX_PRESETS, 20);
    }

    #[test]
    fn test_preset_entry_backwards_compat_without_eq_bands() {
        // Old presets without eq_bands should still deserialize
        let json = r#"{"name":"old","channels":{},"created_at":100,"updated_at":200}"#;
        let parsed: PresetEntry = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.eq_bands, None);
    }

    #[test]
    fn test_preset_entry_with_eq_bands() {
        use crate::EqBand;
        let mut eq = HashMap::new();
        eq.insert(
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
        let preset = PresetEntry {
            name: "with_eq".to_string(),
            channels: HashMap::new(),
            created_at: 100,
            updated_at: 200,
            stems_level_db: None,
            eq_bands: Some(eq),
        };
        let json = serde_json::to_string(&preset).unwrap();
        assert!(json.contains("eq_bands"));
        let parsed: PresetEntry = serde_json::from_str(&json).unwrap();
        assert!(parsed.eq_bands.is_some());
        assert_eq!(parsed.eq_bands.unwrap()[&1].len(), 1);
    }
}
