//! Backup files (F19): `<config dir>/backups/YYYYMMDD_HHMMSS.json`, backups v2
//! only; the predecessor's backups (moved to `legacy/backups/` by the
//! migration) are not listed. Retention prunes files older than the site's
//! `backup_retention_days`.

use crate::atomic_write;
use iem_core::backup::{BackupInfo, MixerBackup};
use std::path::PathBuf;

pub struct BackupStore {
    backups_dir: PathBuf,
}

impl BackupStore {
    pub fn new(config_dir: &std::path::Path) -> Self {
        Self {
            backups_dir: config_dir.join("backups"),
        }
    }

    /// Writes `backup` and returns its file name.
    pub fn save(&self, backup: &MixerBackup) -> Result<String, std::io::Error> {
        std::fs::create_dir_all(&self.backups_dir)?;
        let filename = timestamp_to_filename(&backup.timestamp);
        let path = self.backups_dir.join(&filename);
        let json = serde_json::to_string_pretty(backup).map_err(std::io::Error::other)?;
        atomic_write(&path, &json)?;
        Ok(filename)
    }

    /// Readable backups, newest first.
    pub fn list(&self) -> Vec<BackupInfo> {
        let Ok(entries) = std::fs::read_dir(&self.backups_dir) else {
            return Vec::new();
        };
        let mut infos: Vec<BackupInfo> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
            .filter_map(|path| {
                let filename = path.file_name()?.to_str()?.to_string();
                let size_bytes = std::fs::metadata(&path).ok()?.len();
                let backup = MixerBackup::parse(&std::fs::read_to_string(&path).ok()?).ok()?;
                Some(BackupInfo {
                    filename,
                    timestamp: backup.timestamp.clone(),
                    size_bytes,
                    send_count: backup.level_count(),
                    track_count: backup.state.mixes.len(),
                })
            })
            .collect();
        infos.sort_by_key(|i| std::cmp::Reverse(i.filename.clone()));
        infos
    }

    /// One backup by file name (no path separators).
    pub fn load(&self, filename: &str) -> Result<MixerBackup, String> {
        if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
            return Err(format!("invalid filename: {filename}"));
        }
        let path = self.backups_dir.join(filename);
        let content = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
        MixerBackup::parse(&content).map_err(|e| e.to_string())
    }

    /// Deletes backups older than `retention_days`; returns how many.
    pub fn prune(&self, retention_days: u32) -> usize {
        let cutoff = chrono::Utc::now()
            - chrono::TimeDelta::try_days(i64::from(retention_days)).unwrap_or_default();
        self.prune_before(&cutoff.format("%Y%m%d_%H%M%S").to_string())
    }

    fn prune_before(&self, cutoff: &str) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.backups_dir) else {
            return 0;
        };
        let mut deleted = 0;
        for path in entries.filter_map(|e| e.ok()).map(|e| e.path()) {
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem.len() == 15
                && stem.contains('_')
                && stem < cutoff
                && std::fs::remove_file(&path).is_ok()
            {
                deleted += 1;
            }
        }
        deleted
    }
}

/// "2026-09-27T13:00:00Z" → "20260927_130000.json".
fn timestamp_to_filename(timestamp: &str) -> String {
    let ts = timestamp.get(..19).unwrap_or(timestamp);
    ts.replace(['-', ':'], "").replace('T', "_") + ".json"
}

#[cfg(test)]
mod tests {
    use super::*;
    use iem_engine_proto::{Mix, MixId, MixState};

    fn backup(timestamp: &str) -> MixerBackup {
        let mut state = MixState::default();
        state.mixes.insert(MixId::new("member1"), Mix::default());
        MixerBackup::new(timestamp.into(), 1, state)
    }

    #[test]
    fn saved_backups_load_and_list_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = BackupStore::new(dir.path());
        assert!(store.list().is_empty());
        let a = store.save(&backup("2026-09-27T13:00:00Z")).unwrap();
        assert_eq!(a, "20260927_130000.json");
        store.save(&backup("2026-09-28T21:00:00Z")).unwrap();
        // A predecessor (v1) file in the same folder is not listed.
        std::fs::write(
            dir.path().join("backups/20260101_000000.json"),
            r#"{"version":1,"timestamp":"t","sends":[]}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("backups/notes.txt"), "x").unwrap();
        let list = store.list();
        let names: Vec<&str> = list.iter().map(|i| i.filename.as_str()).collect();
        assert_eq!(names, ["20260928_210000.json", "20260927_130000.json"]);
        assert_eq!((list[0].track_count, list[0].send_count), (1, 0));
        assert!(list[0].size_bytes > 0);
        assert_eq!(store.load(&a).unwrap(), backup("2026-09-27T13:00:00Z"));
        assert!(
            store
                .load("20260101_000000.json")
                .unwrap_err()
                .contains("format")
        );
        assert!(
            store
                .load("missing.json")
                .unwrap_err()
                .starts_with("read error")
        );
    }

    #[test]
    fn path_traversal_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = BackupStore::new(dir.path());
        for bad in ["../x.json", "a/b.json", "a\\b.json", ".."] {
            assert!(
                store.load(bad).unwrap_err().starts_with("invalid filename"),
                "{bad}"
            );
        }
    }

    #[test]
    fn retention_prunes_only_old_backup_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = BackupStore::new(dir.path());
        assert_eq!(store.prune(60), 0, "no folder yet");
        store.save(&backup("2020-01-01T00:00:00Z")).unwrap();
        store.save(&backup("2099-01-01T00:00:00Z")).unwrap();
        std::fs::write(dir.path().join("backups/manual.json"), "{}").unwrap();
        std::fs::write(dir.path().join("backups/20000101_000000.txt"), "{}").unwrap();
        assert_eq!(store.prune_before("20210101_000000"), 1);
        assert_eq!(store.prune(60), 0);
        let mut left: Vec<String> = std::fs::read_dir(dir.path().join("backups"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["20000101_000000.txt", "20990101_000000.json", "manual.json"]
        );
        assert_eq!(timestamp_to_filename("short"), "short.json");
    }
}
