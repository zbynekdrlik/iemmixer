// Ported from fohmixer 9f3d440 crates/fohmixer-hub/src/sealed.rs (iemmixer#91).
//! Secrets the server keeps at rest for its user (#91): the Cloudflare API
//! token (`cf_token.rs`) and the ACME account key (`acme.rs`), in
//! `<config dir>/secrets/<name>.<ext>`.
//!
//! Windows (the PC): DPAPI for the current user (`pepper/dpapi.rs`), so a
//! sealed file only opens for the account that sealed it — the command that
//! stores one (`iem-server cloudflare set-token`) runs as the server's user.
//! Elsewhere (the Gen 3 box): an owner-only file (`secrets::replace_private`)
//! in the service user's 0750 state folder; only the ACME account lands here
//! in production, the token is a systemd credential (`cf_token.rs`).

use std::io;
use std::path::Path;

/// The file extension of a sealed file: `.dpapi` on Windows.
#[cfg(windows)]
pub const SEALED_EXT: &str = "dpapi";
/// The file extension of a stored file: `.private` outside Windows.
#[cfg(not(windows))]
pub const SEALED_EXT: &str = "private";

#[cfg(windows)]
use crate::pepper::dpapi::{protect as seal, unprotect as unseal};

/// `data` as it is: outside Windows the file's owner-only mode protects it.
#[cfg(not(windows))]
fn seal(data: &[u8]) -> io::Result<Vec<u8>> {
    Ok(data.to_vec())
}

#[cfg(not(windows))]
fn unseal(data: &[u8]) -> io::Result<Vec<u8>> {
    Ok(data.to_vec())
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

    #[cfg(unix)]
    #[test]
    fn a_stored_file_opens_for_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("token.{SEALED_EXT}"));
        write(&path, b"secret-bytes").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
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
