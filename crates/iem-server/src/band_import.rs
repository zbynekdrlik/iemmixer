//! Server-side band data from the predecessor (S4, program spec §3.4
//! "Migration", P9): PINs with their current values (hashed), the JWT secret
//! and VAPID key (phones stay logged in and subscribed), push subscriptions,
//! the LAN HTTPS certificate and member photos. Everything is read from files;
//! every function has a dry run that writes nothing. Values are never logged.

use std::io::{self, Write as _};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::pepper;
use crate::pin_hash::{PinHasher, is_valid_pin_format};
use crate::pin_store::{ENGINEER_ID, PinStore};
use crate::push_store::PushSubscription;
use crate::secrets::write_new_private;

/// The predecessor's placeholder JWT secret (never a real one).
pub const LEGACY_JWT_PLACEHOLDER: &str = "change-me-in-production";
pub const PUSH_FILE: &str = "push_subscriptions.json";
/// Without it the server's one-time cleanup empties the subscriptions.
pub const PUSH_MARKER: &str = "push_subs_v2_migrated";
/// F22: member photos are at most 256 KB.
pub const PHOTO_MAX_BYTES: usize = 256 * 1024;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// The flat keys the migration reads from the predecessor's `config.yaml`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct LegacyConfig {
    pub jwt_secret: Option<String>,
    pub vapid_private_key: Option<String>,
    pub engineer_pin: Option<String>,
    pub tls_cert: String,
    pub tls_key: String,
}

impl std::fmt::Debug for LegacyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LegacyConfig")
            .field("tls_cert", &self.tls_cert)
            .field("tls_key", &self.tls_key)
            .finish_non_exhaustive()
    }
}

fn scalar(raw: &str) -> Result<Option<String>, String> {
    let v = raw.trim();
    for quote in ['"', '\''] {
        if let Some(rest) = v.strip_prefix(quote) {
            let end = rest.find(quote).ok_or("unterminated quote")?;
            let (inner, after) = rest.split_at(end);
            let after = after.get(1..).unwrap_or_default().trim();
            if !(after.is_empty() || after.starts_with('#')) {
                return Err("text after the quoted value".into());
            }
            if inner.contains('\\') {
                return Err("escapes are not supported".into());
            }
            return Ok(Some(inner.to_owned()));
        }
    }
    let v = v.split(" #").next().unwrap_or_default().trim();
    Ok(match v {
        "" | "~" | "null" => None,
        _ => Some(v.to_owned()),
    })
}

/// Reads the top-level `jwt_secret`, `vapid_private_key`, `engineer_pin`,
/// `tls_cert` and `tls_key` lines (`key: value`, plain or quoted).
pub fn parse_legacy_config(text: &str) -> Result<LegacyConfig, String> {
    let mut c = LegacyConfig {
        tls_cert: "cert.pem".into(),
        tls_key: "key.pem".into(),
        ..LegacyConfig::default()
    };
    for (n, line) in text.lines().enumerate() {
        if line.starts_with([' ', '\t', '#', '-']) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if !matches!(
            key,
            "jwt_secret" | "vapid_private_key" | "engineer_pin" | "tls_cert" | "tls_key"
        ) {
            continue;
        }
        let v = scalar(value).map_err(|e| format!("config line {}: {key}: {e}", n + 1))?;
        match key {
            "jwt_secret" => c.jwt_secret = v,
            "vapid_private_key" => c.vapid_private_key = v,
            "engineer_pin" => c.engineer_pin = v,
            "tls_cert" => c.tls_cert = v.unwrap_or_else(|| "cert.pem".into()),
            _ => c.tls_key = v.unwrap_or_else(|| "key.pem".into()),
        }
    }
    Ok(c)
}

/// The predecessor's default PINs (compiled into it, never into iemmixer):
/// supplied from a private file of `member=NNNN` / `engineer=NNNN` lines.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct DefaultPins {
    pub member: Option<String>,
    pub engineer: Option<String>,
}

impl std::fmt::Debug for DefaultPins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultPins").finish_non_exhaustive()
    }
}

pub fn parse_default_pins(text: &str) -> Result<DefaultPins, String> {
    let mut d = DefaultPins::default();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, pin) = line
            .split_once('=')
            .ok_or_else(|| format!("default PINs line {}: not key=PIN", n + 1))?;
        let pin = pin.trim();
        if !is_valid_pin_format(pin) {
            return Err(format!("default PINs line {}: not 4 digits", n + 1));
        }
        match key.trim() {
            "member" => d.member = Some(pin.to_owned()),
            "engineer" => d.engineer = Some(pin.to_owned()),
            other => {
                return Err(format!(
                    "default PINs line {}: unknown key {other:?}",
                    n + 1
                ));
            }
        }
    }
    Ok(d)
}

/// One PIN to import: a member id, or [`ENGINEER_ID`] for the engineer PIN.
#[derive(Clone, PartialEq, Eq)]
pub struct PinRequest {
    pub owner: String,
    pub pin: String,
}

impl std::fmt::Debug for PinRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinRequest")
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOutcome {
    /// Written (or, in a dry run, would be written).
    Set,
    /// The imported hash already verifies this PIN.
    Unchanged,
    /// A PIN set in iemmixer is kept (never overwritten by an import).
    KeptIemmixerPin,
}

/// Hashes and stores the PINs in `secrets_dir` with the installation's pepper.
pub fn import_pins(
    secrets_dir: &Path,
    requests: &[PinRequest],
    dry_run: bool,
) -> io::Result<Vec<(String, PinOutcome)>> {
    for r in requests {
        if !is_valid_pin_format(&r.pin) {
            return Err(invalid(format!("the PIN for {} is not 4 digits", r.owner)));
        }
    }
    let mut store = PinStore::load(secrets_dir)?;
    let hasher = if dry_run && !secrets_dir.join(pepper::PEPPER_FILE).exists() {
        None
    } else {
        Some(PinHasher::new(pepper::load_or_create(secrets_dir)?))
    };
    let mut out = Vec::with_capacity(requests.len());
    for r in requests {
        let engineer = r.owner == ENGINEER_ID;
        let existing = if engineer {
            store.engineer_hash()
        } else {
            store.member_hash(&r.owner)
        }
        .map(str::to_owned);
        let outcome = match (&existing, &hasher) {
            (Some(_), _) if !store.is_imported(&r.owner) => PinOutcome::KeptIemmixerPin,
            (Some(phc), Some(h)) if h.verify(&r.pin, phc) => PinOutcome::Unchanged,
            (_, Some(h)) if !dry_run => {
                let phc = h.hash(&r.pin);
                let written = if engineer {
                    store.import_engineer_hash(phc)?
                } else {
                    store.import_member_hash(&r.owner, phc)?
                };
                if written {
                    PinOutcome::Set
                } else {
                    PinOutcome::KeptIemmixerPin
                }
            }
            _ => PinOutcome::Set,
        };
        out.push((r.owner.clone(), outcome));
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOutcome {
    /// Written (or, in a dry run, would be written).
    Created,
    /// The same content is already there.
    Unchanged,
}

/// The JWT secret must be a real one (the predecessor generated or set it).
pub fn check_jwt_secret(v: &str) -> Result<(), String> {
    if v.is_empty() || v == LEGACY_JWT_PLACEHOLDER {
        return Err("the predecessor's JWT secret is missing or the placeholder".into());
    }
    if v.trim() != v || v.contains(['\r', '\n']) {
        return Err("the predecessor's JWT secret has surrounding blanks".into());
    }
    Ok(())
}

/// The VAPID key must be a base64url P-256 private scalar.
pub fn check_vapid(v: &str) -> Result<(), String> {
    let raw = URL_SAFE_NO_PAD
        .decode(v)
        .map_err(|_| "the predecessor's VAPID key is not base64url".to_owned())?;
    p256::SecretKey::from_slice(&raw)
        .map(|_| ())
        .map_err(|_| "the predecessor's VAPID key is not a P-256 private key".into())
}

/// Writes a secret file once (owner-only); an equal one is left alone, a
/// different one is an error — secrets are never overwritten.
pub fn import_secret(path: &Path, value: &str, dry_run: bool) -> io::Result<FileOutcome> {
    match std::fs::read_to_string(path) {
        Ok(existing) if existing.trim() == value => Ok(FileOutcome::Unchanged),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{} already holds a different secret; secrets are never overwritten \
                 (move it aside to take the predecessor's)",
                path.display()
            ),
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if !dry_run {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                write_new_private(path, value.as_bytes())?;
            }
            Ok(FileOutcome::Created)
        }
        Err(e) => Err(e),
    }
}

/// Copies `data` to `path` unless an equal file is there; a different
/// existing file is an error. `private`: owner-only, created exclusively.
fn import_file(path: &Path, data: &[u8], private: bool, dry_run: bool) -> io::Result<FileOutcome> {
    match std::fs::read(path) {
        Ok(existing) if existing == data => Ok(FileOutcome::Unchanged),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{} already exists with other content; it is never overwritten",
                path.display()
            ),
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if !dry_run {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if private {
                    write_new_private(path, data)?;
                } else {
                    let mut f = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(path)?;
                    f.write_all(data)?;
                    f.sync_all()?;
                }
            }
            Ok(FileOutcome::Created)
        }
        Err(e) => Err(e),
    }
}

/// The LAN HTTPS certificate and key (PEM) into `out_dir` as `cert.pem` and
/// `key.pem`, so LAN phones keep trusting the same certificate.
pub fn import_tls(
    cert: &Path,
    key: &Path,
    out_dir: &Path,
    dry_run: bool,
) -> io::Result<[FileOutcome; 2]> {
    let cert_pem = std::fs::read(cert)?;
    let key_pem = std::fs::read(key)?;
    let has = |data: &[u8], marker: &str| String::from_utf8_lossy(data).contains(marker);
    if !has(&cert_pem, "-----BEGIN CERTIFICATE-----") {
        return Err(invalid(format!(
            "{} is not a PEM certificate",
            cert.display()
        )));
    }
    if !(has(&key_pem, "-----BEGIN ") && has(&key_pem, "PRIVATE KEY-----")) {
        return Err(invalid(format!(
            "{} is not a PEM private key",
            key.display()
        )));
    }
    Ok([
        import_file(&out_dir.join("cert.pem"), &cert_pem, false, dry_run)?,
        import_file(&out_dir.join("key.pem"), &key_pem, true, dry_run)?,
    ])
}

/// Push subscriptions: the predecessor's (only if its one-time cleanup ran,
/// marker present) merged into the band directory's by endpoint, plus the
/// marker. Returns (added, total).
pub fn import_push(legacy_dir: &Path, out_dir: &Path, dry_run: bool) -> io::Result<(usize, usize)> {
    let read = |dir: &Path| -> io::Result<Vec<PushSubscription>> {
        match std::fs::read_to_string(dir.join(PUSH_FILE)) {
            Ok(t) => serde_json::from_str(&t)
                .map_err(|e| invalid(format!("{}: {e}", dir.join(PUSH_FILE).display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    };
    let legacy = if legacy_dir.join(PUSH_MARKER).exists() {
        read(legacy_dir)?
    } else {
        Vec::new()
    };
    let mut all = read(out_dir)?;
    let mut added = 0;
    for s in legacy {
        if !all.iter().any(|x| x.endpoint == s.endpoint) {
            all.push(s);
            added += 1;
        }
    }
    if !dry_run {
        std::fs::create_dir_all(out_dir)?;
        let json = serde_json::to_string_pretty(&all).map_err(io::Error::other)?;
        crate::atomic_write(&out_dir.join(PUSH_FILE), &json)?;
        std::fs::write(out_dir.join(PUSH_MARKER), b"")?;
    }
    Ok((added, all.len()))
}

/// A member photo (JPEG, ≤ 256 KB) copied to `dst` (replacing an older copy).
pub fn import_photo(src: &Path, dst: &Path, dry_run: bool) -> io::Result<()> {
    let data = std::fs::read(src)?;
    if data.len() > PHOTO_MAX_BYTES || !data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Err(invalid(format!(
            "{} is not a JPEG of at most 256 KB",
            src.display()
        )));
    }
    if !dry_run {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = dst.with_extension("tmp");
        std::fs::write(&tmp, data)?;
        std::fs::rename(tmp, dst)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{JWT_SECRET_FILE, SECRETS_DIR, VAPID_PRIVATE_FILE};

    #[test]
    fn the_legacy_config_is_read_flat_and_quoted() {
        let c = parse_legacy_config(
            "port: 80\njwt_secret: \"auto-1-2\"\nvapid_private_key: 'abc' # key\n\
             engineer_pin: 4321\ntls_cert: my.pem\nmembers:\n  - jwt_secret: \"nested\"\n# jwt_secret: x\n",
        )
        .unwrap();
        assert_eq!(c.jwt_secret.as_deref(), Some("auto-1-2"));
        assert_eq!(c.vapid_private_key.as_deref(), Some("abc"));
        assert_eq!(c.engineer_pin.as_deref(), Some("4321"));
        assert_eq!(c.tls_cert, "my.pem");
        assert_eq!(c.tls_key, "key.pem");
        assert!(
            !format!("{c:?}").contains("auto-1-2"),
            "Debug never shows secrets"
        );
        let empty = parse_legacy_config("engineer_pin: ~\ntls_key: null\njwt_secret:\n").unwrap();
        assert_eq!(empty.engineer_pin, None);
        assert_eq!(empty.jwt_secret, None);
        assert_eq!(empty.tls_key, "key.pem");
        assert_eq!(parse_legacy_config("").unwrap().tls_cert, "cert.pem");
        for bad in [
            "jwt_secret: \"open",
            "jwt_secret: \"a\" b",
            "jwt_secret: \"a\\\"b\"",
        ] {
            assert!(parse_legacy_config(bad).is_err(), "{bad}");
        }
        assert_eq!(
            parse_legacy_config("jwt_secret: 'x' # c")
                .unwrap()
                .jwt_secret
                .as_deref(),
            Some("x")
        );
        assert_eq!(
            parse_legacy_config("jwt_secret: plain # c")
                .unwrap()
                .jwt_secret
                .as_deref(),
            Some("plain")
        );
    }

    #[test]
    fn default_pins_come_from_their_file_only() {
        let d = parse_default_pins("# defaults\nmember = 1234\nengineer=5678\n\n").unwrap();
        assert_eq!(d.member.as_deref(), Some("1234"));
        assert_eq!(d.engineer.as_deref(), Some("5678"));
        assert!(!format!("{d:?}").contains("1234"));
        assert!(parse_default_pins("member=12345").is_err());
        assert!(parse_default_pins("member").is_err());
        assert!(parse_default_pins("guest=1234").is_err());
        assert_eq!(parse_default_pins("").unwrap(), DefaultPins::default());
    }

    fn req(owner: &str, pin: &str) -> PinRequest {
        PinRequest {
            owner: owner.into(),
            pin: pin.into(),
        }
    }

    #[test]
    fn imported_pins_verify_with_the_same_value() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join(SECRETS_DIR);
        let dry = import_pins(&secrets, &[req("member1", "1357")], true).unwrap();
        assert_eq!(dry, vec![("member1".to_owned(), PinOutcome::Set)]);
        assert!(!secrets.exists(), "a dry run writes nothing");
        let out = import_pins(
            &secrets,
            &[req("member1", "1357"), req(ENGINEER_ID, "2468")],
            false,
        )
        .unwrap();
        assert_eq!(out[0].1, PinOutcome::Set);
        assert_eq!(out[1].1, PinOutcome::Set);
        let h = PinHasher::new(pepper::load_or_create(&secrets).unwrap());
        let store = PinStore::load(&secrets).unwrap();
        assert!(h.verify("1357", store.member_hash("member1").unwrap()));
        assert!(h.verify("2468", store.engineer_hash().unwrap()));
        let again = import_pins(&secrets, &[req("member1", "1357")], true).unwrap();
        assert_eq!(again[0].1, PinOutcome::Unchanged);
        let text =
            std::fs::read_to_string(secrets.join(crate::pin_store::PIN_HASHES_FILE)).unwrap();
        assert!(!text.contains("1357") && !text.contains("2468"));
    }

    #[test]
    fn a_pin_changed_in_iemmixer_survives_a_later_import() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join(SECRETS_DIR);
        import_pins(&secrets, &[req("member1", "1357")], false).unwrap();
        let h = PinHasher::new(pepper::load_or_create(&secrets).unwrap());
        let mut store = PinStore::load(&secrets).unwrap();
        store.set_member_hash("member1", h.hash("9753")).unwrap();
        let out = import_pins(&secrets, &[req("member1", "1357")], false).unwrap();
        assert_eq!(out[0].1, PinOutcome::KeptIemmixerPin);
        let store = PinStore::load(&secrets).unwrap();
        assert!(h.verify("9753", store.member_hash("member1").unwrap()));
        assert!(import_pins(&secrets, &[req("member2", "12a4")], true).is_err());
    }

    #[test]
    fn a_changed_predecessor_pin_replaces_the_imported_hash() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join(SECRETS_DIR);
        import_pins(&secrets, &[req("member1", "1357")], false).unwrap();
        let out = import_pins(&secrets, &[req("member1", "2222")], true).unwrap();
        assert_eq!(out[0].1, PinOutcome::Set);
        import_pins(&secrets, &[req("member1", "2222")], false).unwrap();
        let h = PinHasher::new(pepper::load_or_create(&secrets).unwrap());
        assert!(
            h.verify(
                "2222",
                PinStore::load(&secrets)
                    .unwrap()
                    .member_hash("member1")
                    .unwrap()
            )
        );
    }

    #[test]
    fn secrets_are_created_once_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SECRETS_DIR).join(JWT_SECRET_FILE);
        assert_eq!(
            import_secret(&path, "auto-1", true).unwrap(),
            FileOutcome::Created
        );
        assert!(!path.exists());
        assert_eq!(
            import_secret(&path, "auto-1", false).unwrap(),
            FileOutcome::Created
        );
        assert_eq!(
            import_secret(&path, "auto-1", false).unwrap(),
            FileOutcome::Unchanged
        );
        let err = import_secret(&path, "auto-2", false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(!err.to_string().contains("auto-"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let loaded = crate::secrets::load_or_create(&dir.path().join(SECRETS_DIR)).unwrap();
        assert_eq!(loaded.jwt_secret, "auto-1");
    }

    #[test]
    fn a_token_signed_with_the_legacy_secret_verifies() {
        use jsonwebtoken::{EncodingKey, Header, encode};
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join(SECRETS_DIR);
        let legacy = "auto-5f3a-9c1d";
        import_secret(&secrets.join(JWT_SECRET_FILE), legacy, false).unwrap();
        let loaded = crate::secrets::load_or_create(&secrets).unwrap();
        let exp = u64::try_from(chrono::Utc::now().timestamp()).unwrap() + 3600;
        let claims = iem_core::AuthClaims {
            sub: "member1".into(),
            engineer: false,
            exp,
            iat: exp - 7200,
        };
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(legacy.as_bytes()),
        )
        .unwrap();
        let got = crate::auth::extract_claims(&token, &loaded.jwt_secret).unwrap();
        assert_eq!(got.sub, "member1");
    }

    #[test]
    fn secret_values_are_checked() {
        assert!(check_jwt_secret("auto-1").is_ok());
        assert!(check_jwt_secret("").is_err());
        assert!(check_jwt_secret(LEGACY_JWT_PLACEHOLDER).is_err());
        assert!(check_jwt_secret(" x").is_err());
        let key = URL_SAFE_NO_PAD.encode([7u8; 32]);
        assert!(check_vapid(&key).is_ok());
        assert!(check_vapid(&URL_SAFE_NO_PAD.encode([0u8; 32])).is_err());
        assert!(check_vapid(&URL_SAFE_NO_PAD.encode([7u8; 16])).is_err());
        assert!(check_vapid("not base64!").is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SECRETS_DIR).join(VAPID_PRIVATE_FILE);
        import_secret(&path, &key, false).unwrap();
        let loaded = crate::secrets::load_or_create(&dir.path().join(SECRETS_DIR)).unwrap();
        assert_eq!(loaded.vapid_private_key, key);
    }

    const CERT: &str = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";
    /// A PEM-shaped stand-in (no key material; built so scanners see no key).
    fn key() -> String {
        format!(
            "-----BEGIN {k} KEY-----\nMIIE\n-----END {k} KEY-----\n",
            k = "PRIVATE"
        )
    }

    #[test]
    fn the_certificate_moves_unchanged_and_is_never_overwritten() {
        let legacy = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let (c, k) = (legacy.path().join("c.pem"), legacy.path().join("k.pem"));
        std::fs::write(&c, CERT).unwrap();
        std::fs::write(&k, key()).unwrap();
        assert_eq!(
            import_tls(&c, &k, out.path(), true).unwrap(),
            [FileOutcome::Created; 2]
        );
        assert!(!out.path().join("cert.pem").exists());
        import_tls(&c, &k, out.path(), false).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.path().join("cert.pem")).unwrap(),
            CERT
        );
        assert_eq!(
            std::fs::read_to_string(out.path().join("key.pem")).unwrap(),
            key()
        );
        assert_eq!(
            import_tls(&c, &k, out.path(), false).unwrap(),
            [FileOutcome::Unchanged; 2]
        );
        std::fs::write(&c, CERT.replace("MIIB", "MIIC")).unwrap();
        assert_eq!(
            import_tls(&c, &k, out.path(), false).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        std::fs::write(&c, "not a cert").unwrap();
        assert!(import_tls(&c, &k, out.path(), true).is_err());
        std::fs::write(&c, CERT).unwrap();
        std::fs::write(&k, CERT).unwrap();
        assert!(import_tls(&c, &k, out.path(), true).is_err());
        assert!(import_tls(&legacy.path().join("none"), &k, out.path(), true).is_err());
    }

    fn sub(n: &str) -> PushSubscription {
        PushSubscription {
            endpoint: format!("https://push.example.org/{n}"),
            p256dh: "p".into(),
            auth: "a".into(),
        }
    }

    #[test]
    fn push_subscriptions_merge_by_endpoint_with_the_marker() {
        let legacy = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        fn write(dir: &Path, subs: &[PushSubscription]) {
            std::fs::write(dir.join(PUSH_FILE), serde_json::to_string(subs).unwrap()).unwrap();
        }
        write(legacy.path(), &[sub("a"), sub("b")]);
        // Without the predecessor's marker its cleanup had wiped them.
        assert_eq!(
            import_push(legacy.path(), out.path(), false).unwrap(),
            (0, 0)
        );
        assert!(out.path().join(PUSH_MARKER).exists());
        std::fs::write(legacy.path().join(PUSH_MARKER), b"").unwrap();
        write(out.path(), &[sub("b"), sub("c")]);
        assert_eq!(
            import_push(legacy.path(), out.path(), true).unwrap(),
            (1, 3)
        );
        assert_eq!(
            import_push(legacy.path(), out.path(), false).unwrap(),
            (1, 3)
        );
        let got: Vec<PushSubscription> =
            serde_json::from_str(&std::fs::read_to_string(out.path().join(PUSH_FILE)).unwrap())
                .unwrap();
        assert_eq!(got, vec![sub("b"), sub("c"), sub("a")]);
        assert_eq!(
            import_push(legacy.path(), out.path(), false).unwrap(),
            (0, 3)
        );
        std::fs::write(legacy.path().join(PUSH_FILE), "{").unwrap();
        assert!(import_push(legacy.path(), out.path(), true).is_err());
    }

    #[test]
    fn photos_must_be_small_jpegs() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.jpg");
        let dst = dir.path().join("photos").join("member1.jpg");
        std::fs::write(&src, [0xFF_u8, 0xD8, 0xFF, 0xE0, 1, 2]).unwrap();
        import_photo(&src, &dst, true).unwrap();
        assert!(!dst.exists());
        import_photo(&src, &dst, false).unwrap();
        assert_eq!(
            std::fs::read(&dst).unwrap(),
            vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2]
        );
        std::fs::write(&src, b"GIF89a").unwrap();
        assert!(import_photo(&src, &dst, true).is_err());
        let mut big: Vec<u8> = vec![0xFF, 0xD8, 0xFF];
        big.resize(PHOTO_MAX_BYTES + 1, 0);
        std::fs::write(&src, &big).unwrap();
        assert!(import_photo(&src, &dst, true).is_err());
        big.truncate(PHOTO_MAX_BYTES);
        std::fs::write(&src, big).unwrap();
        assert!(import_photo(&src, &dst, true).is_ok());
    }
}
