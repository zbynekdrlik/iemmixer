//! The PIN pepper: 32 random bytes created once per data directory.
//! Windows: DPAPI-protected for the current user (`pepper.dpapi`).
//! Other platforms are test-only: an owner-only plain file (`pepper.test`).
//! A pepper file that exists but cannot be read is an error — never replaced,
//! because a new pepper would silently invalidate every PIN hash. For the same
//! reason a missing pepper is created only while no PIN hash exists (first
//! run); a missing pepper next to stored PIN hashes is an error too.

use std::io;
use std::path::Path;

use rand_core::{OsRng, RngCore};

use crate::pin_hash::PEPPER_LEN;
use crate::pin_store::{PIN_HASHES_FILE, PinStore};

#[cfg(windows)]
mod dpapi;
// Windows protects the pepper with DPAPI directly (no wrappers here: Linux
// CI cannot build them, so their mutants could never be caught).
#[cfg(windows)]
use dpapi::{protect, unprotect};

/// File name of the stored pepper.
#[cfg(windows)]
pub const PEPPER_FILE: &str = "pepper.dpapi";
/// File name of the stored pepper.
#[cfg(not(windows))]
pub const PEPPER_FILE: &str = "pepper.test";

/// Load the pepper from `dir`, creating it on first use (no PIN hash stored
/// in `dir` yet).
pub fn load_or_create(dir: &Path) -> io::Result<[u8; PEPPER_LEN]> {
    let path = dir.join(PEPPER_FILE);
    match std::fs::read(&path) {
        Ok(stored) => {
            let raw = unprotect(&stored)?;
            <[u8; PEPPER_LEN]>::try_from(raw.as_slice()).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{} does not hold a {PEPPER_LEN}-byte pepper",
                        path.display()
                    ),
                )
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // Hashes made with a lost pepper can never verify again: a new
            // pepper would lock out every member without a sign. A corrupt or
            // unreadable PIN store is its own error here (never guessed empty).
            if PinStore::load(dir)?.has_hashes() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "the PIN pepper {pepper} is missing but {store} holds PIN hashes made \
                         with it; no new pepper was created because it would invalidate every \
                         PIN. Restore the pepper file (on Windows it only decrypts for the \
                         account that created it), or start over: move {store} aside, then run \
                         `iem-server pin set-engineer` and give members new PINs (engineer \
                         reset or `iem-server pin set-member <id>`)",
                        pepper = path.display(),
                        store = dir.join(PIN_HASHES_FILE).display(),
                    ),
                ));
            }
            let mut pepper = [0u8; PEPPER_LEN];
            OsRng.fill_bytes(&mut pepper);
            std::fs::create_dir_all(dir)?;
            crate::secrets::write_new_private(&path, &protect(&pepper)?)?;
            tracing::info!(path = %path.display(), "created a new PIN pepper");
            Ok(pepper)
        }
        Err(e) => Err(e),
    }
}

#[cfg(not(windows))]
fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    tracing::warn!(
        "PIN pepper stored unprotected: only Windows protects it (DPAPI); other platforms are test-only"
    );
    Ok(data.to_vec())
}

#[cfg(not(windows))]
fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    Ok(data.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_once_and_reloads_the_same_pepper() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create(dir.path()).unwrap();
        let second = load_or_create(dir.path()).unwrap();
        assert_eq!(first, second);
        assert_ne!(first, [0u8; PEPPER_LEN]);
    }

    #[test]
    fn different_directories_get_different_peppers() {
        let a = load_or_create(tempfile::tempdir().unwrap().path()).unwrap();
        let b = load_or_create(tempfile::tempdir().unwrap().path()).unwrap();
        assert_ne!(a, b);
    }

    /// A syntactically valid argon2id PHC string (the store checks the prefix).
    const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2hoYXNo";

    fn write_pin_store(dir: &Path, json: &str) {
        std::fs::write(dir.join(PIN_HASHES_FILE), json).unwrap();
    }

    fn assert_refused_without_a_new_pepper(dir: &Path, store: &str) {
        write_pin_store(dir, store);
        let err = load_or_create(dir).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
        let msg = err.to_string();
        assert!(msg.contains("pepper") && msg.contains("missing"), "{msg}");
        assert!(msg.contains("iem-server pin set-engineer"), "{msg}");
        assert!(!dir.join(PEPPER_FILE).exists(), "no new pepper");
        assert_eq!(
            std::fs::read_to_string(dir.join(PIN_HASHES_FILE)).unwrap(),
            store,
            "the PIN store is left as it was"
        );
    }

    #[test]
    fn a_missing_pepper_next_to_an_engineer_hash_is_an_error_not_a_new_pepper() {
        let dir = tempfile::tempdir().unwrap();
        assert_refused_without_a_new_pepper(dir.path(), &format!(r#"{{"engineer":"{PHC}"}}"#));
    }

    #[test]
    fn a_missing_pepper_next_to_a_member_hash_is_an_error_not_a_new_pepper() {
        let dir = tempfile::tempdir().unwrap();
        assert_refused_without_a_new_pepper(
            dir.path(),
            &format!(r#"{{"members":{{"member1":"{PHC}"}}}}"#),
        );
    }

    #[test]
    fn first_run_without_a_pin_store_creates_the_pepper() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!dir.path().join(PIN_HASHES_FILE).exists());
        load_or_create(dir.path()).unwrap();
        assert!(dir.path().join(PEPPER_FILE).is_file());
    }

    #[test]
    fn a_pin_store_without_hashes_does_not_block_a_new_pepper() {
        // Nothing to invalidate: an empty store is the same as no store.
        for store in ["{}", r#"{"engineer":null,"members":{}}"#] {
            let dir = tempfile::tempdir().unwrap();
            write_pin_store(dir.path(), store);
            load_or_create(dir.path()).unwrap();
            assert!(dir.path().join(PEPPER_FILE).is_file(), "{store}");
        }
    }

    #[test]
    fn a_missing_pepper_next_to_a_corrupt_pin_store_is_an_error_not_a_new_pepper() {
        // A zero-byte or corrupt store is never guessed empty.
        let dir = tempfile::tempdir().unwrap();
        write_pin_store(dir.path(), "");
        assert!(load_or_create(dir.path()).is_err());
        assert!(!dir.path().join(PEPPER_FILE).exists());
    }

    #[test]
    fn wrong_length_file_is_an_error_not_a_new_pepper() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PEPPER_FILE);
        std::fs::write(&path, b"short").unwrap();
        assert!(load_or_create(dir.path()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"short");
    }

    #[test]
    fn an_unreadable_pepper_is_an_error_not_a_new_pepper() {
        // The path exists but is not a readable file: the read error comes
        // back; only a missing file creates a pepper (a create attempt would
        // fail with AlreadyExists instead).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PEPPER_FILE);
        std::fs::create_dir(&path).unwrap();
        let err = load_or_create(dir.path()).unwrap_err();
        assert_ne!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
        assert!(path.is_dir());
    }

    #[cfg(not(windows))]
    #[test]
    fn test_platforms_store_the_raw_pepper() {
        let dir = tempfile::tempdir().unwrap();
        let pepper = load_or_create(dir.path()).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join(PEPPER_FILE)).unwrap(),
            pepper.to_vec()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_stores_the_pepper_dpapi_protected() {
        let dir = tempfile::tempdir().unwrap();
        let pepper = load_or_create(dir.path()).unwrap();
        let stored = std::fs::read(dir.path().join(PEPPER_FILE)).unwrap();
        assert_ne!(stored, pepper.to_vec());
        assert!(stored.len() > PEPPER_LEN);
    }

    #[cfg(unix)]
    #[test]
    fn pepper_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        load_or_create(dir.path()).unwrap();
        let mode = std::fs::metadata(dir.path().join(PEPPER_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
