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

async fn run(state: AppState) {
    tracing::info!("Backup daemon started");
    let mut done: HashSet<String> = HashSet::new();
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
        match crate::backup_routes::capture_now(&state) {
            Ok((filename, _)) => {
                tracing::info!(%filename, "Backup daemon: saved scheduled backup");
                let pruned = state.backup_store.prune(retention_days);
                if pruned > 0 {
                    tracing::info!(count = pruned, "Backup daemon: pruned old backups");
                }
                done.insert(key);
            }
            Err(e) => tracing::error!(error = %e, time = %hhmm, "Backup daemon: capture failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
