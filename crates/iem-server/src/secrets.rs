//! Runtime secrets generated on the PC — never in git or the site config
//! (program spec §5.3): the JWT signing key and the VAPID private key, one
//! owner-only file each in `<config dir>/secrets/`. A file is created once and
//! never overwritten; an empty or unreadable file is an error, not a reason to
//! generate a new secret.

use std::io::{self, Write};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::{OsRng, RngCore};

/// Sub-directory of the config directory holding every runtime secret.
pub const SECRETS_DIR: &str = "secrets";
/// JWT signing key (base64url of 32 random bytes).
pub const JWT_SECRET_FILE: &str = "jwt_secret";
/// VAPID private key (base64url P-256 scalar).
pub const VAPID_PRIVATE_FILE: &str = "vapid_private";

/// Secrets the server needs at start-up.
#[derive(Clone)]
pub struct Secrets {
    pub jwt_secret: String,
    pub vapid_private_key: String,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets").finish_non_exhaustive()
    }
}

/// Load the secrets from `dir`, creating each missing one.
pub fn load_or_create(dir: &Path) -> io::Result<Secrets> {
    std::fs::create_dir_all(dir)?;
    let jwt_secret = read_or_create(&dir.join(JWT_SECRET_FILE), new_jwt_secret)?;
    let vapid_private_key = read_or_create(&dir.join(VAPID_PRIVATE_FILE), new_vapid_private_key)?;
    Ok(Secrets {
        jwt_secret,
        vapid_private_key,
    })
}

fn new_jwt_secret() -> String {
    let mut key = [0u8; 32];
    OsRng.fill_bytes(&mut key);
    URL_SAFE_NO_PAD.encode(key)
}

fn new_vapid_private_key() -> String {
    URL_SAFE_NO_PAD.encode(p256::SecretKey::random(&mut OsRng).to_bytes())
}

fn read_or_create(path: &Path, generate: fn() -> String) -> io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let value = text.trim().to_string();
            if value.is_empty() {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is empty", path.display()),
                ))
            } else {
                Ok(value)
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let value = generate();
            write_new_private(path, value.as_bytes())?;
            tracing::info!(path = %path.display(), "generated a new runtime secret");
            Ok(value)
        }
        Err(e) => Err(e),
    }
}

/// Create `path` exclusively (never overwrite) and write `data`; owner-only on Unix.
pub(crate) fn write_new_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    #[test]
    fn creates_both_secrets_once_and_reloads_them() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create(dir.path()).unwrap();
        let second = load_or_create(dir.path()).unwrap();
        assert_eq!(first.jwt_secret, second.jwt_secret);
        assert_eq!(first.vapid_private_key, second.vapid_private_key);
        assert_eq!(URL_SAFE_NO_PAD.decode(&first.jwt_secret).unwrap().len(), 32);
        assert!(iem_core::Config::vapid_public_key_base64url(&first.vapid_private_key).is_ok());
    }

    #[test]
    fn two_installations_get_different_secrets() {
        let a = load_or_create(tempfile::tempdir().unwrap().path()).unwrap();
        let b = load_or_create(tempfile::tempdir().unwrap().path()).unwrap();
        assert_ne!(a.jwt_secret, b.jwt_secret);
        assert_ne!(a.vapid_private_key, b.vapid_private_key);
    }

    #[test]
    fn an_empty_secret_file_is_an_error_not_a_new_secret() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(JWT_SECRET_FILE), "").unwrap();
        assert_eq!(
            load_or_create(dir.path()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join(JWT_SECRET_FILE)).unwrap(),
            ""
        );
    }

    #[test]
    fn an_existing_secret_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(JWT_SECRET_FILE), "kept-value\n").unwrap();
        assert_eq!(load_or_create(dir.path()).unwrap().jwt_secret, "kept-value");
    }

    #[test]
    fn write_new_private_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x");
        write_new_private(&path, b"first").unwrap();
        assert!(write_new_private(&path, b"second").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
    }

    #[cfg(unix)]
    #[test]
    fn secret_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        load_or_create(dir.path()).unwrap();
        for name in [JWT_SECRET_FILE, VAPID_PRIVATE_FILE] {
            let mode = std::fs::metadata(dir.path().join(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{name}");
        }
    }

    #[test]
    fn debug_output_never_shows_secrets() {
        let secrets = load_or_create(tempfile::tempdir().unwrap().path()).unwrap();
        let shown = format!("{secrets:?}");
        assert!(!shown.contains(&secrets.jwt_secret));
        assert!(!shown.contains(&secrets.vapid_private_key));
    }
}
