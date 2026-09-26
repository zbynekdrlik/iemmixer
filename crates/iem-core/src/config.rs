//! Configuration types for band members, inputs, and PINs

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// REAPER server URL
    #[serde(default = "default_reaper_url")]
    pub reaper_url: String,

    /// Server port
    #[serde(default = "default_port")]
    pub port: u16,

    /// Band members with their output assignments (LEGACY - will be removed)
    /// Members are now discovered from REAPER tracks ending in " inear"
    #[serde(default)]
    pub members: Vec<BandMember>,

    /// Dante output channel mappings, keyed by REAPER track name prefix.
    /// Example: "MEMBER1" -> [71, 72] maps REAPER track "MEMBER1 inear" to Dante outputs 71 (L) and 72 (R).
    /// REAPER is the source of truth for member names; this config only provides Dante routing.
    #[serde(default)]
    pub dante_outputs: HashMap<String, [u8; 2]>,

    /// Input tracks
    #[serde(default)]
    pub inputs: Vec<InputTrack>,

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

    /// The engine's topology table (`[engine]`): read by `iem-engine` only
    /// (S3 design note §3.1); the server ignores it until S5.
    #[serde(default, skip_serializing)]
    pub engine: Option<serde::de::IgnoredAny>,
}

fn default_reaper_url() -> String {
    "http://127.0.0.1:8080".to_string()
}

fn default_port() -> u16 {
    80
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
            reaper_url: default_reaper_url(),
            port: default_port(),
            members: Vec::new(),
            dante_outputs: HashMap::new(),
            inputs: Vec::new(),
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
        }
    }
}

impl Config {
    /// Load the site configuration from a TOML file.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let content =
            std::fs::read_to_string(path.as_ref()).map_err(|e| ConfigError::Io(e.to_string()))?;
        toml::from_str(&content).map_err(|e| ConfigError::Parse(e.to_string()))
    }

    /// Find a band member by their ID (lowercase name)
    pub fn find_member(&self, id: &str) -> Option<&BandMember> {
        self.members.iter().find(|m| m.id() == id)
    }

    /// Get member's send index (0-based) for REAPER HTTP API
    ///
    /// REAPER HTTP API sends are 0-based: Send 0 = first member, Send 1 = second, etc.
    /// This matches the position in the config `members` array directly.
    pub fn member_index(&self, id: &str) -> Option<usize> {
        self.members.iter().position(|m| m.id() == id)
    }

    /// Find member and return both the 0-based send index and a reference.
    /// Eliminates the unwrap-after-find pattern in proxy.rs.
    pub fn find_member_with_index(&self, id: &str) -> Option<(usize, &BandMember)> {
        self.members.iter().enumerate().find(|(_, m)| m.id() == id)
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
}

/// Band member configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandMember {
    /// Display name (e.g., "Member3")
    pub name: String,

    /// Left Dante output channel (1-indexed)
    pub dante_output_l: u8,

    /// Right Dante output channel (1-indexed)
    pub dante_output_r: u8,
}

impl BandMember {
    /// Get lowercase ID for URL routing
    pub fn id(&self) -> String {
        self.name.to_lowercase()
    }

    /// Get REAPER track name for this member's output
    pub fn track_name(&self) -> String {
        format!("{} inear", self.name.to_uppercase())
    }
}

/// Input track configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputTrack {
    /// Track name (e.g., "MEMBER3 mic")
    pub name: String,

    /// Dante input channel (1-indexed)
    pub dante_input: u8,

    /// Default send level in dB
    #[serde(default)]
    pub default_level_db: f32,

    /// Category override: "mics", "stems", or "tech".
    /// When present, takes precedence over name-based derivation in proxy.rs.
    #[serde(default)]
    pub category: Option<String>,

    /// Stereo pair key. Tracks sharing this key are merged into one stereo
    /// REAPER track (e.g., "member7 kl" for "MEMBER7 kl L" + "MEMBER7 kl R").
    #[serde(default)]
    pub stereo_pair: Option<String>,
}

/// A band member discovered from REAPER at runtime.
/// Created by querying REAPER for tracks ending in " inear".
/// REAPER is the source of truth for member names.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredMember {
    /// Member name extracted from REAPER track (e.g., "OLDMEMBER1" from "OLDMEMBER1 inear")
    pub name: String,

    /// REAPER track index (1-based)
    pub track_index: usize,

    /// Left Dante output channel (1-indexed)
    pub dante_output_l: u8,

    /// Right Dante output channel (1-indexed)
    pub dante_output_r: u8,

    /// 0-based index in discovered members list (matches send index)
    pub send_index: usize,

    /// Send index on this member's inear track that routes to ENGINEER.
    /// Discovered dynamically by querying REAPER send destinations.
    /// None if no send to engineer exists (e.g., engineer's own track).
    #[serde(default)]
    pub mix_send_index: Option<usize>,

    /// Send indices on OTHER members' inear tracks that route TO this member.
    /// Only populated for elevated members. Maps source member_id → send_index.
    /// Example: { "member3": 3 } means MEMBER3 inear SEND/3 targets this member's inear.
    #[serde(default)]
    pub mix_send_indices: HashMap<String, usize>,
}

impl DiscoveredMember {
    /// Get lowercase ID for URL routing and API
    pub fn id(&self) -> String {
        self.name.to_lowercase()
    }

    /// Get REAPER track name for this member's output
    pub fn track_name(&self) -> String {
        format!("{} inear", self.name)
    }

    /// Create a discovered member from a REAPER track name and config.
    /// Returns None if the track doesn't end in " inear" or has no Dante mapping.
    pub fn from_reaper_track(
        track_name: &str,
        track_index: usize,
        send_index: usize,
        config: &Config,
    ) -> Option<Self> {
        // Extract member name from track name (e.g., "OLDMEMBER1" from "OLDMEMBER1 inear")
        let name = track_name.strip_suffix(" inear")?;

        // Look up Dante outputs from config
        let dante_channels = config.dante_outputs.get(name)?;

        Some(Self {
            name: name.to_string(),
            track_index,
            dante_output_l: dante_channels[0],
            dante_output_r: dante_channels[1],
            send_index,
            mix_send_index: None, // Discovered later by querying REAPER send destinations
            mix_send_indices: HashMap::new(), // Populated for elevated members
        })
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_member_id() {
        let member = BandMember {
            name: "Member3".to_string(),
            dante_output_l: 75,
            dante_output_r: 76,
        };
        assert_eq!(member.id(), "member3");
        assert_eq!(member.track_name(), "MEMBER3 inear");
    }

    #[test]
    fn test_track_name_uses_uppercase() {
        // Track name should use uppercase display name
        // REAPER tracks must match config - no aliases needed
        let member = BandMember {
            name: "Member1".to_string(),
            dante_output_l: 71,
            dante_output_r: 72,
        };
        assert_eq!(member.id(), "member1");
        assert_eq!(member.track_name(), "MEMBER1 inear");
    }

    fn make_test_member(name: &str) -> BandMember {
        BandMember {
            name: name.to_string(),
            dante_output_l: 89,
            dante_output_r: 90,
        }
    }

    #[test]
    fn test_find_member_with_index() {
        let mut config = Config::default();
        config.members.push(make_test_member("Oldmember1"));
        config.members.push(make_test_member("Member2"));
        config.members.push(make_test_member("Member3"));

        let (idx, member) = config.find_member_with_index("member2").unwrap();
        assert_eq!(idx, 1);
        assert_eq!(member.name, "Member2");

        let (idx, member) = config.find_member_with_index("member3").unwrap();
        assert_eq!(idx, 2);
        assert_eq!(member.name, "Member3");

        assert!(config.find_member_with_index("unknown").is_none());
    }

    // === NEW: Tests for REAPER as source of truth architecture ===

    #[test]
    fn test_dante_outputs_lookup() {
        // Config should have dante_outputs map keyed by REAPER track name prefix
        let mut config = Config::default();
        config
            .dante_outputs
            .insert("OLDMEMBER1".to_string(), [89, 90]);
        config.dante_outputs.insert("MEMBER2".to_string(), [73, 74]);

        // Lookup should return Dante channels for a given REAPER track prefix
        assert_eq!(config.dante_outputs.get("OLDMEMBER1"), Some(&[89, 90]));
        assert_eq!(config.dante_outputs.get("MEMBER2"), Some(&[73, 74]));
        assert_eq!(config.dante_outputs.get("UNKNOWN"), None);
    }

    #[test]
    fn test_dante_outputs_toml_parsing() {
        let text = r#"
reaper_url = "http://127.0.0.1:8080"
port = 80
inputs = []

[dante_outputs]
MEMBER1 = [71, 72]
MEMBER2 = [73, 74]
MEMBER3 = [75, 76]
"#;
        let config: Config = toml::from_str(text).expect("TOML should parse");
        assert_eq!(config.dante_outputs.get("MEMBER1"), Some(&[71, 72]));
        assert_eq!(config.dante_outputs.get("MEMBER3"), Some(&[75, 76]));
    }

    #[test]
    fn test_discovered_member_from_reaper_track() {
        // DiscoveredMember should be created from REAPER track name
        let mut config = Config::default();
        config
            .dante_outputs
            .insert("OLDMEMBER1".to_string(), [89, 90]);

        let member = DiscoveredMember::from_reaper_track("OLDMEMBER1 inear", 23, 0, &config)
            .expect("should parse");
        assert_eq!(member.name, "OLDMEMBER1");
        assert_eq!(member.id(), "oldmember1");
        assert_eq!(member.track_index, 23);
        assert_eq!(member.send_index, 0);
        assert_eq!(member.dante_output_l, 89);
        assert_eq!(member.dante_output_r, 90);
        assert_eq!(member.track_name(), "OLDMEMBER1 inear");
    }

    #[test]
    fn test_discovered_member_no_dante_mapping() {
        // Should return None if no Dante mapping exists
        let config = Config::default();
        let result = DiscoveredMember::from_reaper_track("OLDMEMBER1 inear", 23, 0, &config);
        assert!(
            result.is_none(),
            "Should return None when no Dante mapping exists"
        );
    }

    #[test]
    fn test_discovered_member_not_inear_track() {
        // Should return None for tracks that don't end in " inear"
        let mut config = Config::default();
        config
            .dante_outputs
            .insert("OLDMEMBER1".to_string(), [89, 90]);

        let result = DiscoveredMember::from_reaper_track("OLDMEMBER1 mic", 1, 0, &config);
        assert!(
            result.is_none(),
            "Non-inear tracks should not be discovered"
        );
    }

    #[test]
    fn test_backup_schedule_defaults() {
        let config = Config::default();
        assert_eq!(config.backup_schedule, vec!["13:00", "21:00"]);
        assert_eq!(config.backup_retention_days, 60);
    }

    #[test]
    fn test_backup_schedule_custom() {
        let text = "reaper_url = \"http://test:8080\"\nbackup_schedule = [\"09:00\", \"13:00\", \"18:00\", \"22:00\"]\nbackup_retention_days = 30\n";
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.backup_schedule.len(), 4);
        assert_eq!(config.backup_retention_days, 30);
    }

    #[test]
    fn test_tunnel_ready_url_default_and_toml_default() {
        assert_eq!(
            Config::default().tunnel_ready_url,
            "http://127.0.0.1:20241/ready"
        );
        let config: Config = toml::from_str("reaper_url = \"http://test:8080\"\n").unwrap();
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
    fn test_unknown_keys_are_rejected() {
        assert!(toml::from_str::<Config>("reaper_urll = \"http://x\"\n").is_err());
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
    fn test_committed_site_files_parse() {
        let site: Config = toml::from_str(include_str!("../../../config/test-site.toml"))
            .expect("config/test-site.toml");
        assert_eq!(site.members.len(), 10);
        assert_eq!(site.inputs.len(), 24);
        assert_eq!(site.dante_outputs.len(), 10);
        assert_eq!(site.lan_url.as_deref(), Some("http://10.0.0.10"));
        assert_eq!(site.https_domain.as_deref(), Some("mixer.example.org"));
        assert!(site.engine.is_some(), "the [engine] table is accepted");
        assert!(Config::default().engine.is_none());
        let example: Config = toml::from_str(include_str!("../../../config/iemmixer.example.toml"))
            .expect("config/iemmixer.example.toml");
        assert_eq!(example.members.len(), 2);
    }

    #[test]
    fn test_load_reads_a_toml_file_and_reports_parse_errors() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.toml");
        std::fs::write(&good, "port = 8081\n").unwrap();
        assert_eq!(Config::load(&good).unwrap().port, 8081);
        let bad = dir.path().join("bad.toml");
        std::fs::write(&bad, "port = \"eighty\"\n").unwrap();
        assert!(matches!(Config::load(&bad), Err(ConfigError::Parse(_))));
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
    fn test_plaintext_pins_are_rejected_in_the_site_file() {
        for text in ["engineer_pin = \"2468\"\n", "[pins]\nmember1 = \"2468\"\n"] {
            assert!(toml::from_str::<Config>(text).is_err(), "{text}");
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
