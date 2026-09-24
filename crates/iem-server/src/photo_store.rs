//! Profile photo storage (per-member JPEG files)
//!
//! Stores member profile photos as flat JPEG files in {config_dir}/photos/.

use std::path::PathBuf;

/// Profile photo storage backed by per-member JPEG files
pub struct PhotoStore {
    photos_dir: PathBuf,
}

impl PhotoStore {
    pub fn new(config_dir: &std::path::Path) -> Self {
        let photos_dir = config_dir.join("photos");
        Self { photos_dir }
    }

    fn member_path(&self, member_id: &str) -> PathBuf {
        iem_core::config::validate_member_id(member_id)
            .expect("invalid member_id passed to PhotoStore");
        self.photos_dir.join(format!("{}.jpg", member_id))
    }

    pub fn save(&self, member_id: &str, data: &[u8]) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.photos_dir)?;
        let path = self.member_path(member_id);
        std::fs::write(&path, data)
    }

    pub fn load(&self, member_id: &str) -> Option<Vec<u8>> {
        let path = self.member_path(member_id);
        std::fs::read(&path).ok()
    }

    pub fn delete(&self, member_id: &str) -> std::io::Result<()> {
        let path = self.member_path(member_id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn exists(&self, member_id: &str) -> bool {
        self.member_path(member_id).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (PhotoStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let store = PhotoStore::new(dir.path());
        (store, dir)
    }

    #[test]
    fn test_load_nonexistent_returns_none() {
        let (store, _dir) = test_store();
        assert!(store.load("oldmember1").is_none());
    }

    #[test]
    fn test_exists_false_before_save() {
        let (store, _dir) = test_store();
        assert!(!store.exists("oldmember1"));
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let (store, _dir) = test_store();
        let data = b"fake jpeg data";
        store.save("oldmember1", data).unwrap();
        assert_eq!(store.load("oldmember1").unwrap(), data);
    }

    #[test]
    fn test_exists_true_after_save() {
        let (store, _dir) = test_store();
        store.save("oldmember1", b"data").unwrap();
        assert!(store.exists("oldmember1"));
    }

    #[test]
    fn test_delete_removes_file() {
        let (store, _dir) = test_store();
        store.save("oldmember1", b"data").unwrap();
        store.delete("oldmember1").unwrap();
        assert!(!store.exists("oldmember1"));
        assert!(store.load("oldmember1").is_none());
    }

    #[test]
    fn test_delete_nonexistent_ok() {
        let (store, _dir) = test_store();
        assert!(store.delete("oldmember1").is_ok());
    }

    #[test]
    fn test_overwrite_existing() {
        let (store, _dir) = test_store();
        store.save("oldmember1", b"first").unwrap();
        store.save("oldmember1", b"second").unwrap();
        assert_eq!(store.load("oldmember1").unwrap(), b"second");
    }
}
