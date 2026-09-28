//! Web Push to the engineer's devices: the mixer app (the PWA) on the
//! engineer page subscribes them (`push_subscriptions.json`), and
//! `iem-migrate band` carries the predecessor's over with its VAPID keys.
//! They are the one audience, the one the predecessor's alerts reached
//! (owner decision, #9 2026-09-28: "I added the PWA, allowed notifications
//! and everything worked"): SOS (F20), the band-activity notice (§4.2) and
//! the technical alarms — the guard's through `iem-server notify --to
//! alarm`, the running server's refused scheduled backup through
//! [`push_alarm`].
//!
//! Notify mode sends once and exits; `iem-server notify --count alarm`
//! prints how many subscriptions an alarm would go to (the guard's precheck
//! needs at least one for `live` and trials). Both only read.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::AppState;
use crate::push_store::{PushStore, PushSubscription};

/// What a notice from the command line is. It goes to the engineer's
/// devices, the one audience.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// A technical alarm (the guard's).
    Alarm,
}

impl Audience {
    /// `alarm`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "alarm" => Some(Self::Alarm),
            _ => None,
        }
    }
}

/// The push payload of an alarm or notice (the service worker shows title
/// and body). Its tag comes from the text: the same notice again replaces
/// itself on the phone, a different one shows on its own (the band-activity
/// notice and the guard's alarms reach the same devices).
pub fn alarm_payload(title: &str, body: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(title.len().to_le_bytes());
    hasher.update(title.as_bytes());
    hasher.update(body.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    let tag = format!("iem-alarm-{hex}");
    serde_json::json!({ "type": "ALARM", "title": title, "body": body, "tag": tag })
        .to_string()
        .into_bytes()
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

/// To the engineer's devices from the running server: SOS (F20, F21), the
/// band-activity notice and [`push_alarm`]. Expired subscriptions are
/// dropped from the store. Returns how many devices took it (0 without a
/// VAPID key).
pub async fn push_engineers(state: &AppState, payload: &[u8]) -> usize {
    let (key, subject) = {
        let c = state.config.read().await;
        (c.vapid_private_key.clone(), c.vapid_subject.clone())
    };
    if key.is_empty() {
        return 0;
    }
    crate::push::send_push_to_engineers(
        &state.http_client,
        &key,
        &subject,
        &state.push_store,
        payload,
    )
    .await
}

/// A technical alarm from the running server (a scheduled backup it
/// refused, `backup_daemon`) to the engineer's devices: how many took it.
pub async fn push_alarm(state: &AppState, title: &str, body: &str) -> usize {
    push_engineers(state, &alarm_payload(title, body)).await
}

/// `iem-server notify --to <audience> <title> <body>`: one notice to the
/// engineer's devices of the site at `config_path`, without a running
/// server. Returns how many devices took it. It only reads: an expired
/// subscription is logged, never dropped (the running server does that).
pub async fn run_cli(
    config_path: &Path,
    to: Audience,
    title: &str,
    body: &str,
) -> anyhow::Result<usize> {
    let config = iem_core::Config::load(config_path)?;
    let dir = crate::provision::config_dir_of(config_path);
    let subs = match to {
        Audience::Alarm => PushStore::read(&dir)?,
    };
    if subs.is_empty() {
        return Ok(0);
    }
    // Read only: a notice never creates the runtime secrets (see
    // `secrets::load_vapid`).
    let vapid_private_key = crate::secrets::load_vapid(&dir.join(crate::secrets::SECRETS_DIR))?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    Ok(send_to(
        &client,
        &vapid_private_key,
        &config.vapid_subject,
        &subs,
        &alarm_payload(title, body),
    )
    .await)
}

/// `iem-server notify --count alarm`: how many subscriptions an alarm to
/// the site at `config_path` would go to. It only reads.
pub fn count_cli(config_path: &Path) -> anyhow::Result<usize> {
    iem_core::Config::load(config_path)?;
    let dir = crate::provision::config_dir_of(config_path);
    Ok(PushStore::read(&dir)?.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push::tests::{Seen, fake_push_service, subscription, vapid_private_key};

    #[test]
    fn the_payload_carries_type_title_body_and_tag() {
        let v: serde_json::Value = serde_json::from_slice(&alarm_payload("T", "B")).unwrap();
        let tag = v["tag"].as_str().unwrap().to_owned();
        assert_eq!(
            v,
            serde_json::json!({"type": "ALARM", "title": "T", "body": "B", "tag": tag})
        );
    }

    /// The engineer's phone shows every different notice on its own (the
    /// band-activity notice and the guard's alarms reach the same devices,
    /// #9 2026-09-28); the same notice again replaces itself.
    #[test]
    fn each_different_notice_has_its_own_tag() {
        let tag = |t: &str, b: &str| {
            let v: serde_json::Value = serde_json::from_slice(&alarm_payload(t, b)).unwrap();
            v["tag"].as_str().unwrap().to_owned()
        };
        let a = tag("Guard", "the engine stopped");
        assert!(a.starts_with("iem-alarm-"), "{a}");
        let hex = &a["iem-alarm-".len()..];
        assert_eq!(hex.len(), 16, "{a}");
        assert!(hex.bytes().all(|c| c.is_ascii_hexdigit()), "{a}");
        assert_eq!(a, tag("Guard", "the engine stopped"));
        assert_ne!(a, tag("Guard", "the engine started"));
        assert_ne!(a, tag("Guards", "the engine stopped"));
        // Title and body are kept apart: moving text between them changes it.
        assert_ne!(tag("ab", "c"), tag("a", "bc"));
    }

    /// The predecessor had one push audience, the engineer's devices, and no
    /// other kind of notice from the command line (#9 2026-09-28).
    #[test]
    fn alarm_is_the_only_notice_from_the_command_line() {
        assert_eq!(Audience::parse("alarm"), Some(Audience::Alarm));
        for other in ["band-activity", "engineer", "owner", "all", "Alarm", ""] {
            assert_eq!(Audience::parse(other), None, "{other}");
        }
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

    fn paths(seen: &Seen) -> Vec<String> {
        seen.lock()
            .unwrap()
            .iter()
            .map(|(path, _, _)| path.clone())
            .collect()
    }

    /// The running server's state for a site in `dir` whose engineer store
    /// holds `endpoints`, with a VAPID key.
    async fn server(dir: &Path, endpoints: &[String]) -> AppState {
        let config = iem_core::Config {
            vapid_private_key: vapid_private_key(),
            ..iem_core::Config::default()
        };
        let state = AppState::new(config, dir);
        {
            let mut store = state.push_store.write().await;
            for e in endpoints {
                store.add(subscription(e.clone())).unwrap();
            }
        }
        state
    }

    #[tokio::test]
    async fn an_sos_reaches_the_engineers_devices_and_drops_expired_ones() {
        let (base, seen) = fake_push_service().await;
        let dir = tempfile::tempdir().unwrap();
        let state = server(dir.path(), &[format!("{base}/201"), format!("{base}/410")]).await;
        assert_eq!(push_engineers(&state, b"sos").await, 1);
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
        assert_eq!(push_engineers(&bare, b"sos").await, 0);
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_server_alarm_reaches_the_engineers_devices() {
        let (base, seen) = fake_push_service().await;
        let dir = tempfile::tempdir().unwrap();
        let state = server(dir.path(), &[format!("{base}/201"), format!("{base}/410")]).await;
        assert_eq!(push_alarm(&state, "Záloha zlyhala", "B").await, 1);
        assert_eq!(paths(&seen), ["/201", "/410"]);
        assert_eq!(
            state.push_store.read().await.all().len(),
            1,
            "the expired one is gone"
        );
        let empty = tempfile::tempdir().unwrap();
        let none = server(empty.path(), &[]).await;
        assert_eq!(push_alarm(&none, "T", "B").await, 0, "no subscription");
        assert_eq!(paths(&seen).len(), 2);
    }

    /// A site in a temp directory whose server has run (its runtime secrets
    /// exist: a notice only reads them) and whose engineer store holds
    /// `endpoints` (the store's own write: marker and file).
    fn cli_site(endpoints: &[String]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("iemmixer.toml");
        std::fs::write(&site, "port = 8080\n").unwrap();
        crate::secrets::load_or_create(&dir.path().join(crate::secrets::SECRETS_DIR)).unwrap();
        let mut store = PushStore::load(dir.path());
        for e in endpoints {
            store.add(subscription(e.clone())).unwrap();
        }
        (dir, site)
    }

    #[tokio::test]
    async fn an_alarm_reaches_the_engineers_devices_and_prunes_nothing() {
        let (base, seen) = fake_push_service().await;
        let (dir, site) = cli_site(&[format!("{base}/201"), format!("{base}/410")]);
        assert_eq!(
            run_cli(&site, Audience::Alarm, "Strážca", "B")
                .await
                .unwrap(),
            1
        );
        assert_eq!(paths(&seen), ["/201", "/410"]);
        assert_eq!(
            PushStore::read(dir.path()).unwrap().len(),
            2,
            "notify mode only reads: the expired one stays"
        );
    }

    #[tokio::test]
    async fn an_alarm_without_a_subscription_reaches_nobody() {
        let (base, seen) = fake_push_service().await;
        let (dir, site) = cli_site(&[]);
        assert_eq!(
            run_cli(&site, Audience::Alarm, "Strážca", "B")
                .await
                .unwrap(),
            0
        );
        // A list the server's first start would empty (no marker) is none.
        std::fs::remove_file(dir.path().join("push_subs_v2_migrated")).unwrap();
        std::fs::write(
            dir.path().join("push_subscriptions.json"),
            serde_json::to_string(&vec![subscription(format!("{base}/201"))]).unwrap(),
        )
        .unwrap();
        assert_eq!(
            run_cli(&site, Audience::Alarm, "Strážca", "B")
                .await
                .unwrap(),
            0
        );
        assert!(paths(&seen).is_empty());
        assert!(
            run_cli(&dir.path().join("missing.toml"), Audience::Alarm, "T", "B")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn an_unreadable_subscription_list_is_an_error() {
        let (dir, site) = cli_site(&[]);
        std::fs::write(dir.path().join("push_subscriptions.json"), "[oops").unwrap();
        assert!(
            run_cli(&site, Audience::Alarm, "T", "B").await.is_err(),
            "an unreadable list is not \"none\""
        );
        assert!(count_cli(&site).is_err());
    }

    /// A notice never creates the runtime secrets: before the first band
    /// import the site has none, and a JWT or VAPID key made here would stop
    /// `iem-migrate band` from taking the predecessor's (P9; found on the
    /// PC on 2026-09-28, when the guard's first alarm ran this).
    #[tokio::test]
    async fn a_notice_never_creates_the_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("iemmixer.toml");
        std::fs::write(&site, "port = 8080\n").unwrap();
        let secrets = dir.path().join(crate::secrets::SECRETS_DIR);
        // No subscription: nothing to send, nothing read or written.
        assert_eq!(run_cli(&site, Audience::Alarm, "T", "B").await.unwrap(), 0);
        assert!(!secrets.exists(), "no subscription: no secret made");
        assert!(
            !dir.path().join("push_subs_v2_migrated").exists(),
            "no marker made"
        );
        // A subscription but no VAPID key yet: an error, still nothing made.
        std::fs::write(dir.path().join("push_subs_v2_migrated"), b"").unwrap();
        let sub = PushSubscription {
            endpoint: "https://push.example/1".into(),
            p256dh: "k".into(),
            auth: "a".into(),
        };
        std::fs::write(
            dir.path().join("push_subscriptions.json"),
            serde_json::to_string(&vec![sub]).unwrap(),
        )
        .unwrap();
        let err = run_cli(&site, Audience::Alarm, "T", "B").await.unwrap_err();
        assert!(err.to_string().contains("vapid"), "{err}");
        assert!(!secrets.exists(), "a missing key is not made by a notice");
    }

    #[test]
    fn the_count_is_the_number_of_the_engineers_subscriptions() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("iemmixer.toml");
        std::fs::write(&site, "port = 8080\n").unwrap();
        assert_eq!(count_cli(&site).unwrap(), 0);
        let sub = |n: u32| PushSubscription {
            endpoint: format!("https://push.example/{n}"),
            p256dh: "k".into(),
            auth: "a".into(),
        };
        let two = serde_json::to_string(&vec![sub(1), sub(2)]).unwrap();
        std::fs::write(dir.path().join("push_subscriptions.json"), &two).unwrap();
        assert_eq!(
            count_cli(&site).unwrap(),
            0,
            "no marker: the server's first start empties the list"
        );
        std::fs::write(dir.path().join("push_subs_v2_migrated"), b"").unwrap();
        assert_eq!(count_cli(&site).unwrap(), 2);
        assert!(count_cli(&dir.path().join("missing.toml")).is_err());
        assert!(
            !dir.path().join(crate::secrets::SECRETS_DIR).exists(),
            "counting writes nothing"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("push_subscriptions.json")).unwrap(),
            two
        );
    }
}
