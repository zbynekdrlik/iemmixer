//! Backups v2 (F19, F31; S5 design note §6): the engine's whole mix state at
//! one revision plus every member's pins and hides. A backup never holds a
//! PIN or a secret. Restores are previewed as a diff of two states first.

use std::collections::BTreeMap;

use iem_engine_proto::MixState;
use serde::{Deserialize, Serialize};

use crate::band::CustomizationFile;

pub const BACKUP_FORMAT: &str = "iemmixer-backup";
pub const BACKUP_VERSION: u32 = 2;

/// One backup file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MixerBackup {
    pub format: String,
    pub version: u32,
    /// RFC 3339 time of the capture.
    pub timestamp: String,
    /// The engine revision the state was taken at.
    pub rev: u64,
    pub state: MixState,
    /// Member id → pins and hides.
    pub customizations: BTreeMap<String, CustomizationFile>,
}

/// Why a file is not a readable backup.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackupError {
    #[error("not JSON: {0}")]
    Json(String),
    #[error("not an iemmixer backup (format {0:?})")]
    Format(String),
    #[error("backup version {0} is not supported (this server reads version {BACKUP_VERSION})")]
    Version(u32),
}

impl MixerBackup {
    pub fn new(timestamp: String, rev: u64, state: MixState) -> Self {
        Self {
            format: BACKUP_FORMAT.into(),
            version: BACKUP_VERSION,
            timestamp,
            rev,
            state,
            customizations: BTreeMap::new(),
        }
    }

    /// Reads a backup file; the predecessor's version-1 files (REAPER sends)
    /// and foreign JSON are refused with the reason.
    pub fn parse(text: &str) -> Result<Self, BackupError> {
        #[derive(Deserialize)]
        struct Head {
            #[serde(default)]
            format: String,
            #[serde(default)]
            version: u32,
        }
        let head: Head =
            serde_json::from_str(text).map_err(|e| BackupError::Json(e.to_string()))?;
        if head.format != BACKUP_FORMAT {
            return Err(BackupError::Format(head.format));
        }
        if head.version != BACKUP_VERSION {
            return Err(BackupError::Version(head.version));
        }
        serde_json::from_str(text).map_err(|e| BackupError::Json(e.to_string()))
    }

    /// Levels stored across every mix (the listing's "sends").
    pub fn level_count(&self) -> usize {
        self.state
            .mixes
            .values()
            .map(|m| m.inputs.len() + m.mixes.len())
            .sum()
    }
}

/// Metadata for listing available backups
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackupInfo {
    /// Filename of the backup (without directory)
    pub filename: String,
    /// RFC 3339 timestamp from the backup
    pub timestamp: String,
    /// File size in bytes
    pub size_bytes: u64,
    /// Levels stored across every mix
    pub send_count: usize,
    /// Mixes stored
    pub track_count: usize,
}

/// Preview of what a restore operation would change (F31)
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RestorePreview {
    /// Values that differ: current → backup
    pub changes: Vec<RestoreChange>,
    /// Values that already match
    pub unchanged_count: usize,
    /// Entries of the backup the running topology does not have
    pub skipped: Vec<SkippedEntry>,
    /// What the running state has and the backup lacks (added to the
    /// topology after the capture); the restore leaves it as it is
    #[serde(default)]
    pub not_in_backup: Vec<SkippedEntry>,
}

/// A single proposed change from a restore
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RestoreChange {
    pub category: RestoreCategory,
    /// What changes, in the UI's names (e.g. "MEMBER3 mic → Member1")
    pub description: String,
    pub current_value: String,
    pub backup_value: String,
}

/// Category of a restored item
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum RestoreCategory {
    Level,
    Group,
    Output,
    Eq,
    Limiter,
    Input,
    Customization,
}

/// An item skipped during restore (an id the topology no longer has)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkippedEntry {
    pub category: RestoreCategory,
    pub description: String,
    pub reason: String,
}

/// Final result returned after a restore completes
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RestoreResult {
    /// Number of values the restore changed
    pub restored_count: usize,
    pub skipped: Vec<SkippedEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use iem_engine_proto::{InputId, Level, Mix, MixId, Source};

    fn sample() -> MixerBackup {
        let mut state = MixState::default();
        let mut mix = Mix::default();
        mix.inputs.insert(InputId::new("mic1"), Level::default());
        mix.mixes.insert(MixId::new("member2"), Level::default());
        state.mixes.insert(MixId::new("member1"), mix);
        state.mixes.insert(MixId::new("member2"), Mix::default());
        let mut b = MixerBackup::new("2026-09-27T13:00:00Z".into(), 42, state);
        b.customizations.insert(
            "member1".into(),
            CustomizationFile::new("member1", vec![Source::Input(InputId::new("mic1"))], vec![]),
        );
        b
    }

    #[test]
    fn a_backup_round_trips_with_its_header() {
        let b = sample();
        let json = serde_json::to_string(&b).unwrap();
        assert!(
            json.starts_with(r#"{"format":"iemmixer-backup","version":2,"timestamp":"2026-09-27T13:00:00Z","rev":42,"state":"#),
            "{json}"
        );
        // No PIN material ("pinned" is the customization's pin list).
        assert!(
            !json.contains("\"pin\"") && !json.contains("pin_hash"),
            "{json}"
        );
        assert_eq!(MixerBackup::parse(&json).unwrap(), b);
        assert_eq!(b.level_count(), 2);
    }

    #[test]
    fn foreign_and_old_files_are_refused_with_the_reason() {
        let v1 = r#"{"version":1,"timestamp":"t","track_layout":{},"sends":[],"customizations":{"member1":{"pinned":[1,5],"hidden":[]}}}"#;
        assert_eq!(
            MixerBackup::parse(v1),
            Err(BackupError::Format(String::new()))
        );
        let v3 = r#"{"format":"iemmixer-backup","version":3}"#;
        assert_eq!(MixerBackup::parse(v3), Err(BackupError::Version(3)));
        let other = r#"{"format":"iemmixer-presets","version":2}"#;
        assert_eq!(
            MixerBackup::parse(other),
            Err(BackupError::Format("iemmixer-presets".into()))
        );
        assert!(matches!(MixerBackup::parse("{"), Err(BackupError::Json(_))));
        let bad_body = r#"{"format":"iemmixer-backup","version":2,"rev":"x"}"#;
        assert!(matches!(
            MixerBackup::parse(bad_body),
            Err(BackupError::Json(_))
        ));
        assert!(BackupError::Version(3).to_string().contains("version 2"));
    }

    #[test]
    fn preview_types_serialise_their_categories() {
        let p = RestorePreview {
            changes: vec![RestoreChange {
                category: RestoreCategory::Level,
                description: "MEMBER3 mic → Member1".into(),
                current_value: "-6.0 dB".into(),
                backup_value: "-3.0 dB".into(),
            }],
            unchanged_count: 7,
            skipped: vec![SkippedEntry {
                category: RestoreCategory::Input,
                description: "gone".into(),
                reason: "not in the topology".into(),
            }],
            not_in_backup: vec![SkippedEntry {
                category: RestoreCategory::Input,
                description: "KEYS".into(),
                reason: "not in the backup; stays as it is".into(),
            }],
        };
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains(r#""category":"Level""#), "{json}");
        assert!(
            json.contains(r#""not_in_backup":[{"category":"Input","description":"KEYS""#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<RestorePreview>(&json).unwrap(), p);
        assert!(RestoreCategory::Level < RestoreCategory::Customization);
        // A preview without the list (an older server) reads as an empty one.
        let old = r#"{"changes":[],"unchanged_count":0,"skipped":[]}"#;
        assert_eq!(
            serde_json::from_str::<RestorePreview>(old).unwrap(),
            RestorePreview::default()
        );
    }
}
