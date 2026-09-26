//! Argon2id PIN hashes (program spec §5.3): `<secrets>/pin_hashes.json`.
//! Never plaintext: every stored value must be an argon2id PHC string, and a
//! file that holds anything else is a load error instead of being ignored.
//! The predecessor's plaintext `pins.json` is never read here; the S4
//! migration (`band_import`) hashes its values with `import_*_hash`, which
//! marks them `imported`. Setting a PIN in iemmixer drops the mark, so a
//! later import never overwrites an iemmixer-set PIN (program spec §3.4).

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::atomic_write;

/// File name inside the secrets directory.
pub const PIN_HASHES_FILE: &str = "pin_hashes.json";
/// Member id of the engineer; its PIN is the engineer PIN.
pub const ENGINEER_ID: &str = "engineer";

#[derive(Debug, Default, Serialize, Deserialize)]
struct PinFile {
    #[serde(default)]
    engineer: Option<String>,
    #[serde(default)]
    members: BTreeMap<String, String>,
    /// Owners (member ids, `engineer`) whose hash came from an import of the
    /// predecessor's PIN and was not changed in iemmixer since.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    imported: BTreeSet<String>,
}

/// Engineer and member PIN hashes, persisted atomically.
#[derive(Debug)]
pub struct PinStore {
    file: PinFile,
    path: PathBuf,
}

fn check_phc(owner: &str, phc: &str) -> io::Result<()> {
    if phc.starts_with("$argon2id$") {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the PIN entry for {owner} is not an argon2id hash"),
        ))
    }
}

fn check_member(member_id: &str) -> io::Result<()> {
    iem_core::config::validate_member_id(member_id)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    if member_id == ENGINEER_ID {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the engineer PIN is stored with set_engineer_hash",
        ));
    }
    Ok(())
}

impl PinStore {
    /// Load from `secrets_dir` (empty when the file does not exist yet).
    pub fn load(secrets_dir: &Path) -> io::Result<Self> {
        let path = secrets_dir.join(PIN_HASHES_FILE);
        let file = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<PinFile>(&text).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: {e}", path.display()),
                )
            })?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => PinFile::default(),
            Err(e) => return Err(e),
        };
        if let Some(phc) = &file.engineer {
            check_phc(ENGINEER_ID, phc)?;
        }
        for (member, phc) in &file.members {
            check_phc(member, phc)?;
        }
        Ok(Self { file, path })
    }

    pub fn engineer_hash(&self) -> Option<&str> {
        self.file.engineer.as_deref()
    }

    pub fn member_hash(&self, member_id: &str) -> Option<&str> {
        self.file.members.get(member_id).map(String::as_str)
    }

    /// Whether any PIN hash (engineer or member) is stored.
    pub fn has_hashes(&self) -> bool {
        self.file.engineer.is_some() || !self.file.members.is_empty()
    }

    /// Whether `owner`'s hash (a member id or `engineer`) came from an import.
    pub fn is_imported(&self, owner: &str) -> bool {
        self.file.imported.contains(owner)
    }

    pub fn set_engineer_hash(&mut self, phc: String) -> io::Result<()> {
        check_phc(ENGINEER_ID, &phc)?;
        self.file.engineer = Some(phc);
        self.file.imported.remove(ENGINEER_ID);
        self.save()
    }

    pub fn set_member_hash(&mut self, member_id: &str, phc: String) -> io::Result<()> {
        check_member(member_id)?;
        check_phc(member_id, &phc)?;
        self.file.members.insert(member_id.to_string(), phc);
        self.file.imported.remove(member_id);
        self.save()
    }

    /// Stores an imported engineer PIN hash unless an iemmixer-set one exists
    /// (then `false`, nothing written).
    pub fn import_engineer_hash(&mut self, phc: String) -> io::Result<bool> {
        check_phc(ENGINEER_ID, &phc)?;
        if self.file.engineer.is_some() && !self.is_imported(ENGINEER_ID) {
            return Ok(false);
        }
        self.file.engineer = Some(phc);
        self.file.imported.insert(ENGINEER_ID.to_string());
        self.save()?;
        Ok(true)
    }

    /// Stores an imported member PIN hash unless an iemmixer-set one exists
    /// (then `false`, nothing written).
    pub fn import_member_hash(&mut self, member_id: &str, phc: String) -> io::Result<bool> {
        check_member(member_id)?;
        check_phc(member_id, &phc)?;
        if self.file.members.contains_key(member_id) && !self.is_imported(member_id) {
            return Ok(false);
        }
        self.file.members.insert(member_id.to_string(), phc);
        self.file.imported.insert(member_id.to_string());
        self.save()?;
        Ok(true)
    }

    fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.file).map_err(io::Error::other)?;
        atomic_write(&self.path, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pin_hash::{PEPPER_LEN, PinHasher};

    fn hasher() -> PinHasher {
        PinHasher::for_tests([1u8; PEPPER_LEN])
    }

    #[test]
    fn an_empty_directory_has_no_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let store = PinStore::load(dir.path()).unwrap();
        assert!(store.engineer_hash().is_none());
        assert!(store.member_hash("member1").is_none());
    }

    #[test]
    fn has_hashes_counts_the_engineer_and_the_members() {
        let h = hasher();
        let empty = tempfile::tempdir().unwrap();
        assert!(!PinStore::load(empty.path()).unwrap().has_hashes());

        let engineer = tempfile::tempdir().unwrap();
        let mut store = PinStore::load(engineer.path()).unwrap();
        store.set_engineer_hash(h.hash("2468")).unwrap();
        assert!(store.has_hashes());

        let member = tempfile::tempdir().unwrap();
        let mut store = PinStore::load(member.path()).unwrap();
        store.set_member_hash("member1", h.hash("1357")).unwrap();
        assert!(store.has_hashes());
    }

    #[test]
    fn hashes_survive_a_reload_and_never_contain_the_pin() {
        let dir = tempfile::tempdir().unwrap();
        let h = hasher();
        {
            let mut store = PinStore::load(dir.path()).unwrap();
            store.set_engineer_hash(h.hash("2468")).unwrap();
            store.set_member_hash("member1", h.hash("1357")).unwrap();
        }
        let store = PinStore::load(dir.path()).unwrap();
        assert!(h.verify("2468", store.engineer_hash().unwrap()));
        assert!(h.verify("1357", store.member_hash("member1").unwrap()));
        let text = std::fs::read_to_string(dir.path().join(PIN_HASHES_FILE)).unwrap();
        assert!(!text.contains("\"2468\"") && !text.contains("\"1357\""));
    }

    #[test]
    fn a_plaintext_value_is_a_load_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(PIN_HASHES_FILE),
            r#"{"members":{"member1":"1357"}}"#,
        )
        .unwrap();
        assert_eq!(
            PinStore::load(dir.path()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn corrupt_json_is_a_load_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PIN_HASHES_FILE), "{not json").unwrap();
        assert_eq!(
            PinStore::load(dir.path()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn an_unreadable_store_is_a_load_error_not_an_empty_store() {
        // Only a missing file means "no PINs yet".
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(PIN_HASHES_FILE)).unwrap();
        assert!(PinStore::load(dir.path()).is_err());
    }

    #[test]
    fn the_predecessor_plaintext_file_is_never_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pins.json"), r#"{"member1":"1357"}"#).unwrap();
        assert!(
            PinStore::load(dir.path())
                .unwrap()
                .member_hash("member1")
                .is_none()
        );
    }

    #[test]
    fn the_engineer_is_not_a_member_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = PinStore::load(dir.path()).unwrap();
        let err = store
            .set_member_hash(ENGINEER_ID, hasher().hash("2468"))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn member_ids_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = PinStore::load(dir.path()).unwrap();
        assert!(
            store
                .set_member_hash("../x", hasher().hash("2468"))
                .is_err()
        );
    }

    #[test]
    fn non_phc_values_are_refused_on_write() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = PinStore::load(dir.path()).unwrap();
        assert!(
            store
                .set_member_hash("member1", "1357".to_string())
                .is_err()
        );
        assert!(store.set_engineer_hash("2468".to_string()).is_err());
        assert!(!dir.path().join(PIN_HASHES_FILE).exists());
    }

    #[test]
    fn a_file_without_import_marks_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let h = hasher();
        let text = format!(r#"{{"members":{{"member1":"{}"}}}}"#, h.hash("1357"));
        std::fs::write(dir.path().join(PIN_HASHES_FILE), text).unwrap();
        let store = PinStore::load(dir.path()).unwrap();
        assert!(store.member_hash("member1").is_some());
        assert!(!store.is_imported("member1"));
    }

    #[test]
    fn imports_fill_missing_hashes_and_replace_only_imported_ones() {
        let dir = tempfile::tempdir().unwrap();
        let h = hasher();
        let mut store = PinStore::load(dir.path()).unwrap();
        assert!(store.import_member_hash("member1", h.hash("1111")).unwrap());
        assert!(store.import_engineer_hash(h.hash("9999")).unwrap());
        assert!(store.is_imported("member1") && store.is_imported(ENGINEER_ID));
        let text = std::fs::read_to_string(dir.path().join(PIN_HASHES_FILE)).unwrap();
        assert!(text.contains("\"imported\""), "{text}");
        // A newer predecessor PIN replaces an imported hash.
        let mut store = PinStore::load(dir.path()).unwrap();
        assert!(store.import_member_hash("member1", h.hash("2222")).unwrap());
        assert!(h.verify("2222", store.member_hash("member1").unwrap()));
        assert!(
            store
                .import_member_hash("engineer", h.hash("1234"))
                .is_err()
        );
        assert!(store.import_member_hash("a b", h.hash("1234")).is_err());
        assert!(store.import_member_hash("member2", "1234".into()).is_err());
        assert!(store.import_engineer_hash("1234".into()).is_err());
    }

    #[test]
    fn a_later_import_never_overwrites_an_iemmixer_set_pin() {
        let dir = tempfile::tempdir().unwrap();
        let h = hasher();
        let mut store = PinStore::load(dir.path()).unwrap();
        store.import_member_hash("member1", h.hash("1111")).unwrap();
        store.import_engineer_hash(h.hash("9999")).unwrap();
        // The member changes the PIN in iemmixer; the engineer is reset.
        store.set_member_hash("member1", h.hash("3333")).unwrap();
        store.set_engineer_hash(h.hash("8888")).unwrap();
        assert!(!store.is_imported("member1") && !store.is_imported(ENGINEER_ID));
        let mut store = PinStore::load(dir.path()).unwrap();
        assert!(!store.import_member_hash("member1", h.hash("1111")).unwrap());
        assert!(!store.import_engineer_hash(h.hash("9999")).unwrap());
        let store = PinStore::load(dir.path()).unwrap();
        assert!(h.verify("3333", store.member_hash("member1").unwrap()));
        assert!(h.verify("8888", store.engineer_hash().unwrap()));
        // A PIN set in iemmixer before any import is kept too.
        let dir = tempfile::tempdir().unwrap();
        let mut store = PinStore::load(dir.path()).unwrap();
        store.set_member_hash("member2", h.hash("4444")).unwrap();
        assert!(!store.import_member_hash("member2", h.hash("1111")).unwrap());
        assert!(h.verify("4444", store.member_hash("member2").unwrap()));
    }
}
