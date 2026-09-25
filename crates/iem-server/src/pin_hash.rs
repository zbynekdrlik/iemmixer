//! PIN hashing (program spec §5.3): argon2id with OWASP parameters
//! (m = 19 MiB, t = 2, p = 1), keyed with a secret pepper so a copied hash
//! file cannot be brute-forced offline. Stored as PHC strings.

use std::sync::{Arc, OnceLock};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use rand_core::OsRng;

/// Length of the pepper (argon2 secret) in bytes.
pub const PEPPER_LEN: usize = 32;
/// PIN length (4 digits, unchanged from the predecessor — spec P9).
pub const PIN_LEN: usize = 4;
/// argon2id memory cost in KiB (19 MiB).
pub const PIN_M_COST_KIB: u32 = 19 * 1024;
/// argon2id iterations.
pub const PIN_T_COST: u32 = 2;
/// argon2id parallelism.
pub const PIN_P_COST: u32 = 1;

const DUMMY_PIN: &str = "no-pin-set";

/// Whether `pin` is exactly [`PIN_LEN`] ASCII digits.
pub fn is_valid_pin_format(pin: &str) -> bool {
    pin.len() == PIN_LEN && pin.bytes().all(|b| b.is_ascii_digit())
}

/// Hashes and verifies PINs with a fixed pepper.
#[derive(Clone)]
pub struct PinHasher {
    pepper: Arc<[u8; PEPPER_LEN]>,
    params: Params,
    dummy: Arc<OnceLock<String>>,
}

impl std::fmt::Debug for PinHasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinHasher").finish_non_exhaustive()
    }
}

impl PinHasher {
    /// Production hasher (OWASP parameters).
    pub fn new(pepper: [u8; PEPPER_LEN]) -> Self {
        let params = Params::new(PIN_M_COST_KIB, PIN_T_COST, PIN_P_COST, None)
            .expect("OWASP argon2id parameters are valid");
        Self::with_params(pepper, params)
    }

    /// Minimal-cost hasher for fast tests.
    #[cfg(test)]
    pub fn for_tests(pepper: [u8; PEPPER_LEN]) -> Self {
        let params = Params::new(Params::MIN_M_COST, 1, 1, None)
            .expect("minimal argon2 parameters are valid");
        Self::with_params(pepper, params)
    }

    fn with_params(pepper: [u8; PEPPER_LEN], params: Params) -> Self {
        Self {
            pepper: Arc::new(pepper),
            params,
            dummy: Arc::new(OnceLock::new()),
        }
    }

    fn argon2(&self) -> Argon2<'_> {
        Argon2::new_with_secret(
            &self.pepper[..],
            Algorithm::Argon2id,
            Version::V0x13,
            self.params.clone(),
        )
        .expect("a 32-byte pepper is within the argon2 secret limit")
    }

    /// argon2id PHC string of `pin` with a fresh random salt.
    pub fn hash(&self, pin: &str) -> String {
        let salt = SaltString::generate(&mut OsRng);
        self.argon2()
            .hash_password(pin.as_bytes(), &salt)
            .expect("argon2id hashing with valid parameters cannot fail")
            .to_string()
    }

    /// Whether `pin` matches the PHC string `phc` (false for malformed input).
    pub fn verify(&self, pin: &str, phc: &str) -> bool {
        match PasswordHash::new(phc) {
            Ok(parsed) => self
                .argon2()
                .verify_password(pin.as_bytes(), &parsed)
                .is_ok(),
            Err(_) => false,
        }
    }

    /// Like [`Self::verify`]; with no stored hash it still does one
    /// verification's work (against a dummy hash) and returns false, so a
    /// missing PIN is not revealed by timing.
    pub fn verify_optional(&self, pin: &str, phc: Option<&str>) -> bool {
        match phc {
            Some(phc) => self.verify(pin, phc),
            None => {
                let dummy = self.dummy.get_or_init(|| self.hash(DUMMY_PIN));
                let _ = self.verify(pin, dummy);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEPPER_A: [u8; PEPPER_LEN] = [7u8; PEPPER_LEN];
    const PEPPER_B: [u8; PEPPER_LEN] = [9u8; PEPPER_LEN];

    #[test]
    fn hash_verifies_the_same_pin_and_rejects_another() {
        let hasher = PinHasher::for_tests(PEPPER_A);
        let phc = hasher.hash("2468");
        assert!(hasher.verify("2468", &phc));
        assert!(!hasher.verify("2469", &phc));
    }

    #[test]
    fn hashes_are_salted() {
        let hasher = PinHasher::for_tests(PEPPER_A);
        assert_ne!(hasher.hash("2468"), hasher.hash("2468"));
    }

    #[test]
    fn the_pepper_is_part_of_the_hash() {
        let phc = PinHasher::for_tests(PEPPER_A).hash("2468");
        assert!(!PinHasher::for_tests(PEPPER_B).verify("2468", &phc));
    }

    #[test]
    fn production_parameters_are_owasp_argon2id() {
        let hasher = PinHasher::new(PEPPER_A);
        let phc = hasher.hash("2468");
        assert!(phc.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{phc}");
        assert!(hasher.verify("2468", &phc));
    }

    #[test]
    fn malformed_hashes_never_verify() {
        let hasher = PinHasher::for_tests(PEPPER_A);
        assert!(!hasher.verify("2468", "not-a-phc"));
        assert!(!hasher.verify("2468", ""));
    }

    #[test]
    fn missing_hash_never_verifies() {
        let hasher = PinHasher::for_tests(PEPPER_A);
        assert!(!hasher.verify_optional("2468", None));
        assert!(!hasher.verify_optional("no-pin-set", None));
    }

    #[test]
    fn verify_optional_checks_a_present_hash() {
        let hasher = PinHasher::for_tests(PEPPER_A);
        let phc = hasher.hash("2468");
        assert!(hasher.verify_optional("2468", Some(&phc)));
        assert!(!hasher.verify_optional("1357", Some(&phc)));
    }

    #[test]
    fn pin_format_is_exactly_four_ascii_digits() {
        for ok in ["0000", "2468", "9999"] {
            assert!(is_valid_pin_format(ok), "{ok}");
        }
        for bad in ["", "123", "12345", "12a4", " 123", "１２３４"] {
            assert!(!is_valid_pin_format(bad), "{bad:?}");
        }
    }

    #[test]
    fn debug_output_never_shows_the_pepper() {
        let shown = format!("{:?}", PinHasher::for_tests(PEPPER_A));
        assert_eq!(shown, "PinHasher { .. }");
    }
}
