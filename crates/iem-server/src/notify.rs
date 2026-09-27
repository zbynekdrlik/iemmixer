//! Web Push to two audiences that never mix (P9; design note §5.4, a
//! deviation from spec §4.2, which names the engineer for alarms):
//!
//! - the **engineer's** devices (`push_subscriptions.json`, subscribed in
//!   the mixer UI): SOS (F20) and the band-activity notice (§4.2), from the
//!   running server or `iem-server notify --to band-activity`;
//! - the **alarm recipients** (`alarm_subscriptions.json` next to the site
//!   file: the owner's phone through the one-time link, S6 bootstrap): the
//!   guard's technical alarms, only through `iem-server notify --to alarm`.
//!
//! Notify mode sends once and exits; `iem-server notify --count alarm`
//! prints how many alarm recipients there are (the guard's precheck needs
//! at least one).

use std::io::{self, ErrorKind};
use std::path::Path;

use crate::AppState;
use crate::push_store::PushSubscription;

/// The alarm recipients (same format as the engineer store).
pub const ALARM_SUBSCRIPTIONS_FILE: &str = "alarm_subscriptions.json";

/// Whom a notice from the command line reaches; there is no other audience.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// The alarm recipients only: the guard's technical alarms.
    Alarm,
    /// The engineer's devices only: the band-activity notice.
    BandActivity,
}

impl Audience {
    /// `alarm` or `band-activity`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "alarm" => Some(Self::Alarm),
            "band-activity" => Some(Self::BandActivity),
            _ => None,
        }
    }
}

/// The push payload of an alarm or notice (the service worker shows title
/// and body).
pub fn alarm_payload(title: &str, body: &str) -> Vec<u8> {
    serde_json::json!({ "type": "ALARM", "title": title, "body": body })
        .to_string()
        .into_bytes()
}

/// The alarm recipients; none without the file. A file that cannot be read
/// or parsed is an error: never taken for "none", never overwritten.
pub fn load_alarm_recipients(config_dir: &Path) -> io::Result<Vec<PushSubscription>> {
    let path = config_dir.join(ALARM_SUBSCRIPTIONS_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            io::Error::new(ErrorKind::InvalidData, format!("{}: {e}", path.display()))
        }),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(io::Error::new(e.kind(), format!("{}: {e}", path.display()))),
    }
}

/// Sends `payload` to every subscription; returns how many took it.
pub async fn send_to(
    client: &reqwest::Client,
    vapid_key: &str,
    subject: &str,
    subs: &[PushSubscription],
    payload: &[u8],
) -> usize {
    if vapid_key.is_empty() {
        return 0;
    }
    let mut sent = 0;
    for sub in subs {
        match crate::push::send_push(client, vapid_key, subject, sub, payload).await {
            Ok(true) => sent += 1,
            Ok(false) => tracing::info!("push: a subscription has expired"),
            Err(e) => tracing::warn!(error = %e, "push failed"),
        }
    }
    sent
}

/// To the engineer's devices: SOS (F20, F21) and the band-activity notice.
/// Expired subscriptions are dropped from the engineer store.
pub async fn push_engineers(state: &AppState, payload: &[u8]) {
    let (key, subject) = {
        let c = state.config.read().await;
        (c.vapid_private_key.clone(), c.vapid_subject.clone())
    };
    if key.is_empty() {
        return;
    }
    crate::push::send_push_to_engineers(
        &state.http_client,
        &key,
        &subject,
        &state.push_store,
        payload,
    )
    .await;
}

/// `iem-server notify --to <audience> <title> <body>`: one notice to that
/// audience of the site at `config_path`, without a running server. Returns
/// how many devices took it.
pub async fn run_cli(
    config_path: &Path,
    to: Audience,
    title: &str,
    body: &str,
) -> anyhow::Result<usize> {
    let config = iem_core::Config::load(config_path)?;
    let dir = crate::provision::config_dir_of(config_path);
    let subs = match to {
        Audience::Alarm => load_alarm_recipients(&dir)?,
        Audience::BandActivity => crate::push_store::PushStore::load(&dir).all().to_vec(),
    };
    let secrets = crate::secrets::load_or_create(&dir.join(crate::secrets::SECRETS_DIR))?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    Ok(send_to(
        &client,
        &secrets.vapid_private_key,
        &config.vapid_subject,
        &subs,
        &alarm_payload(title, body),
    )
    .await)
}

/// `iem-server notify --count alarm`: how many alarm recipients the site at
/// `config_path` has. It only reads.
pub fn count_cli(config_path: &Path) -> anyhow::Result<usize> {
    iem_core::Config::load(config_path)?;
    let dir = crate::provision::config_dir_of(config_path);
    Ok(load_alarm_recipients(&dir)?.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push::tests::{Seen, fake_push_service, subscription, vapid_private_key};

    #[test]
    fn the_payload_carries_type_title_and_body() {
        let v: serde_json::Value = serde_json::from_slice(&alarm_payload("T", "B")).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"type": "ALARM", "title": "T", "body": "B"})
        );
    }

    #[test]
    fn there_are_two_audiences_and_no_other() {
        assert_eq!(Audience::parse("alarm"), Some(Audience::Alarm));
        assert_eq!(
            Audience::parse("band-activity"),
            Some(Audience::BandActivity)
        );
        for other in ["engineer", "owner", "all", "Alarm", ""] {
            assert_eq!(Audience::parse(other), None, "{other}");
        }
    }

    #[test]
    fn alarm_recipients_are_optional_but_never_guessed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ALARM_SUBSCRIPTIONS_FILE);
        assert!(load_alarm_recipients(dir.path()).unwrap().is_empty());
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(
            load_alarm_recipients(dir.path()).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        let sub = PushSubscription {
            endpoint: "https://push.example/x".into(),
            p256dh: "k".into(),
            auth: "a".into(),
        };
        std::fs::write(&path, serde_json::to_string(&vec![sub.clone()]).unwrap()).unwrap();
        assert_eq!(load_alarm_recipients(dir.path()).unwrap(), vec![sub]);
        // Something that is not a readable file is an error, not "none".
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(load_alarm_recipients(dir.path()).is_err());
    }

    #[tokio::test]
    async fn send_to_counts_the_deliveries() {
        let (base, seen) = fake_push_service().await;
        let client = reqwest::Client::new();
        let subs = vec![
            subscription(format!("{base}/201")),
            subscription(format!("{base}/410")),
            subscription(format!("{base}/500")),
        ];
        let key = vapid_private_key();
        assert_eq!(
            send_to(&client, &key, "mailto:a@example.org", &subs, b"x").await,
            1
        );
        assert_eq!(seen.lock().unwrap().len(), 3);
        assert_eq!(
            send_to(&client, "", "mailto:a@example.org", &subs, b"x").await,
            0
        );
    }

    #[tokio::test]
    async fn an_sos_reaches_the_engineers_devices_and_drops_expired_ones() {
        let (base, seen) = fake_push_service().await;
        let dir = tempfile::tempdir().unwrap();
        let config = iem_core::Config {
            vapid_private_key: vapid_private_key(),
            ..iem_core::Config::default()
        };
        let state = AppState::new(config, dir.path());
        {
            let mut store = state.push_store.write().await;
            store.add(subscription(format!("{base}/201"))).unwrap();
            store.add(subscription(format!("{base}/410"))).unwrap();
        }
        push_engineers(&state, b"sos").await;
        assert_eq!(seen.lock().unwrap().len(), 2);
        let left: Vec<String> = state
            .push_store
            .read()
            .await
            .all()
            .iter()
            .map(|s| s.endpoint.clone())
            .collect();
        assert_eq!(left, [format!("{base}/201")], "the expired one is gone");
        // Without a VAPID key nothing is sent.
        let bare = AppState::new(iem_core::Config::default(), dir.path());
        push_engineers(&bare, b"sos").await;
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    /// A site in a temp directory whose engineer store holds `/201` and
    /// whose alarm recipients are `/202` (both answered 2xx by the fake push
    /// service, so the path tells the audience).
    async fn two_audiences() -> (tempfile::TempDir, std::path::PathBuf, Seen) {
        let (base, seen) = fake_push_service().await;
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("iemmixer.toml");
        std::fs::write(&site, "port = 8080\n").unwrap();
        crate::push_store::PushStore::load(dir.path())
            .add(subscription(format!("{base}/201")))
            .unwrap();
        std::fs::write(
            dir.path().join(ALARM_SUBSCRIPTIONS_FILE),
            serde_json::to_string(&vec![subscription(format!("{base}/202"))]).unwrap(),
        )
        .unwrap();
        (dir, site, seen)
    }

    fn paths(seen: &Seen) -> Vec<String> {
        seen.lock()
            .unwrap()
            .iter()
            .map(|(path, _, _)| path.clone())
            .collect()
    }

    #[tokio::test]
    async fn an_alarm_reaches_only_the_alarm_recipients() {
        let (_dir, site, seen) = two_audiences().await;
        assert_eq!(
            run_cli(&site, Audience::Alarm, "Strážca", "B")
                .await
                .unwrap(),
            1
        );
        assert_eq!(paths(&seen), ["/202"]);
    }

    #[tokio::test]
    async fn a_band_activity_notice_never_reaches_an_alarm_recipient() {
        let (_dir, site, seen) = two_audiences().await;
        assert_eq!(
            run_cli(&site, Audience::BandActivity, "Kapela hrá", "B")
                .await
                .unwrap(),
            1
        );
        assert_eq!(paths(&seen), ["/201"]);
    }

    #[tokio::test]
    async fn an_alarm_with_only_engineer_subscriptions_reaches_nobody() {
        let (dir, site, seen) = two_audiences().await;
        std::fs::remove_file(dir.path().join(ALARM_SUBSCRIPTIONS_FILE)).unwrap();
        assert_eq!(
            run_cli(&site, Audience::Alarm, "Strážca", "B")
                .await
                .unwrap(),
            0
        );
        assert!(paths(&seen).is_empty(), "the engineer got nothing");
        assert!(
            run_cli(&dir.path().join("missing.toml"), Audience::Alarm, "T", "B")
                .await
                .is_err()
        );
    }

    #[test]
    fn the_count_is_the_number_of_alarm_recipients() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("iemmixer.toml");
        std::fs::write(&site, "port = 8080\n").unwrap();
        assert_eq!(count_cli(&site).unwrap(), 0);
        let sub = |n: u32| PushSubscription {
            endpoint: format!("https://push.example/{n}"),
            p256dh: "k".into(),
            auth: "a".into(),
        };
        std::fs::write(
            dir.path().join(ALARM_SUBSCRIPTIONS_FILE),
            serde_json::to_string(&vec![sub(1), sub(2)]).unwrap(),
        )
        .unwrap();
        assert_eq!(count_cli(&site).unwrap(), 2);
        std::fs::write(dir.path().join(ALARM_SUBSCRIPTIONS_FILE), "[oops").unwrap();
        assert!(count_cli(&site).is_err(), "unreadable is not zero");
        assert!(count_cli(&dir.path().join("missing.toml")).is_err());
        assert!(
            !dir.path().join(crate::secrets::SECRETS_DIR).exists(),
            "counting writes nothing"
        );
    }
}
