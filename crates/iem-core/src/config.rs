//! The server's part of the site file (`site.toml`): who the members are and
//! which engine mix each hears, how the engine's inputs are shown, and the
//! web, push, backup and tunnel settings (S5 design note §3). The engine
//! reads the `[engine]` table itself; the server ignores it.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use subtle::ConstantTimeEq;

/// Constant-time string comparison to prevent timing attacks on PIN verification.
/// Returns true if both strings are equal, false otherwise.
#[inline]
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// The UI's channel categories (the Mics, Stems and Tech tabs, F4). Inputs
/// of an engine group are always `stems`.
pub const CATEGORIES: [&str; 3] = ["mics", "stems", "tech"];

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Server port
    #[serde(default = "default_port")]
    pub port: u16,

    /// The engine's control pipe: a socket path on Unix, a pipe name on
    /// Windows; the media pipe is `<engine_pipe>.media`.
    /// `IEMMIXER_ENGINE_PIPE` overrides it.
    #[serde(default = "default_engine_pipe")]
    pub engine_pipe: String,

    /// Band members and the engineer: login tiles, URLs and JWT subjects.
    #[serde(default)]
    pub members: Vec<SiteMember>,

    /// How the engine's inputs are shown (names, tabs, owners).
    #[serde(default)]
    pub inputs: Vec<SiteInputMeta>,

    /// Command line of the engineer's "Back to REAPER" switch (§4.3; S6
    /// provides `iemmode event`). Empty: no button.
    #[serde(default)]
    pub back_to_reaper: Vec<String>,

    /// The band-activity alarm in `dev` (§4.2).
    #[serde(default)]
    pub activity: ActivityConfig,

    /// PIN changes in the web UI (P9, design note §5.4). Until the cutover
    /// the predecessor is the only place a PIN changes and every entry into
    /// `dev` brings its PINs over, so the guard writes `false` into every
    /// `dev` and trial site: the change and reset route then answers 409.
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

    /// The engine's topology table (`[engine]`): read by `iem-engine` only.
    #[serde(default, skip_serializing)]
    pub engine: Option<serde::de::IgnoredAny>,

    /// The engine's ASIO card (`[card]`, S6): read by `iem-engine` and the
    /// guard only.
    #[serde(default, skip_serializing)]
    pub card: Option<serde::de::IgnoredAny>,
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

/// Band-activity alarm (§4.2): peaks of the watched inputs above
/// `threshold_dbfs` for at least `sustain_s` seconds within the last
/// `window_s` seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ActivityConfig {
    pub threshold_dbfs: f64,
    pub window_s: u64,
    pub sustain_s: u64,
    /// The engine input ids that count: the stage. Empty (the default) means
    /// every input of category `mics`. The program input carries signal
    /// while the band is silent (S1a), so it never belongs here.
    pub inputs: Vec<String>,
}

impl Default for ActivityConfig {
    fn default() -> Self {
        Self {
            threshold_dbfs: -50.0,
            window_s: 300,
            sustain_s: 120,
            inputs: Vec::new(),
        }
    }
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
            back_to_reaper: Vec::new(),
            activity: ActivityConfig::default(),
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
            vapid_subject: default_vapid_subject(),
            backup_schedule: default_backup_schedule(),
            backup_retention_days: default_backup_retention_days(),
            tunnel_ready_url: default_tunnel_ready_url(),
            engine: None,
            card: None,
        }
    }
}

impl Config {
    /// Load and validate the site configuration from a TOML file.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let content =
            std::fs::read_to_string(path.as_ref()).map_err(|e| ConfigError::Io(e.to_string()))?;
        let config: Self =
            toml::from_str(&content).map_err(|e| ConfigError::Parse(e.to_string()))?;
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
        let a = &self.activity;
        if !(a.threshold_dbfs.is_finite() && a.threshold_dbfs < 0.0) {
            out.push("activity.threshold_dbfs must be below 0 dBFS".to_string());
        }
        if a.window_s == 0 || a.sustain_s == 0 || a.sustain_s > a.window_s {
            out.push("activity needs 0 < sustain_s <= window_s".to_string());
        }
        for id in &a.inputs {
            if !iem_engine_proto::valid_id(id) {
                out.push(format!("activity input '{id}' is not an input id"));
            }
        }
        out
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
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(private_key_b64)
            .map_err(|e| ConfigError::Io(format!("invalid VAPID key base64: {}", e)))?;
        let sk = p256::SecretKey::from_slice(&raw)
            .map_err(|e| ConfigError::Io(format!("invalid VAPID P-256 key: {}", e)))?;
        let pk = sk.public_key();
        let point = pk.to_encoded_point(false);
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(point.as_bytes()))
    }

    /// LAN URL and public host for the UI (`GET /api/site`).
    pub fn site_links(&self) -> crate::tunnel::SiteLinks {
        crate::tunnel::SiteLinks {
            lan_url: self.lan_url.clone(),
            public_host: self.https_domain.clone(),
        }
    }

    /// URL the tray's "Copy URL" shares: the public host over HTTPS, else the
    /// LAN URL, else none.
    pub fn share_url(&self) -> Option<String> {
        self.https_domain
            .as_ref()
            .map(|domain| format!("https://{domain}"))
            .or_else(|| self.lan_url.clone())
    }

    /// URL the tray's "Open Mixer" opens on the PC: the local server on the
    /// site's port, never the LAN URL. The tray runs no server of its own
    /// (F27): `iem-server` serves it. Loopback always reaches it (the HTTPS
    /// redirect applies to the public host only), and its origin is a secure
    /// context, so Copy URL's `navigator.clipboard` works in the same window;
    /// a plain-http LAN address has no clipboard API.
    pub fn mixer_url(&self) -> String {
        format!("http://localhost:{}", self.port)
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
    fn engine_pipe_and_activity_have_defaults() {
        let config: Config = toml::from_str("port = 81\n").unwrap();
        assert_eq!(config.engine_pipe, "iemmixer-engine");
        assert_eq!(Config::default().engine_pipe, "iemmixer-engine");
        assert_eq!(
            config.activity,
            ActivityConfig {
                threshold_dbfs: -50.0,
                window_s: 300,
                sustain_s: 120,
                inputs: Vec::new(),
            }
        );
        assert!(config.back_to_reaper.is_empty());
        let custom: Config = toml::from_str("[activity]\nwindow_s = 30\nsustain_s = 5\n").unwrap();
        assert_eq!(custom.activity.threshold_dbfs, -50.0);
        assert_eq!(
            (custom.activity.window_s, custom.activity.sustain_s),
            (30, 5)
        );
        assert!(custom.activity.inputs.is_empty(), "empty: every mics input");
        let stage: Config = toml::from_str("[activity]\ninputs = [\"mic1\", \"keys\"]\n").unwrap();
        assert_eq!(stage.activity.inputs, ["mic1", "keys"]);
        assert_eq!(stage.activity.sustain_s, 120);
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
            "[activity]\nwindow = 3\n",
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
        assert!(site.card.is_some(), "the [card] table is accepted");
        assert!(Config::default().card.is_none());
        assert_eq!(
            site.member("engineer").map(|m| m.mix.as_str()),
            Some("engineer")
        );
        assert_eq!(
            site.member("member3").map(|m| m.name.as_str()),
            Some("Member3")
        );
        assert!(site.member("translator").is_none());
        assert!(!site.back_to_reaper.is_empty());
        assert!(!site.pin_changes, "the test site is a dev site: frozen");
        let example: Config = toml::from_str(include_str!("../../../config/iemmixer.example.toml"))
            .expect("config/iemmixer.example.toml");
        assert_eq!(example.members.len(), 2);
        assert!(example.pin_changes);
        assert_eq!(example.problems(), Vec::<String>::new());
    }

    /// The PC's one site file carries the guard's `[guard]` and the
    /// engine's `[card]` beside the server's tables (S6): the server ignores
    /// both, so a guard-started `iem-server` loads the same file.
    #[test]
    fn a_site_with_the_guard_and_card_tables_loads() {
        let text = format!(
            "{}\n[guard]\nreaper_url = \"http://127.0.0.1:8080\"\nstage_tracks = [1, 2, 3]\nhil_tx = [89, 90]\non_pref_fail = \"start_reaper_with_alarm\"\n",
            include_str!("../../../config/test-site.toml")
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("site.toml");
        std::fs::write(&path, &text).unwrap();
        let site = Config::load(&path).expect("a site with [guard] and [card] loads");
        assert!(site.card.is_some(), "the [card] table is accepted");
        assert!(site.engine.is_some(), "the [engine] table is accepted");
        assert_eq!(site.members.len(), 10);
        // Never written back: the tables belong to the engine and the guard.
        let written = serde_json::to_string(&site).unwrap();
        for table in ["\"guard\"", "\"card\"", "\"engine\""] {
            assert!(!written.contains(table), "{table} in {written}");
        }
        // Another unknown table is still refused.
        let other = format!("{text}\n[nonsense]\nx = 1\n");
        std::fs::write(&path, other).unwrap();
        assert!(matches!(Config::load(&path), Err(ConfigError::Parse(_))));
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
            activity: ActivityConfig {
                threshold_dbfs: 0.0,
                window_s: 10,
                sustain_s: 11,
                inputs: vec!["mic1".into(), "Stage Mic".into()],
            },
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
        assert!(has("threshold_dbfs"), "{p:?}");
        assert!(has("sustain_s <= window_s"), "{p:?}");
        assert!(
            has("activity input 'Stage Mic' is not an input id"),
            "{p:?}"
        );
        assert!(!has("activity input 'mic1'"), "{p:?}");
        assert_eq!(p.len(), 11, "{p:?}");
        assert!(matches!(config.validate(), Err(ConfigError::Invalid(_))));
        assert!(Config::default().validate().is_ok());
        let zero = Config {
            activity: ActivityConfig {
                window_s: 0,
                sustain_s: 0,
                ..ActivityConfig::default()
            },
            ..Config::default()
        };
        assert_eq!(zero.problems().len(), 1);
        let no_sustain = Config {
            activity: ActivityConfig {
                sustain_s: 0,
                ..ActivityConfig::default()
            },
            ..Config::default()
        };
        assert_eq!(no_sustain.problems().len(), 1, "sustain_s must be above 0");
        let nan = Config {
            activity: ActivityConfig {
                threshold_dbfs: f64::NAN,
                ..ActivityConfig::default()
            },
            ..Config::default()
        };
        assert_eq!(nan.problems().len(), 1);
        let edge = Config {
            activity: ActivityConfig {
                window_s: 5,
                sustain_s: 5,
                ..ActivityConfig::default()
            },
            ..Config::default()
        };
        assert!(edge.problems().is_empty());
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
    fn test_share_url_prefers_the_public_host_then_the_lan_url() {
        let both = Config {
            lan_url: Some("http://10.0.0.10".to_string()),
            https_domain: Some("mixer.example.org".to_string()),
            ..Config::default()
        };
        assert_eq!(
            both.share_url().as_deref(),
            Some("https://mixer.example.org")
        );
        let lan_only = Config {
            lan_url: Some("http://10.0.0.10".to_string()),
            ..Config::default()
        };
        assert_eq!(lan_only.share_url().as_deref(), Some("http://10.0.0.10"));
        assert_eq!(Config::default().share_url(), None);
    }

    #[test]
    fn test_mixer_url_is_the_local_server_on_the_site_port() {
        let both = Config {
            lan_url: Some("http://10.0.0.10".to_string()),
            https_domain: Some("mixer.example.org".to_string()),
            port: 8080,
            ..Config::default()
        };
        assert_eq!(both.mixer_url(), "http://localhost:8080");
        let https_lan = Config {
            lan_url: Some("https://10.0.0.10".to_string()),
            port: 8081,
            ..Config::default()
        };
        assert_eq!(https_lan.mixer_url(), "http://localhost:8081");
        assert_eq!(Config::default().mixer_url(), "http://localhost:80");
    }

    #[test]
    fn test_plaintext_pins_are_rejected_in_the_site_file() {
        for text in ["engineer_pin = \"2468\"\n", "[pins]\nmember1 = \"2468\"\n"] {
            assert!(toml::from_str::<Config>(text).is_err(), "{text}");
        }
    }

    #[test]
    fn constant_time_eq_compares_whole_strings() {
        assert!(constant_time_eq("1234", "1234"));
        assert!(!constant_time_eq("1234", "1235"));
        assert!(!constant_time_eq("123", "1234"));
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
