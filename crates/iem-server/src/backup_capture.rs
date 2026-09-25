//! S5: REAPER control plane — replaced by the engine client (program spec §6 S5); imported only so the server builds and its tests run.
//! Mixer state capture — reads all current state from REAPER and local files
//!
//! Used by backup operations to snapshot the full system state into a `MixerBackup`.

use iem_core::{
    CaptureAudit, ServerMsg,
    backup::{BACKUP_VERSION, EqBandBackup, LimiterBackup, MixerBackup, SendBackup},
};
use std::collections::HashMap;
use std::time::Instant;

use crate::{AppState, proxy};

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("capture incomplete: sends_count={got} below minimum={min}")]
    InsufficientSends { got: usize, min: usize },
    #[error("capture incomplete: track_mutes_count={got} below minimum={min}")]
    InsufficientTrackMutes { got: usize, min: usize },
}

/// Computes operational minimum capture counts from the live project shape.
///
/// Sends: each input track has one send per band member → `inputs × members`.
/// Track mutes: every visible track contributes one entry → roughly
/// `inputs + members × 2 (inear + stems)` plus a few aux/tech tracks.
///
/// We accept 90% of the nominal as the lower bound so a single missing
/// optional track (e.g. a temporarily-removed aux) does not trip the gate.
/// Anything substantially below that signals REAPER was unresponsive and
/// the backup would be silently corrupt — fail loudly instead.
pub fn compute_capture_thresholds(input_tracks: usize, band_members: usize) -> (usize, usize) {
    let nominal_sends = input_tracks * band_members;
    let nominal_track_mutes = input_tracks + band_members * 2;
    let min_sends = (nominal_sends * 9) / 10;
    let min_track_mutes = (nominal_track_mutes * 9) / 10;
    (min_sends, min_track_mutes)
}

/// Refuses to accept a capture whose entry counts fall below operational minimums.
/// A capture below threshold likely means REAPER was unresponsive for some queries
/// and the resulting backup would be silently corrupt — fail loudly instead.
pub fn assert_capture_completeness(
    audit: &CaptureAudit,
    min_sends: usize,
    min_track_mutes: usize,
) -> Result<(), CaptureError> {
    if audit.sends_count < min_sends {
        return Err(CaptureError::InsufficientSends {
            got: audit.sends_count,
            min: min_sends,
        });
    }
    if audit.track_mutes_count < min_track_mutes {
        return Err(CaptureError::InsufficientTrackMutes {
            got: audit.track_mutes_count,
            min: min_track_mutes,
        });
    }
    Ok(())
}

/// Returns true if the track should be skipped from mute/volume capture.
///
/// MASTER (index 0) is never captured: restoring its mute would silence the entire session.
pub(crate) fn should_skip_master(track_idx: usize) -> bool {
    track_idx == 0
}

/// Returns true if the track name qualifies for output-volume capture.
///
/// Only "inear" and "stems" tracks have their output volume backed up — those are the only
/// tracks whose volume the engineer typically needs to restore.
pub(crate) fn track_name_qualifies_for_volume_capture(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.contains("inear") || lower.contains("stems")
}

/// Capture the complete current mixer state.
///
/// Reads:
/// - All tracks from REAPER (index, name, volume, send count)
/// - All sends per track (vol/pan/mute/dest) — hardware outputs skipped
/// - Track output volumes for "inear" and "stems" tracks
/// - EQ bands for all tracks
/// - Limiter params for "inear" tracks
/// - Per-member UI customizations
/// - Per-member PINs
pub async fn capture_mixer_state(state: &AppState) -> Result<(MixerBackup, CaptureAudit), String> {
    let (reaper_url, min_sends, min_track_mutes) = {
        let config = state.config.read().await;
        let (s, m) = compute_capture_thresholds(config.inputs.len(), config.members.len());
        (config.reaper_url.clone(), s, m)
    };

    let capture_started = Instant::now();
    tracing::info!("Backup capture: starting full mixer state snapshot");

    // --- 1. Query all tracks ---
    let tracks_url = proxy::reaper_api::query_tracks(&reaper_url);
    let tracks_text = state
        .http_client
        .get(&tracks_url)
        .send()
        .await
        .map_err(|e| format!("Failed to query REAPER tracks: {e}"))?
        .text()
        .await
        .map_err(|e| format!("Failed to read REAPER tracks response: {e}"))?;

    // Parse track lines
    // Format: TRACK\tidx\tname\tflags\tvol\tpan\t...\tsendcnt\t...
    // Field indices (0-based): 0=TRACK, 1=idx, 2=name, 3=flags, 4=vol, 5=pan, ...
    // sendcnt is at field 10 when VU fields are present (14+ fields) or field 8 (12 fields)
    struct TrackInfo {
        index: usize,
        name: String,
        vol_linear: f32,
        muted: bool,
        send_count: usize,
    }

    let mut tracks: Vec<TrackInfo> = Vec::new();
    let mut track_layout: HashMap<u32, String> = HashMap::new();

    for line in tracks_text.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.first() != Some(&"TRACK") || parts.len() < 12 {
            continue;
        }
        let Ok(track_idx) = parts[1].parse::<usize>() else {
            continue;
        };
        // Skip master track (index 0)
        if track_idx == 0 {
            continue;
        }
        let track_name = parts[2].to_string();
        let flags: i32 = parts[3].parse().unwrap_or(0);
        let vol_linear: f32 = parts[4].parse().unwrap_or(1.0);
        let muted = (flags & 8) != 0;

        // sendcnt: field 10 when 14+ fields (VU present), field 8 when 12 fields
        let send_count = if parts.len() >= 14 {
            parts[10].parse().unwrap_or(0)
        } else {
            parts[8].parse().unwrap_or(0)
        };

        track_layout.insert(track_idx as u32, track_name.clone());
        tracks.push(TrackInfo {
            index: track_idx,
            name: track_name,
            vol_linear,
            muted,
            send_count,
        });
    }

    tracing::info!(
        count = tracks.len(),
        "Backup capture: found {} tracks",
        tracks.len()
    );

    // Build name→track map for send destination resolution
    let track_by_index: HashMap<usize, String> =
        tracks.iter().map(|t| (t.index, t.name.clone())).collect();

    // --- 2. Collect sends for all tracks that have sends ---
    let mut sends: Vec<SendBackup> = Vec::new();

    for track in &tracks {
        if track.send_count == 0 {
            continue;
        }
        for send_idx in 0..track.send_count {
            let send_url = proxy::reaper_api::get_send_state(&reaper_url, track.index, send_idx);
            let resp = match state.http_client.get(&send_url).send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        track = %track.name,
                        send_idx,
                        error = %e,
                        "Backup capture: failed to get send state, skipping"
                    );
                    continue;
                }
            };
            let text = match resp.text().await {
                Ok(t) => t,
                Err(_) => continue,
            };
            // Parse SEND line: SEND\tsrc\tsend_idx\tmute\tvol\tpan\tdest
            for part_line in text.lines() {
                let parts: Vec<&str> = part_line.split('\t').collect();
                if parts.first() != Some(&"SEND") || parts.len() < 7 {
                    continue;
                }
                let dest: i32 = parts[6].parse().unwrap_or(-1);
                // dest < 1 means hardware output — skip
                if dest < 1 {
                    continue;
                }
                let vol: f32 = parts[4].parse().unwrap_or(1.0);
                let mute_flag: i32 = parts[3].parse().unwrap_or(0);
                let pan: f32 = parts[5].parse().unwrap_or(0.0);
                let mute = mute_flag != 0;

                let dest_name = track_by_index
                    .get(&(dest as usize))
                    .cloned()
                    .unwrap_or_else(|| format!("track_{}", dest));

                // Store LINEAR volume directly (same as REAPER API returns/accepts)
                sends.push(SendBackup {
                    src_name: track.name.clone(),
                    dest_name,
                    vol: vol as f64,
                    pan: pan as f64,
                    mute,
                });
            }
        }
    }

    tracing::info!(
        count = sends.len(),
        "Backup capture: captured {} sends",
        sends.len()
    );

    // --- 3. Collect track mute state for ALL tracks (volume only for inear/stems) ---
    //
    // Why: track-level mute applies to any track (CG, hand mics, BGV bus, etc).
    // Filtering by name silently excluded CG and broke the 2026-04-26 morning restore —
    // the engineer expected restore to re-mute CG and it didn't, because CG was never
    // in the backup. See docs/superpowers/investigation/2026-04-26-incident.md.
    //
    // Track output VOLUMES are still captured only for inear/stems — those are the only
    // tracks whose volume the engineer typically restores. Mute is the safety-critical
    // dimension; volume restoration on every track has no use case.
    let mut track_volumes: HashMap<String, f64> = HashMap::new();
    let mut track_mutes: HashMap<String, bool> = HashMap::new();
    for track in &tracks {
        // Skip MASTER (idx 0) — its mute would silence everything; never restore it.
        if should_skip_master(track.index) {
            continue;
        }
        track_mutes.insert(track.name.clone(), track.muted);

        if track_name_qualifies_for_volume_capture(&track.name) {
            track_volumes.insert(track.name.clone(), track.vol_linear as f64);
        }
    }

    tracing::info!(
        count = track_volumes.len(),
        "Backup capture: captured {} output volumes",
        track_volumes.len()
    );

    // --- 4. Read EQ for ALL tracks ---
    let mut eq: HashMap<String, Vec<EqBandBackup>> = HashMap::new();
    for track in &tracks {
        match proxy::handle_get_eq_params(state, track.index).await {
            Some(ServerMsg::EqParams {
                track_name, bands, ..
            }) => {
                let band_backups: Vec<EqBandBackup> = bands
                    .iter()
                    .enumerate()
                    .map(|(i, b)| EqBandBackup::from((i as u8, b)))
                    .collect();
                let key = if track_name.trim().is_empty() {
                    track.name.clone()
                } else {
                    track_name.trim().to_string()
                };
                eq.insert(key, band_backups);
            }
            Some(_) | None => {
                tracing::debug!(track = %track.name, "Backup capture: no EQ data for track");
            }
        }
    }

    tracing::info!(
        count = eq.len(),
        "Backup capture: captured EQ for {} tracks",
        eq.len()
    );

    // --- 5. Read limiter for "inear" tracks ---
    let mut limiter: HashMap<String, LimiterBackup> = HashMap::new();
    for track in &tracks {
        if !track.name.to_lowercase().contains("inear") {
            continue;
        }
        match proxy::handle_get_limiter_params(state, track.index).await {
            Some(ServerMsg::LimiterParams {
                track_name,
                limit_db,
                limit_norm,
                enabled,
                ..
            }) => {
                let key = if track_name.trim().is_empty() {
                    track.name.clone()
                } else {
                    track_name.trim().to_string()
                };
                limiter.insert(
                    key,
                    LimiterBackup {
                        limit_db,
                        limit_norm,
                        enabled,
                    },
                );
            }
            Some(_) | None => {
                tracing::debug!(track = %track.name, "Backup capture: no limiter data for track");
            }
        }
    }

    tracing::info!(
        count = limiter.len(),
        "Backup capture: captured limiter for {} inear tracks",
        limiter.len()
    );

    // --- 6. Read customizations for all known members ---
    let known_members = [
        "member1", "member2", "member3", "member4", "member5", "member6", "member7", "member8",
        "member9", "engineer",
    ];
    let mut customizations = HashMap::new();
    for member_id in &known_members {
        let cust = state.customization_store.load(member_id);
        if !cust.pinned.is_empty() || !cust.hidden.is_empty() {
            customizations.insert(member_id.to_string(), cust);
        }
    }

    tracing::info!(
        count = customizations.len(),
        "Backup capture: captured customizations for {} members",
        customizations.len()
    );

    // --- 7. PINs are never part of a backup (security baseline) ---
    let pins = std::collections::HashMap::new();

    // --- 8. Build timestamp ---
    let timestamp = chrono::Local::now()
        .format("%Y-%m-%dT%H:%M:%S%z")
        .to_string();

    tracing::info!(%timestamp, "Backup capture: complete");

    tracing::info!(
        count = track_mutes.len(),
        "Backup capture: captured {} track mute states",
        track_mutes.len()
    );

    let backup = MixerBackup {
        version: BACKUP_VERSION,
        timestamp,
        track_layout,
        sends,
        track_volumes,
        eq,
        limiter,
        customizations,
        pins,
        track_mutes,
    };

    let audit = CaptureAudit {
        tracks_total: backup.track_layout.len(),
        tracks_named: backup.track_layout.values().cloned().collect::<Vec<_>>(),
        sends_count: backup.sends.len(),
        track_mutes_count: backup.track_mutes.len(),
        track_volumes_count: backup.track_volumes.len(),
        eq_count: backup.eq.len(),
        limiter_count: backup.limiter.len(),
        customizations_count: backup.customizations.len(),
        pins_count: backup.pins.len(),
        reaper_query_duration_ms: capture_started.elapsed().as_millis() as u64,
        warnings: vec![],
    };

    assert_capture_completeness(&audit, min_sends, min_track_mutes).map_err(|e| e.to_string())?;

    tracing::info!(
        sends = audit.sends_count,
        track_mutes = audit.track_mutes_count,
        eq = audit.eq_count,
        duration_ms = audit.reaper_query_duration_ms,
        "Backup capture complete"
    );

    Ok((backup, audit))
}

#[cfg(test)]
mod completeness_tests {
    use super::*;
    use iem_core::CaptureAudit;

    fn audit_with_counts(sends: usize, track_mutes: usize) -> CaptureAudit {
        CaptureAudit {
            tracks_total: 56,
            tracks_named: vec![],
            sends_count: sends,
            track_mutes_count: track_mutes,
            track_volumes_count: 10,
            eq_count: 22,
            limiter_count: 10,
            customizations_count: 10,
            pins_count: 10,
            reaper_query_duration_ms: 1000,
            warnings: vec![],
        }
    }

    #[test]
    fn complete_capture_passes_assertion() {
        let audit = audit_with_counts(220, 56);
        assert!(assert_capture_completeness(&audit, 200, 30).is_ok());
    }

    #[test]
    fn capture_below_sends_threshold_fails() {
        let audit = audit_with_counts(150, 56);
        let err = assert_capture_completeness(&audit, 200, 30).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("sends_count") || msg.contains("InsufficientSends"));
    }

    #[test]
    fn capture_below_track_mutes_threshold_fails() {
        let audit = audit_with_counts(220, 5);
        let err = assert_capture_completeness(&audit, 200, 30).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("track_mutes") || msg.contains("InsufficientTrackMutes"));
    }

    // Boundary tests that kill the `<` → `<=` surviving mutants.
    // At the exact threshold value the original passes, the mutant rejects.

    #[test]
    fn complete_capture_at_exact_sends_threshold_passes() {
        // sends_count == min_sends — must be accepted (< is the right operator)
        let audit = audit_with_counts(200, 56);
        assert!(assert_capture_completeness(&audit, 200, 30).is_ok());
    }

    #[test]
    fn complete_capture_at_one_below_sends_threshold_fails() {
        // 199 < 200 — must be rejected
        let audit = audit_with_counts(199, 56);
        assert!(assert_capture_completeness(&audit, 200, 30).is_err());
    }

    #[test]
    fn complete_capture_at_exact_track_mutes_threshold_passes() {
        // track_mutes_count == min_track_mutes — must be accepted
        let audit = audit_with_counts(220, 30);
        assert!(assert_capture_completeness(&audit, 200, 30).is_ok());
    }

    #[test]
    fn complete_capture_at_one_below_track_mutes_threshold_fails() {
        // 29 < 30 — must be rejected
        let audit = audit_with_counts(220, 29);
        assert!(assert_capture_completeness(&audit, 200, 30).is_err());
    }

    // Threshold-derivation tests — confirm the formula tracks the project shape.

    #[test]
    fn capture_thresholds_for_current_band_size() {
        // 22 input tracks × 10 members = 220 sends; threshold = 198.
        // 22 + 10*2 = 42 track mutes nominal; threshold = 37.
        let (min_sends, min_track_mutes) = compute_capture_thresholds(22, 10);
        assert_eq!(min_sends, 198);
        assert_eq!(min_track_mutes, 37);
    }

    #[test]
    fn capture_thresholds_scale_with_band_size() {
        // Adding a band member shifts both thresholds upward — gates must follow.
        let (sends_10, mutes_10) = compute_capture_thresholds(22, 10);
        let (sends_11, mutes_11) = compute_capture_thresholds(22, 11);
        assert!(sends_11 > sends_10);
        assert!(mutes_11 > mutes_10);
    }

    #[test]
    fn capture_thresholds_are_zero_for_empty_project() {
        // No inputs and no members ⇒ no minimum (assertion always passes).
        let (min_sends, min_track_mutes) = compute_capture_thresholds(0, 0);
        assert_eq!(min_sends, 0);
        assert_eq!(min_track_mutes, 0);
    }

    // Unit tests for the extracted helper predicates (kills line-213 == and line-219 || mutants).

    #[test]
    fn should_skip_master_returns_true_for_idx_0() {
        assert!(should_skip_master(0));
    }

    #[test]
    fn should_skip_master_returns_false_for_idx_1() {
        assert!(!should_skip_master(1));
    }

    #[test]
    fn should_skip_master_returns_false_for_large_idx() {
        assert!(!should_skip_master(22));
    }

    #[test]
    fn volume_capture_qualifies_inear_track() {
        assert!(track_name_qualifies_for_volume_capture("MEMBER1 inear"));
    }

    #[test]
    fn volume_capture_qualifies_stems_track() {
        assert!(track_name_qualifies_for_volume_capture("MEMBER7 stems"));
    }

    #[test]
    fn volume_capture_does_not_qualify_mic_track() {
        assert!(!track_name_qualifies_for_volume_capture("MEMBER3 mic"));
    }

    #[test]
    fn volume_capture_does_not_qualify_cg_track() {
        assert!(!track_name_qualifies_for_volume_capture("CG"));
    }

    #[test]
    fn volume_capture_does_not_qualify_empty_name() {
        assert!(!track_name_qualifies_for_volume_capture(""));
    }

    #[test]
    fn volume_capture_qualifies_uppercase_inear() {
        // Case-insensitive: INEAR should still qualify
        assert!(track_name_qualifies_for_volume_capture("OLDMEMBER1 INEAR"));
    }
}
