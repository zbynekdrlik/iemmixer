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

/// The refusal alarms already raised for scheduled captures.
#[derive(Debug, Default)]
pub struct RefusalAlarms {
    raised: HashSet<String>,
}

/// A scheduled capture's outcome at `slot` (HH:MM): `Ok` is the saved
/// file, `Err` the refusal, which is logged (the slot is tried again at the
/// next tick). Returns whether the slot is done.
pub async fn settle(
    state: &AppState,
    slot: &str,
    outcome: Result<String, String>,
    alarms: &mut RefusalAlarms,
) -> bool {
    match outcome {
        Ok(filename) => {
            tracing::info!(%filename, "Backup daemon: saved scheduled backup");
            true
        }
        Err(e) => {
            tracing::error!(error = %e, time = %slot, "Backup daemon: capture failed");
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
    use crate::notify::ALARM_SUBSCRIPTIONS_FILE;
    use crate::push::tests::{Seen, fake_push_service, subscription, vapid_private_key};

    /// A site in a temp directory whose engineer's device is `/201` (the
    /// band-activity and SOS audience) and, with `recipient`, whose alarm
    /// recipient is `/202`; both answered 2xx by the fake push service.
    async fn site(recipient: bool) -> (tempfile::TempDir, AppState, String, Seen) {
        let (base, seen) = fake_push_service().await;
        let dir = tempfile::tempdir().unwrap();
        let config = iem_core::Config {
            vapid_private_key: vapid_private_key(),
            ..iem_core::Config::default()
        };
        let state = AppState::new(config, dir.path());
        state
            .push_store
            .write()
            .await
            .add(subscription(format!("{base}/201")))
            .unwrap();
        if recipient {
            add_recipient(dir.path(), &base);
        }
        (dir, state, base, seen)
    }

    fn add_recipient(dir: &std::path::Path, base: &str) {
        std::fs::write(
            dir.join(ALARM_SUBSCRIPTIONS_FILE),
            serde_json::to_string(&vec![subscription(format!("{base}/202"))]).unwrap(),
        )
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

    #[tokio::test]
    async fn a_refused_capture_alarms_once_per_error_until_a_capture_succeeds() {
        let (_dir, state, _base, seen) = site(true).await;
        let mut alarms = RefusalAlarms::default();
        // Refused at 13:00 and again at the retry 30 s later: one alarm, to
        // the alarm recipient only (never the engineer's device).
        assert!(!settle(&state, "13:00", Err(UNREADABLE.into()), &mut alarms).await);
        assert!(!settle(&state, "13:00", Err(UNREADABLE.into()), &mut alarms).await);
        assert_eq!(paths(&seen), ["/202"]);
        // Another reason is news.
        let unsynced = "the engine state is not synced";
        assert!(!settle(&state, "21:00", Err(unsynced.into()), &mut alarms).await);
        assert!(!settle(&state, "21:00", Err(UNREADABLE.into()), &mut alarms).await);
        assert_eq!(paths(&seen), ["/202", "/202"]);
        // A saved backup re-arms every alarm.
        assert!(settle(&state, "13:00", Ok("backup.json".into()), &mut alarms).await);
        assert_eq!(paths(&seen).len(), 2, "a success alarms nobody");
        assert!(!settle(&state, "21:00", Err(UNREADABLE.into()), &mut alarms).await);
        assert_eq!(paths(&seen), ["/202", "/202", "/202"]);
        assert!(!settle(&state, "21:00", Err(UNREADABLE.into()), &mut alarms).await);
        assert_eq!(paths(&seen).len(), 3);
    }

    #[tokio::test]
    async fn a_refusal_alarm_that_reached_nobody_is_tried_again() {
        let (dir, state, base, seen) = site(false).await;
        let mut alarms = RefusalAlarms::default();
        assert!(!settle(&state, "13:00", Err(UNREADABLE.into()), &mut alarms).await);
        assert!(
            paths(&seen).is_empty(),
            "no recipient, and never the engineer"
        );
        // The owner subscribes; the next refusal reaches the phone, once.
        add_recipient(dir.path(), &base);
        assert!(!settle(&state, "13:00", Err(UNREADABLE.into()), &mut alarms).await);
        assert!(!settle(&state, "21:00", Err(UNREADABLE.into()), &mut alarms).await);
        assert_eq!(paths(&seen), ["/202"]);
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
