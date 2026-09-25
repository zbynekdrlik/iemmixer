//! PIN provisioning for `iem-server pin …`: the first engineer PIN at
//! bootstrap (server stopped), member PINs for tests. Members normally get
//! their PIN from an engineer reset in the UI (F3). The PIN is read from
//! stdin, never from argv (process listings, shell history).

use std::io::BufRead;
use std::path::{Path, PathBuf};

use crate::pepper;
use crate::pin_hash::{PinHasher, is_valid_pin_format};
use crate::pin_store::{ENGINEER_ID, PinStore};
use crate::secrets::SECRETS_DIR;

/// Whose PIN is being set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinTarget {
    Engineer,
    Member(String),
}

impl PinTarget {
    pub fn label(&self) -> String {
        match self {
            Self::Engineer => "engineer".to_string(),
            Self::Member(id) => format!("member {id}"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProvisionError {
    /// Bad input: exit code 2.
    #[error("{0}")]
    Invalid(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("site config: {0}")]
    Config(String),
}

/// Directory holding the site config, its data and `secrets/`.
pub fn config_dir_of(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Read one line from `input` and check it is a valid PIN.
pub fn read_pin(mut input: impl BufRead) -> Result<String, ProvisionError> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    let pin = line.trim_end_matches(['\r', '\n']).to_string();
    if is_valid_pin_format(&pin) {
        Ok(pin)
    } else {
        Err(ProvisionError::Invalid(
            "the PIN must be exactly 4 digits".to_string(),
        ))
    }
}

/// A member target must be a configured member other than the engineer.
pub fn check_target(config: &iem_core::Config, target: &PinTarget) -> Result<(), ProvisionError> {
    if let PinTarget::Member(id) = target {
        if id == ENGINEER_ID {
            return Err(ProvisionError::Invalid(
                "the engineer PIN is set with `pin set-engineer`".to_string(),
            ));
        }
        iem_core::config::validate_member_id(id).map_err(ProvisionError::Invalid)?;
        if !config.members.iter().any(|m| m.id() == *id) {
            return Err(ProvisionError::Invalid(format!(
                "unknown member `{id}` (not in the site config)"
            )));
        }
    }
    Ok(())
}

/// Hash `pin` with the installation's pepper and store it.
pub fn store_pin(config_dir: &Path, target: &PinTarget, pin: &str) -> Result<(), ProvisionError> {
    let secrets_dir = config_dir.join(SECRETS_DIR);
    let hasher = PinHasher::new(pepper::load_or_create(&secrets_dir)?);
    let mut store = PinStore::load(&secrets_dir)?;
    let phc = hasher.hash(pin);
    match target {
        PinTarget::Engineer => store.set_engineer_hash(phc)?,
        PinTarget::Member(id) => store.set_member_hash(id, phc)?,
    }
    Ok(())
}

/// The whole `pin set-…` command.
pub fn run(
    config_path: &Path,
    target: &PinTarget,
    input: impl BufRead,
) -> Result<(), ProvisionError> {
    let config =
        iem_core::Config::load(config_path).map_err(|e| ProvisionError::Config(e.to_string()))?;
    check_target(&config, target)?;
    let pin = read_pin(input)?;
    store_pin(&config_dir_of(config_path), target, &pin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site() -> iem_core::Config {
        toml::from_str(
            "[[members]]\nname = \"Member1\"\ndante_output_l = 71\ndante_output_r = 72\n",
        )
        .unwrap()
    }

    #[test]
    fn read_pin_accepts_four_digits_and_trims_the_newline() {
        assert_eq!(read_pin("2468\r\n".as_bytes()).unwrap(), "2468");
        assert!(matches!(
            read_pin("246\n".as_bytes()),
            Err(ProvisionError::Invalid(_))
        ));
        assert!(matches!(
            read_pin("".as_bytes()),
            Err(ProvisionError::Invalid(_))
        ));
    }

    #[test]
    fn check_target_rules() {
        let config = site();
        assert!(check_target(&config, &PinTarget::Engineer).is_ok());
        assert!(check_target(&config, &PinTarget::Member("member1".to_string())).is_ok());
        for bad in ["engineer", "member9", "../x"] {
            assert!(
                matches!(
                    check_target(&config, &PinTarget::Member(bad.to_string())),
                    Err(ProvisionError::Invalid(_))
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn config_dir_is_the_parent_or_the_current_directory() {
        assert_eq!(
            config_dir_of(Path::new("/srv/site/iemmixer.toml")),
            PathBuf::from("/srv/site")
        );
        assert_eq!(
            config_dir_of(Path::new("iemmixer.toml")),
            PathBuf::from(".")
        );
    }

    #[test]
    fn store_pin_writes_a_hash_the_installation_can_verify() {
        let dir = tempfile::tempdir().unwrap();
        store_pin(dir.path(), &PinTarget::Engineer, "2468").unwrap();
        let secrets = dir.path().join(SECRETS_DIR);
        let hasher = PinHasher::new(pepper::load_or_create(&secrets).unwrap());
        let store = PinStore::load(&secrets).unwrap();
        assert!(hasher.verify("2468", store.engineer_hash().unwrap()));
    }

    #[test]
    fn store_pin_never_re_peppers_existing_hashes() {
        // Provisioning one PIN after the pepper was lost must not create a
        // new pepper: every other stored hash would silently stop verifying.
        let dir = tempfile::tempdir().unwrap();
        store_pin(dir.path(), &PinTarget::Engineer, "2468").unwrap();
        let secrets = dir.path().join(SECRETS_DIR);
        let hashes = secrets.join(crate::pin_store::PIN_HASHES_FILE);
        std::fs::remove_file(secrets.join(pepper::PEPPER_FILE)).unwrap();
        let before = std::fs::read(&hashes).unwrap();
        let err = store_pin(
            dir.path(),
            &PinTarget::Member("member1".to_string()),
            "1357",
        )
        .unwrap_err();
        assert!(matches!(err, ProvisionError::Io(_)), "{err}");
        assert!(!secrets.join(pepper::PEPPER_FILE).exists());
        assert_eq!(std::fs::read(&hashes).unwrap(), before);
    }

    #[test]
    fn labels_name_the_target() {
        assert_eq!(PinTarget::Engineer.label(), "engineer");
        assert_eq!(
            PinTarget::Member("member1".to_string()).label(),
            "member member1"
        );
    }
}
