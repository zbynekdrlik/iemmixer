//! The PIN pepper: 32 random bytes created once per data directory.
//! Windows: DPAPI-protected for the current user (`pepper.dpapi`).
//! Other platforms are test-only: an owner-only plain file (`pepper.test`).
//! A pepper file that exists but cannot be read is an error — never replaced,
//! because a new pepper would silently invalidate every PIN hash.

use std::io;
use std::path::Path;

use rand_core::{OsRng, RngCore};

use crate::pin_hash::PEPPER_LEN;

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

/// Load the pepper from `dir`, creating it on first use.
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
