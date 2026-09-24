//! Mix snapshot types for history and restore
//!
//! Captures the state of a member's mixer settings at a point in time,
//! allowing restoration to previous mix states.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Maximum number of snapshots to keep per member
pub const MAX_SNAPSHOTS: usize = 50;

/// A snapshot of a single channel's settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelSnapshot {
    /// Volume level (0.0 to 1.0 in REAPER linear scale)
    pub vol: f32,
    /// Muted state
    pub mute: bool,
    /// Pan position in UI range (0.0 left, 0.5 center, 1.0 right) — copied
    /// verbatim from `Channel.pan`, which the poller already converted from
    /// REAPER's -1.0..1.0. Restore converts it back before writing. (reaperiem#203)
    pub pan: f32,
}

/// A complete mix snapshot capturing all channel states
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixSnapshot {
    /// Unix timestamp (seconds) when snapshot was created
    pub timestamp: i64,
    /// Label: "auto" for automatic snapshots, user-provided for manual
    pub label: String,
    /// Whether this snapshot is pinned (protected from pruning)
    pub pinned: bool,
    /// Channel states keyed by track index
    pub channels: HashMap<usize, ChannelSnapshot>,
    /// EQ band data per track (None for old snapshots)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq_bands: Option<HashMap<usize, Vec<crate::EqBand>>>,
}

impl MixSnapshot {
    /// Create a new automatic snapshot with the current timestamp
    pub fn new_auto(
        channels: HashMap<usize, ChannelSnapshot>,
        eq_bands: Option<HashMap<usize, Vec<crate::EqBand>>>,
    ) -> Self {
        Self {
            timestamp: chrono::Utc::now().timestamp(),
            label: "auto".to_string(),
            pinned: false,
            channels,
            eq_bands,
        }
    }

    /// Create a new manual snapshot with a custom label
    pub fn new_manual(
        label: String,
        channels: HashMap<usize, ChannelSnapshot>,
        eq_bands: Option<HashMap<usize, Vec<crate::EqBand>>>,
    ) -> Self {
        Self {
            timestamp: chrono::Utc::now().timestamp(),
            label,
            pinned: false,
            channels,
            eq_bands,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_snapshot_serde_roundtrip() {
        let channel = ChannelSnapshot {
            vol: 0.75,
            mute: false,
            pan: -0.5,
        };

        let json = serde_json::to_string(&channel).unwrap();
        let parsed: ChannelSnapshot = serde_json::from_str(&json).unwrap();

        assert_eq!(channel, parsed);
    }

    #[test]
    fn test_mix_snapshot_serde_roundtrip() {
        let mut channels = HashMap::new();
        channels.insert(
            1,
            ChannelSnapshot {
                vol: 0.5,
                mute: true,
                pan: 0.0,
            },
        );
        channels.insert(
            5,
            ChannelSnapshot {
                vol: 1.0,
                mute: false,
                pan: 0.75,
            },
        );

        let snapshot = MixSnapshot {
            timestamp: 1709582400,
            label: "rehearsal".to_string(),
            pinned: true,
            channels,
            eq_bands: None,
        };

        let json = serde_json::to_string(&snapshot).unwrap();
        let parsed: MixSnapshot = serde_json::from_str(&json).unwrap();

        assert_eq!(snapshot.timestamp, parsed.timestamp);
        assert_eq!(snapshot.label, parsed.label);
        assert_eq!(snapshot.pinned, parsed.pinned);
        assert_eq!(snapshot.channels.len(), parsed.channels.len());
        assert_eq!(snapshot.channels[&1], parsed.channels[&1]);
        assert_eq!(snapshot.channels[&5], parsed.channels[&5]);
        assert!(parsed.eq_bands.is_none());
    }

    #[test]
    fn test_new_auto_snapshot() {
        let mut channels = HashMap::new();
        channels.insert(
            1,
            ChannelSnapshot {
                vol: 0.5,
                mute: false,
                pan: 0.0,
            },
        );

        let snapshot = MixSnapshot::new_auto(channels, None);

        assert_eq!(snapshot.label, "auto");
        assert!(!snapshot.pinned);
        assert!(snapshot.timestamp > 0);
        assert!(snapshot.eq_bands.is_none());
    }

    #[test]
    fn test_new_manual_snapshot() {
        let channels = HashMap::new();
        let snapshot = MixSnapshot::new_manual("before soundcheck".to_string(), channels, None);

        assert_eq!(snapshot.label, "before soundcheck");
        assert!(!snapshot.pinned);
    }

    #[test]
    fn test_max_snapshots_constant() {
        assert_eq!(MAX_SNAPSHOTS, 50);
    }

    #[test]
    fn test_old_snapshot_without_eq_bands_deserializes() {
        let json = r#"{
            "timestamp": 1709582400,
            "label": "auto",
            "pinned": false,
            "channels": {
                "1": {"vol": 0.5, "mute": false, "pan": 0.0}
            }
        }"#;
        let parsed: MixSnapshot = serde_json::from_str(json).unwrap();
        assert!(parsed.eq_bands.is_none());
        assert_eq!(parsed.channels.len(), 1);
    }

    #[test]
    fn test_snapshot_with_eq_bands_roundtrip() {
        let mut channels = HashMap::new();
        channels.insert(
            1,
            ChannelSnapshot {
                vol: 0.5,
                mute: false,
                pan: 0.0,
            },
        );

        let mut eq_map = HashMap::new();
        eq_map.insert(
            1,
            vec![crate::EqBand {
                band_type: "band".to_string(),
                freq_hz: 1000.0,
                gain_db: 3.0,
                bw: 1.0,
                freq_norm: 0.5,
                gain_norm: 0.3125,
                bw_norm: 0.5,
                gain_db_min: -12.0,
                gain_db_max: 12.0,
                enabled: true,
            }],
        );

        let snapshot = MixSnapshot {
            timestamp: 1709582400,
            label: "test".to_string(),
            pinned: false,
            channels,
            eq_bands: Some(eq_map),
        };

        let json = serde_json::to_string(&snapshot).unwrap();
        let parsed: MixSnapshot = serde_json::from_str(&json).unwrap();

        let bands = parsed.eq_bands.unwrap();
        assert_eq!(bands[&1].len(), 1);
        assert_eq!(bands[&1][0].band_type, "band");
        assert!((bands[&1][0].freq_hz - 1000.0).abs() < 0.01);
    }
}
