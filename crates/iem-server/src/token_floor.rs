//! Login tokens valid only from a subject's last PIN change (#72, from #86):
//! `<secrets>/token_floor.json`, subject → the change's time in Unix
//! seconds. A token whose `iat` is before its subject's floor is refused, so
//! a PIN change logs out every other device of that subject (the engineer's
//! change: every engineer token). A subject without a floor keeps every
//! token until it expires. Kept beside the PIN hashes, owner-only, replaced
//! whole ([`crate::secrets::replace_private`]), so a restart keeps it.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};

/// File name inside the secrets directory.
pub const TOKEN_FLOOR_FILE: &str = "token_floor.json";

/// The floors, read once at start and written at every raise.
#[derive(Debug)]
pub struct TokenFloor {
    floors: RwLock<BTreeMap<String, u64>>,
    path: PathBuf,
}

impl TokenFloor {
    /// Loads `secrets_dir`'s file (no floors when there is none). A file
    /// that cannot be read as floors is an error: the server does not start
    /// rather than take tokens a PIN change ended.
    pub fn load(secrets_dir: &Path) -> io::Result<Self> {
        let path = secrets_dir.join(TOKEN_FLOOR_FILE);
        let floors = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: {e}", path.display()),
                )
            })?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            floors: RwLock::new(floors),
            path,
        })
    }

    /// Whether a token of `subject` issued at `iat` is still valid: no
    /// floor, or issued at or after it.
    pub fn admits(&self, subject: &str, iat: u64) -> bool {
        self.floors
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(subject)
            .is_none_or(|floor| iat >= *floor)
    }

    /// Ends `subject`'s tokens issued before `now`, saved before it takes
    /// effect; a failed save changes nothing. Never lowers a floor.
    pub fn raise(&self, subject: &str, now: u64) -> io::Result<()> {
        let mut floors = self.floors.write().unwrap_or_else(PoisonError::into_inner);
        let mut next = floors.clone();
        let floor = next.entry(subject.to_string()).or_insert(0);
        *floor = (*floor).max(now);
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec(&next).map_err(io::Error::other)?;
        crate::secrets::replace_private(&self.path, &json)?;
        *floors = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_floor_ends_older_tokens_and_survives_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let f = TokenFloor::load(dir.path()).unwrap();
        assert!(f.admits("alice", 5), "no floor: every token");
        f.raise("alice", 100).unwrap();
        assert!(!f.admits("alice", 99));
        assert!(f.admits("alice", 100));
        assert!(f.admits("bob", 1), "another subject keeps its tokens");
        let again = TokenFloor::load(dir.path()).unwrap();
        assert!(!again.admits("alice", 99));
        assert!(again.admits("alice", 100));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(TOKEN_FLOOR_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_floor_never_goes_down() {
        let dir = tempfile::tempdir().unwrap();
        let f = TokenFloor::load(dir.path()).unwrap();
        f.raise("alice", 100).unwrap();
        f.raise("alice", 50).unwrap();
        assert!(!f.admits("alice", 99));
    }

    #[test]
    fn a_damaged_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(TOKEN_FLOOR_FILE), "{not json").unwrap();
        assert_eq!(
            TokenFloor::load(dir.path()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn a_failed_save_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let f = TokenFloor::load(dir.path()).unwrap();
        // A folder where the file goes: the replace fails.
        std::fs::create_dir(dir.path().join(TOKEN_FLOOR_FILE)).unwrap();
        assert!(f.raise("alice", 100).is_err());
        assert!(f.admits("alice", 1));
    }
}
