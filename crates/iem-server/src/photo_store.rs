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

    /// The photo's path, or an error for an id that is no member id (never
    /// a panic: a removal gets ids from files on disk).
    fn checked_path(&self, member_id: &str) -> std::io::Result<PathBuf> {
        iem_core::config::validate_member_id(member_id)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        Ok(self.photos_dir.join(format!("{member_id}.jpg")))
    }

    /// Deletes the member's photo with `delete` (a person the roster
    /// removes, #58: `people_removal`'s delete); `false` when there is
    /// none. A bad id is an error, never a panic.
    pub fn remove(
        &self,
        member_id: &str,
        delete: &dyn Fn(&std::path::Path) -> std::io::Result<()>,
    ) -> std::io::Result<bool> {
        let path = self.checked_path(member_id)?;
        if !path.try_exists()? {
            return Ok(false);
        }
        delete(&path)?;
        Ok(true)
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

    /// #58: a removed person's photo is deleted; none is no removal; a bad
    /// id is an error, never a panic.
    #[test]
    fn remove_deletes_the_photo_and_a_bad_id_is_an_error() {
        let (store, _dir) = test_store();
        let delete = |p: &std::path::Path| std::fs::remove_file(p);
        assert!(!store.remove("member1", &delete).unwrap(), "no photo yet");
        store.save("member1", b"jpeg").unwrap();
        store.save("member2", b"jpeg").unwrap();
        assert!(store.remove("member1", &delete).unwrap());
        assert!(!store.exists("member1"));
        assert!(store.exists("member2"));
        assert!(!store.remove("member1", &delete).unwrap(), "gone");
        for bad in ["../x", ""] {
            let err = store.remove(bad, &delete).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn test_overwrite_existing() {
        let (store, _dir) = test_store();
        store.save("oldmember1", b"first").unwrap();
        store.save("oldmember1", b"second").unwrap();
        assert_eq!(store.load("oldmember1").unwrap(), b"second");
    }
}
