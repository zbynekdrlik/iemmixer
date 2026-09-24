//! S5: REAPER control plane — replaced by the engine client (program spec §6 S5); imported only so the server builds and its tests run.
//! REST API routes for preset management

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
};
use iem_core::{ApiError, ChannelPreset, PresetEntry};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::AppState;

/// Preset info for list response
#[derive(Serialize)]
pub struct PresetInfo {
    pub name: String,
    pub channel_count: usize,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<&PresetEntry> for PresetInfo {
    fn from(p: &PresetEntry) -> Self {
        Self {
            name: p.name.clone(),
            channel_count: p.channels.len(),
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

/// Request to save/update a preset
#[derive(Deserialize)]
pub struct SavePresetRequest {
    pub name: String,
    pub channels: HashMap<usize, ChannelPreset>,
    #[serde(default)]
    pub stems_level_db: Option<f32>,
    #[serde(default)]
    pub eq_bands: Option<HashMap<usize, Vec<iem_core::EqBand>>>,
}

/// Preset routes
pub fn preset_routes() -> Router<AppState> {
    Router::new()
        // List presets
        .route("/api/presets/{member}", get(list_presets))
        // Save preset
        .route("/api/presets/{member}", post(save_preset))
        // Get specific preset
        .route("/api/presets/{member}/{name}", get(get_preset))
        // Update preset
        .route("/api/presets/{member}/{name}", put(update_preset))
        // Delete preset
        .route("/api/presets/{member}/{name}", delete(delete_preset))
        // Restore preset (apply to REAPER)
        .route("/api/presets/{member}/{name}/restore", post(restore_preset))
}

/// List all presets for a member (newest first)
async fn list_presets(
    State(state): State<AppState>,
    Path(member): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<PresetInfo>>, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;
    crate::auth::verify_member_access(&headers, &member, &config.jwt_secret)?;
    {
        let discovered = state.discovered_members.read().await;
        if !discovered.iter().any(|m| m.id() == member) {
            return Err((StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))));
        }
    }
    drop(config);

    let presets = state.preset_store.list(&member);
    let info: Vec<PresetInfo> = presets.iter().map(PresetInfo::from).collect();
    Ok(Json(info))
}

/// Save a new preset
async fn save_preset(
    State(state): State<AppState>,
    Path(member): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<SavePresetRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;
    crate::auth::verify_member_access(&headers, &member, &config.jwt_secret)?;
    {
        let discovered = state.discovered_members.read().await;
        if !discovered.iter().any(|m| m.id() == member) {
            return Err((StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))));
        }
    }
    drop(config);

    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError::new("INVALID_NAME", "Preset name cannot be empty")),
        ));
    }

    // reaperiem#205: capture EQ server-side for ALL channels (same helper the snapshot
    // path uses); fall back to any EQ the client sent only if REAPER had none.
    let track_indices: Vec<usize> = req.channels.keys().copied().collect();
    let eq_bands = crate::proxy::capture_eq_bands(&state, &track_indices)
        .await
        .or(req.eq_bands);

    let entry = state
        .preset_store
        .save_with_stems(&member, &name, req.channels, req.stems_level_db, eq_bands)
        .map_err(|e| {
            let (code, err) = match &e {
                crate::preset_store::PresetError::LimitReached => (
                    StatusCode::CONFLICT,
                    ApiError::new("LIMIT_REACHED", e.to_string()),
                ),
                crate::preset_store::PresetError::Io(_) => {
                    tracing::error!("Failed to save preset: {}", e);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        ApiError::new("IO_ERROR", "Failed to save preset"),
                    )
                }
            };
            (code, Json(err))
        })?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "name": entry.name,
            "created_at": entry.created_at,
            "updated_at": entry.updated_at
        })),
    ))
}

/// Get a specific preset
async fn get_preset(
    State(state): State<AppState>,
    Path((member, name)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Result<Json<PresetEntry>, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;
    crate::auth::verify_member_access(&headers, &member, &config.jwt_secret)?;
    drop(config);
    state
        .preset_store
        .get(&member, &name)
        .map(Json)
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(ApiError::not_found("Preset"))))
}

/// Update an existing preset
async fn update_preset(
    State(state): State<AppState>,
    Path((member, name)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<SavePresetRequest>,
) -> Result<Json<PresetEntry>, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;
    crate::auth::verify_member_access(&headers, &member, &config.jwt_secret)?;
    drop(config);
    // Verify preset exists.
    if state.preset_store.get(&member, &name).is_none() {
        return Err((StatusCode::NOT_FOUND, Json(ApiError::not_found("Preset"))));
    }

    // reaperiem#205: capture EQ server-side for ALL channels (overwrite = save the CURRENT
    // mix, including its live EQ); fall back to client EQ only if REAPER had none.
    let track_indices: Vec<usize> = req.channels.keys().copied().collect();
    let eq_bands = crate::proxy::capture_eq_bands(&state, &track_indices)
        .await
        .or(req.eq_bands);

    let entry = state
        .preset_store
        .save_with_stems(&member, &name, req.channels, req.stems_level_db, eq_bands)
        .map_err(|e| {
            tracing::error!("Failed to update preset: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("IO_ERROR", "Failed to update preset")),
            )
        })?;

    Ok(Json(entry))
}

/// Delete a preset
async fn delete_preset(
    State(state): State<AppState>,
    Path((member, name)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;
    crate::auth::verify_member_access(&headers, &member, &config.jwt_secret)?;
    drop(config);
    let deleted = state.preset_store.delete(&member, &name).map_err(|e| {
        tracing::error!("Failed to delete preset: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("IO_ERROR", "Failed to delete preset")),
        )
    })?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::NOT_FOUND, Json(ApiError::not_found("Preset"))))
    }
}

/// Restore a preset by applying all channel values to REAPER
async fn restore_preset(
    State(state): State<AppState>,
    Path((member, name)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;
    crate::auth::verify_member_access(&headers, &member, &config.jwt_secret)?;
    drop(config);
    // Get the preset
    let preset = state
        .preset_store
        .get(&member, &name)
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(ApiError::not_found("Preset"))))?;

    // Get member index from discovered members and REAPER URL from config
    let discovered = state.discovered_members.read().await;
    let member_index = discovered
        .iter()
        .find(|m| m.id() == member)
        .map(|m| m.send_index)
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))))?;
    // reaperiem#204: mix channels route to a DIFFERENT send than the member's own.
    let mix_members = crate::proxy::compute_mix_members(&discovered, &member);
    drop(discovered);
    let config = state.config.read().await;
    let reaper_url = config.reaper_url.clone();
    drop(config);

    // Execute in parallel: send level, pan, mute for each channel
    let mut handles = Vec::new();
    for (track_index, ch) in &preset.channels {
        let url_base = reaper_url.clone();
        let client = state.http_client.clone();
        let track_idx = *track_index;
        // reaperiem#204: resolve the correct send for this track — mix channels use their
        // discovered mix_send_index; a missing one is a SAFETY error, not a fallback.
        let send_index = crate::proxy::resolve_send_index(track_idx, member_index, &mix_members)
            .map_err(|e| {
                tracing::error!(track_idx, error = %e, "Preset restore send_index resolution failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError::new("SEND_INDEX", e)),
                )
            })?;
        let vol_db = ch.vol;
        // reaperiem#203: stored pan is UI-range 0..1 — convert to REAPER -1..1 before writing.
        let pan = crate::proxy::restore_send_pan(ch.pan);
        let mute = ch.mute;

        handles.push(tokio::spawn(async move {
            let mut op_count = 0;

            // Convert dB to REAPER linear scale
            let vol_linear = if vol_db <= -60.0 {
                0.0
            } else {
                10.0_f32.powf(vol_db / 20.0).clamp(0.0, 4.0)
            };

            // Set level
            let url = format!(
                "{}/_/SET/TRACK/{}/SEND/{}/VOL/{:.6}",
                url_base, track_idx, send_index, vol_linear
            );
            if let Err(e) = client.get(&url).send().await {
                tracing::error!("Restore level failed: {}", e);
            }
            op_count += 1;

            // Set pan
            let url = format!(
                "{}/_/SET/TRACK/{}/SEND/{}/PAN/{:.6}",
                url_base, track_idx, send_index, pan
            );
            if let Err(e) = client.get(&url).send().await {
                tracing::error!("Restore pan failed: {}", e);
            }
            op_count += 1;

            // Set mute
            let mute_val = if mute { 1 } else { 0 };
            let url = format!(
                "{}/_/SET/TRACK/{}/SEND/{}/MUTE/{}",
                url_base, track_idx, send_index, mute_val
            );
            if let Err(e) = client.get(&url).send().await {
                tracing::error!("Restore mute failed: {}", e);
            }
            op_count += 1;

            op_count
        }));
    }

    // Restore stems bus volume if present in preset
    if let Some(stems_db) = preset.stems_level_db {
        let cache = state.mixer_cache.read().await;
        if let Some(&stems_track) = cache.stems_bus_indices.get(&member) {
            drop(cache);
            let vol_linear = if stems_db <= -60.0 {
                0.0
            } else {
                10.0_f32.powf(stems_db / 20.0).clamp(0.0, 4.0)
            };
            let url = format!(
                "{}/_/SET/TRACK/{}/VOL/{:.6}",
                reaper_url, stems_track, vol_linear
            );
            let client = state.http_client.clone();
            handles.push(tokio::spawn(async move {
                if let Err(e) = client.get(&url).send().await {
                    tracing::error!("Restore stems level failed: {}", e);
                }
                1
            }));
        }
    }

    // Wait for all volume/pan/mute commands to complete
    let mut total_ops = 0;
    for handle in handles {
        if let Ok(count) = handle.await {
            total_ops += count;
        }
    }

    // Restore EQ bands if present in preset (serialized via eq_write_lock)
    if let Some(ref eq_bands_map) = preset.eq_bands {
        for (track_index, bands) in eq_bands_map {
            for (band_idx, band) in bands.iter().enumerate() {
                // NOTE: preset replay uses the LEGACY norm protocol for all params:
                // `param=freq` (norm), `param=gain` (norm), `param=bw` (norm).
                // The interactive UI slider uses the value-domain protocols:
                // `param=gain_db` (reaperiem#194), `param=freq_hz` (reaperiem#196), `param=bw_oct` (reaperiem#196).
                // The ReaScript supports BOTH families. Don't remove any legacy
                // norm branch from set_eq_param.lua without updating preset/snapshot
                // apply paths — preset bit-exactness depends on the norm protocol.
                for (param_name, value) in [
                    ("freq", band.freq_norm),
                    ("gain", band.gain_norm),
                    ("bw", band.bw_norm),
                    ("enabled", if band.enabled { 1.0 } else { 0.0 }),
                ] {
                    let _lock = state.eq_write_lock.lock().await;
                    let input = format!(
                        "track={}|band={}|param={}|value={:.6}",
                        track_index, band_idx, param_name, value
                    );
                    let _ = state
                        .http_client
                        .get(format!(
                            "{}/_/SET/EXTSTATE/reaperiem/eq_set/{}",
                            reaper_url, input
                        ))
                        .send()
                        .await;
                    let _ = state
                        .http_client
                        .get(format!("{}/_/_RS_REAPERIEM_SET_EQ", reaper_url))
                        .send()
                        .await;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    total_ops += 1;
                }
            }
        }
    }

    tracing::info!(
        member = %member,
        preset = %name,
        operations = total_ops,
        "Restored preset"
    );

    Ok(Json(serde_json::json!({
        "restored": true,
        "channels": preset.channels.len(),
        "operations": total_ops
    })))
}
