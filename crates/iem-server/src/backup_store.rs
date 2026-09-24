//! Backup file store — persists MixerBackup snapshots as JSON files
//!
//! Files are named by timestamp (YYYYMMDD_HHMMSS.json) and stored in
//! `<config_dir>/backups/`. Provides list, load, save, and prune operations.

use crate::atomic_write;
use iem_core::backup::{BACKUP_VERSION, BackupInfo, MixerBackup};
use std::path::PathBuf;

/// Persistent storage for full mixer backups
pub struct BackupStore {
    /// Directory where backup JSON files are written
    backups_dir: PathBuf,
}

impl BackupStore {
    /// Create a new BackupStore rooted at `<config_dir>/backups`
    pub fn new(config_dir: &std::path::Path) -> Self {
        Self {
            backups_dir: config_dir.join("backups"),
        }
    }

    /// Save a backup to disk.
    ///
    /// Generates a filename from the backup's `timestamp` field.
    /// For "2026-04-05T13:00:00+02:00" this produces "20260405_130000.json".
    /// Returns the filename (without directory) on success.
    pub fn save(&self, backup: &MixerBackup) -> Result<String, std::io::Error> {
        std::fs::create_dir_all(&self.backups_dir)?;

        let filename = timestamp_to_filename(&backup.timestamp);
        let path = self.backups_dir.join(&filename);

        let json = serde_json::to_string_pretty(backup).map_err(std::io::Error::other)?;
        atomic_write(&path, &json)?;

        Ok(filename)
    }

    /// List all available backups, sorted newest-first.
    ///
    /// Files that cannot be parsed are silently skipped.
    pub fn list(&self) -> Vec<BackupInfo> {
        let Ok(entries) = std::fs::read_dir(&self.backups_dir) else {
            return Vec::new();
        };

        let mut infos: Vec<BackupInfo> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext == "json")
                    .unwrap_or(false)
            })
            .filter_map(|e| {
                let path = e.path();
                let filename = path.file_name()?.to_str()?.to_string();

                let metadata = std::fs::metadata(&path).ok()?;
                let size_bytes = metadata.len();

                let content = std::fs::read_to_string(&path).ok()?;
                let backup: MixerBackup = serde_json::from_str(&content).ok()?;

                Some(BackupInfo {
                    filename,
                    timestamp: backup.timestamp,
                    size_bytes,
                    send_count: backup.sends.len(),
                    track_count: backup.track_layout.len(),
                })
            })
            .collect();

        // Sort newest-first by filename (YYYYMMDD_HHMMSS lexicographic order)
        infos.sort_by_key(|i| std::cmp::Reverse(i.filename.clone()));

        infos
    }

    /// Load a backup by filename.
    ///
    /// Returns `Err` if the filename contains path traversal characters
    /// (`/`, `\`, or `..`), if the file cannot be read, or if the version
    /// field does not match `BACKUP_VERSION`.
    pub fn load(&self, filename: &str) -> Result<MixerBackup, String> {
        // Path traversal protection
        if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
            return Err(format!("invalid filename: {filename}"));
        }

        let path = self.backups_dir.join(filename);
        let content = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;

        let backup: MixerBackup =
            serde_json::from_str(&content).map_err(|e| format!("parse error: {e}"))?;

        if backup.version != BACKUP_VERSION {
            return Err(format!(
                "unsupported backup version {} (expected {BACKUP_VERSION})",
                backup.version
            ));
        }

        Ok(backup)
    }

    /// Delete backup files older than `retention_days`.
    ///
    /// Only files whose stem matches the `YYYYMMDD_HHMMSS` pattern are
    /// considered. Returns the number of files deleted.
    pub fn prune(&self, retention_days: u32) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.backups_dir) else {
            return 0;
        };

        let cutoff = chrono::Utc::now()
            - chrono::TimeDelta::try_days(retention_days as i64).unwrap_or_default();
        let cutoff_str = cutoff.format("%Y%m%d_%H%M%S").to_string();

        let mut deleted = 0;
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            // Only consider files that look like our timestamp format
            if stem.len() != 15 || !stem.contains('_') {
                continue;
            }
            if stem < cutoff_str.as_str() && std::fs::remove_file(&path).is_ok() {
                deleted += 1;
            }
        }

        deleted
    }
}

/// Convert an ISO 8601 timestamp to a backup filename.
///
/// Takes the first 19 characters ("2026-04-05T13:00:00"),
/// strips `-` and `:`, replaces `T` with `_`, appends `.json`.
///
/// "2026-04-05T13:00:00+02:00" → "20260405_130000.json"
fn timestamp_to_filename(timestamp: &str) -> String {
    let ts = if timestamp.len() >= 19 {
        &timestamp[..19]
    } else {
        timestamp
    };
    ts.replace(['-', ':'], "").replace('T', "_") + ".json"
}

#[cfg(test)]
mod tests {
    use super::*;
    use iem_core::backup::SendBackup;
    use std::collections::HashMap;

    fn make_backup(timestamp: &str) -> MixerBackup {
        let mut track_layout = HashMap::new();
        track_layout.insert(1u32, "OLDMEMBER1 mic".to_string());

        MixerBackup {
            version: BACKUP_VERSION,
            timestamp: timestamp.to_string(),
            track_layout,
            sends: vec![SendBackup {
                src_name: "OLDMEMBER1 mic".to_string(),
                dest_name: "OLDMEMBER1 inear".to_string(),
                vol: 0.0,
                pan: 0.0,
                mute: false,
            }],
            track_volumes: HashMap::new(),
            eq: HashMap::new(),
            limiter: HashMap::new(),
            customizations: HashMap::new(),
            pins: HashMap::new(),
            track_mutes: HashMap::new(),
        }
    }

    #[test]
    fn test_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = BackupStore::new(dir.path());

        let backup = make_backup("2026-04-05T13:00:00Z");
        let filename = store.save(&backup).unwrap();

        assert_eq!(filename, "20260405_130000.json");

        let loaded = store.load(&filename).unwrap();
        assert_eq!(loaded.version, BACKUP_VERSION);
        assert_eq!(loaded.timestamp, "2026-04-05T13:00:00Z");
        assert_eq!(loaded.sends.len(), 1);
        assert_eq!(loaded.sends[0].src_name, "OLDMEMBER1 mic");
        assert_eq!(loaded.track_layout.get(&1u32).unwrap(), "OLDMEMBER1 mic");
    }

    #[test]
    fn test_list_sorted_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = BackupStore::new(dir.path());

        let older = make_backup("2026-01-01T08:00:00Z");
        let newer = make_backup("2026-04-05T13:00:00Z");

        store.save(&older).unwrap();
        store.save(&newer).unwrap();

        let list = store.list();
        assert_eq!(list.len(), 2);
        // Newest first: April before January
        assert_eq!(list[0].filename, "20260405_130000.json");
        assert_eq!(list[1].filename, "20260101_080000.json");
    }

    #[test]
    fn test_path_traversal_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let store = BackupStore::new(dir.path());

        assert!(store.load("../etc/passwd").is_err());
        assert!(store.load("../../secret.json").is_err());
        assert!(store.load("sub/file.json").is_err());
        assert!(store.load("C:\\Windows\\win.ini").is_err());
    }
}
