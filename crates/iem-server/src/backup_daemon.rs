//! Backup daemon — runs scheduled mixer state captures.
//!
//! Reads `backup_schedule` from config each loop iteration (supports hot-reload).
//! Captures a full mixer snapshot at each configured HH:MM time and saves it to disk.
//! Old backups beyond `backup_retention_days` are pruned after each capture.

use crate::{AppState, backup_capture};
use std::collections::HashSet;

/// Spawn the backup daemon as a detached background task.
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        run(state).await;
    });
}

async fn run(state: AppState) {
    tracing::info!("Backup daemon started");

    // Tracks "YYYY-MM-DD HH:MM" keys for captures already triggered today.
    let mut triggered: HashSet<String> = HashSet::new();

    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;

        let now = chrono::Local::now();
        let today = now.format("%Y-%m-%d").to_string();
        let hhmm = now.format("%H:%M").to_string();

        // Remove keys from previous days to avoid unbounded growth.
        triggered.retain(|key| key.starts_with(&today));

        // Read schedule from config each iteration so hot-reload is supported.
        let (schedule, retention_days) = {
            let config = state.config.read().await;
            (config.backup_schedule.clone(), config.backup_retention_days)
        };

        for entry in &schedule {
            if *entry != hhmm {
                continue;
            }

            let trigger_key = format!("{today} {hhmm}");
            if triggered.contains(&trigger_key) {
                // Already ran for this slot today.
                continue;
            }

            tracing::info!(time = %hhmm, "Backup daemon: scheduled capture starting");

            match backup_capture::capture_mixer_state(&state).await {
                Ok((backup, _audit)) => match state.backup_store.save(&backup) {
                    Ok(filename) => {
                        tracing::info!(
                            filename = %filename,
                            "Backup daemon: saved scheduled backup"
                        );
                        let pruned = state.backup_store.prune(retention_days);
                        if pruned > 0 {
                            tracing::info!(
                                count = pruned,
                                "Backup daemon: pruned {} old backup(s)",
                                pruned
                            );
                        }
                        triggered.insert(trigger_key);
                    }
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            "Backup daemon: failed to save scheduled backup"
                        );
                    }
                },
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "Backup daemon: capture failed for scheduled time {}",
                        hhmm
                    );
                }
            }
        }
    }
}
