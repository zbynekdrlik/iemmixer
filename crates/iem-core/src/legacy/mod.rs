//! The predecessor's (REAPER-era) data formats: presets, snapshots and
//! customizations keyed by REAPER track numbers, ReaEQ bands with their
//! normalised slider values, and the backup JSON v1. Only the importer
//! (`iem-rpp`, `iem-migrate`) reads them; the server and the UI speak the
//! engine's ids (`crate::band`, `crate::ws`, `crate::backup`).

pub mod backup;
pub mod preset;
pub mod snapshot;

use serde::{Deserialize, Serialize};

pub use backup::{BACKUP_VERSION, EqBandBackup, LimiterBackup, MixerBackup, SendBackup};
pub use preset::{ChannelPreset, PresetEntry};
pub use snapshot::{ChannelSnapshot, MixSnapshot};

/// A ReaEQ band as the predecessor stored it in presets and snapshots.
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

/// The predecessor's per-member pins and hides, keyed by REAPER track number.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Customization {
    #[serde(default)]
    pub pinned: Vec<usize>,
    #[serde(default)]
    pub hidden: Vec<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eq_band_defaults_its_missing_bounds_and_enable() {
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
        assert!(band.enabled);
        assert_eq!(default_gain_db_min(), -12.0);
        assert_eq!(default_gain_db_max(), 12.0);
        assert!(default_enabled());
    }

    #[test]
    fn customization_reads_missing_fields_as_empty() {
        let c: Customization = serde_json::from_str("{}").unwrap();
        assert_eq!(c, Customization::default());
        let c: Customization = serde_json::from_str(r#"{"pinned":[1,5],"hidden":[3]}"#).unwrap();
        assert_eq!((c.pinned, c.hidden), (vec![1, 5], vec![3]));
    }
}
