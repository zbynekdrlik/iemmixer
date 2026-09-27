//! The owner's one-time alarm link (S6 bootstrap; design note §5.4, §6).
//! `iem-server alarm-link [--ttl-h 24]` stores the SHA-256 of one random
//! 128-bit token in `alarm_link.json` next to the site file (never the token)
//! and prints `https://<public host>/alarms?t=<token>`. The `/alarms` page
//! subscribes the phone to Web Push and posts the subscription with the
//! token to `POST /api/alarms/subscribe`, which accepts the token once,
//! before it expires: the token file is removed first, then the subscription
//! joins the alarm recipients (`alarm_subscriptions.json`, atomic write).

use std::io::{self, ErrorKind};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{Json, extract::State, http::StatusCode};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use iem_core::ApiError;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::notify::ALARM_SUBSCRIPTIONS_FILE;
use crate::push_store::PushSubscription;

/// The pending link (the token's hash and its expiry), next to the site file.
pub const ALARM_LINK_FILE: &str = "alarm_link.json";
/// How long a new link stays valid unless `--ttl-h` says otherwise.
pub const DEFAULT_TTL_H: u64 = 24;
/// The longest `--ttl-h` (one week).
pub const MAX_TTL_H: u64 = 168;

/// The stored link: the base64url SHA-256 of the token and when it expires
/// (Unix seconds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredLink {
    pub sha256: String,
    pub expires_at: u64,
}

/// Seconds since the Unix epoch.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The base64url SHA-256 of a token.
pub fn token_hash(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}

/// Whether `token` opens `link` at `now`: the hashes are compared in
/// constant time, and both checks always run.
pub fn accepts(link: &StoredLink, token: &str, now: u64) -> bool {
    let matches = iem_core::config::constant_time_eq(&token_hash(token), &link.sha256);
    let fresh = now < link.expires_at;
    matches & fresh
}

/// `--ttl-h N` (hours, 1 to [`MAX_TTL_H`]); no arguments: [`DEFAULT_TTL_H`].
pub fn parse_ttl(args: &[&str]) -> Result<u64, String> {
    match args {
        [] => Ok(DEFAULT_TTL_H),
        ["--ttl-h", n] => match n.parse::<u64>() {
            Ok(h) if (1..=MAX_TTL_H).contains(&h) => Ok(h),
            _ => Err(format!("--ttl-h takes 1 to {MAX_TTL_H} hours, not '{n}'")),
        },
        _ => Err("usage: iem-server alarm-link [--ttl-h <hours>]".to_string()),
    }
}

/// Writes a new link valid for `ttl_h` hours from `now`, replacing any
/// earlier one, and returns its token. Only the hash is stored.
pub fn create(config_dir: &Path, ttl_h: u64, now: u64) -> io::Result<String> {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let link = StoredLink {
        sha256: token_hash(&token),
        expires_at: now + ttl_h * 3600,
    };
    let json = serde_json::to_string_pretty(&link).map_err(io::Error::other)?;
    crate::atomic_write(&config_dir.join(ALARM_LINK_FILE), &json)?;
    Ok(token)
}

/// The link the owner opens: `<base>/alarms?t=<token>` (the token is
/// base64url, so it needs no escaping).
pub fn link_url(base: &str, token: &str) -> String {
    format!("{}/alarms?t={token}", base.trim_end_matches('/'))
}

/// Why a token was not taken.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// No link, a wrong token, an expired or an already used link.
    Link,
    /// The link file could not be removed.
    Io(String),
}

/// Takes the link when `token` opens it at `now`. The file is removed before
/// anything is stored, so a token opens once, also for two posts at a time.
pub fn redeem(config_dir: &Path, token: &str, now: u64) -> Result<(), Refused> {
    let path = config_dir.join(ALARM_LINK_FILE);
    let link = match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<StoredLink>(&text) {
            Ok(link) => Some(link),
            Err(e) => {
                tracing::error!(path = %path.display(), error = %e, "alarm link unreadable");
                None
            }
        },
        Err(_) => None,
    };
    if !link.is_some_and(|l| accepts(&l, token, now)) {
        return Err(Refused::Link);
    }
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Err(Refused::Link),
        Err(e) => Err(Refused::Io(e.to_string())),
    }
}

/// Adds `sub` to the alarm recipients (deduplicated by endpoint) and returns
/// how many there are. An unreadable file is an error, never overwritten.
pub fn add_recipient(config_dir: &Path, sub: PushSubscription) -> io::Result<usize> {
    let path = config_dir.join(ALARM_SUBSCRIPTIONS_FILE);
    let mut subs: Vec<PushSubscription> = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            io::Error::new(ErrorKind::InvalidData, format!("{}: {e}", path.display()))
        })?,
        Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    match subs.iter_mut().find(|s| s.endpoint == sub.endpoint) {
        Some(known) => *known = sub,
        None => subs.push(sub),
    }
    let json = serde_json::to_string_pretty(&subs).map_err(io::Error::other)?;
    crate::atomic_write(&path, &json)?;
    Ok(subs.len())
}

/// `iem-server alarm-link`: a new link for the site at `config_path`, valid
/// for `ttl_h` hours; the URL to send the owner.
pub fn run_cli(config_path: &Path, ttl_h: u64) -> anyhow::Result<String> {
    let config = iem_core::Config::load(config_path)?;
    let Some(base) = config.share_url() else {
        anyhow::bail!("the site has neither https_domain nor lan_url: no address for the link");
    };
    let token = create(
        &crate::provision::config_dir_of(config_path),
        ttl_h,
        unix_now(),
    )?;
    Ok(link_url(&base, &token))
}

/// The browser's `PushSubscription.toJSON()` (other fields are ignored).
#[derive(Debug, Deserialize)]
pub struct BrowserSubscription {
    pub endpoint: String,
    pub keys: BrowserKeys,
}

#[derive(Debug, Deserialize)]
pub struct BrowserKeys {
    pub p256dh: String,
    pub auth: String,
}

impl BrowserSubscription {
    /// The stored form; `None` unless the endpoint is `https://` and both
    /// keys are present.
    pub fn valid(self) -> Option<PushSubscription> {
        let ok = self.endpoint.starts_with("https://")
            && !self.keys.p256dh.is_empty()
            && !self.keys.auth.is_empty();
        ok.then_some(PushSubscription {
            endpoint: self.endpoint,
            p256dh: self.keys.p256dh,
            auth: self.keys.auth,
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct SubscribeRequest {
    pub token: String,
    pub subscription: BrowserSubscription,
}

type Reject = (StatusCode, Json<ApiError>);

/// `POST /api/alarms/subscribe {token, subscription}` — public: the token is
/// the credential. 400 for an incomplete subscription (the token stays),
/// 403 for any refused token, 200 once the phone is an alarm recipient.
pub async fn subscribe(
    State(state): State<AppState>,
    Json(req): Json<SubscribeRequest>,
) -> Result<Json<serde_json::Value>, Reject> {
    let Some(sub) = req.subscription.valid() else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError::bad_request(
                "An https endpoint and both keys are needed",
            )),
        ));
    };
    match redeem(&state.config_dir, &req.token, unix_now()) {
        Ok(()) => {}
        Err(Refused::Link) => {
            tracing::warn!("alarm link refused: no link, a wrong token, expired or used");
            return Err((
                StatusCode::FORBIDDEN,
                Json(ApiError::new(
                    "LINK_REFUSED",
                    "The link is invalid, expired or already used",
                )),
            ));
        }
        Err(Refused::Io(e)) => {
            tracing::error!(error = %e, "alarm link could not be removed");
            return Err(server_error());
        }
    }
    match add_recipient(&state.config_dir, sub) {
        Ok(n) => {
            tracing::info!(
                recipients = n,
                "alarm recipient added through the one-time link"
            );
            Ok(Json(serde_json::json!({ "ok": true })))
        }
        Err(e) => {
            tracing::error!(error = %e, "alarm recipient not stored; the link is used, a new one is needed");
            Err(server_error())
        }
    }
}

fn server_error() -> Reject {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError::new(
            "NOT_STORED",
            "The subscription was not stored; ask for a new link",
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::api_tests::{app, call};
    use axum::http::Method;

    const NOW: u64 = 1_800_000_000;

    fn stored(dir: &Path) -> StoredLink {
        serde_json::from_str(&std::fs::read_to_string(dir.join(ALARM_LINK_FILE)).unwrap()).unwrap()
    }

    fn recipients(dir: &Path) -> Vec<PushSubscription> {
        crate::notify::alarm_subscriptions(dir)
    }

    fn body(token: &str, endpoint: &str) -> String {
        serde_json::json!({
            "token": token,
            "subscription": {
                "endpoint": endpoint,
                "expirationTime": null,
                "keys": { "p256dh": "BKey", "auth": "secret" }
            }
        })
        .to_string()
    }

    #[test]
    fn a_link_stores_only_the_hash_and_its_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let token = create(dir.path(), 24, NOW).unwrap();
        assert_eq!(token.len(), 22, "128 bits in base64url");
        let link = stored(dir.path());
        assert_eq!(link.expires_at, NOW + 24 * 3600);
        assert_eq!(link.sha256, token_hash(&token));
        assert_ne!(link.sha256, token);
        let text = std::fs::read_to_string(dir.path().join(ALARM_LINK_FILE)).unwrap();
        assert!(!text.contains(&token), "the token itself is never stored");
        let second = create(dir.path(), 1, NOW).unwrap();
        assert_ne!(second, token, "every link is new");
        assert_eq!(
            stored(dir.path()).expires_at,
            NOW + 3600,
            "it replaces the first"
        );
    }

    #[test]
    fn a_token_opens_its_link_until_it_expires() {
        let link = StoredLink {
            sha256: token_hash("right"),
            expires_at: NOW,
        };
        assert!(accepts(&link, "right", NOW - 1));
        assert!(!accepts(&link, "right", NOW), "expired at expires_at");
        assert!(!accepts(&link, "wrong", NOW - 1));
        assert!(!accepts(&link, "wrong", NOW));
        assert_eq!(
            token_hash("right"),
            "JwQvTm7KfQsqfuQCbfLs-lHTM55tEiqgmRGOzYVjutk",
            "base64url SHA-256"
        );
    }

    #[test]
    fn a_token_is_redeemed_once() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            redeem(dir.path(), "none", NOW),
            Err(Refused::Link),
            "no link"
        );
        let token = create(dir.path(), 24, NOW).unwrap();
        assert_eq!(redeem(dir.path(), "wrong", NOW), Err(Refused::Link));
        assert!(
            dir.path().join(ALARM_LINK_FILE).exists(),
            "a wrong token uses nothing"
        );
        assert_eq!(
            redeem(dir.path(), &token, NOW + 24 * 3600),
            Err(Refused::Link)
        );
        assert_eq!(redeem(dir.path(), &token, NOW + 24 * 3600 - 1), Ok(()));
        assert!(!dir.path().join(ALARM_LINK_FILE).exists());
        assert_eq!(redeem(dir.path(), &token, NOW), Err(Refused::Link), "used");
        std::fs::write(dir.path().join(ALARM_LINK_FILE), "not json").unwrap();
        assert_eq!(redeem(dir.path(), &token, NOW), Err(Refused::Link));
    }

    // Unix permissions: a link file that cannot be removed is not taken.
    #[cfg(unix)]
    #[test]
    fn a_link_that_cannot_be_removed_is_not_taken() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let token = create(dir.path(), 24, NOW).unwrap();
        let mode = |m: u32| std::fs::Permissions::from_mode(m);
        std::fs::set_permissions(dir.path(), mode(0o555)).unwrap();
        let refused = redeem(dir.path(), &token, NOW);
        std::fs::set_permissions(dir.path(), mode(0o755)).unwrap();
        assert!(matches!(refused, Err(Refused::Io(_))), "{refused:?}");
        assert_eq!(redeem(dir.path(), &token, NOW), Ok(()), "still there");
    }

    #[test]
    fn recipients_are_added_once_per_endpoint_and_a_bad_file_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let sub = |endpoint: &str, auth: &str| PushSubscription {
            endpoint: endpoint.into(),
            p256dh: "k".into(),
            auth: auth.into(),
        };
        assert_eq!(
            add_recipient(dir.path(), sub("https://p/1", "a")).unwrap(),
            1
        );
        assert_eq!(
            add_recipient(dir.path(), sub("https://p/2", "b")).unwrap(),
            2
        );
        assert_eq!(
            add_recipient(dir.path(), sub("https://p/1", "c")).unwrap(),
            2
        );
        assert_eq!(
            recipients(dir.path()),
            [sub("https://p/1", "c"), sub("https://p/2", "b")]
        );
        std::fs::write(dir.path().join(ALARM_SUBSCRIPTIONS_FILE), "[oops").unwrap();
        let err = add_recipient(dir.path(), sub("https://p/3", "d")).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(ALARM_SUBSCRIPTIONS_FILE)).unwrap(),
            "[oops"
        );
    }

    #[test]
    fn the_ttl_argument_is_one_hour_to_one_week() {
        assert_eq!(parse_ttl(&[]), Ok(24));
        assert_eq!(parse_ttl(&["--ttl-h", "1"]), Ok(1));
        assert_eq!(parse_ttl(&["--ttl-h", "168"]), Ok(168));
        assert!(parse_ttl(&["--ttl-h", "0"]).is_err());
        assert!(parse_ttl(&["--ttl-h", "169"]).is_err());
        assert!(parse_ttl(&["--ttl-h", "x"]).is_err());
        assert!(parse_ttl(&["--ttl-h"]).is_err());
        assert!(parse_ttl(&["24"]).is_err());
    }

    #[test]
    fn the_url_is_the_public_host_with_the_token() {
        assert_eq!(
            link_url("https://mixer.example.org", "abc"),
            "https://mixer.example.org/alarms?t=abc"
        );
        assert_eq!(
            link_url("http://10.0.0.10/", "abc"),
            "http://10.0.0.10/alarms?t=abc"
        );
    }

    #[test]
    fn the_cli_prints_a_link_for_the_sites_address() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("iemmixer.toml");
        std::fs::write(&site, "https_domain = \"mixer.example.org\"\n").unwrap();
        let url = run_cli(&site, 2).unwrap();
        let token = url
            .strip_prefix("https://mixer.example.org/alarms?t=")
            .unwrap();
        let link = stored(dir.path());
        assert_eq!(link.sha256, token_hash(token));
        let now = unix_now();
        assert!(link.expires_at > now + 3600 && link.expires_at <= now + 7200);
        std::fs::write(&site, "port = 8080\n").unwrap();
        assert!(run_cli(&site, 2).is_err(), "no address");
    }

    #[test]
    fn only_complete_https_subscriptions_are_kept() {
        let parse = |json: serde_json::Value| {
            serde_json::from_value::<BrowserSubscription>(json)
                .unwrap()
                .valid()
        };
        let keys = serde_json::json!({ "p256dh": "k", "auth": "a" });
        assert_eq!(
            parse(serde_json::json!({ "endpoint": "https://p/1", "keys": keys })),
            Some(PushSubscription {
                endpoint: "https://p/1".into(),
                p256dh: "k".into(),
                auth: "a".into()
            })
        );
        assert_eq!(
            parse(serde_json::json!({ "endpoint": "http://p/1", "keys": keys })),
            None
        );
        assert_eq!(
            parse(
                serde_json::json!({ "endpoint": "https://p/1", "keys": { "p256dh": "", "auth": "a" } })
            ),
            None
        );
        assert_eq!(
            parse(
                serde_json::json!({ "endpoint": "https://p/1", "keys": { "p256dh": "k", "auth": "" } })
            ),
            None
        );
    }

    #[tokio::test]
    async fn the_route_takes_a_token_once_and_refuses_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let (_s, app) = app(dir.path());
        let token = create(dir.path(), 1, unix_now()).unwrap();
        let post = |b: String| {
            let app = app.clone();
            async move { call(&app, Method::POST, "/api/alarms/subscribe", None, Some(&b)).await }
        };
        let (status, json) = post(body("wrong", "https://push.example.org/1")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(json["code"], "LINK_REFUSED");
        // An incomplete subscription leaves the token usable.
        let (status, _) = post(body(&token, "http://push.example.org/1")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, json) = post(body(&token, "https://push.example.org/1")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, serde_json::json!({ "ok": true }));
        assert_eq!(
            recipients(dir.path()),
            [PushSubscription {
                endpoint: "https://push.example.org/1".into(),
                p256dh: "BKey".into(),
                auth: "secret".into()
            }]
        );
        let (status, _) = post(body(&token, "https://push.example.org/2")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "used");
        assert_eq!(recipients(dir.path()).len(), 1);
    }

    // Unix permissions make the link file impossible to remove.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_route_answers_500_when_nothing_can_be_stored() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (_s, app) = app(dir.path());
        let post = |b: String| {
            let app = app.clone();
            async move { call(&app, Method::POST, "/api/alarms/subscribe", None, Some(&b)).await }
        };
        let token = create(dir.path(), 1, unix_now()).unwrap();
        let mode = |m: u32| std::fs::Permissions::from_mode(m);
        std::fs::set_permissions(dir.path(), mode(0o555)).unwrap();
        let (status, json) = post(body(&token, "https://push.example.org/1")).await;
        std::fs::set_permissions(dir.path(), mode(0o755)).unwrap();
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json["code"], "NOT_STORED");
        // An unreadable recipient file: the link is used up, nothing stored.
        std::fs::write(dir.path().join(ALARM_SUBSCRIPTIONS_FILE), "[oops").unwrap();
        let (status, _) = post(body(&token, "https://push.example.org/1")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!dir.path().join(ALARM_LINK_FILE).exists());
        let (status, _) = post(body(&token, "https://push.example.org/1")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn the_route_refuses_an_expired_link_and_large_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let (_s, app) = app(dir.path());
        // Valid for one hour, created two hours ago.
        let token = create(dir.path(), 1, unix_now() - 7200).unwrap();
        let (status, _) = call(
            &app,
            Method::POST,
            "/api/alarms/subscribe",
            None,
            Some(&body(&token, "https://push.example.org/1")),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(recipients(dir.path()).is_empty());
        let big = body(&"t".repeat(5_000), "https://push.example.org/1");
        let (status, _) = call(
            &app,
            Method::POST,
            "/api/alarms/subscribe",
            None,
            Some(&big),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }
}
