//! Internet access (Cloudflare tunnel) health, shared by iem-server and iem-ui (reaperiem#202).
//!
//! The mixer is published on the internet through the `cloudflared` Windows
//! service on the iem PC. iem-server watches that service (see
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
    /// The watchdog restarted the cloudflared service and waits for it to reconnect.
    Restarting,
}

impl TunnelState {
    /// Whether internet access is currently broken (anything but `Ok`).
    pub fn is_broken(self) -> bool {
        !matches!(self, TunnelState::Ok)
    }

    /// Slovak label for the engineer header indicator.
    pub fn engineer_label(self) -> &'static str {
        match self {
            TunnelState::Ok => "Vonkajší prístup: OK",
            TunnelState::Down => "Vonkajší prístup: nefunguje",
            TunnelState::Restarting => "Vonkajší prístup: nefunguje — opravujem",
        }
    }

    /// CSS modifier class for the indicator (`ok` / `down` / `restarting`).
    pub fn css_class(self) -> &'static str {
        match self {
            TunnelState::Ok => "ok",
            TunnelState::Down => "down",
            TunnelState::Restarting => "restarting",
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
    /// Seconds since the current state began (for Down/Restarting: since the
    /// tunnel went down).
    pub since_secs: u64,
    /// Seconds since the watchdog last restarted cloudflared (`None` = never).
    #[serde(default)]
    pub last_restart_secs_ago: Option<u64>,
    /// Whether this outage's last restart succeeded (`None` = no restart in
    /// the current/last outage yet, or the restart is still running).
    #[serde(default)]
    pub last_restart_ok: Option<bool>,
}

impl TunnelStatusInfo {
    /// Engineer indicator text; while the tunnel is broken it says so when
    /// this outage's repair attempt failed.
    pub fn engineer_label(&self) -> &'static str {
        if self.state.is_broken() && self.last_restart_ok == Some(false) {
            "Vonkajší prístup: nefunguje — oprava zlyhala"
        } else {
            self.state.engineer_label()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_is_not_broken_down_and_restarting_are() {
        assert!(!TunnelState::Ok.is_broken());
        assert!(TunnelState::Down.is_broken());
        assert!(TunnelState::Restarting.is_broken());
    }

    #[test]
    fn engineer_labels_are_the_approved_slovak_texts() {
        assert_eq!(TunnelState::Ok.engineer_label(), "Vonkajší prístup: OK");
        assert_eq!(
            TunnelState::Down.engineer_label(),
            "Vonkajší prístup: nefunguje"
        );
        assert_eq!(
            TunnelState::Restarting.engineer_label(),
            "Vonkajší prístup: nefunguje — opravujem"
        );
    }

    #[test]
    fn css_classes_distinguish_every_state() {
        assert_eq!(TunnelState::Ok.css_class(), "ok");
        assert_eq!(TunnelState::Down.css_class(), "down");
        assert_eq!(TunnelState::Restarting.css_class(), "restarting");
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
        };
        let json = serde_json::to_value(&links).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"lan_url": "http://10.0.0.10", "public_host": "mixer.example.org"})
        );
        let empty: SiteLinks = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, SiteLinks::default());
    }

    fn info(state: TunnelState, last_restart_ok: Option<bool>) -> TunnelStatusInfo {
        TunnelStatusInfo {
            state,
            ready_connections: 0,
            since_secs: 0,
            last_restart_secs_ago: None,
            last_restart_ok,
        }
    }

    #[test]
    fn status_label_reports_a_failed_repair() {
        assert_eq!(
            info(TunnelState::Restarting, Some(false)).engineer_label(),
            "Vonkajší prístup: nefunguje — oprava zlyhala"
        );
        assert_eq!(
            info(TunnelState::Restarting, None).engineer_label(),
            "Vonkajší prístup: nefunguje — opravujem"
        );
        assert_eq!(
            info(TunnelState::Restarting, Some(true)).engineer_label(),
            "Vonkajší prístup: nefunguje — opravujem"
        );
        // After the Restarting grace the state is Down again; the failed
        // repair must stay visible until the next attempt or recovery.
        assert_eq!(
            info(TunnelState::Down, Some(false)).engineer_label(),
            "Vonkajší prístup: nefunguje — oprava zlyhala"
        );
        assert_eq!(
            info(TunnelState::Down, None).engineer_label(),
            "Vonkajší prístup: nefunguje"
        );
        assert_eq!(
            info(TunnelState::Down, Some(true)).engineer_label(),
            "Vonkajší prístup: nefunguje"
        );
        assert_eq!(
            info(TunnelState::Ok, Some(false)).engineer_label(),
            "Vonkajší prístup: OK"
        );
    }

    #[test]
    fn status_info_wire_format() {
        let info = TunnelStatusInfo {
            state: TunnelState::Restarting,
            ready_connections: 0,
            since_secs: 150,
            last_restart_secs_ago: Some(3),
            last_restart_ok: Some(true),
        };
        let json = serde_json::to_value(info).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "state": "Restarting",
                "ready_connections": 0,
                "since_secs": 150,
                "last_restart_secs_ago": 3,
                "last_restart_ok": true
            })
        );
        let back: TunnelStatusInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back, info);
    }

    #[test]
    fn status_info_last_restart_defaults_to_none() {
        let info: TunnelStatusInfo =
            serde_json::from_str(r#"{"state":"Ok","ready_connections":4,"since_secs":10}"#)
                .unwrap();
        assert_eq!(info.state, TunnelState::Ok);
        assert_eq!(info.ready_connections, 4);
        assert_eq!(info.last_restart_secs_ago, None);
        assert_eq!(info.last_restart_ok, None);
    }
}
