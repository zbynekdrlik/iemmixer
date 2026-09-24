//! Channel customization storage (pin/hide preferences)
//!
//! Stores per-member channel customization with JSON file persistence.
//! Server-side storage ensures preferences follow the member to any device.

use crate::atomic_write;
use iem_core::Customization;
use std::path::PathBuf;

/// Channel customization storage backed by per-member JSON files
pub struct CustomizationStore {
    /// Base directory for customization files
    customizations_dir: PathBuf,
}

impl CustomizationStore {
    /// Create a new customization store in the given config directory
    pub fn new(config_dir: &std::path::Path) -> Self {
        let customizations_dir = config_dir.join("customizations");
        Self { customizations_dir }
    }

    /// Get the file path for a member's customization.
    /// Panics if member_id contains invalid characters (path traversal protection).
    fn member_path(&self, member_id: &str) -> PathBuf {
        iem_core::config::validate_member_id(member_id)
            .expect("invalid member_id passed to CustomizationStore");
        self.customizations_dir.join(format!("{}.json", member_id))
    }

    /// Load customization for a member (returns default if file doesn't exist)
    pub fn load(&self, member_id: &str) -> Customization {
        let path = self.member_path(member_id);
        if !path.exists() {
            return Customization::default();
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Save customization for a member
    pub fn save(
        &self,
        member_id: &str,
        customization: &Customization,
    ) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(&self.customizations_dir)?;
        let path = self.member_path(member_id);
        let json = serde_json::to_string_pretty(customization).map_err(std::io::Error::other)?;
        atomic_write(&path, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(test_name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "iem_cust_test_{}_{}",
            std::process::id(),
            test_name
        ))
    }

    #[test]
    fn test_empty_store_returns_default() {
        let dir = temp_dir("empty");
        let _ = std::fs::remove_dir_all(&dir);
        let store = CustomizationStore::new(&dir);

        let cust = store.load("oldmember1");
        assert!(cust.pinned.is_empty());
        assert!(cust.hidden.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = temp_dir("roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        let store = CustomizationStore::new(&dir);

        let cust = Customization {
            pinned: vec![1, 5, 8],
            hidden: vec![3, 7, 12],
        };
        store.save("oldmember1", &cust).unwrap();

        let loaded = store.load("oldmember1");
        assert_eq!(loaded.pinned, vec![1, 5, 8]);
        assert_eq!(loaded.hidden, vec![3, 7, 12]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_different_members_independent() {
        let dir = temp_dir("independent");
        let _ = std::fs::remove_dir_all(&dir);
        let store = CustomizationStore::new(&dir);

        let cust_oldmember1 = Customization {
            pinned: vec![1],
            hidden: vec![],
        };
        let cust_member3 = Customization {
            pinned: vec![],
            hidden: vec![5],
        };
        store.save("oldmember1", &cust_oldmember1).unwrap();
        store.save("member3", &cust_member3).unwrap();

        assert_eq!(store.load("oldmember1").pinned, vec![1]);
        assert!(store.load("oldmember1").hidden.is_empty());
        assert!(store.load("member3").pinned.is_empty());
        assert_eq!(store.load("member3").hidden, vec![5]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_overwrite_existing() {
        let dir = temp_dir("overwrite");
        let _ = std::fs::remove_dir_all(&dir);
        let store = CustomizationStore::new(&dir);

        let cust1 = Customization {
            pinned: vec![1, 2],
            hidden: vec![3],
        };
        store.save("oldmember1", &cust1).unwrap();

        let cust2 = Customization {
            pinned: vec![5],
            hidden: vec![],
        };
        store.save("oldmember1", &cust2).unwrap();

        let loaded = store.load("oldmember1");
        assert_eq!(loaded.pinned, vec![5]);
        assert!(loaded.hidden.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_empty_customization_serialization() {
        let dir = temp_dir("empty_ser");
        let _ = std::fs::remove_dir_all(&dir);
        let store = CustomizationStore::new(&dir);

        let cust = Customization::default();
        store.save("oldmember1", &cust).unwrap();

        let loaded = store.load("oldmember1");
        assert_eq!(loaded, Customization::default());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
