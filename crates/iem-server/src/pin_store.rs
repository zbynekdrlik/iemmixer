//! Argon2id PIN hashes: `<secrets>/pin_hashes.json`.
//! Never plaintext: every stored value must be an argon2id PHC string, and a
//! file that holds anything else is a load error instead of being ignored.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
    /// Hashes an older server kept for the people it archived (#58
    /// decision 6, overruled by the owner on 2026-10-24: a removed person is
    /// removed, not archived), member id → hash. Read only to be deleted
    /// ([`PinStore::drop_archived`]): never a login, counted nowhere and
    /// never written back.
    #[serde(default, skip_serializing)]
    archived: BTreeMap<String, String>,
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
        // Never plaintext, the hashes an older server archived included: a
        // file holding anything else is an error, never read as nobody.
        for (member, phc) in file.members.iter().chain(&file.archived) {
            check_phc(member, phc)?;
        }
        Ok(Self { file, path })
    }

    /// Deletes a member's hash (#58: a person the
    /// roster removes is removed, nothing of theirs kept). `false` when
    /// there is none (nothing is written).
    pub fn remove_member(&mut self, member_id: &str) -> io::Result<bool> {
        check_member(member_id)?;
        if self.file.members.remove(member_id).is_none() {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }

    /// Deletes the hashes an older server archived (#58: a removed person
    /// is removed); their ids, in id order. Nothing is written when there
    /// are none; a write that fails keeps them for the next try.
    pub fn drop_archived(&mut self) -> io::Result<Vec<String>> {
        if self.file.archived.is_empty() {
            return Ok(Vec::new());
        }
        let kept = std::mem::take(&mut self.file.archived);
        if let Err(e) = self.save() {
            self.file.archived = kept;
            return Err(e);
        }
        let ids: Vec<String> = kept.into_keys().collect();
        tracing::info!(people = ?ids, "the PIN hashes an older server archived are deleted");
        Ok(ids)
    }

    pub fn engineer_hash(&self) -> Option<&str> {
        self.file.engineer.as_deref()
    }

    pub fn member_hash(&self, member_id: &str) -> Option<&str> {
        self.file.members.get(member_id).map(String::as_str)
    }

    /// Every member with a live PIN hash (not the engineer), in id order.
    pub fn member_ids(&self) -> Vec<String> {
        self.file.members.keys().cloned().collect()
    }

    /// Whether any PIN hash (engineer or member) is stored: a new pepper
    /// would void each of them. The hashes an older server archived count
    /// for nothing: none of them is ever checked again.
    pub fn has_hashes(&self) -> bool {
        self.file.engineer.is_some() || !self.file.members.is_empty()
    }

    pub fn set_engineer_hash(&mut self, phc: String) -> io::Result<()> {
        check_phc(ENGINEER_ID, &phc)?;
        self.file.engineer = Some(phc);
        self.save()
    }

    pub fn set_member_hash(&mut self, member_id: &str, phc: String) -> io::Result<()> {
        check_member(member_id)?;
        check_phc(member_id, &phc)?;
        self.file.members.insert(member_id.to_string(), phc);
        self.save()
    }

    fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.file).map_err(io::Error::other)?;
        crate::secrets::replace_private(&self.path, json.as_bytes())?;
        // The rename reaches the disk too: a power cut never brings an old
        // PIN back.
        match self.path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => std::fs::File::open(dir)?.sync_all(),
            _ => Ok(()),
        }
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

    /// The hashes are a secret like the others: a change through the web
    /// leaves the file owner-only, also when it was not before.
    #[cfg(unix)]
    #[test]
    fn a_saved_store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PIN_HASHES_FILE);
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut store = PinStore::load(dir.path()).unwrap();
        store
            .set_member_hash("member1", hasher().hash("1357"))
            .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(
            hasher().verify(
                "1357",
                PinStore::load(dir.path())
                    .unwrap()
                    .member_hash("member1")
                    .unwrap()
            )
        );
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

    /// A file written before the importer went holds `imported`: it loads,
    /// and the next save drops the list.
    #[test]
    fn a_file_with_the_old_import_marks_loads_and_loses_them_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let h = hasher();
        let text = format!(
            r#"{{"members":{{"member1":"{}"}},"imported":["member1"]}}"#,
            h.hash("1357")
        );
        std::fs::write(dir.path().join(PIN_HASHES_FILE), text).unwrap();
        let mut store = PinStore::load(dir.path()).unwrap();
        assert!(store.member_hash("member1").is_some());
        store.set_member_hash("member2", h.hash("2468")).unwrap();
        let saved = std::fs::read_to_string(dir.path().join(PIN_HASHES_FILE)).unwrap();
        assert!(!saved.contains("imported"), "{saved}");
    }

    /// #58, the owner's ruling (2026-10-24): a removed person is removed,
    /// not archived. The `archived` hashes an older server kept are no
    /// login, never written back and count for nothing: the pepper may be
    /// made anew beside them, since no hash of theirs is checked again.
    #[test]
    fn an_older_files_archived_hashes_are_no_login_and_never_written_back() {
        let dir = tempfile::tempdir().unwrap();
        let h = hasher();
        let text = format!(
            r#"{{"members":{{"member1":"{}"}},"archived":{{"member2":"{}"}}}}"#,
            h.hash("1357"),
            h.hash("2468")
        );
        std::fs::write(dir.path().join(PIN_HASHES_FILE), text).unwrap();
        let mut store = PinStore::load(dir.path()).unwrap();
        assert!(store.member_hash("member1").is_some());
        assert_eq!(store.member_hash("member2"), None, "no login");
        assert_eq!(store.member_ids(), ["member1"]);
        store.set_member_hash("member3", h.hash("8642")).unwrap();
        let written = std::fs::read_to_string(dir.path().join(PIN_HASHES_FILE)).unwrap();
        assert!(!written.contains("archived"), "{written}");
        assert!(!written.contains("member2"), "{written}");
        let only = format!(r#"{{"archived":{{"member2":"{}"}}}}"#, h.hash("2468"));
        std::fs::write(dir.path().join(PIN_HASHES_FILE), only).unwrap();
        assert!(!PinStore::load(dir.path()).unwrap().has_hashes());
    }

    /// #58: the roster's watcher removes whoever holds a live PIN and is no
    /// person any more.
    #[test]
    fn member_ids_are_the_live_member_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let h = hasher();
        let mut store = PinStore::load(dir.path()).unwrap();
        assert!(store.member_ids().is_empty());
        store.set_engineer_hash(h.hash("2468")).unwrap();
        for id in ["member2", "member1", "member3"] {
            store.set_member_hash(id, h.hash("1357")).unwrap();
        }
        assert_eq!(store.member_ids(), ["member1", "member2", "member3"]);
    }

    /// #58: a removed member's hash is deleted and stay
    /// deleted; a member without a hash writes nothing; the engineer and a
    /// bad id are refused.
    #[test]
    fn remove_member_deletes_the_hash() {
        let dir = tempfile::tempdir().unwrap();
        let h = hasher();
        let mut store = PinStore::load(dir.path()).unwrap();
        store.set_member_hash("member1", h.hash("1357")).unwrap();
        store.set_member_hash("member2", h.hash("2468")).unwrap();
        assert!(store.remove_member("member1").unwrap());
        assert_eq!(store.member_hash("member1"), None);
        let store = PinStore::load(dir.path()).unwrap();
        assert_eq!(store.member_hash("member1"), None, "after a reload too");
        assert!(h.verify("2468", store.member_hash("member2").unwrap()));
        let text = std::fs::read_to_string(dir.path().join(PIN_HASHES_FILE)).unwrap();
        assert!(!text.contains("member1"), "{text}");
        let mut store = store;
        std::fs::remove_file(dir.path().join(PIN_HASHES_FILE)).unwrap();
        assert!(!store.remove_member("member3").unwrap());
        assert!(
            !dir.path().join(PIN_HASHES_FILE).exists(),
            "nothing written"
        );
        for bad in [ENGINEER_ID, "../x"] {
            let err = store.remove_member(bad).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad}");
        }
    }

    /// #58: the hashes an older server archived are deleted once, their
    /// ids named; a write that fails keeps them for the next try, and with
    /// none nothing is written.
    #[test]
    fn drop_archived_deletes_an_older_servers_hashes_once() {
        let dir = tempfile::tempdir().unwrap();
        let h = hasher();
        let text = format!(
            r#"{{"members":{{"member1":"{}"}},"archived":{{"member3":"{}","member2":"{}"}}}}"#,
            h.hash("1357"),
            h.hash("2468"),
            h.hash("8642")
        );
        let file = dir.path().join(PIN_HASHES_FILE);
        std::fs::write(&file, &text).unwrap();
        let mut store = PinStore::load(dir.path()).unwrap();
        // The write's temporary file cannot be made: a directory is there.
        let tmp = file.with_extension("new");
        std::fs::create_dir(&tmp).unwrap();
        assert!(store.drop_archived().is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text, "unchanged");
        std::fs::remove_dir(tmp).unwrap();
        assert_eq!(store.drop_archived().unwrap(), ["member2", "member3"]);
        let written = std::fs::read_to_string(&file).unwrap();
        assert!(!written.contains("archived"), "{written}");
        assert!(!written.contains("member2") && !written.contains("member3"));
        assert!(h.verify("1357", store.member_hash("member1").unwrap()));
        std::fs::remove_file(&file).unwrap();
        assert_eq!(store.drop_archived().unwrap(), Vec::<String>::new());
        assert!(!file.exists(), "nothing written");
    }

    /// Never plaintext, also in the hashes an older server archived: such a
    /// file is a load error, never read as nobody.
    #[test]
    fn an_older_files_archived_hash_must_be_argon2id() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(PIN_HASHES_FILE),
            r#"{"archived":{"member1":"1357"}}"#,
        )
        .unwrap();
        assert_eq!(
            PinStore::load(dir.path()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
