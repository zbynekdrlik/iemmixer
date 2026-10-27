//! The PIN pepper: 32 random bytes created once per data directory.
//! Windows: DPAPI-protected for the current user (`pepper.dpapi`).
//! Linux with systemd's credentials folder (`$CREDENTIALS_DIRECTORY`, the
//! Gen 3 box): the systemd credential `pepper`, sealed on the box and
//! decrypted by systemd at each start, never created or written here; a
//! plain test pepper beside it is an error, never a fall back.
//! Otherwise (CI, local runs) test-only: an owner-only plain file
//! (`pepper.test`).
//! A pepper file that exists but cannot be read is an error — never replaced,
//! because a new pepper would silently invalidate every PIN hash. For the same
//! reason a missing pepper is created only while no PIN hash exists (first
//! run); a missing pepper next to stored PIN hashes is an error too.

use std::io;
use std::path::{Path, PathBuf};

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

/// The systemd credential that holds the pepper on Linux: the server's unit
/// loads it (`LoadCredentialEncrypted=pepper:…`), the PIN commands get it
/// through `systemd-run`.
pub const CREDENTIAL: &str = "pepper";

/// Load the pepper ([`load_from`]): on Linux from systemd's credentials
/// folder when systemd gives this process one (`$CREDENTIALS_DIRECTORY`),
/// else from `dir`. Windows keeps its DPAPI file.
pub fn load_or_create(dir: &Path) -> io::Result<[u8; PEPPER_LEN]> {
    #[cfg(windows)]
    let credentials: Option<PathBuf> = None;
    #[cfg(not(windows))]
    let credentials = std::env::var_os("CREDENTIALS_DIRECTORY").map(PathBuf::from);
    load_from(dir, credentials.as_deref())
}

/// With systemd's credentials folder `credentials`: the pepper is the
/// credential [`CREDENTIAL`] there, exactly [`PEPPER_LEN`] bytes, never
/// created or written; a missing one, one of another length and a plain test
/// pepper in `dir` are errors (never a fall back to the file). Without it:
/// the pepper stored in `dir`, created on first use (no PIN hash stored in
/// `dir` yet).
pub fn load_from(dir: &Path, credentials: Option<&Path>) -> io::Result<[u8; PEPPER_LEN]> {
    match credentials {
        Some(c) => from_credential(dir, c),
        None => from_file(dir),
    }
}

fn from_credential(dir: &Path, credentials: &Path) -> io::Result<[u8; PEPPER_LEN]> {
    // PINs set with a test pepper never verify with the credential: such a
    // file is refused, never used and never ignored.
    let test = dir.join(PEPPER_FILE);
    let beside = test.try_exists().map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "cannot tell whether a plain test pepper lies at {}: {e}",
                test.display()
            ),
        )
    })?;
    if beside {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "a plain test pepper lies beside the systemd credential: remove {} and give \
                 every PIN again",
                test.display()
            ),
        ));
    }
    // Errors name the credential and its folder, never its bytes.
    let named = |what: String| {
        format!(
            "the PIN pepper's systemd credential '{CREDENTIAL}' in {} {what}",
            credentials.display()
        )
    };
    let raw = std::fs::read(credentials.join(CREDENTIAL))
        .map_err(|e| io::Error::new(e.kind(), named(format!("cannot be read: {e}"))))?;
    let pepper = <[u8; PEPPER_LEN]>::try_from(raw.as_slice()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            named(format!("holds {} bytes, not {PEPPER_LEN}", raw.len())),
        )
    })?;
    tracing::info!(
        credential = CREDENTIAL,
        "PIN pepper from the systemd credential"
    );
    Ok(pepper)
}

/// The pepper stored in `dir`, created on first use (no PIN hash stored in
/// `dir` yet).
fn from_file(dir: &Path) -> io::Result<[u8; PEPPER_LEN]> {
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
        "PIN pepper stored unprotected in a test file: Windows protects it with DPAPI, Linux takes systemd's credential (CREDENTIALS_DIRECTORY)"
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

    /// A credentials folder holding `bytes` as the credential, as systemd
    /// decrypts it for the server (`LoadCredentialEncrypted=pepper:…`).
    fn credentials_with(bytes: &[u8]) -> tempfile::TempDir {
        let c = tempfile::tempdir().unwrap();
        std::fs::write(c.path().join(CREDENTIAL), bytes).unwrap();
        c
    }

    fn synthetic() -> [u8; PEPPER_LEN] {
        std::array::from_fn(|i| 0x40 + i as u8)
    }

    /// Every file below `dir` with its bytes, sorted.
    fn files(dir: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.extend(files(&path));
            } else {
                let bytes = std::fs::read(&path).unwrap();
                out.push((path, bytes));
            }
        }
        out.sort();
        out
    }

    #[tracing_test::traced_test]
    #[test]
    fn the_credential_is_the_pepper() {
        let c = credentials_with(&synthetic());
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_from(dir.path(), Some(c.path())).unwrap(), synthetic());
        // Next to PIN hashes too: the credential is the pepper they were
        // made with, never a missing one.
        write_pin_store(dir.path(), &format!(r#"{{"engineer":"{PHC}"}}"#));
        assert_eq!(load_from(dir.path(), Some(c.path())).unwrap(), synthetic());
        logs_assert(|lines: &[&str]| {
            let from = lines
                .iter()
                .filter(|l| l.contains("PIN pepper from the systemd credential"))
                .count();
            match (from, lines.iter().any(|l| l.contains("unprotected"))) {
                (2, false) => Ok(()),
                other => Err(format!("one info line per load, no warning: {other:?}")),
            }
        });
    }

    #[test]
    fn a_credential_of_another_length_is_an_error() {
        for len in [0, 1, PEPPER_LEN - 1, PEPPER_LEN + 1, 2 * PEPPER_LEN] {
            let bytes = vec![b'q'; len];
            let c = credentials_with(&bytes);
            let dir = tempfile::tempdir().unwrap();
            let err = load_from(dir.path(), Some(c.path())).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{len}: {err}");
            let msg = err.to_string();
            assert_eq!(
                msg,
                format!(
                    "the PIN pepper's systemd credential '{CREDENTIAL}' in {} holds {len} bytes, \
                     not {PEPPER_LEN}",
                    c.path().display()
                )
            );
            // The temporary folder's random name may hold "qq" itself (CI,
            // 2026-10-27: `/tmp/.tmpJTqqVo`): the bytes are looked for in
            // the message without its path.
            let without_path = msg.replace(&c.path().display().to_string(), "");
            assert!(
                len == 0 || !without_path.contains("qq"),
                "never the bytes: {msg}"
            );
            assert!(!dir.path().join(PEPPER_FILE).exists(), "no test pepper");
            assert_eq!(std::fs::read(c.path().join(CREDENTIAL)).unwrap(), bytes);
        }
    }

    #[test]
    fn a_credentials_folder_without_the_pepper_is_an_error_never_the_test_file() {
        let c = tempfile::tempdir().unwrap();
        std::fs::write(c.path().join("other"), synthetic()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let err = load_from(dir.path(), Some(c.path())).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
        let msg = err.to_string();
        assert!(
            msg.starts_with(&format!(
                "the PIN pepper's systemd credential '{CREDENTIAL}' in {} cannot be read: ",
                c.path().display()
            )),
            "{msg}"
        );
        assert!(!dir.path().join(PEPPER_FILE).exists(), "no test pepper");
        // A folder systemd names but that is gone: the same.
        let gone = c.path().join("gone");
        let err = load_from(dir.path(), Some(&gone)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
        assert!(err.to_string().contains(CREDENTIAL), "{err}");
        assert!(!dir.path().join(PEPPER_FILE).exists(), "no test pepper");
    }

    #[test]
    fn a_test_pepper_beside_the_credential_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let test = load_from(dir.path(), None).unwrap();
        let path = dir.path().join(PEPPER_FILE);
        let stored = std::fs::read(&path).unwrap();
        let c = credentials_with(&synthetic());
        let err = load_from(dir.path(), Some(c.path())).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
        assert_eq!(
            err.to_string(),
            format!(
                "a plain test pepper lies beside the systemd credential: remove {} and give \
                 every PIN again",
                path.display()
            )
        );
        assert_eq!(std::fs::read(&path).unwrap(), stored, "left as it was");
        // Without the credential the test pepper is still today's.
        assert_eq!(load_from(dir.path(), None).unwrap(), test);
    }

    #[cfg(unix)]
    #[test]
    fn a_test_pepper_that_cannot_be_looked_for_is_an_error() {
        // `dir` is a file: whether a test pepper lies there cannot be told,
        // so the credential is not taken.
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("secrets");
        std::fs::write(&dir, b"").unwrap();
        let c = credentials_with(&synthetic());
        let err = load_from(&dir, Some(c.path())).unwrap_err();
        assert_ne!(err.kind(), io::ErrorKind::NotFound, "{err}");
        assert!(
            err.to_string()
                .contains(&dir.join(PEPPER_FILE).display().to_string()),
            "{err}"
        );
    }

    #[test]
    fn nothing_is_written_with_a_credential() {
        let c = credentials_with(&synthetic());
        let before = files(c.path());
        // A secrets folder that does not exist yet stays so.
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("secrets");
        load_from(&dir, Some(c.path())).unwrap();
        assert!(!dir.exists(), "no folder made");
        // One that exists keeps exactly what it held.
        let dir = tempfile::tempdir().unwrap();
        write_pin_store(dir.path(), "{}");
        let held = files(dir.path());
        load_from(dir.path(), Some(c.path())).unwrap();
        assert_eq!(files(dir.path()), held);
        assert_eq!(files(c.path()), before, "the credential untouched");
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
