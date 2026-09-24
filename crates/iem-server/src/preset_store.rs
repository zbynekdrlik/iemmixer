//! Preset storage with JSON file persistence
//!
//! Stores mix presets per member, following the SnapshotStore pattern.
//! Each member has their own JSON file in the presets/ directory.

use crate::atomic_write;
use iem_core::{ChannelPreset, MAX_PRESETS, PresetEntry};
use std::collections::HashMap;
use std::path::PathBuf;

/// Preset storage backed by per-member JSON files
pub struct PresetStore {
    /// Base directory for preset files
    presets_dir: PathBuf,
}

impl PresetStore {
    /// Create a new preset store in the given config directory
    pub fn new(config_dir: &std::path::Path) -> Self {
        let presets_dir = config_dir.join("presets");
        Self { presets_dir }
    }

    /// Get the file path for a member's presets.
    /// Panics if member_id contains invalid characters (path traversal protection).
    fn member_path(&self, member_id: &str) -> PathBuf {
        iem_core::config::validate_member_id(member_id)
            .expect("invalid member_id passed to PresetStore");
        self.presets_dir.join(format!("{}.json", member_id))
    }

    /// Load all presets for a member (returns empty map if file doesn't exist)
    fn load_all(&self, member_id: &str) -> HashMap<String, PresetEntry> {
        let path = self.member_path(member_id);
        if !path.exists() {
            return HashMap::new();
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Save all presets for a member
    fn save_all(
        &self,
        member_id: &str,
        presets: &HashMap<String, PresetEntry>,
    ) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(&self.presets_dir)?;
        let path = self.member_path(member_id);
        let json = serde_json::to_string_pretty(presets).map_err(std::io::Error::other)?;
        atomic_write(&path, &json)
    }

    /// List all presets for a member (newest first by updated_at)
    pub fn list(&self, member_id: &str) -> Vec<PresetEntry> {
        let presets = self.load_all(member_id);
        let mut list: Vec<PresetEntry> = presets.into_values().collect();
        list.sort_by_key(|p| std::cmp::Reverse(p.updated_at));
        list
    }

    /// Save a new preset (or overwrite existing with same name)
    /// Returns error if at MAX_PRESETS and name is new
    pub fn save(
        &self,
        member_id: &str,
        name: &str,
        channels: HashMap<usize, ChannelPreset>,
    ) -> Result<PresetEntry, PresetError> {
        self.save_with_stems(member_id, name, channels, None, None)
    }

    /// Save a preset with optional stems bus volume
    pub fn save_with_stems(
        &self,
        member_id: &str,
        name: &str,
        channels: HashMap<usize, ChannelPreset>,
        stems_level_db: Option<f32>,
        eq_bands: Option<HashMap<usize, Vec<iem_core::EqBand>>>,
    ) -> Result<PresetEntry, PresetError> {
        let mut presets = self.load_all(member_id);

        // Check limit only for new presets
        if !presets.contains_key(name) && presets.len() >= MAX_PRESETS {
            return Err(PresetError::LimitReached);
        }

        let now = chrono::Utc::now().timestamp();
        let entry = if let Some(existing) = presets.get(name) {
            PresetEntry {
                name: name.to_string(),
                channels,
                created_at: existing.created_at,
                updated_at: now,
                stems_level_db,
                eq_bands,
            }
        } else {
            PresetEntry {
                name: name.to_string(),
                channels,
                created_at: now,
                updated_at: now,
                stems_level_db,
                eq_bands,
            }
        };

        let result = entry.clone();
        presets.insert(name.to_string(), entry);
        self.save_all(member_id, &presets)
            .map_err(PresetError::Io)?;
        Ok(result)
    }

    /// Get a specific preset by name
    pub fn get(&self, member_id: &str, name: &str) -> Option<PresetEntry> {
        self.load_all(member_id).remove(name)
    }

    /// Delete a preset by name
    pub fn delete(&self, member_id: &str, name: &str) -> Result<bool, std::io::Error> {
        let mut presets = self.load_all(member_id);
        if presets.remove(name).is_some() {
            self.save_all(member_id, &presets)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// Errors from preset operations
#[derive(Debug)]
pub enum PresetError {
    /// Maximum number of presets reached
    LimitReached,
    /// IO error during persistence
    Io(std::io::Error),
}

impl std::fmt::Display for PresetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PresetError::LimitReached => write!(
                f,
                "Maximum preset limit ({}) reached. Delete a preset first.",
                MAX_PRESETS
            ),
            PresetError::Io(e) => write!(f, "IO error: {}", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(test_name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "iem_preset_test_{}_{}",
            std::process::id(),
            test_name
        ))
    }

    #[test]
    fn test_empty_store_returns_empty() {
        let dir = temp_dir("empty");
        let _ = std::fs::remove_dir_all(&dir);
        let store = PresetStore::new(&dir);
        assert!(store.list("oldmember1").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_save_and_load_preset() {
        let dir = temp_dir("save_load");
        let _ = std::fs::remove_dir_all(&dir);
        let store = PresetStore::new(&dir);

        let mut channels = HashMap::new();
        channels.insert(
            1,
            ChannelPreset {
                vol: -6.0,
                mute: false,
                pan: 0.0,
            },
        );
        channels.insert(
            3,
            ChannelPreset {
                vol: -12.0,
                mute: true,
                pan: 0.5,
            },
        );

        store
            .save("oldmember1", "rehearsal", channels.clone())
            .unwrap();

        let loaded = store.get("oldmember1", "rehearsal").unwrap();
        assert_eq!(loaded.name, "rehearsal");
        assert_eq!(loaded.channels.len(), 2);
        assert_eq!(loaded.channels[&1], channels[&1]);
        assert_eq!(loaded.channels[&3], channels[&3]);
        assert!(loaded.created_at > 0);
        assert_eq!(loaded.created_at, loaded.updated_at);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_list_presets_sorted_by_updated_at() {
        let dir = temp_dir("list_sorted");
        let _ = std::fs::remove_dir_all(&dir);
        let store = PresetStore::new(&dir);

        // Save two presets — they'll have close timestamps, so manipulate manually
        store.save("oldmember1", "first", HashMap::new()).unwrap();

        // Hack: load, modify timestamps, save directly
        let mut all = store.load_all("oldmember1");
        all.get_mut("first").unwrap().updated_at = 1000;
        store.save_all("oldmember1", &all).unwrap();

        store.save("oldmember1", "second", HashMap::new()).unwrap();

        let list = store.list("oldmember1");
        assert_eq!(list.len(), 2);
        // "second" should be first (newer updated_at)
        assert_eq!(list[0].name, "second");
        assert_eq!(list[1].name, "first");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_delete_preset() {
        let dir = temp_dir("delete");
        let _ = std::fs::remove_dir_all(&dir);
        let store = PresetStore::new(&dir);

        store
            .save("oldmember1", "to_delete", HashMap::new())
            .unwrap();
        assert_eq!(store.list("oldmember1").len(), 1);

        let deleted = store.delete("oldmember1", "to_delete").unwrap();
        assert!(deleted);
        assert!(store.list("oldmember1").is_empty());

        // Deleting non-existent returns false
        let deleted = store.delete("oldmember1", "nonexistent").unwrap();
        assert!(!deleted);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_update_preserves_created_at() {
        let dir = temp_dir("update");
        let _ = std::fs::remove_dir_all(&dir);
        let store = PresetStore::new(&dir);

        let channels1 = HashMap::new();
        let saved = store.save("oldmember1", "test", channels1).unwrap();
        let original_created = saved.created_at;

        // Ensure updated_at will differ
        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut channels2 = HashMap::new();
        channels2.insert(
            1,
            ChannelPreset {
                vol: 0.0,
                mute: false,
                pan: 0.0,
            },
        );
        let updated = store.save("oldmember1", "test", channels2).unwrap();

        assert_eq!(updated.created_at, original_created);
        assert!(updated.updated_at >= updated.created_at);
        assert_eq!(updated.channels.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_preset_name_uniqueness() {
        let dir = temp_dir("uniqueness");
        let _ = std::fs::remove_dir_all(&dir);
        let store = PresetStore::new(&dir);

        let mut ch1 = HashMap::new();
        ch1.insert(
            1,
            ChannelPreset {
                vol: -6.0,
                mute: false,
                pan: 0.0,
            },
        );
        store.save("oldmember1", "same_name", ch1).unwrap();

        let mut ch2 = HashMap::new();
        ch2.insert(
            1,
            ChannelPreset {
                vol: -12.0,
                mute: true,
                pan: 1.0,
            },
        );
        store.save("oldmember1", "same_name", ch2).unwrap();

        // Only one preset with that name
        let list = store.list("oldmember1");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].channels[&1].vol, -12.0);
        assert!(list[0].channels[&1].mute);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_empty_member_returns_empty() {
        let dir = temp_dir("empty_member");
        let _ = std::fs::remove_dir_all(&dir);
        let store = PresetStore::new(&dir);

        // Save for one member, check another
        store.save("oldmember1", "test", HashMap::new()).unwrap();
        assert!(store.list("member3").is_empty());
        assert!(store.get("member3", "test").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_preset_limit() {
        let dir = temp_dir("limit");
        let _ = std::fs::remove_dir_all(&dir);
        let store = PresetStore::new(&dir);

        // Save MAX_PRESETS presets
        for i in 0..MAX_PRESETS {
            store
                .save("oldmember1", &format!("preset_{}", i), HashMap::new())
                .unwrap();
        }

        // Next new preset should fail
        let result = store.save("oldmember1", "one_too_many", HashMap::new());
        assert!(matches!(result, Err(PresetError::LimitReached)));

        // But updating an existing one should work
        let result = store.save("oldmember1", "preset_0", HashMap::new());
        assert!(result.is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_get_nonexistent_returns_none() {
        let dir = temp_dir("get_none");
        let _ = std::fs::remove_dir_all(&dir);
        let store = PresetStore::new(&dir);

        assert!(store.get("oldmember1", "nope").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
