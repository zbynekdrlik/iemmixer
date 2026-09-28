//! Backup daemon (F19): captures the engine's state at each `backup_schedule`
//! time (local HH:MM) and prunes files older than `backup_retention_days`.

use crate::AppState;
use std::collections::HashSet;

/// Spawn the backup daemon as a detached background task.
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        run(state).await;
    });
}

/// The schedule entries due at `hhmm` on `today` that have not run yet.
pub fn due(schedule: &[String], today: &str, hhmm: &str, done: &HashSet<String>) -> Option<String> {
    schedule
        .iter()
        .any(|e| e == hhmm)
        .then(|| format!("{today} {hhmm}"))
        .filter(|key| !done.contains(key))
}

/// The engineer's devices hear of a refused scheduled capture once per
/// distinct error, until a capture succeeds again: a refused slot is tried
/// again at every tick of its minute and at the next slot, and each retry
/// refused for the same reason is no news.
#[derive(Debug, Default)]
pub struct RefusalAlarms {
    /// The refusals whose alarm reached a device since the last success.
    raised: HashSet<String>,
}

impl RefusalAlarms {
    /// Whether a refusal with `error` still needs its alarm.
    pub fn due(&self, error: &str) -> bool {
        !self.raised.contains(error)
    }

    /// The alarm for `error` reached a device.
    pub fn raised(&mut self, error: &str) {
        self.raised.insert(error.to_owned());
    }

    /// A capture succeeded: every refusal alarms again.
    pub fn succeeded(&mut self) {
        self.raised.clear();
    }
}

/// The alarm's title and body for a refusal at `slot` (the owner reads
/// Slovak; the reason stays as the server logged it).
pub fn refusal_alarm(slot: &str, error: &str) -> (&'static str, String) {
    (
        "Záloha zlyhala",
        format!("Plánovaná záloha iemmixera o {slot} neprebehla: {error}"),
    )
}

/// A scheduled capture's outcome at `slot` (HH:MM): `Ok` is the saved
/// file, `Err` the refusal, which is logged and, once per distinct error
/// until a capture succeeds, sent to the engineer's devices (the PWA's
/// notification subscriptions, #9 2026-09-28). An alarm that reached nobody
/// (no subscription yet, every push failed) is tried again at the next
/// refusal. Returns whether the slot is done.
pub async fn settle(
    state: &AppState,
    slot: &str,
    outcome: Result<String, String>,
    alarms: &mut RefusalAlarms,
) -> bool {
    match outcome {
        Ok(filename) => {
            tracing::info!(%filename, "Backup daemon: saved scheduled backup");
            alarms.succeeded();
            true
        }
        Err(error) => {
            tracing::error!(%error, time = %slot, "Backup daemon: capture failed");
            if alarms.due(&error) {
                let (title, body) = refusal_alarm(slot, &error);
                let devices = crate::notify::push_alarm(state, title, &body).await;
                if devices > 0 {
                    tracing::info!(devices, "Backup daemon: refusal alarm sent");
                    alarms.raised(&error);
                } else {
                    tracing::error!(
                        "Backup daemon: the refusal alarm reached no device; tried again at the next refusal"
                    );
                }
            }
            false
        }
    }
}

async fn run(state: AppState) {
    tracing::info!("Backup daemon started");
    let mut done: HashSet<String> = HashSet::new();
    let mut alarms = RefusalAlarms::default();
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
        let now = chrono::Local::now();
        let today = now.format("%Y-%m-%d").to_string();
        let hhmm = now.format("%H:%M").to_string();
        done.retain(|key| key.starts_with(&today));
        let (schedule, retention_days) = {
            let config = state.config.read().await;
            (config.backup_schedule.clone(), config.backup_retention_days)
        };
        let Some(key) = due(&schedule, &today, &hhmm, &done) else {
            continue;
        };
        let outcome = crate::backup_routes::capture_now(&state).map(|(filename, _)| filename);
        if settle(&state, &hhmm, outcome, &mut alarms).await {
            let pruned = state.backup_store.prune(retention_days);
            if pruned > 0 {
                tracing::info!(count = pruned, "Backup daemon: pruned old backups");
            }
            done.insert(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push::tests::{Seen, fake_push_service, subscription, vapid_private_key};

    /// A site in a temp directory with a VAPID key and, with `subscribed`,
    /// the engineer's device `/201` (answered 2xx by the fake push service).
    async fn site(subscribed: bool) -> (tempfile::TempDir, AppState, String, Seen) {
        let (base, seen) = fake_push_service().await;
        let dir = tempfile::tempdir().unwrap();
        let config = iem_core::Config {
            vapid_private_key: vapid_private_key(),
            ..iem_core::Config::default()
        };
        let state = AppState::new(config, dir.path());
        if subscribed {
            subscribe(&state, &base).await;
        }
        (dir, state, base, seen)
    }

    /// The engineer allows notifications in the mixer app.
    async fn subscribe(state: &AppState, base: &str) {
        state
            .push_store
            .write()
            .await
            .add(subscription(format!("{base}/201")))
            .unwrap();
    }

    fn paths(seen: &Seen) -> Vec<String> {
        seen.lock()
            .unwrap()
            .iter()
            .map(|(path, _, _)| path.clone())
            .collect()
    }

    const UNREADABLE: &str = "the pins and hides of member3 are unreadable: Is a directory";

    #[test]
    fn the_refusal_alarm_names_the_slot_and_the_reason() {
        let (title, body) = refusal_alarm("13:00", UNREADABLE);
        assert_eq!(title, "Záloha zlyhala");
        assert_eq!(
            body,
            format!("Plánovaná záloha iemmixera o 13:00 neprebehla: {UNREADABLE}")
        );
    }

    #[tokio::test]
    async fn a_refused_capture_alarms_once_per_error_until_a_capture_succeeds() {
        let (_dir, state, _base, seen) = site(true).await;
        let mut alarms = RefusalAlarms::default();
        // Refused at 13:00 and again at the retry 30 s later: one alarm, to
        // the engineer's device.
        assert!(!settle(&state, "13:00", Err(UNREADABLE.into()), &mut alarms).await);
        assert!(!settle(&state, "13:00", Err(UNREADABLE.into()), &mut alarms).await);
        assert_eq!(paths(&seen), ["/201"]);
        // Another reason is news.
        let unsynced = "the engine state is not synced";
        assert!(!settle(&state, "21:00", Err(unsynced.into()), &mut alarms).await);
        assert!(!settle(&state, "21:00", Err(UNREADABLE.into()), &mut alarms).await);
        assert_eq!(paths(&seen), ["/201", "/201"]);
        // A saved backup re-arms every alarm.
        assert!(settle(&state, "13:00", Ok("backup.json".into()), &mut alarms).await);
        assert_eq!(paths(&seen).len(), 2, "a success alarms nobody");
        assert!(!settle(&state, "21:00", Err(UNREADABLE.into()), &mut alarms).await);
        assert_eq!(paths(&seen), ["/201", "/201", "/201"]);
        assert!(!settle(&state, "21:00", Err(UNREADABLE.into()), &mut alarms).await);
        assert_eq!(paths(&seen).len(), 3);
    }

    #[tokio::test]
    async fn a_refusal_alarm_that_reached_nobody_is_tried_again() {
        let (_dir, state, base, seen) = site(false).await;
        let mut alarms = RefusalAlarms::default();
        assert!(!settle(&state, "13:00", Err(UNREADABLE.into()), &mut alarms).await);
        assert!(paths(&seen).is_empty(), "no subscription yet");
        // The engineer subscribes; the next refusal reaches the phone, once.
        subscribe(&state, &base).await;
        assert!(!settle(&state, "13:00", Err(UNREADABLE.into()), &mut alarms).await);
        assert!(!settle(&state, "21:00", Err(UNREADABLE.into()), &mut alarms).await);
        assert_eq!(paths(&seen), ["/201"]);
    }

    #[test]
    fn a_slot_runs_once_per_day() {
        let schedule = vec!["13:00".to_string(), "21:00".to_string()];
        let mut done = HashSet::new();
        assert_eq!(due(&schedule, "2026-09-27", "12:59", &done), None);
        let key = due(&schedule, "2026-09-27", "13:00", &done).unwrap();
        assert_eq!(key, "2026-09-27 13:00");
        done.insert(key);
        assert_eq!(due(&schedule, "2026-09-27", "13:00", &done), None);
        assert!(due(&schedule, "2026-09-28", "13:00", &done).is_some());
        assert!(due(&[], "2026-09-27", "13:00", &done).is_none());
    }
}
