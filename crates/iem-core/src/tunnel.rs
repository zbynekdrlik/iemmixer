//! Internet access (Cloudflare tunnel) health, shared by iem-server and iem-ui (reaperiem#202).
//!
//! The mixer is published on the internet through `cloudflared`. iem-server
//! watches it (see
//! `iem_server::tunnel_watch`) and pushes a [`TunnelStatusInfo`] to every
//! client: the engineer sees a header indicator, band members see a banner
//! telling them to use the local network address while the tunnel is broken.

use serde::{Deserialize, Serialize};

/// Where the mixer is reachable, from the site config (`lan_url`,
/// `https_domain`); served at `GET /api/site`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SiteLinks {
    #[serde(default)]
    pub lan_url: Option<String>,
    #[serde(default)]
    pub public_host: Option<String>,
    /// The ACME keeper's status of `public_host`'s certificate (#91);
    /// absent without `[acme]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<CertStatus>,
}

/// What the ACME keeper (#91) knows of the certificate it keeps.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CertStatus {
    /// The served certificate's end of validity (Unix seconds); `None`
    /// while no certificate of the name is stored.
    #[serde(default)]
    pub expires: Option<i64>,
    /// Why the last attempt failed; `None` after a success or before any.
    #[serde(default)]
    pub last_error: Option<String>,
    /// Failed attempts in a row.
    #[serde(default)]
    pub failures: u32,
}

const MEMBER_BANNER_PREFIX: &str = "Internetový prístup nefunguje";

/// Banner text for band members while internet access is broken.
pub fn member_banner_text(lan_url: Option<&str>) -> String {
    match lan_url {
        Some(url) => format!("{MEMBER_BANNER_PREFIX} — na tejto sieti otvorte {url}"),
        None => MEMBER_BANNER_PREFIX.to_string(),
    }
}

/// Hint under the "Reconnecting" banner for pages opened via the public host.
pub fn reconnect_lan_hint(lan_url: &str) -> String {
    format!("Ak nejde internet a ste na miestnej sieti, otvorte {lan_url}")
}

/// Whether a page loaded from `hostname` depends on the tunnel.
pub fn needs_lan_hint(hostname: &str, public_host: Option<&str>) -> bool {
    public_host.is_some_and(|host| hostname.eq_ignore_ascii_case(host))
}

/// Health of the cloudflared tunnel.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TunnelState {
    /// cloudflared reports at least one ready edge connection.
    Ok,
    /// Zero ready edge connections, or cloudflared is unreachable.
    Down,
}

impl TunnelState {
    /// Whether internet access is currently broken.
    pub fn is_broken(self) -> bool {
        self == TunnelState::Down
    }

    /// Slovak label for the engineer header indicator.
    pub fn engineer_label(self) -> &'static str {
        match self {
            TunnelState::Ok => "Vonkajší prístup: OK",
            TunnelState::Down => "Vonkajší prístup: nefunguje",
        }
    }

    /// CSS modifier class for the indicator (`ok` / `down`).
    pub fn css_class(self) -> &'static str {
        match self {
            TunnelState::Ok => "ok",
            TunnelState::Down => "down",
        }
    }
}

/// Tunnel status as sent over the WebSocket (`ServerMsg::TunnelStatus`) and
/// returned by `GET /api/tunnel`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct TunnelStatusInfo {
    pub state: TunnelState,
    /// Ready edge connections reported by cloudflared's `/ready` at the last poll.
    pub ready_connections: u32,
    /// Seconds since the current state began.
    pub since_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_is_not_broken_down_is() {
        assert!(!TunnelState::Ok.is_broken());
        assert!(TunnelState::Down.is_broken());
    }

    #[test]
    fn engineer_labels_are_the_approved_slovak_texts() {
        assert_eq!(TunnelState::Ok.engineer_label(), "Vonkajší prístup: OK");
        assert_eq!(
            TunnelState::Down.engineer_label(),
            "Vonkajší prístup: nefunguje"
        );
    }

    #[test]
    fn css_classes_distinguish_every_state() {
        assert_eq!(TunnelState::Ok.css_class(), "ok");
        assert_eq!(TunnelState::Down.css_class(), "down");
    }

    #[test]
    fn member_banner_points_to_the_configured_lan_url() {
        assert_eq!(
            member_banner_text(Some("http://10.0.0.10")),
            "Internetový prístup nefunguje — na tejto sieti otvorte http://10.0.0.10"
        );
        assert_eq!(member_banner_text(None), "Internetový prístup nefunguje");
        assert_eq!(
            reconnect_lan_hint("http://10.0.0.10"),
            "Ak nejde internet a ste na miestnej sieti, otvorte http://10.0.0.10"
        );
    }

    #[test]
    fn lan_hint_only_for_the_configured_public_host() {
        let host = Some("mixer.example.org");
        assert!(needs_lan_hint("mixer.example.org", host));
        assert!(needs_lan_hint("MIXER.example.org", host));
        assert!(!needs_lan_hint("10.0.0.10", host));
        assert!(!needs_lan_hint("localhost", host));
        assert!(!needs_lan_hint("mixer.example.org", None));
    }

    #[test]
    fn site_links_wire_format() {
        let links = SiteLinks {
            lan_url: Some("http://10.0.0.10".to_string()),
            public_host: Some("mixer.example.org".to_string()),
            certificate: None,
        };
        let json = serde_json::to_value(&links).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"lan_url": "http://10.0.0.10", "public_host": "mixer.example.org"})
        );
        let empty: SiteLinks = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, SiteLinks::default());
        // With the ACME keeper (#91): its status, read back the same.
        let kept = SiteLinks {
            certificate: Some(CertStatus {
                expires: Some(1_900_000_000),
                last_error: Some("HTTP 500".to_string()),
                failures: 2,
            }),
            ..links
        };
        let json = serde_json::to_value(&kept).unwrap();
        assert_eq!(
            json["certificate"],
            serde_json::json!({"expires": 1_900_000_000, "last_error": "HTTP 500", "failures": 2})
        );
        assert_eq!(serde_json::from_value::<SiteLinks>(json).unwrap(), kept);
    }

    #[test]
    fn status_info_wire_format() {
        let info = TunnelStatusInfo {
            state: TunnelState::Down,
            ready_connections: 0,
            since_secs: 150,
        };
        let json = serde_json::to_value(info).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "state": "Down",
                "ready_connections": 0,
                "since_secs": 150
            })
        );
        let back: TunnelStatusInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back, info);
    }
}
