//! Runtime PIN storage with JSON persistence
//!
//! Stores custom PINs set by band members in pins.json alongside the config.
//! Takes priority over config.yaml pins and the default PIN.

use crate::atomic_write;
use std::collections::HashMap;
use std::path::PathBuf;

/// Runtime PIN storage backed by a JSON file
pub struct PinStore {
    pins: HashMap<String, String>,
    path: PathBuf,
}

impl PinStore {
    /// Load from pins.json in the given directory (or create empty if not found)
    pub fn load(config_dir: &std::path::Path) -> Self {
        let path = config_dir.join("pins.json");
        let pins = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            HashMap::new()
        };
        Self { pins, path }
    }

    /// Get custom PIN for a member (None = use config/default)
    pub fn get_pin(&self, member_id: &str) -> Option<&str> {
        self.pins.get(member_id).map(|s| s.as_str())
    }

    /// Get all stored PINs (for backup)
    pub fn all_pins(&self) -> HashMap<String, String> {
        self.pins.clone()
    }

    /// Set a new PIN for a member and persist to disk.
    /// Returns error if member_id contains invalid characters.
    pub fn set_pin(&mut self, member_id: &str, pin: &str) -> Result<(), std::io::Error> {
        iem_core::config::validate_member_id(member_id).map_err(std::io::Error::other)?;
        self.pins.insert(member_id.to_string(), pin.to_string());
        self.save()
    }

    fn save(&self) -> Result<(), std::io::Error> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.pins).map_err(std::io::Error::other)?;
        atomic_write(&self.path, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_store_returns_none() {
        let dir = std::env::temp_dir().join("iem_test_empty_store");
        let _ = std::fs::remove_dir_all(&dir);
        let store = PinStore::load(&dir);
        assert!(store.get_pin("oldmember1").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_set_and_get_pin() {
        let dir = std::env::temp_dir().join("iem_test_set_get");
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = PinStore::load(&dir);
        store.set_pin("oldmember1", "1234").unwrap();
        assert_eq!(store.get_pin("oldmember1"), Some("1234"));
        assert!(store.get_pin("member3").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_save_and_reload() {
        let dir = std::env::temp_dir().join("iem_test_save_reload");
        let _ = std::fs::remove_dir_all(&dir);

        // Save a PIN
        {
            let mut store = PinStore::load(&dir);
            store.set_pin("oldmember1", "5678").unwrap();
        }

        // Reload from disk
        let store = PinStore::load(&dir);
        assert_eq!(store.get_pin("oldmember1"), Some("5678"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
