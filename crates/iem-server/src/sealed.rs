// Ported from fohmixer 9f3d440 crates/fohmixer-hub/src/sealed.rs (iemmixer#91).
//! Secrets sealed at rest for the server's user (#91): the Cloudflare API
//! token (`cf_token.rs`) and the ACME account key (`acme.rs`), in
//! `<config dir>/secrets/<name>.dpapi`.
//!
//! Windows (the PC): DPAPI for the current user (`pepper/dpapi.rs`), so a
//! sealed file only opens for the account that sealed it — the command that
//! stores one (`iem-server cloudflare set-token`) runs as the server's user.
//! Other platforms have no sealed store yet (Gen 3 brings one): reading or
//! writing is an error ([`NO_SEALED_STORE`]) and the ACME keeper does
//! nothing. Only this crate's own tests store the bytes as they are there.

use std::io;
use std::path::Path;

/// The file extension of a sealed file: `.dpapi` on Windows.
#[cfg(windows)]
pub const SEALED_EXT: &str = "dpapi";
/// The file extension of a sealed file: `.test` where nothing seals it.
#[cfg(not(windows))]
pub const SEALED_EXT: &str = "test";

/// Why nothing is sealed on this platform.
pub const NO_SEALED_STORE: &str = "no sealed store on this platform: the Cloudflare token and the ACME account are kept on Windows (DPAPI) only";

/// Whether this build has a sealed store: Windows, and the unit tests.
pub const AVAILABLE: bool = cfg!(any(windows, test));

#[cfg(windows)]
use crate::pepper::dpapi::{protect as seal, unprotect as unseal};

/// `data` stored as it is: this crate's tests on platforms without DPAPI.
#[cfg(all(not(windows), test))]
fn seal(data: &[u8]) -> io::Result<Vec<u8>> {
    Ok(data.to_vec())
}

#[cfg(all(not(windows), test))]
fn unseal(data: &[u8]) -> io::Result<Vec<u8>> {
    Ok(data.to_vec())
}

#[cfg(all(not(windows), not(test)))]
fn seal(_data: &[u8]) -> io::Result<Vec<u8>> {
    Err(io::Error::new(io::ErrorKind::Unsupported, NO_SEALED_STORE))
}

#[cfg(all(not(windows), not(test)))]
fn unseal(_data: &[u8]) -> io::Result<Vec<u8>> {
    Err(io::Error::new(io::ErrorKind::Unsupported, NO_SEALED_STORE))
}

/// The unsealed content of `path`, or `None` when it does not exist. A file
/// that exists but does not open is an error, never taken as missing.
pub fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(stored) => unseal(&stored).map(Some),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Seals `data` into `path`, replacing it whole (owner-only on Unix).
pub fn write(path: &Path, data: &[u8]) -> io::Result<()> {
    crate::secrets::replace_private(path, &seal(data)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sealed_file_reads_back_and_a_missing_one_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("token.{SEALED_EXT}"));
        assert_eq!(read(&path).unwrap(), None);
        write(&path, b"first").unwrap();
        assert_eq!(read(&path).unwrap(), Some(b"first".to_vec()));
        write(&path, b"second").unwrap();
        assert_eq!(read(&path).unwrap(), Some(b"second".to_vec()));
    }

    #[test]
    fn an_unreadable_sealed_file_is_an_error_not_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::create_dir(&path).unwrap();
        assert!(read(&path).is_err());
    }

    #[test]
    fn the_tests_have_a_sealed_store() {
        const { assert!(AVAILABLE) };
        assert!(NO_SEALED_STORE.starts_with("no sealed store"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_seals_with_dpapi() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token.dpapi");
        write(&path, b"secret-bytes").unwrap();
        let stored = std::fs::read(&path).unwrap();
        assert_ne!(stored, b"secret-bytes".to_vec());
        assert_eq!(read(&path).unwrap(), Some(b"secret-bytes".to_vec()));
    }
}
