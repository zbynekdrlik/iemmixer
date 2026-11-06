//! The server's part of the site file (`site.toml`): who the members are and
//! which engine mix each hears, how the engine's inputs are shown, and the
//! web, push, backup and tunnel settings (S5 design note §3). The engine
//! reads the `[engine]` table and `[guard] hil_tx` itself; the server only
//! accepts them.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// The UI's channel categories (the Mics, Stems and Tech tabs, F4). Inputs
/// of an engine group are always `stems`.
pub const CATEGORIES: [&str; 3] = ["mics", "stems", "tech"];

/// An https origin with no path: `https://host` or `https://host:port`.
/// Anything else could inject into the CSP header.
fn valid_frame_origin(origin: &str) -> bool {
    let Some(rest) = origin.strip_prefix("https://") else {
        return false;
    };
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (rest, None),
    };
    let host_ok = !host.is_empty()
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    let port_ok =
        port.is_none_or(|p| p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u16>().is_ok());
    host_ok && port_ok
}

/// Let's Encrypt's production directory: `[acme] directory`'s default.
pub const LETS_ENCRYPT: &str = "https://acme-v02.api.letsencrypt.org/directory";
/// `[acme] propagation_s`'s default: the wait between writing the DNS-01
/// record and asking the CA to check it (seconds).
pub const DEFAULT_PROPAGATION_S: u64 = 20;
/// The longest allowed propagation wait (seconds).
pub const MAX_PROPAGATION_S: u64 = 600;

/// `[acme]` (#91): keep a certificate for `https_domain` from an ACME CA by
/// DNS-01 through the Cloudflare DNS API (`iem_server::acme`). The
/// Cloudflare token and the ACME account key are never in this file: they
/// are sealed under the config dir (`iem-server cloudflare set-token`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcmeConfig {
    /// The CA's directory URL (default [`LETS_ENCRYPT`]); https, or http
    /// to a loopback address only (a test double).
    #[serde(default = "default_acme_directory")]
    pub directory: String,
    /// The account's contact address (`owner@example.org`), optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,
    /// Seconds between writing the challenge record and the CA's check.
    #[serde(default = "default_propagation_s")]
    pub propagation_s: u64,
}

impl Default for AcmeConfig {
    fn default() -> Self {
        Self {
            directory: default_acme_directory(),
            contact: None,
            propagation_s: default_propagation_s(),
        }
    }
}

fn default_acme_directory() -> String {
    LETS_ENCRYPT.to_string()
}

fn default_propagation_s() -> u64 {
    DEFAULT_PROPAGATION_S
}

/// Whether a URL may be fetched by the server's ACME client: `https://…`,
/// or `http://` to a loopback address (a test double), never plain http
/// across a network.
pub fn acme_url_allowed(url: &str) -> bool {
    if let Some(rest) = url.strip_prefix("https://") {
        return !rest.is_empty() && !rest.starts_with('/');
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let host = if let Some(v6) = authority.strip_prefix('[') {
        match v6.split_once(']') {
            Some((host, _)) => host,
            None => return false,
        }
    } else {
        authority.split(':').next().unwrap_or("")
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Whether `name` is a DNS name like `mixer.example.org`: two labels or
/// more of letters, digits and inner `-`.
pub fn is_dns_name(name: &str) -> bool {
    let labels: Vec<&str> = name.split('.').collect();
    name.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|label| {
            (1..=63).contains(&label.len())
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

/// Whether `address` is an e-mail address like `owner@example.org`.
pub fn is_email(address: &str) -> bool {
    match address.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && local
                    .chars()
                    .all(|c| c.is_ascii_graphic() && !matches!(c, '@' | '<' | '>' | ',' | ':'))
                && is_dns_name(domain)
        }
        None => false,
    }
}

/// A site file's parse error: its line and the parser's message, never the
/// excerpt of the file `toml`'s own text holds, and never a value the message
/// quotes (`invalid type: string "…"`). Logs and a deploy's output, which may
/// be pasted, stay free of the site's values (P6).
fn parse_error(content: &str, e: &toml::de::Error) -> String {
    let message = e.message();
    let mut parts = message.split('"');
    let mut text = parts.next().unwrap_or_default().to_owned();
    for (i, part) in parts.enumerate() {
        text.push_str(if i % 2 == 0 { "\"…\"" } else { part });
    }
    match e.span() {
        Some(span) => {
            let before = content.get(..span.start).unwrap_or_default();
            format!("line {}: {text}", before.matches('\n').count() + 1)
        }
        None => text,
    }
}

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Server port
    #[serde(default = "default_port")]
    pub port: u16,

    /// The engine's control pipe: a socket path; the media pipe is `<engine_pipe>.media`.
    /// `IEMMIXER_ENGINE_PIPE` overrides it.
    #[serde(default = "default_engine_pipe")]
    pub engine_pipe: String,

    /// Band members and the engineer: login tiles, URLs and JWT subjects.
    #[serde(default)]
    pub members: Vec<SiteMember>,

    /// How the engine's inputs are shown (names, tabs, owners).
    #[serde(default)]
    pub inputs: Vec<SiteInputMeta>,

    /// PIN changes in the web UI (P9, design note §5.4). `false` (the test
    /// site) freezes them: the change and reset route then answers 409.
    #[serde(default = "default_pin_changes")]
    pub pin_changes: bool,

    /// JWT signing key. Never read from the site file: the server loads it
    /// from `<config dir>/secrets/jwt_secret` (`iem_server::secrets`).
    #[serde(skip)]
    pub jwt_secret: String,

    /// VAPID private key (base64url P-256 scalar). Never read from the site
    /// file: loaded from `<config dir>/secrets/vapid_private`.
    #[serde(skip)]
    pub vapid_private_key: String,

    /// Enable HTTPS (for PWA installability on phones)
    #[serde(default)]
    pub tls: bool,

    /// HTTPS port (default 443)
    #[serde(default = "default_https_port")]
    pub https_port: u16,

    /// TLS certificate file path (relative to config dir)
    #[serde(default = "default_tls_cert")]
    pub tls_cert: String,

    /// TLS private key file path (relative to config dir)
    #[serde(default = "default_tls_key")]
    pub tls_key: String,

    /// Domain for HTTPS redirect (HTTP requests to this domain → HTTPS)
    #[serde(default)]
    pub https_domain: Option<String>,

    /// Public IP of the local network (for LAN/WAN detection via Cloudflare Tunnel).
    /// When a request comes through Cloudflare with CF-Connecting-IP matching this IP,
    /// the client is on the local church WiFi. Different IP = remote.
    #[serde(default)]
    pub local_public_ip: Option<String>,

    /// Local-network URL of the mixer, shown to band members while the
    /// tunnel is down (e.g. "http://10.0.0.10").
    #[serde(default)]
    pub lan_url: Option<String>,

    /// Origins allowed to show the UI in a frame (CSP `frame-ancestors`),
    /// e.g. "https://surface.example". Empty: no page may frame it.
    #[serde(default)]
    pub frame_ancestors: Vec<String>,

    /// `[acme]` (#91): the server keeps `https_domain`'s certificate
    /// itself. Absent: the certificate files are served as they are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acme: Option<AcmeConfig>,

    /// Web Push contact (VAPID `sub` claim).
    #[serde(default = "default_vapid_subject")]
    pub vapid_subject: String,

    /// Backup schedule times (HH:MM format, 24h), e.g. ["13:00", "21:00"]
    #[serde(default = "default_backup_schedule")]
    pub backup_schedule: Vec<String>,

    /// How many days to keep backups before pruning
    #[serde(default = "default_backup_retention_days")]
    pub backup_retention_days: u32,

    /// cloudflared readiness endpoint polled by the tunnel watchdog (reaperiem#202).
    /// Returns `{"status":200,"readyConnections":N}` (HTTP 503 when N = 0).
    #[serde(default = "default_tunnel_ready_url")]
    pub tunnel_ready_url: String,

    /// The engine's topology table (`[engine]`): `iem-engine` reads it; the
    /// server only checks a roster against it at the start
    /// (`roster::engine_facts`).
    #[serde(default, skip_serializing)]
    pub engine: Option<toml::Table>,

    /// The `[guard]` table: `iem-engine` reads its `hil_tx`.
    #[serde(default, skip_serializing)]
    pub guard: Option<serde::de::IgnoredAny>,
}

/// A member of the band (or the engineer): the id is the URL, the login
/// tile and the JWT subject — the predecessor's ids, so nobody logs in
/// again (P9); `mix` is the engine mix this person hears.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteMember {
    pub id: String,
    /// Display name, as the band knows it.
    pub name: String,
    pub mix: String,
}

/// How one engine input is shown on every mixer page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteInputMeta {
    /// The engine input id.
    pub id: String,
    /// The channel label, as the band knows it (e.g. "MEMBER3 mic").
    pub name: String,
    /// `mics` (default), `stems` or `tech`; grouped inputs are always stems.
    #[serde(default)]
    pub category: Option<String>,
    /// The member who may edit this input's EQ besides the engineer (X7);
    /// a member's first owned input is their own ("more me") channel.
    #[serde(default)]
    pub owner: Option<String>,
}

fn default_port() -> u16 {
    80
}

fn default_pin_changes() -> bool {
    true
}

fn default_engine_pipe() -> String {
    "iemmixer-engine".to_string()
}

fn default_vapid_subject() -> String {
    "mailto:admin@example.org".to_string()
}

fn default_https_port() -> u16 {
    443
}

fn default_tls_cert() -> String {
    "cert.pem".to_string()
}

fn default_tls_key() -> String {
    "key.pem".to_string()
}

fn default_backup_schedule() -> Vec<String> {
    vec!["13:00".to_string(), "21:00".to_string()]
}

fn default_backup_retention_days() -> u32 {
    60
}

fn default_tunnel_ready_url() -> String {
    "http://127.0.0.1:20241/ready".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: default_port(),
            engine_pipe: default_engine_pipe(),
            members: Vec::new(),
            inputs: Vec::new(),
            pin_changes: default_pin_changes(),
            jwt_secret: String::new(),
            vapid_private_key: String::new(),
            tls: false,
            https_port: default_https_port(),
            tls_cert: default_tls_cert(),
            tls_key: default_tls_key(),
            https_domain: None,
            local_public_ip: None,
            lan_url: None,
            frame_ancestors: Vec::new(),
            acme: None,
            vapid_subject: default_vapid_subject(),
            backup_schedule: default_backup_schedule(),
            backup_retention_days: default_backup_retention_days(),
            tunnel_ready_url: default_tunnel_ready_url(),
            engine: None,
            guard: None,
        }
    }
}

impl Config {
    /// Load and validate the site configuration from a TOML file.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let content =
            std::fs::read_to_string(path.as_ref()).map_err(|e| ConfigError::Io(e.to_string()))?;
        let config: Self =
            toml::from_str(&content).map_err(|e| ConfigError::Parse(parse_error(&content, &e)))?;
        config.validate()?;
        Ok(config)
    }

    /// Every problem of the member and input tables, listed together.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut ids = HashSet::new();
        for m in &self.members {
            if let Err(e) = validate_member_id(&m.id) {
                out.push(e);
            } else if m.id.chars().any(|c| c.is_ascii_uppercase()) {
                out.push(format!("member id '{}' must be lower case", m.id));
            }
            if !ids.insert(m.id.as_str()) {
                out.push(format!("member '{}' is listed twice", m.id));
            }
            if !iem_engine_proto::valid_id(&m.mix) {
                out.push(format!("member '{}': '{}' is not a mix id", m.id, m.mix));
            }
        }
        let mut inputs = HashSet::new();
        for i in &self.inputs {
            if !iem_engine_proto::valid_id(&i.id) {
                out.push(format!("input '{}' is not an input id", i.id));
            }
            if !inputs.insert(i.id.as_str()) {
                out.push(format!("input '{}' is listed twice", i.id));
            }
            if let Some(c) = &i.category
                && !CATEGORIES.contains(&c.as_str())
            {
                out.push(format!(
                    "input '{}': category '{c}' is not one of {}",
                    i.id,
                    CATEGORIES.join(", ")
                ));
            }
            if let Some(o) = &i.owner
                && !ids.contains(o.as_str())
            {
                out.push(format!("input '{}': owner '{o}' is not a member", i.id));
            }
        }
        for a in &self.frame_ancestors {
            if !valid_frame_origin(a) {
                out.push(format!(
                    "frame_ancestors: '{a}' is not an https origin (https://host or https://host:port)"
                ));
            }
        }
        if let Some(acme) = &self.acme {
            out.extend(self.acme_problems(acme));
        }
        out
    }

    /// The problems of the `[acme]` table: it needs `tls` and a DNS name in
    /// `https_domain` (the certificate's name), an allowed directory, a
    /// contact that is an address, a bounded propagation wait.
    fn acme_problems(&self, acme: &AcmeConfig) -> Vec<String> {
        let mut out = Vec::new();
        if !self.tls {
            out.push("[acme] needs tls = true: the certificate is for the HTTPS listener".into());
        }
        match &self.https_domain {
            None => out.push("[acme] needs https_domain: the certificate is for that name".into()),
            Some(name) if !is_dns_name(name) => out.push(format!(
                "[acme] https_domain '{name}' is not a DNS name like mixer.example.org"
            )),
            Some(_) => {}
        }
        if !acme_url_allowed(&acme.directory) {
            out.push(format!(
                "[acme] directory '{}' is not an https URL",
                acme.directory
            ));
        }
        if let Some(contact) = &acme.contact
            && !is_email(contact)
        {
            out.push(format!(
                "[acme] contact '{contact}' is not an address like owner@example.org"
            ));
        }
        if acme.propagation_s > MAX_PROPAGATION_S {
            out.push(format!(
                "[acme] propagation_s {} is above {MAX_PROPAGATION_S}",
                acme.propagation_s
            ));
        }
        out
    }

    /// The CSP `frame-ancestors` source list: the configured origins, or
    /// `'none'` when there are none.
    pub fn frame_ancestors_source(&self) -> String {
        if self.frame_ancestors.is_empty() {
            "'none'".to_string()
        } else {
            self.frame_ancestors.join(" ")
        }
    }

    /// `Ok` when [`Config::problems`] is empty.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let problems = self.problems();
        if problems.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid(problems.join("; ")))
        }
    }

    /// The member with this id.
    pub fn member(&self, id: &str) -> Option<&SiteMember> {
        self.members.iter().find(|m| m.id == id)
    }

    /// Derive the VAPID public key (base64url, uncompressed P-256 point) from the private key.
    #[cfg(feature = "vapid")]
    pub fn vapid_public_key_base64url(private_key_b64: &str) -> Result<String, ConfigError> {
        use base64::Engine;
        use p256::elliptic_curve::sec1::ToSec1Point;
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(private_key_b64)
            .map_err(|e| ConfigError::Io(format!("invalid VAPID key base64: {}", e)))?;
        let sk = p256::SecretKey::from_slice(&raw)
            .map_err(|e| ConfigError::Io(format!("invalid VAPID P-256 key: {}", e)))?;
        let pk = sk.public_key();
        let point = pk.to_sec1_point(false);
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(point.as_bytes()))
    }

    /// LAN URL and public host for the UI (`GET /api/site`).
    pub fn site_links(&self) -> crate::tunnel::SiteLinks {
        crate::tunnel::SiteLinks {
            lan_url: self.lan_url.clone(),
            public_host: self.https_domain.clone(),
            certificate: None,
        }
    }
}

/// Validate that a member_id is safe for use in filesystem paths.
/// Rejects path traversal attempts and special characters.
/// Returns Ok(()) if valid, Err with message if invalid.
pub fn validate_member_id(member_id: &str) -> Result<(), String> {
    if member_id.is_empty() {
        return Err("member_id cannot be empty".to_string());
    }
    // Only allow alphanumeric, underscore, and hyphen
    if !member_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "member_id '{}' contains invalid characters (only a-z, A-Z, 0-9, _, - allowed)",
            member_id
        ));
    }
    Ok(())
}

/// Configuration errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(String),
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("invalid site: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acme_site(acme: &str) -> Config {
        toml::from_str(&format!(
            "tls = true\nhttps_domain = \"mixer.example.org\"\n[acme]\n{acme}"
        ))
        .unwrap()
    }

    #[test]
    fn the_acme_table_takes_its_defaults() {
        let site = acme_site("");
        assert_eq!(site.acme, Some(AcmeConfig::default()));
        let acme = site.acme.clone().unwrap();
        assert_eq!(acme.directory, LETS_ENCRYPT);
        assert_eq!(
            LETS_ENCRYPT,
            "https://acme-v02.api.letsencrypt.org/directory"
        );
        assert_eq!(acme.propagation_s, 20);
        assert_eq!(acme.contact, None);
        assert!(site.validate().is_ok(), "{:?}", site.problems());
        assert_eq!(Config::default().acme, None);
        let explicit = acme_site(
            "directory = \"http://127.0.0.1:9/dir\"\ncontact = \"owner@example.org\"\npropagation_s = 600\n",
        );
        let acme = explicit.acme.clone().unwrap();
        assert_eq!(acme.directory, "http://127.0.0.1:9/dir");
        assert_eq!(acme.contact.as_deref(), Some("owner@example.org"));
        assert_eq!(acme.propagation_s, 600);
        assert!(explicit.validate().is_ok(), "{:?}", explicit.problems());
    }

    #[test]
    fn the_acme_table_refuses_unknown_keys_and_secrets() {
        for key in [
            "token = \"x\"",
            "account_key = \"x\"",
            "email = \"a@example.org\"",
        ] {
            let text = format!("tls = true\nhttps_domain = \"mixer.example.org\"\n[acme]\n{key}\n");
            assert!(toml::from_str::<Config>(&text).is_err(), "{key}");
        }
    }

    #[test]
    fn acme_problems_name_every_offender() {
        let mut site = acme_site(
            "directory = \"http://10.0.0.10/dir\"\ncontact = \"nobody\"\npropagation_s = 601\n",
        );
        site.tls = false;
        site.https_domain = None;
        assert_eq!(
            site.problems(),
            vec![
                "[acme] needs tls = true: the certificate is for the HTTPS listener".to_string(),
                "[acme] needs https_domain: the certificate is for that name".to_string(),
                "[acme] directory 'http://10.0.0.10/dir' is not an https URL".to_string(),
                "[acme] contact 'nobody' is not an address like owner@example.org".to_string(),
                "[acme] propagation_s 601 is above 600".to_string(),
            ]
        );
        site.https_domain = Some("mixer.example.org&x=1".into());
        assert!(
            site.problems().contains(
                &"[acme] https_domain 'mixer.example.org&x=1' is not a DNS name like mixer.example.org"
                    .to_string()
            ),
            "{:?}",
            site.problems()
        );
    }

    #[test]
    fn an_acme_url_is_https_or_http_on_loopback() {
        for url in [
            "https://acme-v02.api.letsencrypt.org/directory",
            "http://127.0.0.1:9/directory",
            "http://localhost:9/x",
            "http://[::1]:9/x",
            "http://127.1.2.3/",
        ] {
            assert!(acme_url_allowed(url), "{url}");
        }
        for url in [
            "http://10.0.0.10:9/directory",
            "http://example.org/",
            "http://127.0.0.1@example.org/",
            "http://[::1/",
            "https://",
            "https:///x",
            "ftp://127.0.0.1/",
            "/relative",
            "",
        ] {
            assert!(!acme_url_allowed(url), "{url}");
        }
    }

    #[test]
    fn dns_names_and_addresses() {
        for name in ["mixer.example.org", "a-b.example.org", "x.y"] {
            assert!(is_dns_name(name), "{name}");
        }
        for name in [
            "example",
            "",
            ".example.org",
            "example.org.",
            "-a.example.org",
            "a-.example.org",
            "a b.example.org",
            "a_b.example.org",
            &format!("{}.org", "a".repeat(64)),
            &format!("{}.org", ["a"; 127].join(".")),
        ] {
            assert!(!is_dns_name(name), "{name}");
        }
        assert!(is_dns_name(&format!("{}.org", "a".repeat(63))));
        assert!(is_email("owner@example.org"));
        for bad in [
            "owner",
            "@example.org",
            "owner@",
            "owner@example",
            "a b@example.org",
            "a@b@example.org",
            "a<b@example.org",
            "a:b@example.org",
        ] {
            assert!(!is_email(bad), "{bad}");
        }
    }

    #[test]
    fn frame_ancestors_default_to_none() {
        assert_eq!(Config::default().frame_ancestors_source(), "'none'");
    }

    #[test]
    fn frame_ancestors_join_the_configured_origins() {
        let c = Config {
            frame_ancestors: vec![
                "https://surface.example".into(),
                "https://other.example:8443".into(),
            ],
            ..Config::default()
        };
        assert!(c.validate().is_ok());
        assert_eq!(
            c.frame_ancestors_source(),
            "https://surface.example https://other.example:8443"
        );
    }

    #[test]
    fn a_frame_ancestor_that_is_not_an_https_origin_is_refused() {
        for bad in [
            "http://surface.example",
            "https://surface.example/",
            "https://surface.example/path",
            "https://",
            "https://.example",
            "https://surface.example:",
            "https://surface.example:+443",
            "https://surface.example:70000",
            "https://a.example; script-src *",
            "https://a.example https://b.example",
            "*",
            "'self'",
        ] {
            let c = Config {
                frame_ancestors: vec![bad.into()],
                ..Config::default()
            };
            let err = c.validate().expect_err(bad).to_string();
            assert!(err.contains("frame_ancestors"), "{bad}: {err}");
        }
    }

    fn site() -> Config {
        toml::from_str(include_str!("../../../config/test-site.toml")).expect("test site")
    }

    fn member(id: &str, mix: &str) -> SiteMember {
        SiteMember {
            id: id.into(),
            name: id.to_uppercase(),
            mix: mix.into(),
        }
    }

    fn input(id: &str, category: Option<&str>, owner: Option<&str>) -> SiteInputMeta {
        SiteInputMeta {
            id: id.into(),
            name: id.to_uppercase(),
            category: category.map(Into::into),
            owner: owner.map(Into::into),
        }
    }

    #[test]
    fn test_backup_schedule_defaults() {
        let config = Config::default();
        assert_eq!(config.backup_schedule, vec!["13:00", "21:00"]);
        assert_eq!(config.backup_retention_days, 60);
    }

    #[test]
    fn test_backup_schedule_custom() {
        let text = "backup_schedule = [\"09:00\", \"13:00\", \"18:00\", \"22:00\"]\nbackup_retention_days = 30\n";
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.backup_schedule.len(), 4);
        assert_eq!(config.backup_retention_days, 30);
    }

    #[test]
    fn engine_pipe_has_a_default() {
        let config: Config = toml::from_str("port = 81\n").unwrap();
        assert_eq!(config.engine_pipe, "iemmixer-engine");
        assert_eq!(Config::default().engine_pipe, "iemmixer-engine");
    }

    #[test]
    fn pin_changes_are_on_unless_the_site_freezes_them() {
        assert!(Config::default().pin_changes);
        assert!(toml::from_str::<Config>("port = 80\n").unwrap().pin_changes);
        assert!(
            !toml::from_str::<Config>("pin_changes = false\n")
                .unwrap()
                .pin_changes
        );
    }

    #[test]
    fn test_tunnel_ready_url_default_and_toml_default() {
        assert_eq!(
            Config::default().tunnel_ready_url,
            "http://127.0.0.1:20241/ready"
        );
        let config: Config = toml::from_str("port = 80\n").unwrap();
        assert_eq!(config.tunnel_ready_url, "http://127.0.0.1:20241/ready");
    }

    #[test]
    fn test_tunnel_ready_url_custom() {
        let config: Config =
            toml::from_str("tunnel_ready_url = \"http://127.0.0.1:9999/ready\"\n").unwrap();
        assert_eq!(config.tunnel_ready_url, "http://127.0.0.1:9999/ready");
    }

    #[test]
    fn test_secrets_are_never_read_from_the_site_file() {
        for key in ["jwt_secret", "vapid_private_key"] {
            let text = format!("{key} = \"value\"\n");
            assert!(
                toml::from_str::<Config>(&text).is_err(),
                "{key} must be rejected"
            );
        }
        assert!(
            Config::default().jwt_secret.is_empty(),
            "no compiled-in JWT key"
        );
    }

    #[test]
    fn test_unknown_and_reaper_era_keys_are_rejected() {
        for text in [
            "reaper_urll = \"http://x\"\n",
            "reaper_url = \"http://127.0.0.1:8080\"\n",
            "[dante_outputs]\nMEMBER1 = [71, 72]\n",
            "[[members]]\nname = \"Member1\"\ndante_output_l = 71\ndante_output_r = 72\n",
            "[[inputs]]\nname = \"KEYS\"\ndante_input = 109\n",
        ] {
            assert!(toml::from_str::<Config>(text).is_err(), "{text}");
        }
    }

    #[test]
    fn test_site_extras_default_and_parse() {
        let defaults = Config::default();
        assert_eq!(defaults.lan_url, None);
        assert_eq!(defaults.vapid_subject, "mailto:admin@example.org");
        let config: Config = toml::from_str(
            "lan_url = \"http://10.0.0.20\"\nvapid_subject = \"mailto:ops@example.org\"\n",
        )
        .unwrap();
        assert_eq!(config.lan_url.as_deref(), Some("http://10.0.0.20"));
        assert_eq!(config.vapid_subject, "mailto:ops@example.org");
    }

    #[test]
    fn test_committed_site_files_parse_and_validate() {
        let site = site();
        assert_eq!(site.members.len(), 10);
        assert_eq!(site.inputs.len(), 24);
        assert_eq!(site.problems(), Vec::<String>::new());
        assert_eq!(site.lan_url.as_deref(), Some("http://10.0.0.10"));
        assert_eq!(site.https_domain.as_deref(), Some("mixer.example.org"));
        assert!(site.engine.is_some(), "the [engine] table is accepted");
        assert!(Config::default().engine.is_none());
        assert_eq!(
            site.member("engineer").map(|m| m.mix.as_str()),
            Some("engineer")
        );
        assert_eq!(
            site.member("member3").map(|m| m.name.as_str()),
            Some("Member3")
        );
        assert!(site.member("translator").is_none());
        assert!(!site.pin_changes, "the test site is a dev site: frozen");
        let example: Config = toml::from_str(include_str!("../../../config/iemmixer.example.toml"))
            .expect("config/iemmixer.example.toml");
        assert_eq!(example.members.len(), 2);
        assert!(example.pin_changes);
        assert_eq!(example.problems(), Vec::<String>::new());
    }

    /// The site file carries `[engine]` and `[guard] hil_tx` for the engine
    /// beside the server's tables: the server accepts both tables and never
    /// writes them back.
    #[test]
    fn a_site_with_the_engine_and_guard_tables_loads() {
        let text = include_str!("../../../config/test-site.toml");
        assert!(text.contains("\n[guard]"), "the test site's [guard]");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("site.toml");
        std::fs::write(&path, text).unwrap();
        let site = Config::load(&path).expect("a site with [guard] and [engine] loads");
        // Whatever else the table holds is not the server's to read.
        std::fs::write(
            &path,
            text.replacen("\n[guard]", "\n[guard]\nother = [1, 2]", 1),
        )
        .unwrap();
        assert!(Config::load(&path).is_ok());
        assert!(site.engine.is_some(), "the [engine] table is accepted");
        assert!(site.guard.is_some(), "the [guard] table is accepted");
        assert_eq!(site.members.len(), 10);
        let written = serde_json::to_string(&site).unwrap();
        for table in ["\"guard\"", "\"engine\""] {
            assert!(!written.contains(table), "{table} in {written}");
        }
    }

    /// The retired site keys are errors now, like any unknown key:
    /// `iem-server check-site` refuses them before a deploy stops anything.
    #[test]
    fn the_retired_site_keys_and_any_other_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("site.toml");
        for (key, text) in [
            ("back_to_reaper", "back_to_reaper = [\"zyxqw\"]\n"),
            ("card", "[card]\ndriver = \"zyxqw\"\n"),
            ("activity", "[activity]\ninputs = [\"zyxqw\"]\n"),
            ("nonsense", "[nonsense]\nx = \"zyxqw\"\n"),
        ] {
            std::fs::write(&path, format!("port = 80\n{text}")).unwrap();
            match Config::load(&path) {
                Err(ConfigError::Parse(e)) => {
                    assert!(e.contains(key), "{key}: {e}");
                    assert!(e.starts_with("line 2: "), "{key}: {e}");
                    assert!(!e.contains("zyxqw"), "{key}: {e}");
                }
                other => panic!("{key}: {other:?}"),
            }
        }
    }

    /// A wrongly typed value stays out of the error too.
    #[test]
    fn a_parse_error_never_quotes_a_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("site.toml");
        std::fs::write(&path, "tls = false\nport = \"zyxqw\"\n").unwrap();
        match Config::load(&path) {
            Err(ConfigError::Parse(e)) => {
                assert!(e.starts_with("line 2: "), "{e}");
                assert!(e.contains("invalid type"), "{e}");
                assert!(!e.contains("zyxqw"), "{e}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn problems_name_every_offender() {
        let config = Config {
            members: vec![
                member("member1", "member1"),
                member("member1", "m1"),
                member("Member2", "member2"),
                member("../x", "member3"),
                member("member4", "Bad Mix"),
            ],
            inputs: vec![
                input("mic1", Some("mics"), Some("member1")),
                input("mic1", None, None),
                input("Mic2", None, None),
                input("keys", Some("keyboards"), None),
                input("hand1", Some("tech"), Some("nobody")),
            ],
            ..Config::default()
        };
        let p = config.problems();
        let has = |s: &str| p.iter().any(|x| x.contains(s));
        assert!(has("member 'member1' is listed twice"), "{p:?}");
        assert!(has("member id 'Member2' must be lower case"), "{p:?}");
        assert!(has("'../x' contains invalid characters"), "{p:?}");
        assert!(has("'Bad Mix' is not a mix id"), "{p:?}");
        assert!(has("input 'mic1' is listed twice"), "{p:?}");
        assert!(has("input 'Mic2' is not an input id"), "{p:?}");
        assert!(has("category 'keyboards'"), "{p:?}");
        assert!(has("owner 'nobody' is not a member"), "{p:?}");
        assert_eq!(p.len(), 8, "{p:?}");
        assert!(matches!(config.validate(), Err(ConfigError::Invalid(_))));
        assert!(Config::default().validate().is_ok());
    }

    #[test]
    fn test_load_reads_validates_and_reports_errors() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.toml");
        std::fs::write(&good, "port = 8081\n").unwrap();
        assert_eq!(Config::load(&good).unwrap().port, 8081);
        let bad = dir.path().join("bad.toml");
        std::fs::write(&bad, "port = \"eighty\"\n").unwrap();
        assert!(matches!(Config::load(&bad), Err(ConfigError::Parse(_))));
        let invalid = dir.path().join("invalid.toml");
        std::fs::write(
            &invalid,
            "[[inputs]]\nid = \"mic1\"\nname = \"M\"\nowner = \"ghost\"\n",
        )
        .unwrap();
        assert!(matches!(
            Config::load(&invalid),
            Err(ConfigError::Invalid(_))
        ));
        assert!(matches!(
            Config::load(dir.path().join("missing.toml")),
            Err(ConfigError::Io(_))
        ));
    }

    #[test]
    fn test_site_links_come_from_lan_url_and_https_domain() {
        let config = Config {
            lan_url: Some("http://10.0.0.10".to_string()),
            https_domain: Some("mixer.example.org".to_string()),
            ..Config::default()
        };
        let links = config.site_links();
        assert_eq!(links.lan_url.as_deref(), Some("http://10.0.0.10"));
        assert_eq!(links.public_host.as_deref(), Some("mixer.example.org"));
        assert_eq!(
            Config::default().site_links(),
            crate::tunnel::SiteLinks::default()
        );
    }

    #[test]
    fn test_plaintext_pins_are_rejected_in_the_site_file() {
        for text in ["engineer_pin = \"2468\"\n", "[pins]\nmember1 = \"2468\"\n"] {
            assert!(toml::from_str::<Config>(text).is_err(), "{text}");
        }
    }

    /// The browser's `applicationServerKey`: the uncompressed SEC1 point of
    /// the VAPID scalar, base64url. Known answer for the synthetic scalar
    /// 0x07…07, computed with OpenSSL (Python's `cryptography`).
    #[cfg(feature = "vapid")]
    #[test]
    fn the_vapid_public_point_is_the_uncompressed_sec1_point() {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let point = Config::vapid_public_key_base64url(&b64.encode([7u8; 32])).unwrap();
        assert_eq!(
            point,
            "BB4YUy_UdUwC8wQdnHXOszuD_9gax85P6ILMscmLxYlupGwxHE4v9A3ZajZT5uRURdMt_khuztdcepDGoYiBwKM"
        );
        assert_eq!(b64.decode(point.as_bytes()).unwrap().len(), 65);
        for bad in [
            b64.encode([0u8; 32]),
            b64.encode([7u8; 16]),
            "not base64!".into(),
        ] {
            assert!(Config::vapid_public_key_base64url(&bad).is_err(), "{bad}");
        }
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn test_validate_member_id_accepts_valid_names() {
        assert!(validate_member_id("oldmember1").is_ok());
        assert!(validate_member_id("engineer").is_ok());
        assert!(validate_member_id("MEMBER2").is_ok());
        assert!(validate_member_id("member3-2").is_ok());
        assert!(validate_member_id("band_member").is_ok());
    }

    #[test]
    fn test_validate_member_id_rejects_path_traversal() {
        assert!(validate_member_id("../etc").is_err());
        assert!(validate_member_id("..").is_err());
    }

    #[test]
    fn test_validate_member_id_rejects_empty() {
        assert!(validate_member_id("").is_err());
    }

    #[test]
    fn test_validate_member_id_rejects_special_chars() {
        assert!(validate_member_id("foo bar").is_err());
        assert!(validate_member_id("foo.json").is_err());
    }
}
