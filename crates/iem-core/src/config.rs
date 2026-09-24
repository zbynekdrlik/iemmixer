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

    /// PIN codes for authentication (member_id -> PIN)
    #[serde(default)]
    pub pins: HashMap<String, String>,

    /// Engineer PIN (full access)
    #[serde(default)]
    pub engineer_pin: Option<String>,

    /// JWT secret for token signing
    #[serde(default = "default_jwt_secret")]
    pub jwt_secret: String,

    /// VAPID private key for Web Push (base64url-encoded P-256 scalar, 32 bytes)
    #[serde(default)]
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
}

fn default_reaper_url() -> String {
    "http://127.0.0.1:8080".to_string()
}

fn default_port() -> u16 {
    80
}

fn default_jwt_secret() -> String {
    // In production, this should be set via config file or env var
    "change-me-in-production".to_string()
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
            pins: HashMap::new(),
            engineer_pin: None,
            jwt_secret: default_jwt_secret(),
            vapid_private_key: String::new(),
            tls: false,
            https_port: default_https_port(),
            tls_cert: default_tls_cert(),
            tls_key: default_tls_key(),
            https_domain: None,
            local_public_ip: None,
            backup_schedule: default_backup_schedule(),
            backup_retention_days: default_backup_retention_days(),
            tunnel_ready_url: default_tunnel_ready_url(),
        }
    }
}

impl Config {
    /// Load configuration from a YAML file
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let content =
            std::fs::read_to_string(path.as_ref()).map_err(|e| ConfigError::Io(e.to_string()))?;
        serde_yaml::from_str(&content).map_err(|e| ConfigError::Parse(e.to_string()))
    }

    /// YAML key for the JWT signing token
    const JWT_CONFIG_KEY: &'static str = "jwt_secret";

    /// Validate that critical security settings are configured.
    /// If jwt_secret is still the default placeholder, generates a random one
    /// and persists it to the config file so tokens survive restarts.
    pub fn validate_security(&mut self, config_path: Option<&Path>) {
        if self.jwt_secret == "change-me-in-production" || self.jwt_secret.is_empty() {
            // Generate a random value
            use std::time::{SystemTime, UNIX_EPOCH};
            let seed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            self.jwt_secret = format!(
                "auto-{:x}-{:x}",
                seed,
                seed.wrapping_mul(0x517cc1b727220a95)
            );

            // Persist so tokens survive app restarts
            if let Some(path) = config_path
                && let Err(e) = self.persist_jwt_to_config(path)
            {
                eprintln!("WARNING: Failed to save generated JWT config: {}", e);
            }

            eprintln!(
                "INFO: Auto-generated JWT signing key and saved to config file. \
                 Tokens will now persist across restarts."
            );
        }

        // Auto-generate VAPID key pair for Web Push if not set
        #[cfg(feature = "vapid")]
        if self.vapid_private_key.is_empty() {
            let sk = p256::SecretKey::random(&mut rand_core::OsRng);
            use base64::Engine;
            self.vapid_private_key =
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sk.to_bytes());

            if let Some(path) = config_path {
                let key = "vapid_private_key";
                let new_line = format!("{}: \"{}\"", key, self.vapid_private_key);
                if let Ok(content) = std::fs::read_to_string(path) {
                    let updated = if content.contains(&format!("{}:", key)) {
                        content
                            .lines()
                            .map(|line| {
                                if line.trim_start().starts_with(&format!("{}:", key)) {
                                    new_line.as_str()
                                } else {
                                    line
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                            + "\n"
                    } else {
                        let mut result = content;
                        if !result.ends_with('\n') {
                            result.push('\n');
                        }
                        result.push_str(&new_line);
                        result.push('\n');
                        result
                    };
                    let _ = std::fs::write(path, updated);
                }
            }

            eprintln!("INFO: Auto-generated VAPID key pair for Web Push notifications.");
        }
    }

    /// Write the current jwt_secret back to the config file.
    fn persist_jwt_to_config(&self, path: &Path) -> Result<(), ConfigError> {
        let content = std::fs::read_to_string(path).map_err(|e| ConfigError::Io(e.to_string()))?;

        let key = Self::JWT_CONFIG_KEY;
        let new_line = format!("{}: \"{}\"", key, self.jwt_secret);
        let updated = if content.contains(&format!("{}:", key)) {
            let mut result = String::new();
            for line in content.lines() {
                if line.trim_start().starts_with(&format!("{}:", key)) {
                    result.push_str(&new_line);
                } else {
                    result.push_str(line);
                }
                result.push('\n');
            }
            result
        } else {
            let mut result = content;
            if !result.ends_with('\n') {
                result.push('\n');
            }
            result.push_str(&new_line);
            result.push('\n');
            result
        };

        std::fs::write(path, updated).map_err(|e| ConfigError::Io(e.to_string()))?;
        Ok(())
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
    fn test_dante_outputs_yaml_parsing() {
        // Config YAML should parse dante_outputs map correctly
        let yaml = r#"
reaper_url: "http://127.0.0.1:8080"
port: 80
dante_outputs:
  OLDMEMBER1: [89, 90]
  MEMBER2: [73, 74]
  MEMBER3: [75, 76]
inputs: []
"#;
        let config: Config = serde_yaml::from_str(yaml).expect("YAML should parse");
        assert_eq!(config.dante_outputs.get("OLDMEMBER1"), Some(&[89, 90]));
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
        let yaml = r#"
reaper_url: "http://test:8080"
backup_schedule:
  - "09:00"
  - "13:00"
  - "18:00"
  - "22:00"
backup_retention_days: 30
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.backup_schedule.len(), 4);
        assert_eq!(config.backup_retention_days, 30);
    }

    #[test]
    fn test_tunnel_ready_url_default_and_yaml_default() {
        // Default points at cloudflared's local metrics server (reaperiem#202).
        assert_eq!(
            Config::default().tunnel_ready_url,
            "http://127.0.0.1:20241/ready"
        );
        // A deployed config without the key gets the same default.
        let config: Config = serde_yaml::from_str("reaper_url: \"http://test:8080\"\n").unwrap();
        assert_eq!(config.tunnel_ready_url, "http://127.0.0.1:20241/ready");
    }

    #[test]
    fn test_tunnel_ready_url_custom() {
        let yaml = "tunnel_ready_url: \"http://127.0.0.1:9999/ready\"\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.tunnel_ready_url, "http://127.0.0.1:9999/ready");
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

    #[test]
    fn test_validate_security_generates_on_default() {
        let mut config = Config::default();
        config.validate_security(None);
        assert_ne!(config.jwt_secret, "change-me-in-production");
        assert!(config.jwt_secret.starts_with("auto-"));
    }

    #[test]
    fn test_validate_security_keeps_custom() {
        let mut config = Config::default();
        let val = "not-the-default-placeholder".to_string();
        config.jwt_secret = val.clone();
        config.validate_security(None);
        assert_eq!(config.jwt_secret, val);
    }

    #[test]
    fn test_validate_security_persists_to_file() {
        let dir = std::env::temp_dir().join(format!("iem-persist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        std::fs::write(&path, "port: 80\n").unwrap();

        let mut cfg = Config::load(&path).unwrap();
        cfg.validate_security(Some(&path));
        let generated = cfg.jwt_secret.clone();
        assert!(generated.starts_with("auto-"));

        // Reload from file — must be persisted
        let reloaded = Config::load(&path).unwrap();
        assert_eq!(reloaded.jwt_secret, generated);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_validate_security_no_overwrite_custom() {
        let dir = std::env::temp_dir().join(format!("iem-nowrite-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        std::fs::write(&path, "port: 80\n").unwrap();

        let mut cfg = Config::default();
        let val = "my-custom-jwt-value".to_string();
        cfg.jwt_secret = val.clone();
        cfg.validate_security(Some(&path));
        assert_eq!(cfg.jwt_secret, val);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
