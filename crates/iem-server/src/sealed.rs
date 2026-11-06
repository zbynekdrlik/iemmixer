// Ported from fohmixer 9f3d440 crates/fohmixer-hub/src/sealed.rs (iemmixer#91).
//! Secrets the server keeps at rest for its user (#91): the ACME account key
//! (`acme.rs`) and, on a test site, the Cloudflare API token (`cf_token.rs`),
//! in `<config dir>/secrets/<name>.<ext>`: an owner-only file
//! (`secrets::replace_private`) in the service user's 0750 state folder. On
//! the Gen 3 box only the ACME account lands here, the token is a systemd
//! credential (`cf_token.rs`).

use std::io;
use std::path::Path;

/// The file extension of a stored file.
pub const SEALED_EXT: &str = "private";

/// The content of `path`, or `None` when it does not exist. A file that
/// exists but does not open is an error, never taken as missing.
pub fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(stored) => Ok(Some(stored)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Stores `data` in `path`, replacing it whole (owner-only).
pub fn write(path: &Path, data: &[u8]) -> io::Result<()> {
    crate::secrets::replace_private(path, data)
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
    fn a_stored_file_opens_for_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("token.{SEALED_EXT}"));
        write(&path, b"secret-bytes").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
