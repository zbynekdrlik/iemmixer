//! Snapshot storage with JSON file persistence
//!
//! Stores mix snapshots per member, following the PinStore pattern.
//! Each member has their own JSON file in the snapshots/ directory.

use crate::atomic_write;
use iem_core::{ChannelSnapshot, MAX_SNAPSHOTS, MixSnapshot};
use std::collections::HashMap;
use std::path::PathBuf;

/// Snapshot storage backed by per-member JSON files
pub struct SnapshotStore {
    /// Base directory for snapshot files
    snapshots_dir: PathBuf,
}

impl SnapshotStore {
    /// Create a new snapshot store in the given config directory
    pub fn new(config_dir: &std::path::Path) -> Self {
        let snapshots_dir = config_dir.join("snapshots");
        Self { snapshots_dir }
    }

    /// Get the file path for a member's snapshots.
    /// Panics if member_id contains invalid characters (path traversal protection).
    fn member_path(&self, member_id: &str) -> PathBuf {
        iem_core::config::validate_member_id(member_id)
            .expect("invalid member_id passed to SnapshotStore");
        self.snapshots_dir.join(format!("{}.json", member_id))
    }

    /// Load snapshots for a member (returns empty vec if file doesn't exist)
    pub fn load(&self, member_id: &str) -> Vec<MixSnapshot> {
        let path = self.member_path(member_id);
        if !path.exists() {
            return Vec::new();
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Save snapshots for a member
    fn save(&self, member_id: &str, snapshots: &[MixSnapshot]) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(&self.snapshots_dir)?;
        let path = self.member_path(member_id);
        let json = serde_json::to_string_pretty(snapshots).map_err(std::io::Error::other)?;
        atomic_write(&path, &json)
    }

    /// List all snapshots for a member (newest first)
    pub fn list(&self, member_id: &str) -> Vec<MixSnapshot> {
        let mut snapshots = self.load(member_id);
        snapshots.sort_by_key(|s| std::cmp::Reverse(s.timestamp));
        snapshots
    }

    /// Save a new snapshot for a member
    /// Automatically prunes oldest unpinned snapshots if over MAX_SNAPSHOTS
    pub fn save_snapshot(
        &self,
        member_id: &str,
        snapshot: MixSnapshot,
    ) -> Result<(), std::io::Error> {
        let mut snapshots = self.load(member_id);
        snapshots.push(snapshot);

        // Prune if over limit
        self.prune(&mut snapshots);

        self.save(member_id, &snapshots)
    }

    /// Delete a snapshot by timestamp
    pub fn delete_snapshot(&self, member_id: &str, timestamp: i64) -> Result<bool, std::io::Error> {
        let mut snapshots = self.load(member_id);
        let original_len = snapshots.len();
        snapshots.retain(|s| s.timestamp != timestamp);
        if snapshots.len() < original_len {
            self.save(member_id, &snapshots)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Pin a snapshot with a label
    pub fn pin_snapshot(
        &self,
        member_id: &str,
        timestamp: i64,
        label: String,
    ) -> Result<bool, std::io::Error> {
        let mut snapshots = self.load(member_id);
        let mut found = false;
        for s in &mut snapshots {
            if s.timestamp == timestamp {
                s.pinned = true;
                s.label = label;
                found = true;
                break;
            }
        }
        if found {
            self.save(member_id, &snapshots)?;
        }
        Ok(found)
    }

    /// Unpin a snapshot
    pub fn unpin_snapshot(&self, member_id: &str, timestamp: i64) -> Result<bool, std::io::Error> {
        let mut snapshots = self.load(member_id);
        let mut found = false;
        for s in &mut snapshots {
            if s.timestamp == timestamp {
                s.pinned = false;
                found = true;
                break;
            }
        }
        if found {
            self.save(member_id, &snapshots)?;
        }
        Ok(found)
    }

    /// Get a specific snapshot by timestamp
    pub fn get_snapshot(&self, member_id: &str, timestamp: i64) -> Option<MixSnapshot> {
        self.load(member_id)
            .into_iter()
            .find(|s| s.timestamp == timestamp)
    }

    /// Prune oldest unpinned snapshots to stay within MAX_SNAPSHOTS
    fn prune(&self, snapshots: &mut Vec<MixSnapshot>) {
        if snapshots.len() <= MAX_SNAPSHOTS {
            return;
        }

        // Sort by timestamp (oldest first) for pruning
        snapshots.sort_by_key(|s| s.timestamp);

        // Keep removing oldest unpinned until under limit
        while snapshots.len() > MAX_SNAPSHOTS {
            if let Some(idx) = snapshots.iter().position(|s| !s.pinned) {
                snapshots.remove(idx);
            } else {
                // All remaining are pinned, can't prune further
                break;
            }
        }
    }

    /// Check if a snapshot for today already exists for this member
    pub fn has_snapshot_today(&self, member_id: &str) -> bool {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        self.load(member_id).iter().any(|s| {
            let snapshot_date = chrono::DateTime::from_timestamp(s.timestamp, 0)
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_default();
            snapshot_date == today && s.label == "auto"
        })
    }

    /// Create channels snapshot from current mixer state
    pub fn channels_from_state(channels: &[iem_core::Channel]) -> HashMap<usize, ChannelSnapshot> {
        channels
            .iter()
            .map(|ch| {
                // Convert dB to REAPER linear scale (same as db_to_reaper_vol in proxy.rs)
                let vol = if ch.level_db <= -60.0 {
                    0.0
                } else {
                    10.0_f32.powf(ch.level_db / 20.0).clamp(0.0, 4.0)
                };
                (
                    ch.track_index,
                    ChannelSnapshot {
                        vol,
                        mute: ch.muted,
                        pan: ch.pan,
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(test_name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "iem_snapshot_test_{}_{}",
            std::process::id(),
            test_name
        ))
    }

    /// reaperiem#203: capture stores the UI-range pan verbatim; the restore path must
    /// convert it back to REAPER range. This exercises the real capture
    /// function `channels_from_state` end-to-end with `restore_send_pan`.
    #[test]
    fn test_capture_restore_pan_roundtrip() {
        for reaper_pan in [-1.0_f32, -0.5, 0.0, 0.5, 1.0] {
            // The poller stores UI-range pan (reaper_pan_to_ui).
            let ui_pan = crate::proxy::reaper_pan_to_ui(reaper_pan);
            let channels = vec![iem_core::Channel {
                track_index: 5,
                name: "TEST".to_string(),
                level_db: -6.0,
                pan: ui_pan,
                muted: false,
                category: "mics".to_string(),
                stereo_pair: None,
                stereo_side: None,
            }];
            let snap = SnapshotStore::channels_from_state(&channels);
            let stored = snap.get(&5).expect("track 5 captured");
            // Capture keeps pan in UI range (0..1) unchanged.
            assert!(
                (stored.pan - ui_pan).abs() < 0.0001,
                "capture must keep UI-range pan verbatim"
            );
            // Restore converts it back to the original REAPER value.
            let restored = crate::proxy::restore_send_pan(stored.pan);
            assert!(
                (restored - reaper_pan).abs() < 0.001,
                "capture->store->restore must round-trip REAPER {} (got {}) — #203",
                reaper_pan,
                restored
            );
        }
    }

    #[test]
    fn test_empty_store_returns_empty() {
        let dir = temp_dir("empty");
        let _ = std::fs::remove_dir_all(&dir);
        let store = SnapshotStore::new(&dir);
        assert!(store.list("oldmember1").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_save_and_list() {
        let dir = temp_dir("save_list");
        let _ = std::fs::remove_dir_all(&dir);
        let store = SnapshotStore::new(&dir);

        let mut channels = HashMap::new();
        channels.insert(
            1,
            ChannelSnapshot {
                vol: 0.5,
                mute: false,
                pan: 0.0,
            },
        );

        let snapshot = MixSnapshot::new_manual("test".to_string(), channels, None);
        store.save_snapshot("oldmember1", snapshot).unwrap();

        let list = store.list("oldmember1");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].label, "test");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_delete_snapshot() {
        let dir = temp_dir("delete");
        let _ = std::fs::remove_dir_all(&dir);
        let store = SnapshotStore::new(&dir);

        let snapshot = MixSnapshot::new_manual("to_delete".to_string(), HashMap::new(), None);
        let timestamp = snapshot.timestamp;
        store.save_snapshot("oldmember1", snapshot).unwrap();

        assert_eq!(store.list("oldmember1").len(), 1);
        store.delete_snapshot("oldmember1", timestamp).unwrap();
        assert!(store.list("oldmember1").is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_pin_unpin() {
        let dir = temp_dir("pin");
        let _ = std::fs::remove_dir_all(&dir);
        let store = SnapshotStore::new(&dir);

        let snapshot = MixSnapshot::new_auto(HashMap::new(), None);
        let timestamp = snapshot.timestamp;
        store.save_snapshot("oldmember1", snapshot).unwrap();

        // Pin
        store
            .pin_snapshot("oldmember1", timestamp, "important".to_string())
            .unwrap();
        let list = store.list("oldmember1");
        assert!(list[0].pinned);
        assert_eq!(list[0].label, "important");

        // Unpin
        store.unpin_snapshot("oldmember1", timestamp).unwrap();
        let list = store.list("oldmember1");
        assert!(!list[0].pinned);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_pruning() {
        let dir = temp_dir("pruning");
        let _ = std::fs::remove_dir_all(&dir);
        let store = SnapshotStore::new(&dir);

        // Add MAX_SNAPSHOTS + 5 snapshots
        for i in 0..(MAX_SNAPSHOTS + 5) {
            let mut snapshot = MixSnapshot::new_auto(HashMap::new(), None);
            snapshot.timestamp = i as i64;
            store.save_snapshot("oldmember1", snapshot).unwrap();
        }

        let list = store.list("oldmember1");
        assert_eq!(list.len(), MAX_SNAPSHOTS);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_pinned_survives_pruning() {
        let dir = temp_dir("pinned_survives");
        let _ = std::fs::remove_dir_all(&dir);
        let store = SnapshotStore::new(&dir);

        // Add a pinned snapshot with old timestamp
        let mut pinned = MixSnapshot::new_auto(HashMap::new(), None);
        pinned.timestamp = 1;
        pinned.pinned = true;
        pinned.label = "pinned".to_string();
        store.save_snapshot("oldmember1", pinned).unwrap();

        // Add enough to trigger pruning
        for i in 2..=(MAX_SNAPSHOTS + 5) {
            let mut snapshot = MixSnapshot::new_auto(HashMap::new(), None);
            snapshot.timestamp = i as i64;
            store.save_snapshot("oldmember1", snapshot).unwrap();
        }

        let list = store.list("oldmember1");
        // Pinned snapshot should still be there
        assert!(list.iter().any(|s| s.label == "pinned" && s.pinned));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
