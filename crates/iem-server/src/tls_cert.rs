// Ported from fohmixer 9f3d440 crates/fohmixer-hub/src/tls.rs (iemmixer#91).
//! The certificate of `https_domain` (#91): the site's `tls_cert` (the
//! chain, the certificate first) and `tls_key` (its private key, owner-only
//! on Unix) next to the site file, what the certificate covers and until
//! when, and when it is due for renewal. The ACME keeper (`acme.rs`) writes
//! both and reloads the HTTPS listener from them; a certificate put there by
//! hand is served the same way. Without a usable pair the keeper's listener
//! starts on a stand-in certificate ([`stand_in`]) until the first one comes.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::sign::CertifiedKey;

/// A certificate is renewed when it has less than this left: 30 days.
pub const RENEW_BEFORE_SECS: i64 = 30 * 24 * 60 * 60;

/// What a certificate covers and until when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertInfo {
    /// Its DNS names (subject alternative names).
    pub names: Vec<String>,
    /// Its end of validity (Unix seconds).
    pub not_after: i64,
}

impl CertInfo {
    /// Whether it names `name` (exactly; DNS names compare without case).
    pub fn covers(&self, name: &str) -> bool {
        self.names.iter().any(|n| n.eq_ignore_ascii_case(name))
    }

    /// Whole days left at `now` (negative once expired).
    pub fn days_left(&self, now: i64) -> i64 {
        (self.not_after - now).div_euclid(24 * 60 * 60)
    }

    /// Whether it is due for renewal at `now`.
    pub fn renewal_due(&self, now: i64) -> bool {
        self.not_after - now < RENEW_BEFORE_SECS
    }
}

/// A chain and its key, as PEM text, and what the certificate covers.
#[derive(Clone, PartialEq, Eq)]
pub struct Pem {
    pub chain: String,
    pub key: String,
    pub info: CertInfo,
}

impl std::fmt::Debug for Pem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pem")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// The chain's certificates, in order.
fn chain_ders(chain: &str) -> Result<Vec<CertificateDer<'static>>, String> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(chain.as_bytes())
        .collect::<Result<_, _>>()
        .map_err(|e| format!("the chain does not parse: {e}"))?;
    if certs.is_empty() {
        return Err("the chain holds no certificate".to_string());
    }
    Ok(certs)
}

/// What the chain's first certificate covers and until when.
pub fn cert_info(chain: &str) -> Result<CertInfo, String> {
    let certs = chain_ders(chain)?;
    let (_, cert) = x509_parser::parse_x509_certificate(&certs[0])
        .map_err(|e| format!("the certificate does not parse: {e}"))?;
    let names = cert
        .subject_alternative_name()
        .map_err(|e| format!("the certificate's names do not parse: {e}"))?
        .map(|san| {
            san.value
                .general_names
                .iter()
                .filter_map(|name| match name {
                    x509_parser::extensions::GeneralName::DNSName(dns) => Some(dns.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(CertInfo {
        names,
        not_after: cert.validity().not_after.timestamp(),
    })
}

/// `chain` and `key` as rustls serves them, on ring: a key that is not the
/// certificate's is an error (what rustls's `with_single_cert` checks).
pub fn certified_key(chain: &str, key: &str) -> Result<Arc<CertifiedKey>, String> {
    let certs = chain_ders(chain)?;
    let key = PrivateKeyDer::from_pem_slice(key.as_bytes())
        .map_err(|e| format!("the private key does not parse: {e}"))?;
    CertifiedKey::from_der(certs, key, &rustls::crypto::ring::default_provider())
        .map(Arc::new)
        .map_err(|e| format!("the certificate and the key do not go together: {e}"))
}

/// A checked chain and key: they parse, go together, and cover `name`.
pub fn checked(chain: String, key: String, name: &str) -> Result<Pem, String> {
    certified_key(&chain, &key)?;
    let info = cert_info(&chain)?;
    if !info.covers(name) {
        return Err(format!(
            "the certificate names {:?}, not {name}",
            info.names
        ));
    }
    Ok(Pem { chain, key, info })
}

/// A self-signed certificate for `name`, made in memory: what the HTTPS
/// listener serves while the keeper has none stored yet (no browser trusts
/// it; it only lets the listener start, the keeper's first certificate
/// replaces it without a restart).
pub fn stand_in(name: &str) -> Result<Pem, String> {
    let key = rcgen::KeyPair::generate().map_err(|e| format!("a stand-in key: {e}"))?;
    let params = rcgen::CertificateParams::new(vec![name.to_string()])
        .map_err(|e| format!("a stand-in certificate for {name}: {e}"))?;
    let cert = params
        .self_signed(&key)
        .map_err(|e| format!("a stand-in certificate for {name}: {e}"))?;
    checked(cert.pem(), key.serialize_pem(), name)
}

/// The site's certificate files: `tls_cert` and `tls_key`.
#[derive(Debug, Clone)]
pub struct CertFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
}

impl CertFiles {
    /// The files of a site whose `tls_cert` and `tls_key` resolve against
    /// `config_dir`.
    pub fn new(config_dir: &Path, cert: &str, key: &str) -> Self {
        Self {
            cert: config_dir.join(cert),
            key: config_dir.join(key),
        }
    }

    /// The stored chain and key for `name`: `Ok(None)` when there is none,
    /// an error when the files do not parse, do not go together or do not
    /// cover `name` (the caller logs it and gets a new one).
    pub fn load(&self, name: &str) -> io::Result<Option<Result<Pem, String>>> {
        let read = |path: &Path| match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        };
        match (read(&self.cert)?, read(&self.key)?) {
            (Some(chain), Some(key)) => Ok(Some(checked(chain, key, name))),
            _ => Ok(None),
        }
    }

    /// Stores a checked chain and key: the key first, then the chain, each
    /// replaced whole (a crash leaves the old pair or a pair `load` refuses,
    /// never half a file).
    pub fn save(&self, pem: &Pem) -> io::Result<()> {
        for path in [&self.key, &self.cert] {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
        }
        crate::secrets::replace_private(&self.key, pem.key.as_bytes())?;
        crate::secrets::replace_private(&self.cert, pem.chain.as_bytes())
    }
}

#[cfg(test)]
pub(crate) mod test_certs {
    //! Test certificates: a test CA and leaf certificates it signs, and a
    //! TLS client that trusts only such a CA.

    use rcgen::{
        BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose,
    };

    /// A test CA.
    pub struct TestCa {
        pub pem: String,
        issuer: Issuer<'static, KeyPair>,
    }

    impl TestCa {
        #[allow(clippy::new_without_default)] // test support: a new CA each time
        pub fn new() -> Self {
            let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
            params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            params
                .distinguished_name
                .push(DnType::CommonName, "iemmixer test CA");
            params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
            let key = KeyPair::generate().unwrap();
            let cert = params.self_signed(&key).unwrap();
            Self {
                pem: cert.pem(),
                issuer: Issuer::new(params, key),
            }
        }

        /// A leaf for `names` valid from `not_before` to `not_after` (Unix
        /// seconds): its chain PEM and its key PEM.
        pub fn leaf(&self, names: &[&str], not_before: i64, not_after: i64) -> (String, String) {
            let mut params =
                CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
                    .unwrap();
            params.not_before = time::OffsetDateTime::from_unix_timestamp(not_before).unwrap();
            params.not_after = time::OffsetDateTime::from_unix_timestamp(not_after).unwrap();
            let key = KeyPair::generate().unwrap();
            let cert = params.signed_by(&key, &self.issuer).unwrap();
            (cert.pem(), key.serialize_pem())
        }

        /// Signs a CSR (DER) as a CA would: the certificate's PEM when the
        /// CSR is well formed and asks for exactly `name`.
        pub fn sign_csr(&self, der: &[u8], name: &str) -> Result<String, String> {
            let csr = rcgen::CertificateSigningRequestParams::from_der(&der.into())
                .map_err(|e| format!("bad CSR: {e}"))?;
            let names: Vec<String> = csr
                .params
                .subject_alt_names
                .iter()
                .map(|san| match san {
                    rcgen::SanType::DnsName(dns) => dns.as_str().to_string(),
                    other => format!("{other:?}"),
                })
                .collect();
            if names != [name] {
                return Err(format!("the CSR asks for {names:?}, not {name}"));
            }
            let cert = csr.signed_by(&self.issuer).map_err(|e| e.to_string())?;
            Ok(cert.pem())
        }
    }

    /// A TLS handshake with `addr` for `name` (SNI) that trusts only the CA
    /// `ca_pem`: what the certificate the server presents covers and until
    /// when, or why the handshake failed (a certificate of another CA or of
    /// another name, none).
    pub async fn handshake(
        addr: std::net::SocketAddr,
        name: &str,
        ca_pem: &str,
    ) -> Result<super::CertInfo, String> {
        use rustls::pki_types::pem::PemObject as _;
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from_pem_slice(ca_pem.as_bytes()).unwrap())
            .unwrap();
        let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let tcp = tokio::net::TcpStream::connect(addr)
            .await
            .map_err(|e| e.to_string())?;
        let server = rustls::pki_types::ServerName::try_from(name.to_string()).unwrap();
        let stream = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config))
            .connect(server, tcp)
            .await
            .map_err(|e| e.to_string())?;
        let leaf = stream.get_ref().1.peer_certificates().unwrap()[0].clone();
        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &leaf)
        );
        super::cert_info(&pem)
    }
}

#[cfg(test)]
mod tests {
    use super::test_certs::TestCa;
    use super::*;

    const DAY: i64 = 24 * 60 * 60;
    const NAME: &str = "mixer.example.org";

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }

    #[test]
    fn the_info_names_the_certificate_and_its_end() {
        let ca = TestCa::new();
        let end = now() + 90 * DAY;
        let (chain, _) = ca.leaf(&[NAME, "other.example.org"], now() - DAY, end);
        let info = cert_info(&chain).unwrap();
        assert_eq!(
            info.names,
            vec![NAME.to_string(), "other.example.org".to_string()]
        );
        assert_eq!(info.not_after, end);
        assert!(info.covers(NAME));
        assert!(info.covers("MIXER.example.ORG"));
        assert!(!info.covers("example.org"));
        assert!(!info.covers("x.mixer.example.org"));
        // The chain's first certificate counts (a leaf, then the CA).
        let with_ca = format!("{chain}{}", ca.pem);
        assert_eq!(cert_info(&with_ca).unwrap(), info);
        let ca_info = cert_info(&ca.pem).unwrap();
        assert!(ca_info.names.is_empty(), "no names: {ca_info:?}");
    }

    #[test]
    fn bad_chains_are_errors() {
        assert_eq!(cert_info("").unwrap_err(), "the chain holds no certificate");
        let error =
            cert_info("-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n").unwrap_err();
        assert!(error.starts_with("the chain does not parse"), "{error}");
        let garbage = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
        assert!(
            cert_info(garbage)
                .unwrap_err()
                .starts_with("the certificate does not parse")
        );
    }

    #[test]
    fn renewal_is_due_under_thirty_days() {
        let info = |left: i64| CertInfo {
            names: vec![],
            not_after: 1_000_000_000 + left,
        };
        let at = 1_000_000_000;
        assert!(!info(30 * DAY).renewal_due(at));
        assert!(info(30 * DAY - 1).renewal_due(at));
        assert!(info(-DAY).renewal_due(at));
        assert_eq!(RENEW_BEFORE_SECS, 2_592_000);
        assert_eq!(info(30 * DAY).days_left(at), 30);
        assert_eq!(info(30 * DAY - 1).days_left(at), 29);
        assert_eq!(info(0).days_left(at), 0);
        assert_eq!(info(-1).days_left(at), -1);
        assert_eq!(info(DAY + 1).days_left(at + 1), 1);
    }

    #[test]
    fn a_matching_pair_is_checked_and_a_foreign_key_is_not() {
        let ca = TestCa::new();
        let (chain, key) = ca.leaf(&[NAME], now() - DAY, now() + 90 * DAY);
        certified_key(&chain, &key).unwrap();
        let (_, other_key) = ca.leaf(&[NAME], now() - DAY, now() + 90 * DAY);
        let error = certified_key(&chain, &other_key).unwrap_err();
        assert!(
            error.starts_with("the certificate and the key do not go together"),
            "{error}"
        );
        assert!(
            certified_key(&chain, "not a key")
                .unwrap_err()
                .starts_with("the private key does not parse")
        );
        assert!(certified_key("", &key).is_err());
        let pem = checked(chain.clone(), key.clone(), NAME).unwrap();
        assert_eq!(pem.info.names, vec![NAME.to_string()]);
        assert_eq!(
            checked(chain.clone(), other_key, NAME).unwrap_err(),
            error,
            "a foreign key is refused before the names are read"
        );
        assert_eq!(
            checked(chain, key, "other.example.org").unwrap_err(),
            "the certificate names [\"mixer.example.org\"], not other.example.org"
        );
        let shown = format!("{pem:?}");
        assert!(!shown.contains("PRIVATE KEY"), "Debug never shows the key");
        assert!(
            shown.starts_with("Pem { info: CertInfo { names: [\"mixer.example.org\"]"),
            "Debug shows what the certificate covers: {shown}"
        );
    }

    #[test]
    fn a_stand_in_covers_the_name_and_is_checked() {
        let pem = stand_in(NAME).unwrap();
        assert_eq!(pem.info.names, vec![NAME.to_string()]);
        assert!(pem.info.not_after > now(), "{:?}", pem.info);
        assert!(pem.key.contains("PRIVATE KEY"));
    }

    #[test]
    fn the_files_save_and_load_a_checked_pair() {
        let dir = tempfile::tempdir().unwrap();
        let files = CertFiles::new(dir.path(), "cert.pem", "key.pem");
        assert_eq!(files.cert, dir.path().join("cert.pem"));
        assert_eq!(files.key, dir.path().join("key.pem"));
        assert!(files.load(NAME).unwrap().is_none());
        let ca = TestCa::new();
        let (chain, key) = ca.leaf(&[NAME], now() - DAY, now() + 90 * DAY);
        let pem = checked(chain, key, NAME).unwrap();
        files.save(&pem).unwrap();
        assert_eq!(std::fs::read_to_string(&files.cert).unwrap(), pem.chain);
        assert_eq!(std::fs::read_to_string(&files.key).unwrap(), pem.key);
        let loaded = files.load(NAME).unwrap().unwrap().unwrap();
        assert_eq!(loaded, pem);
        // Another configured name: the stored one is refused, not served.
        assert!(files.load("new.example.org").unwrap().unwrap().is_err());
        // Only a chain, no key: nothing stored.
        std::fs::remove_file(&files.key).unwrap();
        assert!(files.load(NAME).unwrap().is_none());
        // An unreadable file is an error.
        std::fs::create_dir(&files.key).unwrap();
        assert!(files.load(NAME).is_err());
    }

    #[test]
    fn the_files_go_into_a_folder_that_does_not_exist_yet() {
        let dir = tempfile::tempdir().unwrap();
        let files = CertFiles::new(dir.path(), "tls/cert.pem", "tls/key.pem");
        let pem = stand_in(NAME).unwrap();
        files.save(&pem).unwrap();
        assert_eq!(files.load(NAME).unwrap().unwrap().unwrap(), pem);
    }

    #[cfg(unix)]
    #[test]
    fn the_stored_key_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let files = CertFiles::new(dir.path(), "cert.pem", "key.pem");
        files.save(&stand_in(NAME).unwrap()).unwrap();
        let mode = std::fs::metadata(&files.key).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
