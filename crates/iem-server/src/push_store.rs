//! Push subscription persistence for Web Push notifications (reaperiem#133)

use serde::{Deserialize, Serialize};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

/// The engineer's subscriptions, next to the site file.
const FILE: &str = "push_subscriptions.json";
/// Present once the one-time cleanup of [`PushStore::load`] has run.
const MARKER: &str = "push_subs_v2_migrated";

/// `e`, of the same kind, naming `path`.
fn at(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PushSubscription {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

pub struct PushStore {
    subscriptions: Vec<PushSubscription>,
    path: PathBuf,
}

impl PushStore {
    /// Load the push-subscription store from disk. On first start after the
    /// 1.164.0 deploy, migrates the store by wiping any orphan subscriptions
    /// left over from before the unsubscribe-on-logout fix (reaperiem#188). The
    /// migration is gated by a marker file `push_subs_v2_migrated` and runs
    /// at most once per host.
    pub fn load(config_dir: &Path) -> Self {
        let path = config_dir.join(FILE);
        let marker_path = config_dir.join(MARKER);

        let migrate = !marker_path.exists();

        let subscriptions = if migrate {
            // Wipe any existing orphan subscriptions then write a clean file.
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = crate::atomic_write(&path, "[]");
            // Best-effort marker creation; failure is logged but non-fatal —
            // worst case the migration runs again on the next start (still
            // safe — the store is already empty).
            if let Err(e) = std::fs::write(&marker_path, b"") {
                eprintln!(
                    "WARN: push_store: failed to write migration marker {}: {}",
                    marker_path.display(),
                    e
                );
            }
            Vec::new()
        } else if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        Self {
            subscriptions,
            path,
        }
    }

    /// The subscriptions [`PushStore::load`] would find, without its write
    /// (notify mode only reads): none before the one-time cleanup ran (no
    /// marker: the server's first start empties the list), none without the
    /// file. A marker or file that cannot be read, or a file that cannot be
    /// parsed, is an error, never taken for "none".
    pub fn read(config_dir: &Path) -> io::Result<Vec<PushSubscription>> {
        let marker = config_dir.join(MARKER);
        match marker.try_exists() {
            Ok(true) => {}
            Ok(false) => return Ok(Vec::new()),
            Err(e) => return Err(at(&marker, e)),
        }
        let path = config_dir.join(FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| at(&path, io::Error::new(ErrorKind::InvalidData, e))),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(at(&path, e)),
        }
    }

    pub fn all(&self) -> &[PushSubscription] {
        &self.subscriptions
    }

    /// Add or update a subscription (dedup by endpoint URL).
    pub fn add(&mut self, sub: PushSubscription) -> Result<(), std::io::Error> {
        if let Some(existing) = self
            .subscriptions
            .iter_mut()
            .find(|s| s.endpoint == sub.endpoint)
        {
            existing.p256dh = sub.p256dh;
            existing.auth = sub.auth;
        } else {
            self.subscriptions.push(sub);
        }
        self.save()
    }

    /// Remove a subscription by endpoint (called when push returns 404/410).
    pub fn remove_endpoint(&mut self, endpoint: &str) {
        self.subscriptions.retain(|s| s.endpoint != endpoint);
        let _ = self.save();
    }

    fn save(&self) -> Result<(), std::io::Error> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json =
            serde_json::to_string_pretty(&self.subscriptions).map_err(std::io::Error::other)?;
        crate::atomic_write(&self.path, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_store_crud() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = PushStore::load(dir.path());
        assert!(store.all().is_empty());

        let sub = PushSubscription {
            endpoint: "https://fcm.googleapis.com/fcm/send/abc123".into(),
            p256dh: "BPK_key".into(),
            auth: "auth_secret".into(),
        };

        store.add(sub.clone()).unwrap();
        assert_eq!(store.all().len(), 1);

        // Dedup by endpoint
        let sub2 = PushSubscription {
            endpoint: "https://fcm.googleapis.com/fcm/send/abc123".into(),
            p256dh: "new_key".into(),
            auth: "new_auth".into(),
        };
        store.add(sub2).unwrap();
        assert_eq!(store.all().len(), 1);
        assert_eq!(store.all()[0].p256dh, "new_key");

        store.remove_endpoint("https://fcm.googleapis.com/fcm/send/abc123");
        assert!(store.all().is_empty());
    }

    #[test]
    fn test_push_store_persistence() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut store = PushStore::load(dir.path());
            store
                .add(PushSubscription {
                    endpoint: "https://example.com/push/1".into(),
                    p256dh: "key1".into(),
                    auth: "auth1".into(),
                })
                .unwrap();
        }
        let store = PushStore::load(dir.path());
        assert_eq!(store.all().len(), 1);
        assert_eq!(store.all()[0].endpoint, "https://example.com/push/1");
    }

    #[test]
    fn test_migration_runs_when_marker_missing_and_file_has_data() {
        let dir = tempfile::tempdir().unwrap();
        // Pre-seed the store with an orphan subscription as if from before the fix.
        let pre = serde_json::to_string(&vec![PushSubscription {
            endpoint: "https://orphan.example/push/abc".into(),
            p256dh: "orphan_key".into(),
            auth: "orphan_auth".into(),
        }])
        .unwrap();
        std::fs::write(dir.path().join("push_subscriptions.json"), &pre).unwrap();
        assert!(!dir.path().join("push_subs_v2_migrated").exists());

        let store = PushStore::load(dir.path());

        // Subscriptions wiped, marker created, file rewritten as `[]`.
        assert!(store.all().is_empty(), "subscriptions should be wiped");
        assert!(
            dir.path().join("push_subs_v2_migrated").exists(),
            "marker file must be created"
        );
        let on_disk = std::fs::read_to_string(dir.path().join("push_subscriptions.json")).unwrap();
        assert_eq!(on_disk.trim(), "[]");
    }

    #[test]
    fn test_migration_runs_when_marker_missing_and_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!dir.path().join("push_subscriptions.json").exists());
        assert!(!dir.path().join("push_subs_v2_migrated").exists());

        let store = PushStore::load(dir.path());

        assert!(store.all().is_empty());
        assert!(dir.path().join("push_subs_v2_migrated").exists());
        // File written by migration as empty array.
        let on_disk = std::fs::read_to_string(dir.path().join("push_subscriptions.json")).unwrap();
        assert_eq!(on_disk.trim(), "[]");
    }

    #[test]
    fn test_migration_skipped_when_marker_present() {
        let dir = tempfile::tempdir().unwrap();
        // Marker pre-created — migration must NOT run.
        std::fs::write(dir.path().join("push_subs_v2_migrated"), b"").unwrap();
        // Pre-existing valid subscription must be preserved.
        let pre = serde_json::to_string(&vec![PushSubscription {
            endpoint: "https://legit.example/push/xyz".into(),
            p256dh: "legit_key".into(),
            auth: "legit_auth".into(),
        }])
        .unwrap();
        std::fs::write(dir.path().join("push_subscriptions.json"), &pre).unwrap();

        let store = PushStore::load(dir.path());

        assert_eq!(store.all().len(), 1);
        assert_eq!(store.all()[0].endpoint, "https://legit.example/push/xyz");
    }

    /// Notify mode's view of the store (the guard's alarms go to these
    /// subscriptions, #9 2026-09-28): what `load` would find, nothing written.
    #[test]
    fn read_finds_what_load_would_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (file, marker) = (dir.path().join(FILE), dir.path().join(MARKER));
        let sub = PushSubscription {
            endpoint: "https://push.example/engineer".into(),
            p256dh: "k".into(),
            auth: "a".into(),
        };
        let one = serde_json::to_string(&vec![sub.clone()]).unwrap();
        // Nothing at all: none, and neither marker nor file is made.
        assert!(PushStore::read(dir.path()).unwrap().is_empty());
        assert!(!marker.exists());
        assert!(!file.exists());
        // A list without the marker is emptied by the server's first start:
        // none, and the list stays as it is.
        std::fs::write(&file, &one).unwrap();
        assert!(PushStore::read(dir.path()).unwrap().is_empty());
        assert!(!marker.exists());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), one);
        // With the marker: the list, as `load` finds it.
        std::fs::write(&marker, b"").unwrap();
        assert_eq!(PushStore::read(dir.path()).unwrap(), vec![sub.clone()]);
        assert_eq!(PushStore::load(dir.path()).all(), [sub]);
        // The marker without the file: none, and neither is touched.
        std::fs::remove_file(&file).unwrap();
        assert!(PushStore::read(dir.path()).unwrap().is_empty());
        assert!(!file.exists());
        assert!(marker.exists());
    }

    #[test]
    fn read_never_takes_an_unreadable_list_for_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        std::fs::write(dir.path().join(MARKER), b"").unwrap();
        std::fs::write(&file, "[oops").unwrap();
        let err = PushStore::read(dir.path()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(err.to_string().contains(FILE), "{err}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "[oops");
        // Something that is not a readable file is an error too.
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(PushStore::read(dir.path()).is_err());
    }
}
