//! Alarms by Web Push (program spec §4.2): to the engineer's subscriptions
//! and the owner's alarm subscriptions (`alarm_subscriptions.json` next to
//! the site file, registered at the S6 bootstrap) — from the running server
//! (band activity, SOS) or, when no server runs, from `iem-server notify
//! <title> <body>` ("notify mode"), which sends once and exits.

use std::path::Path;

use crate::AppState;
use crate::push_store::PushSubscription;

/// The owner's alarm subscriptions (same format as the engineer store).
pub const ALARM_SUBSCRIPTIONS_FILE: &str = "alarm_subscriptions.json";

/// The push payload of an alarm (the service worker shows title and body).
pub fn alarm_payload(title: &str, body: &str) -> Vec<u8> {
    serde_json::json!({ "type": "ALARM", "title": title, "body": body })
        .to_string()
        .into_bytes()
}

/// The owner's alarm subscriptions; none when the file is absent or unreadable.
pub fn alarm_subscriptions(config_dir: &Path) -> Vec<PushSubscription> {
    let path = config_dir.join(ALARM_SUBSCRIPTIONS_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::error!(path = %path.display(), error = %e, "alarm subscriptions unreadable");
            Vec::new()
        }),
        Err(_) => Vec::new(),
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
            Ok(false) => tracing::info!("alarm: a subscription has expired"),
            Err(e) => tracing::warn!(error = %e, "alarm push failed"),
        }
    }
    sent
}

/// SOS to the engineer's devices (F20, F21).
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

/// An alarm to the engineer's and the owner's subscriptions.
pub async fn push_alarm(state: &AppState, payload: &[u8]) -> usize {
    let (key, subject) = {
        let c = state.config.read().await;
        (c.vapid_private_key.clone(), c.vapid_subject.clone())
    };
    let mut subs = state.push_store.read().await.all().to_vec();
    subs.extend(alarm_subscriptions(&state.config_dir));
    send_to(&state.http_client, &key, &subject, &subs, payload).await
}

/// `iem-server notify <title> <body>`: one alarm from the site next to
/// `config_path`, without a running server. Returns how many subscriptions
/// took it.
pub async fn run_cli(config_path: &Path, title: &str, body: &str) -> anyhow::Result<usize> {
    let config = iem_core::Config::load(config_path)?;
    let dir = crate::provision::config_dir_of(config_path);
    let secrets = crate::secrets::load_or_create(&dir.join(crate::secrets::SECRETS_DIR))?;
    let mut subs = crate::push_store::PushStore::load(&dir).all().to_vec();
    subs.extend(alarm_subscriptions(&dir));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push::tests::{fake_push_service, subscription, vapid_private_key};

    #[test]
    fn the_payload_carries_type_title_and_body() {
        let v: serde_json::Value = serde_json::from_slice(&alarm_payload("T", "B")).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"type": "ALARM", "title": "T", "body": "B"})
        );
    }

    #[test]
    fn alarm_subscriptions_are_optional() {
        let dir = tempfile::tempdir().unwrap();
        assert!(alarm_subscriptions(dir.path()).is_empty());
        std::fs::write(dir.path().join(ALARM_SUBSCRIPTIONS_FILE), "not json").unwrap();
        assert!(alarm_subscriptions(dir.path()).is_empty());
        let sub = PushSubscription {
            endpoint: "https://push.example/x".into(),
            p256dh: "k".into(),
            auth: "a".into(),
        };
        std::fs::write(
            dir.path().join(ALARM_SUBSCRIPTIONS_FILE),
            serde_json::to_string(&vec![sub.clone()]).unwrap(),
        )
        .unwrap();
        assert_eq!(alarm_subscriptions(dir.path()), vec![sub]);
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
    async fn an_alarm_reaches_the_engineers_and_the_owners_subscriptions() {
        let (base, seen) = fake_push_service().await;
        let dir = tempfile::tempdir().unwrap();
        let config = iem_core::Config {
            vapid_private_key: vapid_private_key(),
            vapid_subject: "mailto:ops@example.org".into(),
            ..iem_core::Config::default()
        };
        let state = AppState::new(config, dir.path());
        state
            .push_store
            .write()
            .await
            .add(subscription(format!("{base}/201")))
            .unwrap();
        std::fs::write(
            dir.path().join(ALARM_SUBSCRIPTIONS_FILE),
            serde_json::to_string(&vec![subscription(format!("{base}/201"))]).unwrap(),
        )
        .unwrap();
        assert_eq!(push_alarm(&state, &alarm_payload("T", "B")).await, 2);
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn notify_mode_sends_to_the_stored_subscriptions() {
        let (base, seen) = fake_push_service().await;
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("iemmixer.toml");
        std::fs::write(&site, "port = 8080\n").unwrap();
        // No subscription yet (the first load also writes the migration marker).
        assert_eq!(run_cli(&site, "T", "B").await.unwrap(), 0);
        std::fs::write(
            dir.path().join(ALARM_SUBSCRIPTIONS_FILE),
            serde_json::to_string(&vec![subscription(format!("{base}/201"))]).unwrap(),
        )
        .unwrap();
        assert_eq!(run_cli(&site, "Kapela hrá", "B").await.unwrap(), 1);
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(
            run_cli(&dir.path().join("missing.toml"), "T", "B")
                .await
                .is_err()
        );
    }
}
