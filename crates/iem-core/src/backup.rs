//! Backup and restore schema types for the IEM mixer
//! Supports full snapshot of sends, EQ, limiter, customizations, and PINs

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::{Customization, EqBand};

/// Schema version for forward-compatibility checks
pub const BACKUP_VERSION: u32 = 1;

/// Complete mixer backup snapshot
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MixerBackup {
    /// Schema version (always BACKUP_VERSION)
    pub version: u32,
    /// ISO 8601 timestamp of when the backup was created
    pub timestamp: String,
    /// Track layout at backup time: track index → track name
    pub track_layout: HashMap<u32, String>,
    /// All send states captured
    pub sends: Vec<SendBackup>,
    /// Per-track output volume: track name → volume in dB
    pub track_volumes: HashMap<String, f64>,
    /// EQ bands per track: track name → list of bands
    pub eq: HashMap<String, Vec<EqBandBackup>>,
    /// Limiter params per track: track name → limiter state
    pub limiter: HashMap<String, LimiterBackup>,
    /// Per-member UI customizations: member ID → prefs
    pub customizations: HashMap<String, Customization>,
    /// Per-member PINs: member ID → PIN string
    pub pins: HashMap<String, String>,
    /// Track-level mute state for inear/stems tracks: track name → muted
    #[serde(default)]
    pub track_mutes: HashMap<String, bool>,
}

/// State of a single send at backup time
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SendBackup {
    /// Source track name
    pub src_name: String,
    /// Destination track name
    pub dest_name: String,
    /// Volume in dB
    pub vol: f64,
    /// Pan position (-1.0 left … 1.0 right)
    pub pan: f64,
    /// Whether the send is muted
    pub mute: bool,
}

/// EQ band state at backup time (serialisable, band-index aware copy of EqBand)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EqBandBackup {
    /// 0-based band index within the track's EQ
    pub band: u8,
    /// Band type name: "band", "lowshelf", "highshelf", "highpass", "lowpass", "notch"
    pub band_type: String,
    /// Normalized frequency (0-1, ReaEQ internal)
    pub freq_norm: f32,
    /// Normalized gain (0-1, 0.25 = 0 dB)
    pub gain_norm: f32,
    /// Normalized bandwidth (0-1)
    pub bw_norm: f32,
    /// Frequency in Hz
    pub freq_hz: f32,
    /// Gain in dB
    pub gain_db: f32,
    /// Bandwidth in octaves
    pub bw_oct: f32,
    /// Whether the band is enabled
    pub enabled: bool,
}

impl From<(u8, &EqBand)> for EqBandBackup {
    fn from((band, eq): (u8, &EqBand)) -> Self {
        Self {
            band,
            band_type: eq.band_type.clone(),
            freq_norm: eq.freq_norm,
            gain_norm: eq.gain_norm,
            bw_norm: eq.bw_norm,
            freq_hz: eq.freq_hz,
            gain_db: eq.gain_db,
            bw_oct: eq.bw,
            enabled: eq.enabled,
        }
    }
}

/// Limiter state for a single output track
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LimiterBackup {
    /// Max output level in dB (-6 to 0)
    pub limit_db: f32,
    /// Normalized slider position (0-1)
    pub limit_norm: f32,
    /// Whether the limiter FX is enabled
    pub enabled: bool,
}

/// Metadata for listing available backups
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupInfo {
    /// Filename of the backup (without directory)
    pub filename: String,
    /// ISO 8601 timestamp from the backup header
    pub timestamp: String,
    /// File size in bytes
    pub size_bytes: u64,
    /// Number of sends captured
    pub send_count: usize,
    /// Number of tracks in the layout
    pub track_count: usize,
}

/// Preview of what a restore operation would change
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestorePreview {
    /// Proposed changes (backup value differs from current value)
    pub changes: Vec<RestoreChange>,
    /// Number of items that already match — no change needed
    pub unchanged_count: usize,
    /// Items that will be skipped (e.g., track no longer exists)
    pub skipped: Vec<SkippedEntry>,
    /// Estimated restore time in seconds (EQ is slow: ~0.24s per band)
    #[serde(default)]
    pub estimated_seconds: u32,
    /// REAPER tracks that exist now but were not in the backup's track_mutes.
    /// These tracks were added after the backup was captured.
    #[serde(default)]
    pub tracks_in_reaper_not_in_backup: Vec<String>,
    /// Backup track_mutes keys that no longer exist in REAPER.
    /// These tracks were removed or renamed since the backup was captured.
    #[serde(default)]
    pub tracks_in_backup_not_in_reaper: Vec<String>,
}

/// A single proposed change from a restore
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreChange {
    /// Category of the changed item
    pub category: RestoreCategory,
    /// Human-readable description (e.g., "MEMBER3 mic → OLDMEMBER1 inear")
    pub description: String,
    /// Current live value as a display string
    pub current_value: String,
    /// Value that will be written from the backup
    pub backup_value: String,
}

/// Category of a restored item
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RestoreCategory {
    Send,
    TrackVolume,
    TrackMute,
    Eq,
    Limiter,
    Customization,
    Pin,
}

/// An item skipped during restore (track missing, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedEntry {
    /// Category of the skipped item
    pub category: RestoreCategory,
    /// Human-readable description of the item
    pub description: String,
    /// Reason the item was skipped
    pub reason: String,
}

/// Real-time progress of an in-flight restore operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreProgress {
    /// Total number of items to restore
    pub total: usize,
    /// Number of items completed so far
    pub completed: usize,
    /// Description of the step currently being executed
    pub current_step: String,
}

/// Final result returned after a restore completes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreResult {
    /// Number of items successfully restored
    pub restored_count: usize,
    /// Items that were skipped
    pub skipped: Vec<SkippedEntry>,
    /// Whether the REAPER project was saved after the restore
    pub project_saved: bool,
    /// Track names from the backup that could not be resolved to a current
    /// REAPER track (missing in any category: sends, track_mutes, track_volumes,
    /// EQ, or limiter).
    #[serde(default)]
    pub skipped_tracks: Vec<String>,
}

/// Counts and timing metadata about a capture run. Embedded in the
/// `/api/backups/capture` response (and in v2 file format in Phase 2)
/// so the engineer can verify the capture is complete before relying on it.
///
/// Used by `assert_capture_completeness` (in backup_capture.rs) to refuse
/// captures that fall below operational minimums — silent partial captures
/// are the most dangerous failure mode for restore.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CaptureAudit {
    pub tracks_total: usize,
    pub tracks_named: Vec<String>,
    pub sends_count: usize,
    pub track_mutes_count: usize,
    pub track_volumes_count: usize,
    pub eq_count: usize,
    pub limiter_count: usize,
    pub customizations_count: usize,
    pub pins_count: usize,
    pub reaper_query_duration_ms: u64,
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backup_roundtrip() {
        let mut track_layout = HashMap::new();
        track_layout.insert(1u32, "OLDMEMBER1 mic".to_string());
        track_layout.insert(2u32, "MEMBER2 mic".to_string());

        let mut track_volumes = HashMap::new();
        track_volumes.insert("OLDMEMBER1 inear".to_string(), -3.0f64);

        let mut eq = HashMap::new();
        eq.insert(
            "OLDMEMBER1 mic".to_string(),
            vec![EqBandBackup {
                band: 0,
                band_type: "lowshelf".to_string(),
                freq_norm: 0.28,
                gain_norm: 0.18,
                bw_norm: 0.30,
                freq_hz: 287.5,
                gain_db: -2.7,
                bw_oct: 1.18,
                enabled: true,
            }],
        );

        let mut limiter = HashMap::new();
        limiter.insert(
            "OLDMEMBER1 inear".to_string(),
            LimiterBackup {
                limit_db: -6.0,
                limit_norm: 0.0,
                enabled: true,
            },
        );

        let mut customizations = HashMap::new();
        customizations.insert(
            "oldmember1".to_string(),
            Customization {
                pinned: vec![1, 5],
                hidden: vec![3],
            },
        );

        let mut pins = HashMap::new();
        pins.insert("oldmember1".to_string(), "1234".to_string());

        let mut track_mutes = HashMap::new();
        track_mutes.insert("OLDMEMBER1 inear".to_string(), false);

        let backup = MixerBackup {
            version: BACKUP_VERSION,
            timestamp: "2026-04-07T10:00:00Z".to_string(),
            track_layout,
            sends: vec![SendBackup {
                src_name: "OLDMEMBER1 mic".to_string(),
                dest_name: "OLDMEMBER1 inear".to_string(),
                vol: 0.0,
                pan: 0.0,
                mute: false,
            }],
            track_volumes,
            eq,
            limiter,
            customizations,
            pins,
            track_mutes,
        };

        let json = serde_json::to_string(&backup).unwrap();
        let decoded: MixerBackup = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.version, BACKUP_VERSION);
        assert_eq!(decoded.timestamp, "2026-04-07T10:00:00Z");
        assert_eq!(decoded.track_layout.get(&1u32).unwrap(), "OLDMEMBER1 mic");
        assert_eq!(decoded.sends.len(), 1);
        assert_eq!(decoded.sends[0].src_name, "OLDMEMBER1 mic");
        assert_eq!(decoded.sends[0].dest_name, "OLDMEMBER1 inear");
        assert!(!decoded.sends[0].mute);
        assert_eq!(
            *decoded.track_volumes.get("OLDMEMBER1 inear").unwrap(),
            -3.0f64
        );
        let eq_bands = decoded.eq.get("OLDMEMBER1 mic").unwrap();
        assert_eq!(eq_bands.len(), 1);
        assert_eq!(eq_bands[0].band_type, "lowshelf");
        assert!(eq_bands[0].enabled);
        let lim = decoded.limiter.get("OLDMEMBER1 inear").unwrap();
        assert_eq!(lim.limit_db, -6.0);
        assert!(lim.enabled);
        let cust = decoded.customizations.get("oldmember1").unwrap();
        assert_eq!(cust.pinned, vec![1, 5]);
        assert_eq!(cust.hidden, vec![3]);
        assert_eq!(decoded.pins.get("oldmember1").unwrap(), "1234");
    }

    #[test]
    fn test_eq_band_backup_from_eq_band() {
        let eq_band = EqBand {
            band_type: "highshelf".to_string(),
            freq_hz: 8000.0,
            gain_db: 3.5,
            bw: 0.75,
            freq_norm: 0.72,
            gain_norm: 0.35,
            bw_norm: 0.19,
            gain_db_min: -12.0,
            gain_db_max: 12.0,
            enabled: false,
        };

        let backup = EqBandBackup::from((2u8, &eq_band));

        assert_eq!(backup.band, 2);
        assert_eq!(backup.band_type, "highshelf");
        assert_eq!(backup.freq_hz, 8000.0);
        assert_eq!(backup.gain_db, 3.5);
        assert_eq!(backup.bw_oct, 0.75);
        assert_eq!(backup.freq_norm, 0.72);
        assert_eq!(backup.gain_norm, 0.35);
        assert_eq!(backup.bw_norm, 0.19);
        assert!(!backup.enabled);
    }
}
