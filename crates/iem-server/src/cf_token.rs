// Ported from fohmixer 9f3d440 crates/fohmixer-hub/src/cf_token.rs (iemmixer#91).
//! The Cloudflare API token the server's ACME keeper uses to write its
//! DNS-01 records (#91): set with `iem-server cloudflare set-token` (read
//! from stdin, never from argv: process listings, shell history), sealed for
//! the server's user in `<config dir>/secrets/cloudflare_token.dpapi`, never
//! in the site file and never logged. It needs `Zone > DNS > Edit` on the
//! zone of `https_domain` only.
//!
//! The file is read at each certificate attempt, so a token set while the
//! server runs is used at its next attempt.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use crate::provision::ProvisionError;
use crate::secrets::SECRETS_DIR;

/// The token file's name without its extension (`sealed::SEALED_EXT`).
pub const TOKEN_STEM: &str = "cloudflare_token";

/// The longest token accepted (Cloudflare's are 40 to about 60 characters).
pub const MAX_TOKEN_LEN: usize = 256;

/// A Cloudflare API token. Its `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct CfToken(String);

impl CfToken {
    /// The value, for the `Authorization` header only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for CfToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CfToken(..)")
    }
}

/// `<secrets_dir>/cloudflare_token.<ext>`.
pub fn token_path(secrets_dir: &Path) -> PathBuf {
    secrets_dir.join(format!("{TOKEN_STEM}.{}", crate::sealed::SEALED_EXT))
}

/// Whether `token` looks like a Cloudflare API token: letters, digits, `_`
/// and `-` (a legacy token or a `cfat_` account token), at least 20 and at
/// most [`MAX_TOKEN_LEN`] characters. A pasted line with spaces or quotes is
/// refused before anything is stored.
pub fn is_token_shaped(token: &str) -> bool {
    (20..=MAX_TOKEN_LEN).contains(&token.len())
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The token of one line of `input` (its line end and outer spaces cut).
pub fn read_token(mut input: impl BufRead) -> Result<CfToken, ProvisionError> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    let token = line.trim();
    if is_token_shaped(token) {
        Ok(CfToken(token.to_string()))
    } else {
        Err(ProvisionError::Invalid(
            "a Cloudflare API token is one line of 20 to 256 letters, digits, `_` or `-`"
                .to_string(),
        ))
    }
}

/// Seals `token` into `<config_dir>/secrets/`, replacing a stored one.
pub fn store(config_dir: &Path, token: &CfToken) -> std::io::Result<()> {
    let secrets_dir = config_dir.join(SECRETS_DIR);
    std::fs::create_dir_all(&secrets_dir)?;
    crate::sealed::write(&token_path(&secrets_dir), token.expose().as_bytes())
}

/// The stored token, `None` when none is set. A file that does not open
/// or does not hold a token is an error, never taken as missing.
pub fn load(secrets_dir: &Path) -> std::io::Result<Option<CfToken>> {
    let path = token_path(secrets_dir);
    let Some(bytes) = crate::sealed::read(&path)? else {
        return Ok(None);
    };
    match String::from_utf8(bytes) {
        Ok(token) if is_token_shaped(&token) => Ok(Some(CfToken(token))),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} does not hold a Cloudflare API token: set it again with \
                 `iem-server cloudflare set-token`",
                path.display()
            ),
        )),
    }
}

/// The whole `cloudflare set-token` command for the site in `config_dir`:
/// nothing is read or stored on a platform without a sealed store.
pub fn run(config_dir: &Path, input: impl BufRead) -> Result<(), ProvisionError> {
    if !crate::sealed::AVAILABLE {
        return Err(ProvisionError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            crate::sealed::NO_SEALED_STORE,
        )));
    }
    let token = read_token(input)?;
    store(config_dir, &token)?;
    tracing::info!("Cloudflare API token stored (sealed)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Low-entropy test values: a token-shaped literal in a test would read as
    // a leaked credential to the secret scan.
    fn token() -> String {
        format!("{}_{}", "t".repeat(20), "x".repeat(4))
    }

    #[test]
    fn a_token_is_one_line_of_token_characters() {
        assert_eq!(
            read_token(format!("  {}\r\n", token()).as_bytes())
                .unwrap()
                .expose(),
            token()
        );
        for bad in [
            "",
            "\n",
            "sssssssssssssssssss\n",
            "has space in it aaaaaaaaaa\n",
            "\"qqqqqqqqqqqqqqqqqqqqqqqq\"\n",
        ] {
            let error = read_token(bad.as_bytes()).unwrap_err();
            assert!(matches!(error, ProvisionError::Invalid(_)), "{bad:?}");
            assert!(error.to_string().contains("20 to 256"), "{error}");
        }
        assert!(is_token_shaped(&"a".repeat(20)));
        assert!(!is_token_shaped(&"a".repeat(19)));
        assert!(is_token_shaped(&"a".repeat(256)));
        assert!(!is_token_shaped(&"a".repeat(257)));
        assert!(is_token_shaped(&format!(
            "cfat_{}-{}",
            "a".repeat(10),
            "B".repeat(10)
        )));
        assert!(!is_token_shaped(&format!("{}.", "a".repeat(20))));
    }

    #[test]
    fn set_token_stores_a_sealed_token_the_keeper_loads() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join(SECRETS_DIR);
        assert_eq!(load(&secrets).unwrap(), None, "none set yet");
        run(dir.path(), format!("{}\n", token()).as_bytes()).unwrap();
        assert_eq!(load(&secrets).unwrap().unwrap().expose(), token());
        // Set again: the new token replaces the old one.
        let second = "u".repeat(24);
        run(dir.path(), format!("{second}\n").as_bytes()).unwrap();
        assert_eq!(load(&secrets).unwrap().unwrap().expose(), second);
        assert!(token_path(&secrets).is_file());
    }

    #[test]
    fn a_bad_token_stores_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(run(dir.path(), "x\n".as_bytes()).is_err());
        assert!(!dir.path().join(SECRETS_DIR).exists());
    }

    #[test]
    fn a_stored_file_that_is_no_token_is_an_error_naming_the_fix() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join(SECRETS_DIR);
        std::fs::create_dir_all(&secrets).unwrap();
        crate::sealed::write(&token_path(&secrets), b"not a token").unwrap();
        let error = load(&secrets).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            error
                .to_string()
                .contains("iem-server cloudflare set-token"),
            "{error}"
        );
        crate::sealed::write(&token_path(&secrets), &[0xff; 30]).unwrap();
        assert!(load(&secrets).is_err(), "not UTF-8");
    }

    #[test]
    fn debug_never_shows_the_token() {
        let token = read_token(format!("{}\n", token()).as_bytes()).unwrap();
        assert_eq!(format!("{token:?}"), "CfToken(..)");
    }

    #[test]
    fn the_token_file_name_carries_the_sealed_extension() {
        let path = token_path(Path::new("/data/secrets"));
        assert_eq!(
            path,
            Path::new("/data/secrets")
                .join(format!("cloudflare_token.{}", crate::sealed::SEALED_EXT))
        );
    }
}
