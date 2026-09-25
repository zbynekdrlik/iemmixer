//! S5: REAPER control plane — replaced by the engine client (program spec §6 S5); imported only so the server builds and its tests run.
//! REAPER HTTP API proxy

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use iem_core::{ApiError, BatchControlRequest, BatchOperation, PollResponse};
use std::collections::HashMap;

use crate::AppState;

/// Error report sent by the WASM client when a panic occurs.
///
/// All fields except `panic_message` are optional so that degraded clients
/// (e.g. broken Leptos graph, missing window globals) can still send a report.
#[derive(Debug, serde::Deserialize)]
pub struct ClientErrorReport {
    pub panic_message: String,
    pub version: Option<String>,
    pub git_hash: Option<String>,
    pub url: Option<String>,
    pub user_agent: Option<String>,
    pub location: Option<String>,
    pub backtrace: Option<String>,
}

/// POST /api/client-error — receive a client-side panic report and log it.
///
/// Public route (no auth) because panics may occur when auth itself is broken.
/// Request body is capped at 10 KB by the route-level `DefaultBodyLimit` layer
/// in routes.rs. Always returns 204 No Content on success; malformed bodies
/// return 400 via Axum's default JSON rejection.
///
/// Logs via `tracing::warn!` with a structured `client_error` prefix so the
/// line is grep-able: `journalctl -u iem-mixer | grep client_error`.
pub async fn client_error(
    axum::Json(report): axum::Json<ClientErrorReport>,
) -> axum::http::StatusCode {
    tracing::warn!(
        target: "iem_server::client_error",
        version = report.version.as_deref().unwrap_or("?"),
        git_hash = report.git_hash.as_deref().unwrap_or("?"),
        url = report.url.as_deref().unwrap_or("?"),
        user_agent = report.user_agent.as_deref().unwrap_or("?"),
        location = report.location.as_deref().unwrap_or("?"),
        panic = %report.panic_message,
        "client_error",
    );
    if let Some(bt) = report.backtrace.as_deref() {
        tracing::warn!(
            target: "iem_server::client_error",
            backtrace = %bt,
            "client_error_backtrace",
        );
    }
    axum::http::StatusCode::NO_CONTENT
}

/// Query parameters for WebSocket connection
#[derive(Debug, serde::Deserialize)]
pub struct WsQuery {
    pub token: Option<String>,
}

/// Get current mixer state for a member
pub async fn get_mixer_state(
    State(state): State<AppState>,
    Path(member_id): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<iem_core::MixerState>, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;
    crate::auth::verify_member_access(&headers, &member_id, &config.jwt_secret)?;

    // Use discovered members (REAPER source of truth) instead of config
    let discovered = state.discovered_members.read().await;
    let member_index = discovered
        .iter()
        .find(|m| m.id() == member_id)
        .map(|m| m.send_index)
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))))?;
    drop(discovered);
    let reaper_url = config.reaper_url.clone();

    let resolved = state.mixer_cache.read().await.input_track_indices.clone();
    let resolved_ref = if resolved.is_empty() {
        None
    } else {
        Some(&resolved)
    };
    let channels = build_channel_templates(&config.inputs, resolved_ref);
    drop(config);

    // Try to get actual levels from REAPER (all channels in parallel)
    let mut result_channels = channels.clone();
    let send_futures: Vec<_> = result_channels
        .iter()
        .map(|ch| {
            let client = state.http_client.clone();
            let url = reaper_url.clone();
            let track_index = ch.track_index;
            async move {
                let result = query_send_state(&client, &url, track_index, member_index).await;
                (track_index, result)
            }
        })
        .collect();

    let send_results = futures::future::join_all(send_futures).await;
    for (track_index, result) in send_results {
        if let Ok((level, mute, pan)) = result
            && let Some(ch) = result_channels
                .iter_mut()
                .find(|c| c.track_index == track_index)
        {
            ch.level_db = reaper_vol_to_db(level);
            ch.muted = mute;
            ch.pan = reaper_pan_to_ui(pan);
        }
    }

    // For engineer or elevated: append mix channels
    let is_elevated = member_id == "member1";
    if member_id == "engineer" || is_elevated {
        let discovered = state.discovered_members.read().await;
        let mut mix_channels = build_mix_channel_templates(&discovered, &member_id);

        let mix_futures: Vec<_> = mix_channels
            .iter()
            .filter_map(|ch| {
                let mix_si = if member_id == "engineer" {
                    discovered
                        .iter()
                        .find(|m| m.track_index == ch.track_index)
                        .and_then(|m| m.mix_send_index)
                } else {
                    // mix_send_indices stored on elevated member, keyed by source ID
                    let source_id = discovered
                        .iter()
                        .find(|m| m.track_index == ch.track_index)
                        .map(|m| m.id());
                    let elevated = discovered.iter().find(|m| m.id() == member_id);
                    source_id.and_then(|sid| {
                        elevated.and_then(|e| e.mix_send_indices.get(&sid).copied())
                    })
                }?;
                let client = state.http_client.clone();
                let url = reaper_url.clone();
                let track_index = ch.track_index;
                Some(async move {
                    let result = query_send_state(&client, &url, track_index, mix_si).await;
                    (track_index, result)
                })
            })
            .collect();
        drop(discovered);

        let mix_results = futures::future::join_all(mix_futures).await;
        for (track_index, result) in mix_results {
            if let Ok((level, mute, pan)) = result
                && let Some(ch) = mix_channels
                    .iter_mut()
                    .find(|c| c.track_index == track_index)
            {
                ch.level_db = reaper_vol_to_db(level);
                ch.muted = mute;
                ch.pan = reaper_pan_to_ui(pan);
            }
        }

        result_channels.extend(mix_channels);
    }

    Ok(Json(iem_core::MixerState {
        member_id: member_id.clone(),
        channels: result_channels,
    }))
}

/// Poll current mixer state with meters (optimized for frequent calls)
pub async fn poll_mixer_state(
    State(state): State<AppState>,
    Path(member_id): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<PollResponse>, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;
    crate::auth::verify_member_access(&headers, &member_id, &config.jwt_secret)?;

    // Use discovered members (REAPER source of truth) instead of config
    let discovered = state.discovered_members.read().await;
    let member_index = discovered
        .iter()
        .find(|m| m.id() == member_id)
        .map(|m| m.send_index)
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))))?;
    drop(discovered);
    let reaper_url = config.reaper_url.clone();

    let resolved = state.mixer_cache.read().await.input_track_indices.clone();
    let resolved_ref = if resolved.is_empty() {
        None
    } else {
        Some(&resolved)
    };
    let channels = build_channel_templates(&config.inputs, resolved_ref);
    drop(config);

    let mut result_channels = channels;
    let mut meters: HashMap<usize, [f32; 2]> = HashMap::new();
    let mut connected = false;

    // Query REAPER for all track states in a batch
    // First try to get all track info
    let tracks_url = reaper_api::query_tracks(&reaper_url);
    if let Ok(resp) = state.http_client.get(&tracks_url).send().await
        && let Ok(text) = resp.text().await
    {
        connected = true;
        // Parse track data for meters
        // REAPER TRACK format varies by VU availability:
        //   With VU (14 fields): TRACK idx name flags vol pan VU_L VU_R width panmode sendcnt recvcnt hwout color
        //   Without VU (12 fields): TRACK idx name flags vol pan width panmode sendcnt recvcnt hwout color
        // VU fields only present when track is record-armed (input monitoring)
        for line in text.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.first() == Some(&"TRACK")
                && parts.len() >= 14
                && let Ok(track_idx) = parts[1].parse::<usize>()
                && let (Ok(peak_db10), Ok(pos_db10)) =
                    (parts[6].parse::<f32>(), parts[7].parse::<f32>())
            {
                // REAPER HTTP API docs: "last_meter_peak and last_meter_pos
                // are integers that are dB*10, so -100 would be -10dB."
                // Floor: -1500 = -150 dB = digital silence (no signal).
                let db10_to_linear = |v: f32| -> f32 {
                    if v <= -1500.0 {
                        0.0
                    } else {
                        10.0_f32.powf(v / 10.0 / 20.0)
                    }
                };
                meters.insert(
                    track_idx,
                    [db10_to_linear(peak_db10), db10_to_linear(pos_db10)],
                );
            }
            // No VU (< 14 fields) → don't insert → frontend defaults to 0.0
        }

        // Try meter bridge EXTSTATE for true L/R per-channel peaks
        let extstate_url = reaper_api::get_extstate(&reaper_url, "REAPERIEM_METERS", "peaks");
        if let Ok(resp) = state.http_client.get(&extstate_url).send().await
            && let Ok(text) = resp.text().await
        {
            // REAPER EXTSTATE response: "EXTSTATE\tSECTION\tKEY\tvalue"
            if let Some(value) = text.split('\t').nth(3)
                && !value.is_empty()
            {
                let bridge_meters = crate::poller::parse_meter_bridge(value);
                if !bridge_meters.is_empty() {
                    meters = bridge_meters;
                }
            }
        }
    }

    // Query send states for all channels in parallel
    let send_futures: Vec<_> = result_channels
        .iter()
        .map(|ch| {
            let client = state.http_client.clone();
            let url = reaper_url.clone();
            let track_index = ch.track_index;
            async move {
                let result = query_send_state(&client, &url, track_index, member_index).await;
                (track_index, result)
            }
        })
        .collect();

    let send_results = futures::future::join_all(send_futures).await;
    for (track_index, result) in send_results {
        if let Ok((level, mute, pan)) = result {
            if let Some(ch) = result_channels
                .iter_mut()
                .find(|c| c.track_index == track_index)
            {
                ch.level_db = reaper_vol_to_db(level);
                ch.muted = mute;
                ch.pan = reaper_pan_to_ui(pan);
            }
            connected = true;
        }
    }

    Ok(Json(PollResponse {
        member_id: member_id.clone(),
        channels: result_channels,
        meters,
        connected,
    }))
}

/// Batch control operations (Reset, MuteAll)
pub async fn batch_control(
    State(state): State<AppState>,
    Path(member_id): Path<String>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<BatchControlRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;
    crate::auth::verify_member_access(&headers, &member_id, &config.jwt_secret)?;

    // Use discovered members (REAPER source of truth) instead of config
    let discovered = state.discovered_members.read().await;
    let member_index = discovered
        .iter()
        .find(|m| m.id() == member_id)
        .map(|m| m.send_index)
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))))?;
    drop(discovered);

    let reaper_url = config.reaper_url.clone();
    let inputs = config.inputs.clone();

    // Use resolved track indices from poller (name-based lookup) instead of sequential i+1
    let resolved = state.mixer_cache.read().await.input_track_indices.clone();

    drop(config);

    match payload.operation {
        BatchOperation::Reset => {
            let vol = db_to_reaper_vol(0.0);

            // Reset all channels in parallel using resolved track indices
            let urls: Vec<String> = inputs
                .iter()
                .enumerate()
                .flat_map(|(i, input)| {
                    // Use resolved index from poller if available, else fall back to i+1
                    let t = resolved.get(&input.name).copied().unwrap_or(i + 1);
                    vec![
                        reaper_api::set_send_vol(&reaper_url, t, member_index, vol),
                        reaper_api::set_send_mute(&reaper_url, t, member_index, 0),
                        reaper_api::set_send_pan(&reaper_url, t, member_index, 0.0),
                    ]
                })
                .collect();

            let results =
                futures::future::join_all(urls.iter().map(|url| state.http_client.get(url).send()))
                    .await;
            let errors: Vec<_> = results.iter().filter(|r| r.is_err()).collect();
            if !errors.is_empty() {
                tracing::warn!(
                    error_count = errors.len(),
                    "Batch reset: some REAPER calls failed"
                );
                return Err((
                    StatusCode::BAD_GATEWAY,
                    Json(ApiError::new(
                        "REAPER_ERROR",
                        format!(
                            "{} of {} reset commands failed",
                            errors.len(),
                            results.len()
                        ),
                    )),
                ));
            }
        }
        BatchOperation::MuteAll => {
            // Mute all input channels for this member using resolved track indices
            let mut urls: Vec<String> = inputs
                .iter()
                .enumerate()
                .map(|(i, input)| {
                    let t = resolved.get(&input.name).copied().unwrap_or(i + 1);
                    reaper_api::set_send_mute(&reaper_url, t, member_index, 1)
                })
                .collect();

            // For engineer or elevated: also mute mix channels
            // CRITICAL: Use discovered send indices, NOT hardcoded 0!
            // Send 0 on member inear tracks is the hardware output (Dante to speakers).
            // Muting Send 0 kills the member's audio entirely.
            {
                let member_ref: &str = member_id.as_ref();
                let is_elev = member_ref == "member1";
                if member_id == "engineer" || is_elev {
                    let discovered = state.discovered_members.read().await;
                    let elevated_member = discovered.iter().find(|m| m.id() == member_id);
                    let mix_urls: Vec<String> = discovered
                        .iter()
                        .filter(|m| m.id() != member_id && m.id() != "engineer")
                        .filter_map(|m| {
                            let si = if member_id == "engineer" {
                                m.mix_send_index
                            } else {
                                // mix_send_indices stored on elevated member, keyed by source ID
                                elevated_member
                                    .and_then(|e| e.mix_send_indices.get(&m.id()).copied())
                            };
                            si.map(|s| reaper_api::set_send_mute(&reaper_url, m.track_index, s, 1))
                        })
                        .collect();
                    urls.extend(mix_urls);
                }
            }

            let results =
                futures::future::join_all(urls.iter().map(|url| state.http_client.get(url).send()))
                    .await;
            let errors: Vec<_> = results.iter().filter(|r| r.is_err()).collect();
            if !errors.is_empty() {
                tracing::warn!(
                    error_count = errors.len(),
                    "Batch mute_all: some REAPER calls failed"
                );
                return Err((
                    StatusCode::BAD_GATEWAY,
                    Json(ApiError::new(
                        "REAPER_ERROR",
                        format!("{} of {} mute commands failed", errors.len(), results.len()),
                    )),
                ));
            }
        }
    }

    Ok(StatusCode::OK)
}

/// Query send state from REAPER (single HTTP call for all fields)
pub(crate) async fn query_send_state(
    client: &reqwest::Client,
    reaper_url: &str,
    track_index: usize,
    send_index: usize,
) -> Result<(f32, bool, f32), ()> {
    let url = reaper_api::get_send_state(reaper_url, track_index, send_index);
    let resp = client.get(&url).send().await.map_err(|_| ())?;
    let text = resp.text().await.map_err(|_| ())?;
    parse_send_state(&text).ok_or(())
}

/// Parse a REAPER SEND response for volume
/// Response format: SEND\ttrack\tsend\tflag\tVOLUME\tpan\tmode
#[cfg(test)]
fn parse_send_volume(text: &str) -> Option<f32> {
    for line in text.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.first() == Some(&"SEND")
            && parts.len() >= 5
            && let Ok(val) = parts[4].parse::<f32>()
        {
            return Some(val);
        }
    }
    None
}

/// Parse a REAPER SEND response for mute (flag at position 3)
/// Response format: SEND\ttrack\tsend\tMUTE\tvolume\tpan\tmode
/// Mute flag is a bitfield: bit 3 (value 8) = muted
#[cfg(test)]
fn parse_send_mute(text: &str) -> Option<bool> {
    for line in text.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.first() == Some(&"SEND")
            && parts.len() >= 4
            && let Ok(val) = parts[3].parse::<i32>()
        {
            return Some((val & 8) != 0);
        }
    }
    None
}

/// Parse a REAPER SEND response for pan
/// Response format: SEND\ttrack\tsend\tflag\tvolume\tPAN\tmode
#[cfg(test)]
fn parse_send_pan(text: &str) -> Option<f32> {
    for line in text.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.first() == Some(&"SEND")
            && parts.len() >= 6
            && let Ok(val) = parts[5].parse::<f32>()
        {
            return Some(val);
        }
    }
    None
}

/// Parse the destination track index from a REAPER SEND response.
/// Response format: SEND\ttrack\tsend\tflag\tvolume\tpan\tDESTINATION
/// Returns Some(destination) where negative values indicate hardware outputs (e.g. -1),
/// or None if no SEND line is found (meaning the send doesn't exist).
pub(crate) fn parse_send_destination(text: &str) -> Option<i32> {
    for line in text.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.first() == Some(&"SEND") && parts.len() >= 7 {
            return parts[6].parse().ok();
        }
    }
    None
}

/// Parse a full REAPER SEND response for vol, mute, and pan in one call
/// Response format: SEND\ttrack\tsend\tMUTE_FLAG\tVOLUME\tPAN\tmode
/// Mute flag is a bitfield: bit 3 (value 8) = muted
fn parse_send_state(text: &str) -> Option<(f32, bool, f32)> {
    for line in text.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.first() == Some(&"SEND") && parts.len() >= 6 {
            let vol = parts[4].parse::<f32>().ok()?;
            let mute_flag = parts[3].parse::<i32>().ok()?;
            let pan = parts[5].parse::<f32>().ok()?;
            return Some((vol, (mute_flag & 8) != 0, pan));
        }
    }
    None
}

/// Query the destination track of a specific send on a given track.
/// Returns Some(dest_track_index) or None if query fails.
pub(crate) async fn query_send_destination(
    client: &reqwest::Client,
    reaper_url: &str,
    track_index: usize,
    send_index: usize,
) -> Option<i32> {
    let url = reaper_api::get_send_state(reaper_url, track_index, send_index);
    let resp = client.get(&url).send().await.ok()?;
    let text = resp.text().await.ok()?;
    parse_send_destination(&text)
}

/// Parse a REAPER response value (format: "COMMAND\tVALUE")
/// Used for simple responses like NTRACK (only in tests now)
#[cfg(test)]
fn parse_reaper_value(text: &str) -> Option<f32> {
    for line in text.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 2
            && let Ok(val) = parts[1].parse::<f32>()
        {
            return Some(val);
        }
    }
    None
}

/// Build channel templates from config inputs with category and stereo info.
/// If `resolved_indices` is provided, track indices are looked up by name
/// (handling REAPER track insertions/reordering). Falls back to sequential i+1.
///
/// **The `i+1` fallback must stay symmetric with `collect_valid_input_indices`.**
/// During the startup window before the poller runs (or if REAPER is
/// unreachable), both functions apply the same fallback so the validator
/// and the client agree on indices — even though those fallback indices do
/// NOT address the right REAPER track for any input that isn't in its
/// natural 1..N position. See `collect_valid_input_indices` for the full
/// startup-window caveat.
pub(crate) fn build_channel_templates(
    inputs: &[iem_core::config::InputTrack],
    resolved_indices: Option<&HashMap<String, usize>>,
) -> Vec<iem_core::Channel> {
    inputs
        .iter()
        .enumerate()
        .map(|(i, input)| {
            let track_index = resolved_indices
                .and_then(|m| m.get(&input.name).copied())
                .unwrap_or(i + 1);
            // Prefer explicit config fields; fall back to name-based derivation for
            // configs that lack category/stereo_pair (REAPER-discovered tracks,
            // legacy configs).
            let (category, stereo_pair, stereo_side) = if let Some(cat) = &input.category {
                (
                    cat.clone(),
                    input.stereo_pair.clone(),
                    derive_stereo_side(&input.name),
                )
            } else {
                categorize_track(&input.name)
            };
            iem_core::Channel {
                track_index,
                name: input.name.clone(),
                level_db: 0.0,
                pan: 0.5,
                muted: false,
                category,
                stereo_pair,
                stereo_side,
            }
        })
        .collect()
}

/// Build channel templates for member mix monitoring (engineer or elevated members).
/// Each discovered member (except self and engineer) becomes a "mixes" channel
/// with track_index = member's inear track index.
pub(crate) fn build_mix_channel_templates(
    discovered: &[iem_core::DiscoveredMember],
    viewer_id: &str,
) -> Vec<iem_core::Channel> {
    discovered
        .iter()
        .filter(|m| m.id() != viewer_id && (viewer_id == "engineer" || m.id() != "engineer"))
        .map(|m| {
            let name = m
                .name
                .strip_suffix(" inear")
                .or_else(|| m.name.strip_suffix(" INEAR"))
                .unwrap_or(&m.name)
                .to_string();
            iem_core::Channel {
                track_index: m.track_index,
                name,
                level_db: 0.0,
                pan: 0.5,
                muted: true,
                category: "mixes".to_string(),
                stereo_pair: None,
                stereo_side: None,
            }
        })
        .collect()
}

/// Collect the set of REAPER track indices that represent valid input tracks.
///
/// Mirrors the index resolution used by `build_channel_templates`: each input
/// resolves to its REAPER-discovered track index (by name), falling back to
/// `position+1` when discovery has not yet populated the map.
///
/// This is the authoritative set for validating `track_index` arguments on
/// incoming WS commands. Callers must NOT use `inputs.len()` (a count, not a
/// membership set) — REAPER indices can exceed the input count when tracks
/// are added out-of-position (e.g. MEMBER7 kl at REAPER track 44 with only 23
/// input entries).
///
/// **Startup-window caveat.** The `position+1` fallback is intended only for
/// the brief window on process startup before the poller has had a chance
/// to query REAPER. During that window, validator and channel-builder share
/// the SAME fallback so they agree with each other — but the indices they
/// agree on are wrong (e.g. MEMBER7 kl would be "track 23" rather than "track
/// 44"). The poller overwrites the cache with real REAPER indices on its
/// first successful tick (~150 ms), and the app's job at that point is to
/// prefer "agree with client but address wrong REAPER track briefly" over
/// "reject everything". If the poller NEVER succeeds (REAPER unreachable),
/// the fallback persists and commands reach wrong REAPER tracks — that is
/// a pre-existing behaviour inherited from `build_channel_templates` and
/// is tracked separately from reaperiem#179.
pub(crate) fn collect_valid_input_indices(
    inputs: &[iem_core::config::InputTrack],
    resolved: &HashMap<String, usize>,
) -> std::collections::HashSet<usize> {
    inputs
        .iter()
        .enumerate()
        .map(|(i, input)| resolved.get(&input.name).copied().unwrap_or(i + 1))
        .collect()
}

/// Returns true when `ti` is a valid track index to mutate: either a
/// REAPER-resolved input track (from `valid_input_indices`) or an engineer
/// mix channel (from `mix_track_indices`). Separate function so it can be
/// unit-tested directly — both a deleted `!` in the REST validator and a
/// `||`→`&&` mutation in this OR-clause survived cargo-mutants until we
/// extracted it.
pub(crate) fn is_valid_track_index(
    ti: usize,
    valid_input_indices: &std::collections::HashSet<usize>,
    mix_track_indices: &[usize],
) -> bool {
    valid_input_indices.contains(&ti) || mix_track_indices.contains(&ti)
}

/// Reverse-lookup an input track's name from its REAPER index, falling back
/// to the 1-based config-position convention when the index is not yet
/// resolved by the poller.
///
/// **Single caller.** Used only by `set_send_level` for log-message
/// enrichment (the WS `SetLevel` handler logs its own track name via a
/// different path). It exists as a free function — rather than inlined
/// into that one caller — specifically so cargo-mutants can prove the
/// primary match (`**idx == track_index`) is exercised by a test: a
/// silent `==`→`!=` mutation would return a wrong name in logs, confusing
/// future incident response.
pub(crate) fn lookup_input_name(
    resolved: &HashMap<String, usize>,
    track_index: usize,
    fallback_inputs: &[iem_core::config::InputTrack],
) -> Option<String> {
    resolved
        .iter()
        .find(|(_, idx)| **idx == track_index)
        .map(|(name, _)| name.clone())
        .or_else(|| {
            fallback_inputs
                .get(track_index.saturating_sub(1))
                .map(|i| i.name.clone())
        })
}

/// Extract trailing " L" or " R" stereo-side suffix from a track name.
/// Returns None when no suffix is present.
pub(crate) fn derive_stereo_side(name: &str) -> Option<String> {
    if name.ends_with(" L") {
        Some("L".to_string())
    } else if name.ends_with(" R") {
        Some("R".to_string())
    } else {
        None
    }
}

/// Categorize a track by name
pub(crate) fn categorize_track(name: &str) -> (String, Option<String>, Option<String>) {
    let name_lower = name.to_lowercase();

    // Determine category — check hand/engineer BEFORE mic/gtr because
    // "HAND1 mic" contains both "hand" and "mic" but must be "tech"
    let category = if name_lower.contains("hand") || name_lower.contains("engineer") {
        "tech"
    } else if name_lower.contains("mic") || name_lower.contains("gtr") {
        "mics"
    } else {
        "stems"
    };

    let stereo_side = derive_stereo_side(name);
    let stereo_pair = stereo_side
        .as_ref()
        .map(|side| name.trim_end_matches(&format!(" {}", side)).to_lowercase());

    (category.to_string(), stereo_pair, stereo_side)
}

/// Validate track_index is a valid REAPER track index for an input track.
///
/// Must use the set of resolved REAPER indices (see
/// `collect_valid_input_indices`), NOT `inputs.len()` — see reaperiem#179.
fn validate_track_index(
    track_index: usize,
    valid_input_indices: &std::collections::HashSet<usize>,
) -> Result<(), (StatusCode, Json<ApiError>)> {
    if !valid_input_indices.contains(&track_index) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError::bad_request(&format!(
                "track_index {} is not a known input track",
                track_index
            ))),
        ));
    }
    Ok(())
}

/// Validate level_db is a finite number
fn validate_level_db(level_db: f32) -> Result<(), (StatusCode, Json<ApiError>)> {
    if level_db.is_nan() || level_db.is_infinite() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError::bad_request("level_db must be a finite number")),
        ));
    }
    Ok(())
}

/// Validate pan is between -1.0 and 1.0
fn validate_pan(pan: f32) -> Result<(), (StatusCode, Json<ApiError>)> {
    if !(-1.0..=1.0).contains(&pan) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError::bad_request("pan must be between -1.0 and 1.0")),
        ));
    }
    Ok(())
}

/// Set send level for a member's mix
pub async fn set_send_level(
    State(state): State<AppState>,
    Path((member_id, track_index)): Path<(String, usize)>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<SetLevelRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    // Lock acquisition order for this handler: config → discovered → cache.
    // Any writer that takes these locks in a different order risks a future
    // deadlock; keep the same ordering in set_send_pan / set_send_mute too.
    let config = state.config.read().await;
    crate::auth::verify_member_access(&headers, &member_id, &config.jwt_secret)?;

    // Use discovered members (REAPER source of truth)
    let discovered = state.discovered_members.read().await;
    let member_index = discovered
        .iter()
        .find(|m| m.id() == member_id)
        .map(|m| m.send_index)
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))))?;
    drop(discovered);

    // Validate inputs. Read the precomputed valid-input-index set from the
    // cache — populated by the poller (poller.rs) each time REAPER resolves
    // the input track list. O(1) membership test, no per-command allocation.
    let (valid, resolved) = {
        let cache = state.mixer_cache.read().await;
        (
            cache.valid_input_track_indices.clone(),
            cache.input_track_indices.clone(),
        )
    };
    validate_track_index(track_index, &valid)?;
    validate_level_db(payload.level_db)?;

    let reaper_url = config.reaper_url.clone();

    // Get track name for debugging: reverse-lookup by REAPER index, then
    // fall back to config-position (legacy 1:1 mapping).
    let track_name = lookup_input_name(&resolved, track_index, &config.inputs)
        .unwrap_or_else(|| "unknown".to_string());

    drop(config);

    // Convert dB to REAPER volume
    // REAPER uses linear amplitude where 1.0 = 0 dB
    let vol = db_to_reaper_vol(payload.level_db);

    // Build REAPER API URL for setting send volume
    let url = reaper_api::set_send_vol(&reaper_url, track_index, member_index, vol);

    tracing::info!(
        member_id = %member_id,
        member_index = member_index,
        track_index = track_index,
        track_name = %track_name,
        level_db = payload.level_db,
        reaper_vol = vol,
        url = %url,
        "LEVEL REQUEST"
    );

    // Call REAPER
    state.http_client.get(&url).send().await.map_err(|e| {
        tracing::error!(error = %e, "REAPER error");
        (
            StatusCode::BAD_GATEWAY,
            Json(ApiError::new("REAPER_ERROR", "REAPER unavailable")),
        )
    })?;

    Ok(StatusCode::OK)
}

/// Set send pan for a member's mix
pub async fn set_send_pan(
    State(state): State<AppState>,
    Path((member_id, track_index)): Path<(String, usize)>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<SetPanRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;
    crate::auth::verify_member_access(&headers, &member_id, &config.jwt_secret)?;

    // Use discovered members (REAPER source of truth)
    let discovered = state.discovered_members.read().await;
    let member_index = discovered
        .iter()
        .find(|m| m.id() == member_id)
        .map(|m| m.send_index)
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))))?;
    drop(discovered);

    // Validate inputs via the poller-maintained cache (see set_send_level).
    let valid = state
        .mixer_cache
        .read()
        .await
        .valid_input_track_indices
        .clone();
    validate_track_index(track_index, &valid)?;
    validate_pan(payload.pan)?;

    let reaper_url = config.reaper_url.clone();
    drop(config);

    // Convert UI pan (0.0-1.0) to REAPER pan (-1.0 to 1.0)
    let reaper_pan = ui_pan_to_reaper(payload.pan);
    let url = reaper_api::set_send_pan(&reaper_url, track_index, member_index, reaper_pan);

    tracing::debug!(
        url = %url,
        ui_pan = payload.pan,
        reaper_pan = reaper_pan,
        "Setting send pan"
    );

    state.http_client.get(&url).send().await.map_err(|e| {
        tracing::error!(error = %e, "REAPER pan error");
        (
            StatusCode::BAD_GATEWAY,
            Json(ApiError::new("REAPER_ERROR", "REAPER unavailable")),
        )
    })?;

    Ok(StatusCode::OK)
}

/// Set send mute for a member's mix
pub async fn set_send_mute(
    State(state): State<AppState>,
    Path((member_id, track_index)): Path<(String, usize)>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<SetMuteRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;
    crate::auth::verify_member_access(&headers, &member_id, &config.jwt_secret)?;

    // Use discovered members (REAPER source of truth)
    let discovered = state.discovered_members.read().await;
    let member_index = discovered
        .iter()
        .find(|m| m.id() == member_id)
        .map(|m| m.send_index)
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))))?;
    drop(discovered);

    // Validate track index via the poller-maintained cache (see set_send_level).
    let valid = state
        .mixer_cache
        .read()
        .await
        .valid_input_track_indices
        .clone();
    validate_track_index(track_index, &valid)?;

    let reaper_url = config.reaper_url.clone();

    // Get track name for debugging (owned String to avoid borrow issues)
    let track_name = config
        .inputs
        .get(track_index.saturating_sub(1))
        .map(|i| i.name.clone())
        .unwrap_or_else(|| "unknown".to_string());

    drop(config);

    let mute_val = if payload.muted { 1 } else { 0 };
    let url = reaper_api::set_send_mute(&reaper_url, track_index, member_index, mute_val);

    tracing::info!(
        member_id = %member_id,
        member_index = member_index,
        track_index = track_index,
        track_name = %track_name,
        muted = payload.muted,
        url = %url,
        "MUTE REQUEST"
    );

    state.http_client.get(&url).send().await.map_err(|e| {
        tracing::error!(error = %e, "REAPER mute error");
        (
            StatusCode::BAD_GATEWAY,
            Json(ApiError::new("REAPER_ERROR", "REAPER unavailable")),
        )
    })?;

    Ok(StatusCode::OK)
}

/// Request to set level
#[derive(Debug, serde::Deserialize)]
pub struct SetLevelRequest {
    pub level_db: f32,
}

/// Request to set pan
#[derive(Debug, serde::Deserialize)]
pub struct SetPanRequest {
    pub pan: f32,
}

/// Request to set mute
#[derive(Debug, serde::Deserialize)]
pub struct SetMuteRequest {
    pub muted: bool,
}

/// Convert dB to REAPER volume scale
///
/// REAPER uses standard linear amplitude:
/// - 0.0 = -inf dB (silence)
/// - 1.0 = 0 dB (unity)
/// - 2.0 ≈ +6 dB
pub(crate) fn db_to_reaper_vol(db: f32) -> f32 {
    if db <= -60.0 {
        0.0
    } else {
        10.0_f32.powf(db / 20.0).clamp(0.0, 4.0)
    }
}

/// Convert REAPER volume to dB
pub(crate) fn reaper_vol_to_db(vol: f32) -> f32 {
    if vol <= 0.0 {
        -60.0
    } else {
        20.0 * vol.log10()
    }
}

/// Quantize to 0.2 dB steps (matches UI quantization)
pub(crate) fn quantize_02(value: f32) -> f32 {
    (value * 5.0).round() / 5.0
}

/// Convert REAPER pan (-1.0 to 1.0) to UI pan (0.0 to 1.0)
///
/// REAPER uses: -1.0 = left, 0.0 = center, 1.0 = right
/// UI uses:     0.0 = left, 0.5 = center, 1.0 = right
/// NaN inputs from a malformed REAPER response are mapped to center (0.5).
pub(crate) fn reaper_pan_to_ui(reaper_pan: f32) -> f32 {
    if reaper_pan.is_nan() {
        return 0.5;
    }
    ((reaper_pan + 1.0) / 2.0).clamp(0.0, 1.0)
}

/// Convert UI pan (0.0 to 1.0) to REAPER pan (-1.0 to 1.0)
pub(crate) fn ui_pan_to_reaper(ui_pan: f32) -> f32 {
    ((ui_pan * 2.0) - 1.0).clamp(-1.0, 1.0)
}

/// Pan value the snapshot/preset RESTORE path must write to a REAPER send.
///
/// Snapshots and presets store pan in the UI range 0..1 (0.5 = center), because
/// the poller converts REAPER's -1..1 to 0..1 the moment it reads it
/// (`reaper_pan_to_ui`). REAPER's `SET .../SEND/{}/PAN/` write expects -1..1, so
/// the stored value MUST be converted back before writing. Bug reaperiem#203: the restore
/// path wrote the stored value RAW, mapping center (0.0 UI→REAPER) to 0.5 =
/// half-right and shifting every channel's panorama right.
pub(crate) fn restore_send_pan(stored_pan: f32) -> f32 {
    // reaperiem#203 fix: stored pan is UI-range 0..1; convert to REAPER -1..1 before writing.
    ui_pan_to_reaper(stored_pan)
}

/// Build the `(track_index, mix_send_index)` list of a member's mix channels.
///
/// Mirrors the discovery in `apply_command_to_cache` (the WS write path):
/// - engineer: every other member's inear track + its send TO engineer;
/// - elevated member (member1): every other member's inear track + the send
///   on it that routes TO this elevated member (`mix_send_indices[member]`);
/// - regular member: no mix channels.
///
/// The result feeds `resolve_send_index`, the single place that decides which
/// REAPER send a restore/write targets — so the "never hardcode send_index=0
/// for mix channels" rule is enforced once, for the WS path AND both REST
/// restore handlers.
pub(crate) fn compute_mix_members(
    discovered: &[iem_core::DiscoveredMember],
    member_id: &str,
) -> Vec<(usize, Option<usize>)> {
    let is_elevated = member_id == "member1";
    if member_id == "engineer" {
        discovered
            .iter()
            .filter(|m| m.id() != "engineer")
            .map(|m| (m.track_index, m.mix_send_index))
            .collect()
    } else if is_elevated {
        let elevated = discovered.iter().find(|m| m.id() == member_id);
        discovered
            .iter()
            .filter(|m| m.id() != member_id && m.id() != "engineer")
            .map(|m| {
                let si = elevated.and_then(|e| e.mix_send_indices.get(&m.id()).copied());
                (m.track_index, si)
            })
            .collect()
    } else {
        Vec::new()
    }
}

/// REAPER send index a write/restore must target for `track_idx`.
///
/// A mix channel (its `track_idx` appears in `mix_members`) uses its discovered
/// `mix_send_index` — NEVER a hardcoded 0 and NEVER the member's own send index
/// (bug reaperiem#204: both REST restore handlers used the member's `send_index` for
/// every track, writing mix channels to the wrong send). A missing
/// `mix_send_index` is a SAFETY error, never a silent fallback. A regular input
/// track uses the member's own `member_send_index`.
pub(crate) fn resolve_send_index(
    track_idx: usize,
    member_send_index: usize,
    mix_members: &[(usize, Option<usize>)],
) -> Result<usize, String> {
    if let Some((_, mix_si)) = mix_members.iter().find(|(track, _)| *track == track_idx) {
        // reaperiem#204 fix: a mix channel uses its discovered mix_send_index. A missing
        // one is a SAFETY error — never fall back to the member's own send.
        mix_si.ok_or_else(|| {
            format!(
                "SAFETY: No mix_send_index for track {} — cannot route to engineer",
                track_idx
            )
        })
    } else {
        Ok(member_send_index)
    }
}

/// Validate a pan value for SetPan commands.
/// Returns Err with a user-facing message if pan is NaN, infinite, or out of [-1.0, 1.0].
pub(crate) fn validate_pan_value(pan: f32) -> Result<(), String> {
    if !iem_core::is_valid_pan(pan) {
        return Err("pan must be between -1.0 and 1.0".to_string());
    }
    Ok(())
}

/// Build the alert catch-up message a WebSocket should send immediately
/// after connect, or `None` if no catch-up is needed.
///
/// - Engineer: receives `ActiveAlerts` listing every active alert across all
///   members, so the dashboard can restore pending SOS notifications.
/// - Non-engineer member: receives `EngineerAlert` for their own member_id
///   if and only if the server still holds an active alert for them. This
///   is the fix for reaperiem#150 — without it, reloading the page after triggering
///   SOS leaves the member's UI idle while the server's `active_alerts`
///   cache keeps the alert, and the next `CallEngineer` click no-ops in the
///   `cache.active_alerts.contains_key(...)` short-circuit so the button
///   can never return to active.
pub(crate) fn build_alert_catchup(
    active_alerts: &std::collections::HashMap<String, (String, String)>,
    member_id: &str,
) -> Option<iem_core::ServerMsg> {
    if member_id == "engineer" {
        if active_alerts.is_empty() {
            return None;
        }
        let alerts: Vec<iem_core::AlertInfo> = active_alerts
            .values()
            .map(|(from_member, from_name)| iem_core::AlertInfo {
                from_member: from_member.clone(),
                from_name: from_name.clone(),
            })
            .collect();
        Some(iem_core::ServerMsg::ActiveAlerts { alerts })
    } else if active_alerts.contains_key(member_id) {
        Some(iem_core::ServerMsg::EngineerAlert {
            from_member: member_id.to_string(),
            from_name: String::new(),
        })
    } else {
        None
    }
}

// =============================================================================
// WebSocket handler
// =============================================================================

/// WebSocket mixer endpoint - upgrades HTTP to WebSocket
/// Requires token query param for authentication: /ws/{member_id}?token=<JWT>
pub async fn ws_mixer(
    ws: axum::extract::ws::WebSocketUpgrade,
    State(state): State<AppState>,
    Path(member_id): Path<String>,
    Query(query): Query<WsQuery>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;

    // Token is required for WebSocket connections
    let token = query.token.as_deref().ok_or_else(|| {
        tracing::warn!(member = %member_id, "WS connection without token");
        (StatusCode::UNAUTHORIZED, Json(ApiError::unauthorized()))
    })?;

    // Validate token and check expiration
    let claims = crate::auth::extract_claims(token, &config.jwt_secret).ok_or_else(|| {
        tracing::warn!(member = %member_id, "WS connection with invalid token");
        (StatusCode::UNAUTHORIZED, Json(ApiError::unauthorized()))
    })?;

    // Check token expiration
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    if claims.exp < now {
        tracing::warn!(member = %member_id, "WS connection with expired token");
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiError::new("TOKEN_EXPIRED", "Token has expired")),
        ));
    }

    // Verify member access: member can only connect to own mixer, engineer can access any
    if !claims.engineer && claims.sub != member_id {
        tracing::warn!(
            member = %member_id,
            token_sub = %claims.sub,
            "WS connection denied: cross-member access"
        );
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiError::new(
                "FORBIDDEN",
                "Access denied to this member's mixer",
            )),
        ));
    }

    // Detect network mode before dropping config (needs local_public_ip)
    let network_mode = crate::routes::detect_network_mode(&headers, &config.local_public_ip);

    drop(config);

    // Validate member exists (from REAPER discovered members)
    let discovered = state.discovered_members.read().await;
    let member_exists = discovered.iter().any(|m| m.id() == member_id);
    drop(discovered);
    if !member_exists {
        return Err((StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))));
    }

    let is_engineer = claims.engineer;
    Ok(ws.on_upgrade(move |socket| handle_ws(socket, state, member_id, network_mode, is_engineer)))
}

/// Handle a WebSocket connection for a member
async fn handle_ws(
    mut socket: axum::extract::ws::WebSocket,
    state: AppState,
    member_id: String,
    network_mode: String,
    is_engineer: bool,
) {
    use axum::extract::ws::Message;
    use iem_core::{ClientMsg, ServerMsg};

    tracing::info!(member_id = %member_id, network_mode = %network_mode, "WebSocket connected");

    // Register this member as active (ref-counted for multi-tab support)
    {
        let mut cache = state.mixer_cache.write().await;
        *cache.active_members.entry(member_id.clone()).or_insert(0) += 1;
    }

    // Subscribe to broadcast channel
    let mut rx = state.event_tx.subscribe();

    // Send initial full state
    if let Ok(initial_state) = build_full_state(&state, &member_id).await {
        let json = serde_json::to_string(&initial_state).unwrap_or_default();
        let _ = socket.send(Message::Text(json.into())).await;
    }

    // Send initial customization state
    {
        let cust = state.customization_store.load(&member_id);
        let msg = ServerMsg::CustomizationUpdate {
            pinned: cust.pinned,
            hidden: cust.hidden,
        };
        let json = serde_json::to_string(&msg).unwrap_or_default();
        let _ = socket.send(Message::Text(json.into())).await;
    }

    // Send initial solo state (if any active solos for this member)
    {
        let cache = state.mixer_cache.read().await;
        if let Some(soloed) = cache.solo_states.get(&member_id) {
            let msg = ServerMsg::SoloUpdate {
                soloed: soloed.clone(),
            };
            let json = serde_json::to_string(&msg).unwrap_or_default();
            let _ = socket.send(Message::Text(json.into())).await;
        }
    }

    // Send alert catch-up on WS connect. Engineer gets all active alerts,
    // non-engineer members get only their own (so their UI state survives a
    // page reload — see reaperiem#150 for the stuck-idle bug this fixes).
    {
        let cache = state.mixer_cache.read().await;
        if let Some(msg) = build_alert_catchup(&cache.active_alerts, &member_id) {
            let json = serde_json::to_string(&msg).unwrap_or_default();
            let _ = socket.send(Message::Text(json.into())).await;
        }
    }

    // Send network mode (local/remote) — updates on every WS reconnect,
    // so switching between WiFi and mobile data triggers a fresh detection
    {
        let msg = ServerMsg::NetworkMode { mode: network_mode };
        let json = serde_json::to_string(&msg).unwrap_or_default();
        let _ = socket.send(Message::Text(json.into())).await;
    }

    // Send internet-access (Cloudflare tunnel) status (reaperiem#202); later changes
    // arrive through the broadcast channel.
    {
        let msg = crate::tunnel_watch::current_status_msg(&state).await;
        let json = serde_json::to_string(&msg).unwrap_or_default();
        let _ = socket.send(Message::Text(json.into())).await;
    }

    loop {
        tokio::select! {
            // Client → Server: process commands
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(cmd) = serde_json::from_str::<ClientMsg>(&text) {
                            // Handle customization updates locally (no REAPER command)
                            if let ClientMsg::UpdateCustomization { ref pinned, ref hidden } = cmd {
                                let cust = iem_core::Customization {
                                    pinned: pinned.clone(),
                                    hidden: hidden.clone(),
                                };
                                if let Err(e) = state.customization_store.save(&member_id, &cust) {
                                    tracing::error!(member_id = %member_id, "Failed to save customization: {}", e);
                                }
                                // Broadcast to other tabs of same member
                                let _ = state.event_tx.send((
                                    member_id.clone(),
                                    ServerMsg::CustomizationUpdate {
                                        pinned: pinned.clone(),
                                        hidden: hidden.clone(),
                                    },
                                ));
                                continue;
                            }

                            // Handle EQ commands (async EXTSTATE + ReaScript flow)
                            if let ClientMsg::GetEqParams { track_index } = cmd {
                                let state_clone = state.clone();
                                let member_clone = member_id.clone();
                                tokio::spawn(async move {
                                    if let Some(eq_msg) = handle_get_eq_params(&state_clone, track_index).await {
                                        let _ = state_clone.event_tx.send((member_clone, eq_msg));
                                    }
                                });
                                continue;
                            }
                            if let ClientMsg::GetEqParamsMulti { ref track_indices } = cmd {
                                let state_clone = state.clone();
                                let member_clone = member_id.clone();
                                let indices = track_indices.clone();
                                tokio::spawn(async move {
                                    let mut bands_map = std::collections::HashMap::new();
                                    for ti in indices {
                                        if let Some(iem_core::ServerMsg::EqParams { track_index, bands, .. }) =
                                            handle_get_eq_params(&state_clone, ti).await
                                        {
                                            bands_map.insert(track_index, bands);
                                        }
                                    }
                                    let msg = iem_core::ServerMsg::EqParamsMulti { bands: bands_map };
                                    let _ = state_clone.event_tx.send((member_clone, msg));
                                });
                                continue;
                            }
                            if let ClientMsg::SetEqBand { track_index, band, ref param, value } = cmd {
                                let state_clone = state.clone();
                                let param_clone = param.clone();
                                tokio::spawn(async move {
                                    handle_set_eq_band(&state_clone, track_index, band, &param_clone, value).await;
                                    // Don't broadcast EqParams after SetEqBand — the client
                                    // already has optimistic local state from the drag.
                                    // Broadcasting causes server echo → reactive_graph recursive
                                    // closure panic in the frontend's sync Effect.
                                });
                                continue;
                            }

                            // Handle Limiter commands (async EXTSTATE + ReaScript flow) (reaperiem#72)

                            // Limiter ownership check: non-engineer members can only
                            // control the limiter on their own output track (reaperiem#156).
                            let owns_limiter_track = |track_index: usize| -> bool {
                                // Use try_read to avoid holding the lock across an await point.
                                match state.mixer_cache.try_read() {
                                    Ok(cache) => check_owns_limiter_track(
                                        is_engineer,
                                        &member_id,
                                        &cache.output_track_indices,
                                        track_index,
                                    ),
                                    Err(_) => false, // Lock contended — deny conservatively
                                }
                            };

                            if let iem_core::ClientMsg::GetLimiterParams { track_index } = cmd {
                                if owns_limiter_track(track_index) {
                                    let state_clone = state.clone();
                                    let member_clone = member_id.clone();
                                    tokio::spawn(async move {
                                        if let Some(lim_msg) =
                                            handle_get_limiter_params(&state_clone, track_index)
                                                .await
                                        {
                                            let _ =
                                                state_clone.event_tx.send((member_clone, lim_msg));
                                        }
                                    });
                                }
                                continue;
                            }
                            if let iem_core::ClientMsg::SetLimiterParam {
                                track_index,
                                ref param,
                                value,
                            } = cmd
                            {
                                if owns_limiter_track(track_index) {
                                    let state_clone = state.clone();
                                    let param_clone = param.clone();
                                    tokio::spawn(async move {
                                        handle_set_limiter_param(
                                            &state_clone,
                                            track_index,
                                            &param_clone,
                                            value,
                                        )
                                        .await;
                                    });
                                }
                                continue;
                            }
                            if let iem_core::ClientMsg::SetLimiterEnabled {
                                track_index,
                                enabled,
                            } = cmd
                            {
                                if owns_limiter_track(track_index) {
                                    let state_clone = state.clone();
                                    tokio::spawn(async move {
                                        handle_set_limiter_param(
                                            &state_clone,
                                            track_index,
                                            "enabled",
                                            if enabled { 1.0 } else { 0.0 },
                                        )
                                        .await;
                                    });
                                }
                                continue;
                            }
                            if let iem_core::ClientMsg::ResetLimiterActivity { track_index } = cmd
                            {
                                if owns_limiter_track(track_index) {
                                    let state_clone = state.clone();
                                    tokio::spawn(async move {
                                        handle_reset_limiter_activity(&state_clone, track_index)
                                            .await;
                                    });
                                }
                                continue;
                            }

                            // Handle band member alert to engineer (reaperiem#125)
                            if let ClientMsg::CallEngineer = cmd {
                                // No-op if alert already active for this member
                                let cache = state.mixer_cache.read().await;
                                if cache.active_alerts.contains_key(&member_id) {
                                    drop(cache);
                                    continue;
                                }
                                drop(cache);

                                // Look up member display name
                                let discovered = state.discovered_members.read().await;
                                let display_name = discovered
                                    .iter()
                                    .find(|m| m.id() == member_id)
                                    .map(|m| m.name.clone())
                                    .unwrap_or_else(|| member_id.clone());
                                drop(discovered);

                                // Store active alert
                                let mut cache = state.mixer_cache.write().await;
                                cache.active_alerts.insert(
                                    member_id.clone(),
                                    (member_id.clone(), display_name.clone()),
                                );
                                drop(cache);

                                // Broadcast to all engineer devices
                                let _ = state.event_tx.send((
                                    "engineer".to_string(),
                                    ServerMsg::EngineerAlert {
                                        from_member: member_id.clone(),
                                        from_name: display_name.clone(),
                                    },
                                ));
                                // Also notify the member their alert is active
                                let _ = state.event_tx.send((
                                    member_id.clone(),
                                    ServerMsg::EngineerAlert {
                                        from_member: member_id.clone(),
                                        from_name: String::new(),
                                    },
                                ));
                                // Send Web Push to all engineer devices (fire-and-forget) (reaperiem#133)
                                {
                                    let push_store = state.push_store.clone();
                                    let http_client = state.http_client.clone();
                                    let (vapid_key, vapid_subject) = {
                                        let config = state.config.read().await;
                                        (config.vapid_private_key.clone(), config.vapid_subject.clone())
                                    };
                                    let push_name = display_name.clone();
                                    let push_member = member_id.clone();
                                    if !vapid_key.is_empty() {
                                        tokio::spawn(async move {
                                            let payload = serde_json::json!({
                                                "type": "SOS",
                                                "name": push_name,
                                                "member": push_member,
                                            });
                                            crate::push::send_push_to_engineers(
                                                &http_client,
                                                &vapid_key,
                                                &vapid_subject,
                                                &push_store,
                                                payload.to_string().as_bytes(),
                                            )
                                            .await;
                                        });
                                    }
                                }
                                continue;
                            }

                            // Handle alert clear (from engineer or member)
                            if let ClientMsg::ClearAlert = cmd {
                                let mut cache = state.mixer_cache.write().await;
                                let cleared_members = if member_id == "engineer" {
                                    let keys: Vec<String> =
                                        cache.active_alerts.keys().cloned().collect();
                                    cache.active_alerts.clear();
                                    keys
                                } else {
                                    cache.active_alerts.remove(&member_id);
                                    vec![member_id.clone()]
                                };
                                drop(cache);

                                for cleared in &cleared_members {
                                    let _ = state.event_tx.send((
                                        "engineer".to_string(),
                                        ServerMsg::AlertCleared {
                                            member_id: cleared.clone(),
                                        },
                                    ));
                                    let _ = state.event_tx.send((
                                        cleared.clone(),
                                        ServerMsg::AlertCleared {
                                            member_id: cleared.clone(),
                                        },
                                    ));
                                }
                                continue;
                            }

                            // Handle talkback lock (reaperiem#123)
                            #[cfg(feature = "audio")]
                            if let ClientMsg::TalkStart = cmd {
                                let mut tb = state.talkback_state.write().await;
                                if tb.active_talker.is_none() {
                                    tb.active_talker = Some(member_id.clone());
                                    drop(tb);
                                    let _ = state.event_tx.send((
                                        "engineer".to_string(),
                                        ServerMsg::TalkAcquired,
                                    ));
                                    // Notify ALL band members (red page overlay)
                                    let _ = state.event_tx.send((
                                        String::new(),
                                        ServerMsg::EngineerTalking { active: true },
                                    ));
                                } else {
                                    let holder =
                                        tb.active_talker.clone().unwrap_or_default();
                                    drop(tb);
                                    let json = serde_json::to_string(
                                        &ServerMsg::TalkBusy { holder },
                                    )
                                    .unwrap_or_default();
                                    let _ =
                                        socket.send(Message::Text(json.into())).await;
                                }
                                continue;
                            }

                            #[cfg(feature = "audio")]
                            if let ClientMsg::TalkStop = cmd {
                                let mut tb = state.talkback_state.write().await;
                                if tb.active_talker.as_deref() == Some(&member_id) {
                                    tb.active_talker = None;
                                    drop(tb);
                                    let _ = state.event_tx.send((
                                        "engineer".to_string(),
                                        ServerMsg::TalkReleased,
                                    ));
                                    // Notify ALL band members (remove red overlay)
                                    let _ = state.event_tx.send((
                                        String::new(),
                                        ServerMsg::EngineerTalking { active: false },
                                    ));
                                } else {
                                    drop(tb);
                                }
                                continue;
                            }

                            match apply_command_to_cache(&state, &member_id, &cmd).await {
                                Ok((url, broadcast)) => {
                                    // Broadcast to other clients of same member for cross-device sync
                                    if let Some(event) = broadcast {
                                        let _ = state.event_tx.send((member_id.clone(), event));
                                    }
                                    if !url.is_empty() {
                                        send_to_reaper(
                                            state.http_client.clone(),
                                            url,
                                            member_id.clone(),
                                            cmd,
                                        );
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        member_id = %member_id,
                                        error = %e,
                                        "WS command failed"
                                    );
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {} // Ignore ping/pong/binary
                }
            }
            // Server → Client: forward relevant broadcasts
            event = rx.recv() => {
                match event {
                    Ok((mid, server_msg)) => {
                        // Send meters and connection changes to all;
                        // send state/channel/global updates only to the relevant member
                        let should_send = match &server_msg {
                            ServerMsg::Meters { .. } => true,
                            ServerMsg::ConnectionChanged { .. } => true,
                            _ => mid == member_id || mid.is_empty(),
                        };
                        if should_send {
                            let json = serde_json::to_string(&server_msg).unwrap_or_default();
                            if socket.send(Message::Text(json.into())).await.is_err() {
                                break; // Client disconnected
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(member_id = %member_id, skipped = n, "WS broadcast lagged");
                        // Continue - we'll get the next update
                    }
                    Err(_) => break, // Channel closed
                }
            }
        }
    }

    tracing::info!(member_id = %member_id, "WebSocket disconnected");

    // Release talkback lock if this engineer held it (reaperiem#123)
    #[cfg(feature = "audio")]
    {
        let mut tb = state.talkback_state.write().await;
        if tb.active_talker.as_deref() == Some(&member_id) {
            tb.active_talker = None;
            drop(tb);
            let _ = state
                .event_tx
                .send(("engineer".to_string(), ServerMsg::TalkReleased));
            let _ = state
                .event_tx
                .send((String::new(), ServerMsg::EngineerTalking { active: false }));
        }
    }

    // Cleanup: decrement ref count, remove only when no tabs remain
    let mut cache = state.mixer_cache.write().await;
    if let Some(count) = cache.active_members.get_mut(&member_id) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            cache.active_members.remove(&member_id);
            cache.member_states.remove(&member_id);
        }
    }
}

/// Build full state message for initial WebSocket connection
async fn build_full_state(state: &AppState, member_id: &str) -> Result<iem_core::ServerMsg, ()> {
    // Get member send index from discovered members (REAPER source of truth)
    let discovered = state.discovered_members.read().await;
    let member_index = discovered
        .iter()
        .find(|m| m.id() == member_id)
        .map(|m| m.send_index)
        .ok_or(())?;
    drop(discovered);

    let config = state.config.read().await;
    let reaper_url = config.reaper_url.clone();
    let resolved = state.mixer_cache.read().await.input_track_indices.clone();
    let resolved_ref = if resolved.is_empty() {
        None
    } else {
        Some(&resolved)
    };
    let channels = build_channel_templates(&config.inputs, resolved_ref);
    drop(config);

    // Query all send states in parallel
    let send_futures: Vec<_> = channels
        .iter()
        .map(|ch| {
            let client = state.http_client.clone();
            let url = reaper_url.clone();
            let track_index = ch.track_index;
            async move {
                let result = query_send_state(&client, &url, track_index, member_index).await;
                (track_index, result)
            }
        })
        .collect();

    let send_results = futures::future::join_all(send_futures).await;
    let mut result_channels = channels;
    let mut connected = false;

    for (track_index, result) in send_results {
        if let Ok((level, mute, pan)) = result {
            if let Some(ch) = result_channels
                .iter_mut()
                .find(|c| c.track_index == track_index)
            {
                ch.level_db = reaper_vol_to_db(level);
                ch.muted = mute;
                ch.pan = reaper_pan_to_ui(pan);
            }
            connected = true;
        }
    }

    // For engineer or elevated members: append mix channels
    let is_elevated = member_id == "member1";
    if member_id == "engineer" || is_elevated {
        let discovered = state.discovered_members.read().await;
        let mut mix_channels = build_mix_channel_templates(&discovered, member_id);

        // Look up the correct send index for each mix channel:
        // - Engineer: use mix_send_index (sends from member inear TO engineer inear)
        // - Elevated: use mix_send_indices[member_id] (sends from other inear TO this member's inear)
        let mix_futures: Vec<_> = mix_channels
            .iter()
            .filter_map(|ch| {
                let mix_si = if member_id == "engineer" {
                    discovered
                        .iter()
                        .find(|m| m.track_index == ch.track_index)
                        .and_then(|m| m.mix_send_index)
                } else {
                    // For elevated member: mix_send_indices are stored on the
                    // elevated member, keyed by source member ID.
                    // Find source member's ID from track_index, then look up
                    // the send index on the elevated member.
                    let source_id = discovered
                        .iter()
                        .find(|m| m.track_index == ch.track_index)
                        .map(|m| m.id());
                    let elevated = discovered.iter().find(|m| m.id() == member_id);
                    source_id.and_then(|sid| {
                        elevated.and_then(|e| e.mix_send_indices.get(&sid).copied())
                    })
                }?;
                let client = state.http_client.clone();
                let url = reaper_url.clone();
                let track_index = ch.track_index;
                Some(async move {
                    let result = query_send_state(&client, &url, track_index, mix_si).await;
                    (track_index, result)
                })
            })
            .collect();
        drop(discovered);

        let mix_results = futures::future::join_all(mix_futures).await;
        for (track_index, result) in mix_results {
            if let Ok((level, mute, pan)) = result {
                if let Some(ch) = mix_channels
                    .iter_mut()
                    .find(|c| c.track_index == track_index)
                {
                    ch.level_db = reaper_vol_to_db(level);
                    ch.muted = mute;
                    ch.pan = reaper_pan_to_ui(pan);
                }
                connected = true;
            }
        }

        result_channels.extend(mix_channels);
    }

    // Read cached global volume and output track index for this member
    let cache = state.mixer_cache.read().await;
    let (global_level_db, global_muted) = match cache.global_volumes.get(member_id) {
        Some(gv) => (Some(gv.level_db), Some(gv.muted)),
        None => (None, None),
    };
    let output_track_index = cache.output_track_indices.get(member_id).copied();
    let (stems_level_db, stems_muted) = match cache.stems_volumes.get(member_id) {
        Some(sv) => (Some(sv.level_db), Some(sv.muted)),
        None => (None, None),
    };
    let stems_bus_index = cache.stems_bus_indices.get(member_id).copied();
    drop(cache);

    Ok(iem_core::ServerMsg::State {
        channels: result_channels,
        connected,
        global_level_db,
        global_muted,
        output_track_index,
        stems_level_db,
        stems_muted,
        stems_bus_index,
    })
}

/// Apply a client command to the local cache and record a command timestamp.
/// This prevents the poller from broadcasting echo updates for recently-commanded channels.
/// Returns the REAPER HTTP URL to send, or an error.
async fn apply_command_to_cache(
    state: &AppState,
    member_id: &str,
    cmd: &iem_core::ClientMsg,
) -> Result<(String, Option<iem_core::ServerMsg>), String> {
    // Get member send index from discovered members (REAPER source of truth)
    let discovered = state.discovered_members.read().await;
    let member_index = discovered
        .iter()
        .find(|m| m.id() == member_id)
        .map(|m| m.send_index)
        .ok_or_else(|| "Unknown member".to_string())?;
    // Collect mix channel track indices and their send_index for validation
    // (shared with both REST restore handlers via compute_mix_members).
    let mix_members = compute_mix_members(&discovered, member_id);
    let mix_track_indices: Vec<usize> = mix_members.iter().map(|(ti, _)| *ti).collect();
    drop(discovered);

    let config = state.config.read().await;
    let reaper_url = config.reaper_url.clone();
    drop(config);

    // Read the precomputed valid-input-index set from the cache. The poller
    // (re)populates it whenever REAPER resolves the input tracks (see
    // poller.rs), so per-command validation is O(1) without per-call rebuild.
    // Note: config.read was dropped above before touching mixer_cache — hold
    // at most one of (config, mixer_cache) at a time to avoid lock-inversion
    // hazards with future writers.
    let valid_input_indices = state
        .mixer_cache
        .read()
        .await
        .valid_input_track_indices
        .clone();

    // Helper: check if a track_index is valid (input track OR engineer mix channel)
    let is_valid_track =
        |ti: usize| -> bool { is_valid_track_index(ti, &valid_input_indices, &mix_track_indices) };

    // Helper: determine REAPER send_index for a given track_index via the shared
    // resolver (mix channels use their discovered mix_send_index, never 0). reaperiem#204
    let send_index_for =
        |ti: usize| -> Result<usize, String> { resolve_send_index(ti, member_index, &mix_members) };

    // Validate incoming WS command values
    match cmd {
        iem_core::ClientMsg::SetLevel {
            track_index,
            level_db,
        } => {
            if !is_valid_track(*track_index) {
                return Err(format!("track_index {} out of range", track_index));
            }
            if level_db.is_nan() || level_db.is_infinite() {
                return Err("level_db must be finite".to_string());
            }
        }
        iem_core::ClientMsg::SetPan { track_index, pan } => {
            if !is_valid_track(*track_index) {
                return Err(format!("track_index {} out of range", track_index));
            }
            validate_pan_value(*pan)?;
        }
        iem_core::ClientMsg::SetMute { track_index, .. } => {
            if !is_valid_track(*track_index) {
                return Err(format!("track_index {} out of range", track_index));
            }
        }
        iem_core::ClientMsg::SetGlobalLevel { level_db } => {
            if level_db.is_nan() || level_db.is_infinite() {
                return Err("level_db must be finite".to_string());
            }
        }
        iem_core::ClientMsg::SetGlobalMute { .. } => {}
        iem_core::ClientMsg::SetStemsLevel { level_db } => {
            if level_db.is_nan() || level_db.is_infinite() {
                return Err("level_db must be finite".to_string());
            }
        }
        iem_core::ClientMsg::SetStemsMute { .. } => {}
        iem_core::ClientMsg::UpdateCustomization { .. } => {
            // Handled in WS handler before apply_command_to_cache is called
            return Err("UpdateCustomization should not reach apply_command_to_cache".to_string());
        }
        iem_core::ClientMsg::SetSolo { soloed } => {
            for ti in soloed {
                if !is_valid_track(*ti) {
                    return Err(format!("solo track_index {} out of range", ti));
                }
            }
        }
        iem_core::ClientMsg::CallEngineer => {
            // Handled in WS handler before apply_command_to_cache is called
            return Err("CallEngineer should not reach apply_command_to_cache".to_string());
        }
        iem_core::ClientMsg::ClearAlert => {
            return Err("ClearAlert should not reach apply_command_to_cache".to_string());
        }
        iem_core::ClientMsg::TalkStart | iem_core::ClientMsg::TalkStop => {
            return Err("Talk commands handled before apply_command_to_cache".to_string());
        }
        iem_core::ClientMsg::ListenStart { .. } | iem_core::ClientMsg::ListenStop => {
            // Audio commands are handled by ws_audio, not the mixer WS
            return Err("Audio commands should use /ws/audio endpoint".to_string());
        }
        iem_core::ClientMsg::GetEqParams { .. }
        | iem_core::ClientMsg::SetEqBand { .. }
        | iem_core::ClientMsg::GetEqParamsMulti { .. } => {
            // EQ commands are handled in WS handler before apply_command_to_cache is called
            return Err("EQ commands should not reach apply_command_to_cache".to_string());
        }
        iem_core::ClientMsg::GetLimiterParams { .. }
        | iem_core::ClientMsg::SetLimiterParam { .. }
        | iem_core::ClientMsg::SetLimiterEnabled { .. }
        | iem_core::ClientMsg::ResetLimiterActivity { .. } => {
            return Err("Limiter commands handled before apply_command_to_cache".to_string());
        }
    }

    let (url, track_index, broadcast) = match cmd {
        iem_core::ClientMsg::SetLevel {
            track_index,
            level_db,
        } => {
            let vol = db_to_reaper_vol(*level_db);
            // Pre-write cache with quantized roundtrip value (eliminates f32 artifacts)
            let cached_db = quantize_02(reaper_vol_to_db(vol));
            let mut cache = state.mixer_cache.write().await;
            let mut event = None;
            if let Some(channels) = cache.member_states.get_mut(member_id)
                && let Some(ch) = channels.iter_mut().find(|c| c.track_index == *track_index)
            {
                ch.level_db = cached_db;
                event = Some(iem_core::ServerMsg::ChannelUpdate {
                    track_index: *track_index,
                    level_db: cached_db,
                    muted: ch.muted,
                    pan: ch.pan,
                });
            }
            cache.command_timestamps.insert(
                (member_id.to_string(), *track_index),
                std::time::Instant::now(),
            );
            drop(cache);
            let si = send_index_for(*track_index)?;
            (
                reaper_api::set_send_vol(&reaper_url, *track_index, si, vol),
                *track_index,
                event,
            )
        }
        iem_core::ClientMsg::SetMute { track_index, muted } => {
            let mute_val: u8 = if *muted { 1 } else { 0 };
            let mut cache = state.mixer_cache.write().await;
            let mut event = None;
            if let Some(channels) = cache.member_states.get_mut(member_id)
                && let Some(ch) = channels.iter_mut().find(|c| c.track_index == *track_index)
            {
                ch.muted = *muted;
                event = Some(iem_core::ServerMsg::ChannelUpdate {
                    track_index: *track_index,
                    level_db: ch.level_db,
                    muted: *muted,
                    pan: ch.pan,
                });
            }
            cache.command_timestamps.insert(
                (member_id.to_string(), *track_index),
                std::time::Instant::now(),
            );
            drop(cache);
            let si = send_index_for(*track_index)?;
            (
                reaper_api::set_send_mute(&reaper_url, *track_index, si, mute_val),
                *track_index,
                event,
            )
        }
        iem_core::ClientMsg::SetPan { track_index, pan } => {
            let reaper_pan = ui_pan_to_reaper(*pan);
            let cached_pan = reaper_pan_to_ui(reaper_pan);
            let mut cache = state.mixer_cache.write().await;
            let mut event = None;
            if let Some(channels) = cache.member_states.get_mut(member_id)
                && let Some(ch) = channels.iter_mut().find(|c| c.track_index == *track_index)
            {
                ch.pan = cached_pan;
                event = Some(iem_core::ServerMsg::ChannelUpdate {
                    track_index: *track_index,
                    level_db: ch.level_db,
                    muted: ch.muted,
                    pan: cached_pan,
                });
            }
            cache.command_timestamps.insert(
                (member_id.to_string(), *track_index),
                std::time::Instant::now(),
            );
            drop(cache);
            let si = send_index_for(*track_index)?;
            (
                reaper_api::set_send_pan(&reaper_url, *track_index, si, reaper_pan),
                *track_index,
                event,
            )
        }
        iem_core::ClientMsg::SetGlobalLevel { level_db } => {
            let vol = db_to_reaper_vol(*level_db);
            let cached_db = reaper_vol_to_db(vol);
            let mut cache = state.mixer_cache.write().await;
            let output_track = match cache.output_track_indices.get(member_id) {
                Some(&idx) => idx,
                None => {
                    drop(cache);
                    return Err("Output track not yet discovered".to_string());
                }
            };
            let current_muted = cache
                .global_volumes
                .get(member_id)
                .map(|gv| gv.muted)
                .unwrap_or(false);
            if let Some(gv) = cache.global_volumes.get_mut(member_id) {
                gv.level_db = cached_db;
            } else {
                cache.global_volumes.insert(
                    member_id.to_string(),
                    crate::GlobalVolState {
                        level_db: cached_db,
                        muted: false,
                    },
                );
            }
            // Use output track index as key for echo suppression (offset to avoid collision with send tracks)
            cache.command_timestamps.insert(
                (member_id.to_string(), output_track + 100000),
                std::time::Instant::now(),
            );
            drop(cache);
            (
                reaper_api::set_track_vol(&reaper_url, output_track, vol),
                output_track,
                Some(iem_core::ServerMsg::GlobalVolumeUpdate {
                    level_db: cached_db,
                    muted: current_muted,
                }),
            )
        }
        iem_core::ClientMsg::UpdateCustomization { .. } => {
            unreachable!("UpdateCustomization handled before apply_command_to_cache")
        }
        iem_core::ClientMsg::SetSolo { soloed } => {
            let soloed_set: std::collections::HashSet<usize> = soloed.iter().copied().collect();
            let mut cache = state.mixer_cache.write().await;
            let current_solo = cache
                .solo_states
                .get(member_id)
                .cloned()
                .unwrap_or_default();
            let had_solo = !current_solo.is_empty();
            let wants_solo = !soloed_set.is_empty();

            let mut reaper_urls: Vec<String> = Vec::new();
            let mut events: Vec<iem_core::ServerMsg> = Vec::new();

            if wants_solo && !had_solo {
                // ENTERING SOLO: save current mute states, then mute everything except soloed
                let mut saved = Vec::new();
                if let Some(channels) = cache.member_states.get(member_id) {
                    for ch in channels {
                        if let Ok(si) = send_index_for(ch.track_index) {
                            saved.push((ch.track_index, si, ch.muted));
                        }
                    }
                }
                cache.pre_solo_mutes.insert(member_id.to_string(), saved);

                if let Some(channels) = cache.member_states.get_mut(member_id) {
                    for ch in channels.iter_mut() {
                        let should_mute = !soloed_set.contains(&ch.track_index);
                        if ch.muted != should_mute {
                            ch.muted = should_mute;
                            if let Ok(si) = send_index_for(ch.track_index) {
                                let mute_val: u8 = if should_mute { 1 } else { 0 };
                                reaper_urls.push(reaper_api::set_send_mute(
                                    &reaper_url,
                                    ch.track_index,
                                    si,
                                    mute_val,
                                ));
                            }
                            events.push(iem_core::ServerMsg::ChannelUpdate {
                                track_index: ch.track_index,
                                level_db: ch.level_db,
                                muted: ch.muted,
                                pan: ch.pan,
                            });
                        }
                    }
                }
            } else if wants_solo && had_solo {
                // SWITCHING SOLO: keep pre_solo_mutes, update mutes to new target
                if let Some(channels) = cache.member_states.get_mut(member_id) {
                    for ch in channels.iter_mut() {
                        let should_mute = !soloed_set.contains(&ch.track_index);
                        if ch.muted != should_mute {
                            ch.muted = should_mute;
                            if let Ok(si) = send_index_for(ch.track_index) {
                                let mute_val: u8 = if should_mute { 1 } else { 0 };
                                reaper_urls.push(reaper_api::set_send_mute(
                                    &reaper_url,
                                    ch.track_index,
                                    si,
                                    mute_val,
                                ));
                            }
                            events.push(iem_core::ServerMsg::ChannelUpdate {
                                track_index: ch.track_index,
                                level_db: ch.level_db,
                                muted: ch.muted,
                                pan: ch.pan,
                            });
                        }
                    }
                }
            } else if !wants_solo && had_solo {
                // EXITING SOLO: restore pre-solo mute states
                if let Some(saved) = cache.pre_solo_mutes.remove(member_id)
                    && let Some(channels) = cache.member_states.get_mut(member_id)
                {
                    for (track_idx, send_idx, was_muted) in &saved {
                        if let Some(ch) = channels.iter_mut().find(|c| c.track_index == *track_idx)
                            && ch.muted != *was_muted
                        {
                            ch.muted = *was_muted;
                            let mute_val: u8 = if *was_muted { 1 } else { 0 };
                            reaper_urls.push(reaper_api::set_send_mute(
                                &reaper_url,
                                *track_idx,
                                *send_idx,
                                mute_val,
                            ));
                            events.push(iem_core::ServerMsg::ChannelUpdate {
                                track_index: *track_idx,
                                level_db: ch.level_db,
                                muted: ch.muted,
                                pan: ch.pan,
                            });
                        }
                    }
                }
            }

            // Update solo state
            if wants_solo {
                cache
                    .solo_states
                    .insert(member_id.to_string(), soloed.clone());
            } else {
                cache.solo_states.remove(member_id);
            }

            // Mark command timestamps for all affected tracks (suppresses poller echo)
            let now = std::time::Instant::now();
            let track_indices: Vec<usize> = cache
                .member_states
                .get(member_id)
                .map(|chs| chs.iter().map(|c| c.track_index).collect())
                .unwrap_or_default();
            for ti in track_indices {
                cache
                    .command_timestamps
                    .insert((member_id.to_string(), ti), now);
            }

            drop(cache);

            // Send all REAPER commands
            for url in &reaper_urls {
                let _ = state.http_client.get(url).send().await;
            }

            // Broadcast solo state update
            let _ = state.event_tx.send((
                member_id.to_string(),
                iem_core::ServerMsg::SoloUpdate {
                    soloed: soloed.clone(),
                },
            ));

            // Broadcast channel updates for changed mutes
            for event in events {
                let _ = state.event_tx.send((member_id.to_string(), event));
            }

            // Return empty URL — REAPER commands already sent above
            return Ok(("".to_string(), None));
        }
        iem_core::ClientMsg::CallEngineer => {
            unreachable!("CallEngineer handled before apply_command_to_cache")
        }
        iem_core::ClientMsg::ClearAlert => {
            unreachable!("ClearAlert handled before apply_command_to_cache")
        }
        iem_core::ClientMsg::TalkStart | iem_core::ClientMsg::TalkStop => {
            unreachable!("Talk commands handled before apply_command_to_cache")
        }
        iem_core::ClientMsg::ListenStart { .. } | iem_core::ClientMsg::ListenStop => {
            unreachable!("Audio commands handled by ws_audio, not mixer WS")
        }
        iem_core::ClientMsg::GetEqParams { .. }
        | iem_core::ClientMsg::SetEqBand { .. }
        | iem_core::ClientMsg::GetEqParamsMulti { .. } => {
            unreachable!("EQ commands handled before apply_command_to_cache")
        }
        iem_core::ClientMsg::GetLimiterParams { .. }
        | iem_core::ClientMsg::SetLimiterParam { .. }
        | iem_core::ClientMsg::SetLimiterEnabled { .. }
        | iem_core::ClientMsg::ResetLimiterActivity { .. } => {
            unreachable!("Limiter commands handled before apply_command_to_cache")
        }
        iem_core::ClientMsg::SetGlobalMute { muted } => {
            let mute_val: u8 = if *muted { 1 } else { 0 };
            let mut cache = state.mixer_cache.write().await;
            let output_track = match cache.output_track_indices.get(member_id) {
                Some(&idx) => idx,
                None => {
                    drop(cache);
                    return Err("Output track not yet discovered".to_string());
                }
            };
            let current_db = cache
                .global_volumes
                .get(member_id)
                .map(|gv| gv.level_db)
                .unwrap_or(0.0);
            if let Some(gv) = cache.global_volumes.get_mut(member_id) {
                gv.muted = *muted;
            } else {
                cache.global_volumes.insert(
                    member_id.to_string(),
                    crate::GlobalVolState {
                        level_db: 0.0,
                        muted: *muted,
                    },
                );
            }
            cache.command_timestamps.insert(
                (member_id.to_string(), output_track + 100000),
                std::time::Instant::now(),
            );
            drop(cache);
            (
                reaper_api::set_track_mute(&reaper_url, output_track, mute_val),
                output_track,
                Some(iem_core::ServerMsg::GlobalVolumeUpdate {
                    level_db: current_db,
                    muted: *muted,
                }),
            )
        }
        iem_core::ClientMsg::SetStemsLevel { level_db } => {
            let vol = db_to_reaper_vol(*level_db);
            let cached_db = reaper_vol_to_db(vol);
            let mut cache = state.mixer_cache.write().await;
            let stems_track = match cache.stems_bus_indices.get(member_id) {
                Some(&idx) => idx,
                None => {
                    drop(cache);
                    return Err("Stems bus track not yet discovered".to_string());
                }
            };
            let current_muted = cache
                .stems_volumes
                .get(member_id)
                .map(|sv| sv.muted)
                .unwrap_or(false);
            if let Some(sv) = cache.stems_volumes.get_mut(member_id) {
                sv.level_db = cached_db;
            } else {
                cache.stems_volumes.insert(
                    member_id.to_string(),
                    crate::GlobalVolState {
                        level_db: cached_db,
                        muted: false,
                    },
                );
            }
            cache.command_timestamps.insert(
                (member_id.to_string(), stems_track + 200000),
                std::time::Instant::now(),
            );
            drop(cache);
            (
                reaper_api::set_track_vol(&reaper_url, stems_track, vol),
                stems_track,
                Some(iem_core::ServerMsg::StemsVolumeUpdate {
                    level_db: cached_db,
                    muted: current_muted,
                }),
            )
        }
        iem_core::ClientMsg::SetStemsMute { muted } => {
            let mute_val: u8 = if *muted { 1 } else { 0 };
            let mut cache = state.mixer_cache.write().await;
            let stems_track = match cache.stems_bus_indices.get(member_id) {
                Some(&idx) => idx,
                None => {
                    drop(cache);
                    return Err("Stems bus track not yet discovered".to_string());
                }
            };
            let current_db = cache
                .stems_volumes
                .get(member_id)
                .map(|sv| sv.level_db)
                .unwrap_or(0.0);
            if let Some(sv) = cache.stems_volumes.get_mut(member_id) {
                sv.muted = *muted;
            } else {
                cache.stems_volumes.insert(
                    member_id.to_string(),
                    crate::GlobalVolState {
                        level_db: 0.0,
                        muted: *muted,
                    },
                );
            }
            cache.command_timestamps.insert(
                (member_id.to_string(), stems_track + 200000),
                std::time::Instant::now(),
            );
            drop(cache);
            (
                reaper_api::set_track_mute(&reaper_url, stems_track, mute_val),
                stems_track,
                Some(iem_core::ServerMsg::StemsVolumeUpdate {
                    level_db: current_db,
                    muted: *muted,
                }),
            )
        }
    };

    tracing::debug!(
        member_id = %member_id,
        track_index = track_index,
        cmd = ?cmd,
        "Cache pre-write applied"
    );

    Ok((url, broadcast))
}

/// Send a REAPER HTTP command (fire-and-forget, spawned as background task)
fn send_to_reaper(
    client: reqwest::Client,
    url: String,
    member_id: String,
    cmd: iem_core::ClientMsg,
) {
    tokio::spawn(async move {
        if let Err(e) = client.get(&url).send().await {
            tracing::error!(error = %e, member_id = %member_id, cmd = ?cmd, "REAPER HTTP failed");
        }
    });
}

// =============================================================================
// EQ handlers (EXTSTATE + ReaScript async flow)
// =============================================================================

/// Capture live ReaEQ bands for every track that has a ReaEQ, reading each via
/// the WS EQ path (`handle_get_eq_params`). Returns `None` when no track has
/// EQ. Shared by snapshot create AND preset save/update so EQ is captured
/// identically — server-side, for ALL tracks, independent of any UI modal
/// state (reaperiem#205: presets previously captured EQ for at most one open-modal
/// track, silently losing the rest).
pub async fn capture_eq_bands(
    state: &AppState,
    track_indices: &[usize],
) -> Option<std::collections::HashMap<usize, Vec<iem_core::EqBand>>> {
    let mut per_track = Vec::new();
    for track_idx in track_indices {
        if let Some(iem_core::ServerMsg::EqParams { bands, .. }) =
            handle_get_eq_params(state, *track_idx).await
        {
            per_track.push((*track_idx, bands));
        }
    }
    build_eq_bands_map(per_track)
}

/// Keep only the tracks whose captured band list is non-empty; return `None`
/// when nothing remains. Pure (no REAPER I/O) so the reaperiem#205 "drop empty EQ /
/// None-when-empty" rule is unit-testable — `capture_eq_bands` does the HTTP
/// reads and hands the results here.
pub(crate) fn build_eq_bands_map(
    per_track: Vec<(usize, Vec<iem_core::EqBand>)>,
) -> Option<std::collections::HashMap<usize, Vec<iem_core::EqBand>>> {
    let map: std::collections::HashMap<usize, Vec<iem_core::EqBand>> = per_track
        .into_iter()
        .filter(|(_, bands)| !bands.is_empty())
        .collect();
    if map.is_empty() { None } else { Some(map) }
}

/// Handle GetEqParams: read EQ state from REAPER via EXTSTATE + ReaScript
pub async fn handle_get_eq_params(
    state: &AppState,
    track_index: usize,
) -> Option<iem_core::ServerMsg> {
    let _read_lock = state.eq_read_lock.lock().await;

    let config = state.config.read().await;
    let reaper_url = config.reaper_url.clone();
    drop(config);

    // 1. Set EXTSTATE with track index
    let set_url = reaper_api::set_extstate(
        &reaper_url,
        "reaperiem",
        "eq_read_track",
        &track_index.to_string(),
    );
    if state.http_client.get(&set_url).send().await.is_err() {
        tracing::error!(track_index, "EQ: failed to set eq_read_track EXTSTATE");
        return None;
    }

    // 2. Trigger read_eq_params.lua action
    let action_url = reaper_api::trigger_action(&reaper_url, "_RS_REAPERIEM_READ_EQ");
    if state.http_client.get(&action_url).send().await.is_err() {
        tracing::error!(track_index, "EQ: failed to trigger READ_EQ action");
        return None;
    }

    // 3. Wait for script execution
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // 4. Read result from EXTSTATE
    let get_url = reaper_api::get_extstate(&reaper_url, "reaperiem", "eq_params");
    let resp = state.http_client.get(&get_url).send().await.ok()?;
    let text = resp.text().await.ok()?;

    // Parse EXTSTATE response: "EXTSTATE\tsection\tkey\tvalue"
    let value = text.split('\t').nth(3)?;
    if value.is_empty() || value.starts_with("ERROR") {
        tracing::warn!(track_index, value, "EQ: read failed");
        return None;
    }

    // Parse the result
    parse_eq_params_response(track_index, value)
}

/// Parse the eq_params EXTSTATE response into a ServerMsg::EqParams
fn parse_eq_params_response(track_index: usize, value: &str) -> Option<iem_core::ServerMsg> {
    // Format: "OK:track=N,name=NAME,fx=N,bands=N,gg=X,bypass=X|b0:type,fn=F,gn=G,bn=B,fh=Hz,gd=dB,bo=oct|..."
    // Or: "NO_EQ:TRACKNAME"
    if value.starts_with("NO_EQ:") {
        let track_name = value.strip_prefix("NO_EQ:").unwrap_or("").to_string();
        return Some(iem_core::ServerMsg::EqParams {
            track_index,
            track_name,
            bands: vec![],
        });
    }

    if !value.starts_with("OK:") {
        return None;
    }

    let parts: Vec<&str> = value.splitn(2, '|').collect();
    let header = parts[0]; // "OK:track=N,name=NAME,fx=N,bands=N,gg=X,bypass=X"
    let band_data = if parts.len() > 1 { parts[1] } else { "" };

    // Parse track name from header
    let track_name = header
        .split(',')
        .find(|s| s.starts_with("name="))
        .and_then(|s| s.strip_prefix("name="))
        .unwrap_or("")
        .to_string();

    // Parse bands
    let mut bands = Vec::new();
    if !band_data.is_empty() {
        for band_str in band_data.split('|') {
            if let Some(band) = parse_eq_band(band_str) {
                bands.push(band);
            }
        }
    }

    Some(iem_core::ServerMsg::EqParams {
        track_index,
        track_name,
        bands,
    })
}

/// Parse a single band string like "b0:lowshelf,fn=0.283000,gn=0.184000,bn=0.295000,fh=250,gd=-2.7,bo=1.18"
/// Uses REAPER-formatted display values (fh, gd, bo) when available for accurate display.
/// Falls back to approximations from normalized values (fn, gn, bn) for backward compatibility.
fn parse_eq_band(s: &str) -> Option<iem_core::EqBand> {
    let s = s.trim(); // Strip trailing whitespace/newlines from EXTSTATE
    let colon_pos = s.find(':')?;
    let after_colon = &s[colon_pos + 1..];
    let fields: Vec<&str> = after_colon.split(',').collect();

    if fields.is_empty() {
        return None;
    }

    let band_type = fields[0].to_string();

    let get_field = |prefix: &str| -> Option<f32> {
        fields
            .iter()
            .find(|f| f.starts_with(prefix))
            .and_then(|f| f.strip_prefix(prefix))
            .and_then(|v| v.parse::<f32>().ok())
    };

    let freq_norm = get_field("fn=")?;
    let gain_norm = get_field("gn=")?;
    let bw_norm = get_field("bn=")?;

    // `fh=`, `gd=`, and `bo=` are always emitted by read_eq_params.lua;
    // if any are missing, the input is malformed — fail closed.
    let freq_hz = get_field("fh=")?;
    let gain_db = get_field("gd=")?;
    let bw = get_field("bo=")?;

    // Parse enabled state (en=0/1), default to true for backward compat
    let enabled = get_field("en=").is_none_or(|v| v >= 0.5);

    let gain_db_min = get_field("gd_min=").unwrap_or(-12.0);
    let gain_db_max = get_field("gd_max=").unwrap_or(12.0);

    Some(iem_core::EqBand {
        band_type,
        freq_hz,
        gain_db,
        bw,
        freq_norm,
        gain_norm,
        bw_norm,
        gain_db_min,
        gain_db_max,
        enabled,
    })
}

/// Handle SetEqBand: set a single EQ parameter on a track via EXTSTATE + ReaScript.
///
/// Serialized via `eq_write_lock` — the EXTSTATE key `reaperiem/eq_set` is a single-slot
/// channel. Without serialization, concurrent tasks overwrite each other's data before
/// the Lua script reads it.
async fn handle_set_eq_band(
    state: &AppState,
    track_index: usize,
    band: u8,
    param: &str,
    value: f32,
) {
    // Serialize all EQ writes — only one EXTSTATE write + action trigger at a time
    let _lock = state.eq_write_lock.lock().await;

    let config = state.config.read().await;
    let reaper_url = config.reaper_url.clone();
    drop(config);

    // 1. Set EXTSTATE with parameters: "track=N|band=B|param=P|value=V"
    let eq_set_value = format!(
        "track={}|band={}|param={}|value={:.6}",
        track_index, band, param, value
    );
    let set_url = reaper_api::set_extstate(&reaper_url, "reaperiem", "eq_set", &eq_set_value);
    if state.http_client.get(&set_url).send().await.is_err() {
        tracing::error!(
            track_index,
            band,
            param,
            "EQ: failed to set eq_set EXTSTATE"
        );
        return;
    }

    // 2. Trigger set_eq_param.lua action
    let action_url = reaper_api::trigger_action(&reaper_url, "_RS_REAPERIEM_SET_EQ");
    if state.http_client.get(&action_url).send().await.is_err() {
        tracing::error!(
            track_index,
            band,
            param,
            "EQ: failed to trigger SET_EQ action"
        );
        return;
    }

    // 3. Wait for script execution (50ms matches snapshot/preset restore pattern)
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // 4. Read result (for logging only)
    let get_url = reaper_api::get_extstate(&reaper_url, "reaperiem", "eq_set_result");
    if let Ok(resp) = state.http_client.get(&get_url).send().await
        && let Ok(text) = resp.text().await
        && let Some(result) = text.split('\t').nth(3)
    {
        if result.starts_with("ERROR") {
            tracing::error!(track_index, band, param, result, "EQ: set_eq_param failed");
        } else {
            tracing::debug!(track_index, band, param, result, "EQ: param set OK");
        }
    }
}

// =============================================================================
// Limiter handlers (EXTSTATE + ReaScript async flow) (reaperiem#72)
// =============================================================================

/// Read limiter parameters from REAPER via EXTSTATE + ReaScript
pub async fn handle_get_limiter_params(
    state: &AppState,
    track_index: usize,
) -> Option<iem_core::ServerMsg> {
    let _read_lock = state.limiter_read_lock.lock().await;

    let config = state.config.read().await;
    let reaper_url = config.reaper_url.clone();
    drop(config);

    // Set EXTSTATE with track index
    let set_url = reaper_api::set_extstate(
        &reaper_url,
        "reaperiem",
        "limiter_read_track",
        &track_index.to_string(),
    );
    if state.http_client.get(&set_url).send().await.is_err() {
        tracing::error!(
            track_index,
            "Limiter: failed to set limiter_read_track EXTSTATE"
        );
        return None;
    }

    // Trigger read_limiter_params.lua action
    let action_url = reaper_api::trigger_action(&reaper_url, "_RS_REAPERIEM_READ_LIMITER");
    if state.http_client.get(&action_url).send().await.is_err() {
        tracing::error!(
            track_index,
            "Limiter: failed to trigger READ_LIMITER action"
        );
        return None;
    }

    // Wait for script execution
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Read result from EXTSTATE
    let get_url = reaper_api::get_extstate(&reaper_url, "reaperiem", "limiter_params");
    let resp = state.http_client.get(&get_url).send().await.ok()?;
    let text = resp.text().await.ok()?;

    let value = text.split('\t').nth(3)?;
    if value.is_empty() || value.starts_with("ERROR") {
        tracing::warn!(track_index, value, "Limiter: read failed");
        return None;
    }

    let mut reply = parse_limiter_params_response(track_index, value)?;
    if let iem_core::ServerMsg::LimiterParams {
        ref mut active_seconds,
        ..
    } = reply
    {
        let guard = state.limiter_activity.lock().await;
        let ms = guard.get(&track_index).copied().unwrap_or(0);
        *active_seconds = limiter_ms_to_seconds(ms);
    }
    Some(reply)
}

/// Convert accumulated limiter-activity milliseconds to seconds.
fn limiter_ms_to_seconds(ms: u64) -> f64 {
    (ms as f64) / 1000.0
}

/// Parse limiter EXTSTATE response into ServerMsg
fn parse_limiter_params_response(track_index: usize, value: &str) -> Option<iem_core::ServerMsg> {
    // Format: "OK:track=N,name=TRACKNAME,fx=IDX|limit=V,limit_n=N,enabled=E"
    // Or: "NO_LIMITER:TRACKNAME"
    if value.starts_with("NO_LIMITER:") {
        let track_name = value.strip_prefix("NO_LIMITER:").unwrap_or("").to_string();
        return Some(iem_core::ServerMsg::LimiterParams {
            track_index,
            track_name,
            limit_db: 0.0,
            limit_norm: 0.0,
            enabled: false,
            active_seconds: 0.0,
        });
    }

    if !value.starts_with("OK:") {
        return None;
    }

    let parts: Vec<&str> = value.splitn(2, '|').collect();
    let header = parts[0];
    let params = if parts.len() > 1 { parts[1] } else { "" };

    let track_name = header
        .split(',')
        .find(|s| s.starts_with("name="))
        .and_then(|s| s.strip_prefix("name="))
        .unwrap_or("")
        .to_string();

    let get_field = |prefix: &str| -> f32 {
        params
            .split(',')
            .find(|s| s.trim().starts_with(prefix))
            .and_then(|s| s.trim().strip_prefix(prefix))
            .and_then(|v| v.trim().parse::<f32>().ok())
            .unwrap_or(0.0)
    };

    Some(iem_core::ServerMsg::LimiterParams {
        track_index,
        track_name,
        limit_db: get_field("limit="),
        limit_norm: get_field("limit_n="),
        enabled: get_field("enabled=") >= 0.5,
        active_seconds: 0.0,
    })
}

/// Set a single limiter parameter in REAPER via EXTSTATE + ReaScript
async fn handle_set_limiter_param(state: &AppState, track_index: usize, param: &str, value: f32) {
    let _lock = state.limiter_write_lock.lock().await;

    let config = state.config.read().await;
    let reaper_url = config.reaper_url.clone();
    drop(config);

    let set_value = format!("track={}|param={}|value={:.6}", track_index, param, value);
    let set_url = reaper_api::set_extstate(&reaper_url, "reaperiem", "limiter_set", &set_value);
    if state.http_client.get(&set_url).send().await.is_err() {
        tracing::error!(
            track_index,
            param,
            "Limiter: failed to set limiter_set EXTSTATE"
        );
        return;
    }

    let action_url = reaper_api::trigger_action(&reaper_url, "_RS_REAPERIEM_SET_LIMITER");
    if state.http_client.get(&action_url).send().await.is_err() {
        tracing::error!(
            track_index,
            param,
            "Limiter: failed to trigger SET_LIMITER action"
        );
        return;
    }

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Read result for logging
    let get_url = reaper_api::get_extstate(&reaper_url, "reaperiem", "limiter_set_result");
    if let Ok(resp) = state.http_client.get(&get_url).send().await
        && let Ok(text) = resp.text().await
        && let Some(result) = text.split('\t').nth(3)
    {
        if result.starts_with("ERROR") {
            tracing::error!(track_index, param, result, "Limiter: set param failed");
        } else {
            tracing::debug!(track_index, param, result, "Limiter: param set OK");
        }
    }
}

/// Pure authorization helper for limiter commands (reaperiem#145 — extracted from the
/// WS recv-loop closure so it can be unit-tested). Engineer is always allowed;
/// a band member is allowed only on their own output track.
fn check_owns_limiter_track(
    is_engineer: bool,
    member_id: &str,
    output_track_indices: &std::collections::HashMap<String, usize>,
    track_index: usize,
) -> bool {
    if is_engineer {
        return true;
    }
    output_track_indices
        .get(member_id)
        .is_some_and(|&idx| idx == track_index)
}

/// Reset the activity counter for one limiter track (reaperiem#145).
/// Zeros AppState.limiter_activity[track] AND writes
/// EXTSTATE REAPERIEM_LIMITER_ACTIVITY/reset = "<track_index>"
/// so meter_bridge.lua zeros its local accumulator on its next tick
/// (otherwise the next poller cycle would re-overwrite the server zero).
pub async fn handle_reset_limiter_activity(state: &AppState, track_index: usize) {
    {
        let mut guard = state.limiter_activity.lock().await;
        guard.insert(track_index, 0);
    }

    let config = state.config.read().await;
    let reaper_url = config.reaper_url.clone();
    drop(config);

    let set_url = reaper_api::set_extstate(
        &reaper_url,
        "REAPERIEM_LIMITER_ACTIVITY",
        "reset",
        &track_index.to_string(),
    );
    if state.http_client.get(&set_url).send().await.is_err() {
        tracing::error!(
            track_index,
            "Limiter activity reset: failed to write reset EXTSTATE"
        );
    }
}

/// REAPER HTTP API URL builder
/// CRITICAL: All REAPER API commands MUST use the `/_/` prefix!
/// Without this prefix, REAPER returns empty responses.
pub(crate) mod reaper_api {
    /// Build URL for setting send volume
    pub fn set_send_vol(base_url: &str, track: usize, send: usize, vol: f32) -> String {
        format!(
            "{}/_/SET/TRACK/{}/SEND/{}/VOL/{}",
            base_url, track, send, vol
        )
    }

    /// Build URL for setting send mute
    pub fn set_send_mute(base_url: &str, track: usize, send: usize, mute: u8) -> String {
        format!(
            "{}/_/SET/TRACK/{}/SEND/{}/MUTE/{}",
            base_url, track, send, mute
        )
    }

    /// Build URL for setting send pan
    pub fn set_send_pan(base_url: &str, track: usize, send: usize, pan: f32) -> String {
        format!(
            "{}/_/SET/TRACK/{}/SEND/{}/PAN/{}",
            base_url, track, send, pan
        )
    }

    /// Build URL for getting send volume (retained for test coverage of /_/ prefix)
    #[cfg(test)]
    pub fn get_send_vol(base_url: &str, track: usize, send: usize) -> String {
        format!("{}/_/GET/TRACK/{}/SEND/{}/VOL", base_url, track, send)
    }

    /// Build URL for getting send mute (retained for test coverage of /_/ prefix)
    #[cfg(test)]
    pub fn get_send_mute(base_url: &str, track: usize, send: usize) -> String {
        format!("{}/_/GET/TRACK/{}/SEND/{}/MUTE", base_url, track, send)
    }

    /// Build URL for getting send pan (retained for test coverage of /_/ prefix)
    #[cfg(test)]
    pub fn get_send_pan(base_url: &str, track: usize, send: usize) -> String {
        format!("{}/_/GET/TRACK/{}/SEND/{}/PAN", base_url, track, send)
    }

    /// Build URL for setting track volume (output bus volume)
    pub fn set_track_vol(base_url: &str, track: usize, vol: f32) -> String {
        format!("{}/_/SET/TRACK/{}/VOL/{}", base_url, track, vol)
    }

    /// Build URL for setting track mute (output bus mute)
    pub fn set_track_mute(base_url: &str, track: usize, mute: u8) -> String {
        format!("{}/_/SET/TRACK/{}/MUTE/{}", base_url, track, mute)
    }

    /// Build URL for querying tracks
    pub fn query_tracks(base_url: &str) -> String {
        format!("{}/_/NTRACK;TRACK", base_url)
    }

    /// Build URL for reading EXTSTATE (key-value store in REAPER)
    pub fn get_extstate(base_url: &str, section: &str, key: &str) -> String {
        format!("{}/_/GET/EXTSTATE/{}/{}", base_url, section, key)
    }

    /// Build URL for setting EXTSTATE (key-value store in REAPER)
    pub fn set_extstate(base_url: &str, section: &str, key: &str, value: &str) -> String {
        format!("{}/_/SET/EXTSTATE/{}/{}/{}", base_url, section, key, value)
    }

    /// Build URL for triggering a REAPER action by ID
    pub fn trigger_action(base_url: &str, action_id: &str) -> String {
        format!("{}/_/{}", base_url, action_id)
    }

    /// Build URL for getting full send state (returns vol, mute, pan in one call)
    pub fn get_send_state(base_url: &str, track: usize, send: usize) -> String {
        format!("{}/_/GET/TRACK/{}/SEND/{}", base_url, track, send)
    }
}

// =============================================================================
// Audio WebSocket handler (engineer-only audio streaming)
// =============================================================================

/// WebSocket audio endpoint - streams Opus frames to the engineer's browser
/// Requires token query param for authentication: /ws/audio?token=<JWT>
/// Engineer-only: non-engineer tokens are rejected with 403
#[cfg(feature = "audio")]
pub async fn ws_audio(
    ws: axum::extract::ws::WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;

    // Token is required
    let token = query.token.as_deref().ok_or_else(|| {
        tracing::warn!("Audio WS connection without token");
        (StatusCode::UNAUTHORIZED, Json(ApiError::unauthorized()))
    })?;

    // Validate token
    let claims = crate::auth::extract_claims(token, &config.jwt_secret).ok_or_else(|| {
        tracing::warn!("Audio WS connection with invalid token");
        (StatusCode::UNAUTHORIZED, Json(ApiError::unauthorized()))
    })?;

    // Check token expiration
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    if claims.exp < now {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiError::new("TOKEN_EXPIRED", "Token has expired")),
        ));
    }

    // Engineer-only access
    if !claims.engineer {
        tracing::warn!(sub = %claims.sub, "Audio WS denied: not engineer");
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiError::new(
                "FORBIDDEN",
                "Audio streaming is engineer-only",
            )),
        ));
    }

    drop(config);

    Ok(ws.on_upgrade(move |socket| handle_audio_ws(socket, state)))
}

/// Handle the audio WebSocket connection.
/// Uses a bounded frame dropper (mpsc channel, cap=5) between the broadcast receiver
/// and WebSocket sender to prevent TCP buffer bloat from causing latency accumulation.
/// Restore REAPER send mute states saved during listen mode, then reset to Idle.
/// Called from both ListenStop handler and WebSocket disconnect cleanup.
#[cfg(feature = "audio")]
async fn restore_listen_mutes(state: &AppState) {
    let listen_state = state.engineer_listen_target.read().await.clone();
    if let crate::ListenTarget::Member(saved_mutes) = listen_state {
        let config = state.config.read().await;
        let reaper_url = config.reaper_url.clone();
        drop(config);
        for (track_idx, send_idx, was_muted) in &saved_mutes {
            let mute_val = if *was_muted { 1 } else { 0 };
            let url = reaper_api::set_send_mute(&reaper_url, *track_idx, *send_idx, mute_val);
            let _ = state.http_client.get(&url).send().await;
        }
        tracing::info!("Restored {} send mute states", saved_mutes.len());
    }
    *state.engineer_listen_target.write().await = crate::ListenTarget::Idle;
}

#[cfg(feature = "audio")]
async fn handle_audio_ws(mut socket: axum::extract::ws::WebSocket, state: AppState) {
    use axum::extract::ws::Message;
    use iem_core::{ClientMsg, ServerMsg};
    use tokio::time::{Duration, Instant};

    tracing::info!("Audio WebSocket connected");

    // Recover from orphaned listen state (previous session crashed without cleanup)
    {
        let stale = state.engineer_listen_target.read().await.clone();
        if let crate::ListenTarget::Member(_) = stale {
            tracing::warn!("Found orphaned listen mutes from previous session, restoring");
            restore_listen_mutes(&state).await;
        }
    }

    // Frame dropper: bounded channel between broadcast receiver and WebSocket sender.
    // When TCP backpressure stalls the sender, stale frames are dropped instead of queuing.
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(5);
    let mut producer_handle: Option<tokio::task::JoinHandle<()>> = None;
    let mut last_audio_time = Instant::now();
    let mut is_listening = false;
    let mut first_frame_forwarded_logged = false;

    loop {
        tokio::select! {
            // Client commands (text)
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(cmd) = serde_json::from_str::<ClientMsg>(&text) {
                            match cmd {
                                ClientMsg::ListenStart { member_id } => {
                                    tracing::info!(target_member = %member_id, "Audio listen started");

                                    // Restore any previous listen mutes first (concurrent device safety)
                                    restore_listen_mutes(&state).await;

                                    // On band member page: mute all other member sends to ENGINEER
                                    // except the target member's send (app-level solo via send muting)
                                    if member_id != "engineer" {
                                        let config = state.config.read().await;
                                        let reaper_url = config.reaper_url.clone();
                                        drop(config);

                                        // Get all non-engineer members with mix_send_index
                                        let discovered = state.discovered_members.read().await;
                                        let members_with_sends: Vec<(String, usize, usize)> = discovered.iter()
                                            .filter(|m| m.id() != "engineer" && m.mix_send_index.is_some())
                                            .map(|m| (m.id(), m.track_index, m.mix_send_index.unwrap()))
                                            .collect();
                                        drop(discovered);

                                        // 1. Query current mute states and save them
                                        let mut saved_mutes = Vec::new();
                                        for (_, track_idx, send_idx) in &members_with_sends {
                                            if let Ok((_vol, muted, _pan)) = query_send_state(
                                                &state.http_client, &reaper_url, *track_idx, *send_idx
                                            ).await {
                                                saved_mutes.push((*track_idx, *send_idx, muted));
                                            }
                                        }

                                        // 2. Mute all sends except target member's, unmute target's
                                        for (mid, track_idx, send_idx) in &members_with_sends {
                                            let mute_val = if *mid == member_id { 0 } else { 1 };
                                            let url = reaper_api::set_send_mute(&reaper_url, *track_idx, *send_idx, mute_val);
                                            let _ = state.http_client.get(&url).send().await;
                                        }

                                        // 3. Store saved states for restoration
                                        *state.engineer_listen_target.write().await =
                                            crate::ListenTarget::Member(saved_mutes);
                                    }

                                    // Abort any existing producer before starting a new one
                                    if let Some(handle) = producer_handle.take() {
                                        handle.abort();
                                    }

                                    // Spawn frame dropper producer: reads broadcast, drops stale on backpressure
                                    let mut broadcast_rx = state.audio_tx.subscribe();
                                    let dropper_tx = frame_tx.clone();
                                    let sub_count = state.audio_tx.receiver_count();
                                    tracing::info!(
                                        subscriber_count = sub_count,
                                        "audio producer spawned for /ws/audio listener"
                                    );
                                    producer_handle = Some(tokio::spawn(async move {
                                        let mut first_frame_logged = false;
                                        let mut first_lagged_logged = false;
                                        loop {
                                            match broadcast_rx.recv().await {
                                                Ok(frame) => {
                                                    if !first_frame_logged {
                                                        tracing::info!(
                                                            frame_size = frame.len(),
                                                            "audio producer received first broadcast frame"
                                                        );
                                                        first_frame_logged = true;
                                                    }
                                                    // try_send: if channel full (TCP backpressure), drop this frame
                                                    if let Err(e) = dropper_tx.try_send(frame) {
                                                        tracing::debug!("audio dropper try_send failed: {}", e);
                                                    }
                                                }
                                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                                    // First Lagged at INFO so a recurring Scenario B regression surfaces
                                                    // immediately in logs. Subsequent Lagged events at DEBUG to avoid
                                                    // log spam under sustained backpressure.
                                                    if !first_lagged_logged {
                                                        tracing::info!("audio broadcast Lagged: skipped {} stale frames (first occurrence)", n);
                                                        first_lagged_logged = true;
                                                    } else {
                                                        tracing::debug!("audio broadcast Lagged: skipped {} stale frames", n);
                                                    }
                                                }
                                                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                                    tracing::info!("audio broadcast Closed — producer exiting");
                                                    break;
                                                }
                                            }
                                        }
                                    }));

                                    is_listening = true;
                                    last_audio_time = Instant::now();
                                    let status = ServerMsg::AudioStatus {
                                        status: "listening".to_string(),
                                        target: Some(member_id),
                                    };
                                    let json = serde_json::to_string(&status).unwrap_or_default();
                                    if socket.send(Message::Text(json.into())).await.is_err() {
                                        break;
                                    }
                                }
                                ClientMsg::ListenStop => {
                                    tracing::info!("Audio listen stopped");

                                    // Stop the producer
                                    if let Some(handle) = producer_handle.take() {
                                        handle.abort();
                                    }

                                    // Restore saved mute states if band member listen was active
                                    restore_listen_mutes(&state).await;

                                    is_listening = false;
                                    let status = ServerMsg::AudioStatus {
                                        status: "stopped".to_string(),
                                        target: None,
                                    };
                                    let json = serde_json::to_string(&status).unwrap_or_default();
                                    if socket.send(Message::Text(json.into())).await.is_err() {
                                        break;
                                    }
                                }
                                _ => {} // Ignore non-audio commands
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }

            // Audio frames from dropper channel — only when listening
            frame = frame_rx.recv(), if is_listening => {
                match frame {
                    Some(data) => {
                        last_audio_time = Instant::now();
                        let size = data.len();
                        if let Err(e) = socket.send(Message::Binary(data.to_vec().into())).await {
                            tracing::info!("audio binary send failed: {} — closing /ws/audio", e);
                            break;
                        }
                        if !first_frame_forwarded_logged {
                            tracing::info!(
                                frame_size = size,
                                "first binary frame forwarded on /ws/audio"
                            );
                            first_frame_forwarded_logged = true;
                        }
                        // Bump diagnostic counter (shared across all listeners).
                        // Recover from Mutex poisoning (matches routes.rs:588 handler style).
                        let mut diag = state
                            .audio_diagnostics
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        diag.frames_forwarded = diag.frames_forwarded.saturating_add(1);
                    }
                    None => {
                        // Channel closed — producer died
                        tracing::info!("audio frame_rx closed — producer task died");
                        break;
                    }
                }
            }

            // Not listening — just wait
            _ = tokio::time::sleep(Duration::from_secs(60)), if !is_listening => {}

            // Ping every 30s to detect half-open TCP connections
            _ = tokio::time::sleep(Duration::from_secs(30)), if is_listening => {
                if last_audio_time.elapsed() <= Duration::from_secs(5) {
                    // Audio is flowing, send a ping to keep the connection alive
                    if socket.send(Message::Ping(vec![].into())).await.is_err() {
                        break; // Connection dead — cleanup will restore mutes
                    }
                }
            }

            // No-source timeout: if listening but no audio for 5 seconds
            _ = tokio::time::sleep(Duration::from_secs(5)), if is_listening => {
                if last_audio_time.elapsed() > Duration::from_secs(5) {
                    let status = ServerMsg::AudioStatus {
                        status: "no_source".to_string(),
                        target: None,
                    };
                    let json = serde_json::to_string(&status).unwrap_or_default();
                    if socket.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                    // Reset timer so we don't spam
                    last_audio_time = Instant::now();
                }
            }
        }
    }

    // Cleanup: abort producer and restore mutes if listen was active
    if let Some(handle) = producer_handle {
        handle.abort();
    }
    if is_listening {
        restore_listen_mutes(&state).await;
        tracing::info!("Audio WebSocket disconnected — mutes restored");
    } else {
        tracing::info!("Audio WebSocket disconnected");
    }
}

/// WebSocket talkback endpoint — receives Opus audio from engineer's mic (reaperiem#123)
#[cfg(feature = "audio")]
pub async fn ws_talkback(
    ws: axum::extract::ws::WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;

    let token = query
        .token
        .as_deref()
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(ApiError::unauthorized())))?;

    let claims = crate::auth::extract_claims(token, &config.jwt_secret)
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(ApiError::unauthorized())))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    if claims.exp < now {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiError::new("TOKEN_EXPIRED", "Token has expired")),
        ));
    }

    if !claims.engineer {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiError::new("FORBIDDEN", "Talkback is engineer-only")),
        ));
    }

    drop(config);

    Ok(ws.on_upgrade(move |socket| handle_talkback_ws(socket, state)))
}

#[cfg(feature = "audio")]
async fn handle_talkback_ws(mut socket: axum::extract::ws::WebSocket, state: AppState) {
    use axum::extract::ws::Message;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use tokio::sync::Mutex as AsyncMutex;

    tracing::info!("Talkback WebSocket connected");

    // Record the negotiated bitrate.  The browser side (talkback.js) is
    // hardcoded to 96 kbps Opus mono; if that ever becomes client-configured
    // via a handshake message, wire it in below.
    state
        .talkback_metrics
        .bitrate_kbps
        .store(96, Ordering::Relaxed);

    // Shared jitter buffer between the receive loop and the drain loop.
    let jb = Arc::new(AsyncMutex::new(crate::talkback_buffer::JitterBuffer::new()));
    let last_recv = Arc::new(AsyncMutex::new(std::time::Instant::now()));

    // Drain loop: pop one frame every 20 ms, send over UDP.
    // Runs concurrently with the receive loop below. Aborted on WS close.
    let jb_drain = jb.clone();
    let metrics_drain = state.talkback_metrics.clone();
    let state_drain = state.clone();
    let drain_handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(
            crate::talkback_buffer::FRAME_MS as u64,
        ));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;

            // Look up current VST address (may not be present yet).
            let vst_addr = {
                let tb = state_drain.talkback_state.read().await;
                tb.recv_vst_addr
            };

            // Pop next frame (if any) and record fill gauge.
            let popped = {
                let mut jbg = jb_drain.lock().await;
                let p = jbg.pop();
                metrics_drain
                    .buffer_fill_ms
                    .store(jbg.fill_ms(), Ordering::Relaxed);
                metrics_drain
                    .buffer_overflows
                    .store(jbg.overflows(), Ordering::Relaxed);
                p
            };

            match popped {
                Some((seq, payload)) => {
                    if let Some(addr) = vst_addr {
                        let mut packet = Vec::with_capacity(8 + payload.len());
                        packet.extend_from_slice(b"OIEM");
                        packet.extend_from_slice(&seq.to_le_bytes());
                        packet.extend_from_slice(&(payload.len() as u16).to_le_bytes());
                        packet.extend_from_slice(&payload);
                        let _ = state_drain.talkback_socket.send_to(&packet, addr).await;
                        metrics_drain.packets_out.fetch_add(1, Ordering::Relaxed);
                    }
                    // If no VST addr yet, drop the frame silently — the browser
                    // should not be hammering us before the VST registers.
                }
                None => {
                    metrics_drain.underruns.fetch_add(1, Ordering::Relaxed);
                    // No keepalive emitted — VST tolerates silence via its own
                    // accumulator fallback.
                }
            }
        }
    });

    // Receive loop: push every binary frame into the jitter buffer.
    loop {
        match socket.recv().await {
            Some(Ok(Message::Binary(data))) => {
                state
                    .talkback_metrics
                    .packets_in
                    .fetch_add(1, Ordering::Relaxed);
                {
                    let mut lr = last_recv.lock().await;
                    *lr = std::time::Instant::now();
                }
                let mut jbg = jb.lock().await;
                jbg.push(data.to_vec());
                state
                    .talkback_metrics
                    .buffer_fill_ms
                    .store(jbg.fill_ms(), Ordering::Relaxed);
                state
                    .talkback_metrics
                    .buffer_overflows
                    .store(jbg.overflows(), Ordering::Relaxed);
            }
            Some(Ok(Message::Close(_))) | None => break,
            _ => {}
        }

        // Update last_packet_age_ms on every iteration (cheap).
        let age = last_recv.lock().await.elapsed().as_millis() as u64;
        state
            .talkback_metrics
            .last_packet_age_ms
            .store(age, Ordering::Relaxed);
    }

    // Flush the jitter buffer before aborting the drain loop.
    // This prevents residual frames from being sent to the VST after the
    // client releases the talk button — which would cause REAPER to see
    // a signal tail of up to TARGET_MS (60 ms) after release.
    {
        let mut jbg = jb.lock().await;
        while jbg.pop().is_some() {}
    }

    // Cancel drain loop so it doesn't live past the socket close.
    drain_handle.abort();

    tracing::info!("Talkback WebSocket disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;
    use iem_core::ServerMsg;
    use std::collections::HashMap;

    // ---- Alert catch-up helper (reaperiem#150) -----------------------------------

    fn alert_entry(from_member: &str, from_name: &str) -> (String, String) {
        (from_member.to_string(), from_name.to_string())
    }

    #[test]
    fn test_build_alert_catchup_engineer_with_no_alerts_returns_none() {
        let alerts: HashMap<String, (String, String)> = HashMap::new();
        assert!(build_alert_catchup(&alerts, "engineer").is_none());
    }

    #[test]
    fn test_build_alert_catchup_engineer_with_alerts_returns_active_alerts() {
        let mut alerts = HashMap::new();
        alerts.insert("member1".to_string(), alert_entry("member1", "MEMBER1"));
        alerts.insert("member2".to_string(), alert_entry("member2", "MEMBER2"));

        let msg = build_alert_catchup(&alerts, "engineer")
            .expect("engineer with alerts should receive ActiveAlerts");
        match msg {
            ServerMsg::ActiveAlerts { alerts: out } => {
                assert_eq!(out.len(), 2);
                let mut ids: Vec<String> = out.iter().map(|a| a.from_member.clone()).collect();
                ids.sort();
                assert_eq!(ids, vec!["member1".to_string(), "member2".to_string()]);
            }
            other => panic!("expected ActiveAlerts, got {:?}", other),
        }
    }

    #[test]
    fn test_build_alert_catchup_member_with_own_alert_returns_engineer_alert() {
        let mut alerts = HashMap::new();
        alerts.insert("member1".to_string(), alert_entry("member1", "MEMBER1"));

        let msg = build_alert_catchup(&alerts, "member1")
            .expect("member with active alert should receive catch-up");
        match msg {
            ServerMsg::EngineerAlert {
                from_member,
                from_name,
            } => {
                assert_eq!(from_member, "member1");
                // from_name is intentionally empty on the member side — the
                // member doesn't need to render a name for their own button.
                assert_eq!(from_name, "");
            }
            other => panic!("expected EngineerAlert, got {:?}", other),
        }
    }

    #[test]
    fn test_build_alert_catchup_member_with_no_alert_returns_none() {
        let alerts: HashMap<String, (String, String)> = HashMap::new();
        assert!(build_alert_catchup(&alerts, "member1").is_none());
    }

    #[test]
    fn test_build_alert_catchup_member_only_sees_own_alert_not_other_members() {
        // Regression guard: a member must NOT get another member's alert.
        // If this ever returns Some for a member whose id is not a key, the
        // member's button would incorrectly flash active when a different
        // member triggered SOS.
        let mut alerts = HashMap::new();
        alerts.insert("member2".to_string(), alert_entry("member2", "MEMBER2"));
        assert!(build_alert_catchup(&alerts, "member1").is_none());
    }

    #[test]
    fn test_build_alert_catchup_member_echo_is_own_id() {
        // When a member has an active alert, the EngineerAlert echo must
        // carry THEIR own id, not some other member's. Kills a mutant that
        // swaps `member_id` for a literal or for a value from the map.
        let mut alerts = HashMap::new();
        alerts.insert("member1".to_string(), alert_entry("x", "y"));
        alerts.insert("member2".to_string(), alert_entry("z", "w"));

        let msg = build_alert_catchup(&alerts, "member1").unwrap();
        if let ServerMsg::EngineerAlert { from_member, .. } = msg {
            assert_eq!(from_member, "member1");
        } else {
            panic!("expected EngineerAlert");
        }
    }

    #[test]
    fn test_build_alert_catchup_engineer_with_own_alert_goes_through_engineer_branch() {
        // Edge case: if the engineer somehow ends up in `active_alerts`
        // (e.g. by triggering SOS from their own mixer page), the helper
        // must still route through the engineer branch and return
        // ActiveAlerts including their own entry — never EngineerAlert.
        // This locks in the "engineer_id-first" branching so a future
        // refactor can't accidentally send engineers a member-style echo.
        let mut alerts = HashMap::new();
        alerts.insert("engineer".to_string(), alert_entry("engineer", "ENGINEER"));
        alerts.insert("member1".to_string(), alert_entry("member1", "MEMBER1"));

        let msg = build_alert_catchup(&alerts, "engineer")
            .expect("engineer with alerts should receive ActiveAlerts");
        match msg {
            ServerMsg::ActiveAlerts { alerts: out } => {
                assert_eq!(out.len(), 2);
                let mut ids: Vec<String> = out.iter().map(|a| a.from_member.clone()).collect();
                ids.sort();
                assert_eq!(ids, vec!["engineer".to_string(), "member1".to_string()]);
            }
            other => panic!("expected ActiveAlerts, got {:?}", other),
        }
    }

    // ---- dB conversion --------------------------------------------------

    #[test]
    fn test_db_to_reaper_vol_unity() {
        // 0 dB = linear 1.0 (standard: vol = 10^(0/20) = 1.0)
        let vol = db_to_reaper_vol(0.0);
        assert!((vol - 1.0).abs() < 0.01, "0 dB should be 1.0, got {}", vol);
    }

    #[test]
    fn test_db_to_reaper_vol_minus_6() {
        // -6 dB ≈ 0.501
        let vol = db_to_reaper_vol(-6.0);
        assert!(
            (vol - 0.501).abs() < 0.01,
            "-6 dB should be ~0.501, got {}",
            vol
        );
    }

    #[test]
    fn test_db_to_reaper_vol_plus_6() {
        // +6 dB ≈ 1.995
        let vol = db_to_reaper_vol(6.0);
        assert!(
            (vol - 1.995).abs() < 0.01,
            "+6 dB should be ~1.995, got {}",
            vol
        );
    }

    #[test]
    fn test_db_to_reaper_vol_minus_inf() {
        // -60 dB and below should be 0
        assert_eq!(db_to_reaper_vol(-60.0), 0.0);
        assert_eq!(db_to_reaper_vol(-100.0), 0.0);
    }

    #[test]
    fn test_reaper_vol_to_db_unity() {
        // linear 1.0 = 0 dB
        let db = reaper_vol_to_db(1.0);
        assert!(db.abs() < 0.1, "1.0 should be ~0 dB, got {}", db);
    }

    #[test]
    fn test_reaper_vol_to_db_half() {
        // linear 0.5 ≈ -6.02 dB
        let db = reaper_vol_to_db(0.5);
        assert!(
            (db - (-6.02)).abs() < 0.1,
            "0.5 should be ~-6 dB, got {}",
            db
        );
    }

    #[test]
    fn test_reaper_vol_to_db_zero() {
        // 0.0 should be -60 dB (our floor)
        assert_eq!(reaper_vol_to_db(0.0), -60.0);
    }

    #[test]
    fn test_db_conversion_roundtrip() {
        // Test roundtrip conversion at various levels
        for db in [-20.0, -10.0, -6.0, 0.0, 6.0] {
            let vol = db_to_reaper_vol(db);
            let back = reaper_vol_to_db(vol);
            assert!(
                (back - db).abs() < 0.5,
                "Roundtrip failed for {} dB: got {} dB",
                db,
                back
            );
        }
    }

    #[test]
    fn test_db_roundtrip_quantized_02() {
        // With quantize_02, the roundtrip through REAPER's linear domain
        // should produce exact 0.2-step values
        for db in [-4.0_f32, -6.0, -10.2, -3.8, 0.0, 6.0, -12.0, -0.4] {
            let vol = db_to_reaper_vol(db);
            let roundtrip = quantize_02(reaper_vol_to_db(vol));
            assert_eq!(
                roundtrip, db,
                "Quantized round-trip for {db} dB failed: got {roundtrip}"
            );
        }
    }

    #[test]
    fn test_categorize_track_mics() {
        let (cat, pair, side) = categorize_track("MEMBER3 mic");
        assert_eq!(cat, "mics");
        assert!(pair.is_none());
        assert!(side.is_none());
    }

    #[test]
    fn test_categorize_track_stems() {
        let (cat, _, _) = categorize_track("DRUMS L");
        assert_eq!(cat, "stems");
    }

    #[test]
    fn test_categorize_track_tech() {
        let (cat, _, _) = categorize_track("ENGINEER hand");
        assert_eq!(cat, "tech");
    }

    #[test]
    fn test_categorize_track_stereo_left() {
        let (_, pair, side) = categorize_track("DRUMS L");
        assert_eq!(pair, Some("drums".to_string()));
        assert_eq!(side, Some("L".to_string()));
    }

    #[test]
    fn test_categorize_track_stereo_right() {
        let (_, pair, side) = categorize_track("DRUMS R");
        assert_eq!(pair, Some("drums".to_string()));
        assert_eq!(side, Some("R".to_string()));
    }

    #[test]
    fn test_parse_reaper_value_valid() {
        let input = "VOL\t0.716\n";
        assert_eq!(parse_reaper_value(input), Some(0.716));
    }

    #[test]
    fn test_parse_reaper_value_multiline() {
        let input = "NTRACK\t10\nTRACK\t1\tname\t0.5";
        // Should return first parseable value
        assert_eq!(parse_reaper_value(input), Some(10.0));
    }

    #[test]
    fn test_parse_reaper_value_invalid() {
        let input = "ERROR";
        assert_eq!(parse_reaper_value(input), None);
    }

    // ================================================================
    // REAPER SEND response parsing tests - CRITICAL for reading state!
    // ================================================================

    #[test]
    fn test_parse_send_volume() {
        // Actual REAPER response: SEND\ttrack\tsend\tflag\tVOLUME\tpan\tmode
        let input = "SEND\t1\t1\t0\t0.300000\t0.000000\t24";
        assert_eq!(parse_send_volume(input), Some(0.300000));
    }

    #[test]
    fn test_parse_send_volume_unity() {
        let input = "SEND\t1\t2\t0\t1.000000\t0.000000\t24";
        let vol = parse_send_volume(input).unwrap();
        assert!((vol - 1.0).abs() < 0.001, "Expected 1.0, got {}", vol);
    }

    #[test]
    fn test_parse_send_mute_on() {
        // Flag at position 3 is a bitfield: bit 3 (value 8) = muted
        let input = "SEND\t1\t1\t8\t1.000000\t0.000000\t24";
        assert_eq!(parse_send_mute(input), Some(true));
    }

    #[test]
    fn test_parse_send_mute_off() {
        let input = "SEND\t1\t1\t0\t1.000000\t0.000000\t24";
        assert_eq!(parse_send_mute(input), Some(false));
    }

    #[test]
    fn test_parse_send_pan_center() {
        // Pan is at position 5 (0.0 = center in REAPER)
        let input = "SEND\t1\t1\t0\t1.000000\t0.000000\t24";
        assert_eq!(parse_send_pan(input), Some(0.0));
    }

    #[test]
    fn test_parse_send_pan_left() {
        let input = "SEND\t1\t1\t0\t1.000000\t-1.000000\t24";
        assert_eq!(parse_send_pan(input), Some(-1.0));
    }

    #[test]
    fn test_parse_send_pan_right() {
        let input = "SEND\t1\t1\t0\t1.000000\t1.000000\t24";
        assert_eq!(parse_send_pan(input), Some(1.0));
    }

    #[test]
    fn test_parse_send_invalid_response() {
        // Non-SEND response should return None
        let input = "TRACK\t1\tname\t0.5";
        assert_eq!(parse_send_volume(input), None);
        assert_eq!(parse_send_mute(input), None);
        assert_eq!(parse_send_pan(input), None);
    }

    // ================================================================
    // REAPER API URL format tests - CRITICAL for controls to work!
    // ================================================================

    #[test]
    fn test_reaper_url_must_have_underscore_prefix() {
        // CRITICAL: REAPER HTTP API requires /_/ prefix for all commands
        // Without this, REAPER returns empty responses and controls don't work!
        let base = "http://127.0.0.1:8080";

        // All URLs must start with base/_/
        assert!(
            reaper_api::set_send_vol(base, 1, 1, 0.5).contains("/_/"),
            "set_send_vol must use /_/ prefix"
        );
        assert!(
            reaper_api::set_send_mute(base, 1, 1, 0).contains("/_/"),
            "set_send_mute must use /_/ prefix"
        );
        assert!(
            reaper_api::set_send_pan(base, 1, 1, 0.5).contains("/_/"),
            "set_send_pan must use /_/ prefix"
        );
        assert!(
            reaper_api::get_send_vol(base, 1, 1).contains("/_/"),
            "get_send_vol must use /_/ prefix"
        );
        assert!(
            reaper_api::get_send_mute(base, 1, 1).contains("/_/"),
            "get_send_mute must use /_/ prefix"
        );
        assert!(
            reaper_api::get_send_pan(base, 1, 1).contains("/_/"),
            "get_send_pan must use /_/ prefix"
        );
        assert!(
            reaper_api::query_tracks(base).contains("/_/"),
            "query_tracks must use /_/ prefix"
        );
        assert!(
            reaper_api::set_track_vol(base, 1, 0.5).contains("/_/"),
            "set_track_vol must use /_/ prefix"
        );
        assert!(
            reaper_api::set_track_mute(base, 1, 0).contains("/_/"),
            "set_track_mute must use /_/ prefix"
        );
    }

    #[test]
    fn test_reaper_url_set_send_vol_format() {
        let url = reaper_api::set_send_vol("http://127.0.0.1:8080", 1, 2, 1.0);
        assert_eq!(url, "http://127.0.0.1:8080/_/SET/TRACK/1/SEND/2/VOL/1");
    }

    #[test]
    fn test_reaper_url_set_send_mute_format() {
        let url = reaper_api::set_send_mute("http://127.0.0.1:8080", 3, 4, 1);
        assert_eq!(url, "http://127.0.0.1:8080/_/SET/TRACK/3/SEND/4/MUTE/1");
    }

    #[test]
    fn test_reaper_url_set_send_pan_format() {
        let url = reaper_api::set_send_pan("http://127.0.0.1:8080", 5, 6, 0.25);
        assert_eq!(url, "http://127.0.0.1:8080/_/SET/TRACK/5/SEND/6/PAN/0.25");
    }

    #[test]
    fn test_reaper_url_get_send_vol_format() {
        let url = reaper_api::get_send_vol("http://127.0.0.1:8080", 1, 1);
        assert_eq!(url, "http://127.0.0.1:8080/_/GET/TRACK/1/SEND/1/VOL");
    }

    #[test]
    fn test_reaper_url_query_tracks_format() {
        let url = reaper_api::query_tracks("http://127.0.0.1:8080");
        assert_eq!(url, "http://127.0.0.1:8080/_/NTRACK;TRACK");
    }

    #[test]
    fn test_reaper_url_set_track_vol_format() {
        let url = reaper_api::set_track_vol("http://127.0.0.1:8080", 23, 1.0);
        assert_eq!(url, "http://127.0.0.1:8080/_/SET/TRACK/23/VOL/1");
    }

    #[test]
    fn test_reaper_url_set_track_mute_format() {
        let url = reaper_api::set_track_mute("http://127.0.0.1:8080", 23, 1);
        assert_eq!(url, "http://127.0.0.1:8080/_/SET/TRACK/23/MUTE/1");
    }

    #[test]
    fn test_reaper_url_get_extstate_format() {
        let url = reaper_api::get_extstate("http://127.0.0.1:8080", "REAPERIEM_METERS", "peaks");
        assert_eq!(
            url,
            "http://127.0.0.1:8080/_/GET/EXTSTATE/REAPERIEM_METERS/peaks"
        );
    }

    #[test]
    fn test_reaper_url_trigger_action_format() {
        let url = reaper_api::trigger_action("http://127.0.0.1:8080", "_RS_REAPERIEM_METER_BRIDGE");
        assert_eq!(url, "http://127.0.0.1:8080/_/_RS_REAPERIEM_METER_BRIDGE");
    }

    // ================================================================
    // Pan conversion tests - CRITICAL for correct pan display!
    // ================================================================

    #[test]
    fn test_reaper_pan_to_ui_center() {
        // REAPER center (0.0) -> UI center (0.5)
        let ui_pan = reaper_pan_to_ui(0.0);
        assert!(
            (ui_pan - 0.5).abs() < 0.001,
            "REAPER 0.0 should be UI 0.5, got {}",
            ui_pan
        );
    }

    #[test]
    fn test_reaper_pan_to_ui_left() {
        // REAPER left (-1.0) -> UI left (0.0)
        let ui_pan = reaper_pan_to_ui(-1.0);
        assert!(
            ui_pan.abs() < 0.001,
            "REAPER -1.0 should be UI 0.0, got {}",
            ui_pan
        );
    }

    #[test]
    fn test_reaper_pan_to_ui_right() {
        // REAPER right (1.0) -> UI right (1.0)
        let ui_pan = reaper_pan_to_ui(1.0);
        assert!(
            (ui_pan - 1.0).abs() < 0.001,
            "REAPER 1.0 should be UI 1.0, got {}",
            ui_pan
        );
    }

    #[test]
    fn test_ui_pan_to_reaper_center() {
        // UI center (0.5) -> REAPER center (0.0)
        let reaper_pan = ui_pan_to_reaper(0.5);
        assert!(
            reaper_pan.abs() < 0.001,
            "UI 0.5 should be REAPER 0.0, got {}",
            reaper_pan
        );
    }

    #[test]
    fn test_ui_pan_to_reaper_left() {
        // UI left (0.0) -> REAPER left (-1.0)
        let reaper_pan = ui_pan_to_reaper(0.0);
        assert!(
            (reaper_pan - (-1.0)).abs() < 0.001,
            "UI 0.0 should be REAPER -1.0, got {}",
            reaper_pan
        );
    }

    #[test]
    fn test_ui_pan_to_reaper_right() {
        // UI right (1.0) -> REAPER right (1.0)
        let reaper_pan = ui_pan_to_reaper(1.0);
        assert!(
            (reaper_pan - 1.0).abs() < 0.001,
            "UI 1.0 should be REAPER 1.0, got {}",
            reaper_pan
        );
    }

    #[test]
    fn test_pan_conversion_roundtrip() {
        // Test roundtrip conversion at various UI positions
        for ui_pan in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let reaper = ui_pan_to_reaper(ui_pan);
            let back = reaper_pan_to_ui(reaper);
            assert!(
                (back - ui_pan).abs() < 0.001,
                "Roundtrip failed for UI {}: REAPER {} -> UI {}",
                ui_pan,
                reaper,
                back
            );
        }
    }

    // ================================================================
    // reaperiem#203: snapshot/preset RESTORE must convert stored UI pan (0..1)
    // back to REAPER pan (-1..1). Before the fix, restore wrote the
    // stored value RAW, so center (0.0 REAPER) came back as 0.5 =
    // half-right and every channel's panorama shifted right.
    // ================================================================

    #[test]
    fn test_restore_send_pan_converts_ui_to_reaper() {
        // The value fed to `SET/.../SEND/{}/PAN/{:.6}` must be REAPER-range.
        assert_eq!(
            format!("{:.6}", restore_send_pan(0.5)),
            "0.000000",
            "stored UI center (0.5) must restore to REAPER center (0.0), not 0.5 (half-right) — #203"
        );
        assert_eq!(
            format!("{:.6}", restore_send_pan(0.0)),
            "-1.000000",
            "stored UI hard-left (0.0) must restore to REAPER -1.0 — #203"
        );
        assert_eq!(
            format!("{:.6}", restore_send_pan(1.0)),
            "1.000000",
            "stored UI hard-right (1.0) must restore to REAPER 1.0 — #203"
        );
    }

    #[test]
    fn test_restore_send_pan_roundtrip_from_reaper() {
        // Full capture->store->restore round-trip: a REAPER pan read by the
        // poller (reaper_pan_to_ui) then stored, must restore to the same
        // REAPER value. Identity round-trip is the guarantee reaperiem#203 restores.
        for reaper_pan in [-1.0_f32, -0.5, 0.0, 0.5, 1.0] {
            let stored_ui = reaper_pan_to_ui(reaper_pan); // what the poller/snapshot stores
            let restored = restore_send_pan(stored_ui); // what restore writes back
            assert!(
                (restored - reaper_pan).abs() < 0.001,
                "round-trip failed: REAPER {} -> stored UI {} -> restored {} (#203)",
                reaper_pan,
                stored_ui,
                restored
            );
        }
    }

    #[test]
    fn test_pan_conversion_clamps() {
        // Test that out-of-range values are clamped
        assert!((reaper_pan_to_ui(-2.0) - 0.0).abs() < 0.001);
        assert!((reaper_pan_to_ui(2.0) - 1.0).abs() < 0.001);
        assert!((ui_pan_to_reaper(-1.0) - (-1.0)).abs() < 0.001);
        assert!((ui_pan_to_reaper(2.0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_reaper_pan_to_ui_nan_maps_to_center() {
        // A malformed REAPER response could yield NaN; we must not leak it
        // into the UI pan state. The inline NaN guard in reaper_pan_to_ui
        // returns 0.5 (UI center) directly, avoiding any NaN propagation
        // into the arithmetic expression below it.
        let ui_pan = reaper_pan_to_ui(f32::NAN);
        assert!(
            (ui_pan - 0.5).abs() < 0.001,
            "NaN pan should map to UI center 0.5, got {}",
            ui_pan
        );
    }

    #[test]
    fn test_validate_pan_value_accepts_valid_pans() {
        // Kills body-replace-with-Err and the "delete !" mutant
        // (which would flip the condition and reject valid pans).
        assert!(validate_pan_value(-1.0).is_ok());
        assert!(validate_pan_value(-0.5).is_ok());
        assert!(validate_pan_value(0.0).is_ok());
        assert!(validate_pan_value(0.5).is_ok());
        assert!(validate_pan_value(1.0).is_ok());
    }

    #[test]
    fn test_validate_pan_value_rejects_out_of_range() {
        // Kills body-replace-with-Ok and the "delete !" mutant
        // (which would flip the condition and accept invalid pans).
        assert!(validate_pan_value(-1.0001).is_err());
        assert!(validate_pan_value(1.0001).is_err());
        assert!(validate_pan_value(-2.0).is_err());
        assert!(validate_pan_value(2.0).is_err());
    }

    #[test]
    fn test_validate_pan_value_rejects_non_finite() {
        assert!(validate_pan_value(f32::NAN).is_err());
        assert!(validate_pan_value(f32::INFINITY).is_err());
        assert!(validate_pan_value(f32::NEG_INFINITY).is_err());
    }

    #[test]
    fn test_validate_pan_value_error_message() {
        let err = validate_pan_value(2.0).unwrap_err();
        assert!(
            err.contains("-1.0") && err.contains("1.0"),
            "error message should mention the valid range, got: {}",
            err
        );
    }

    #[test]
    fn test_parse_send_state_full() {
        let input = "SEND\t1\t0\t0\t1.000000\t0.000000\t24";
        let (vol, mute, pan) = parse_send_state(input).unwrap();
        assert!((vol - 1.0).abs() < 0.001);
        assert!(!mute);
        assert!((pan - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_send_state_muted() {
        // Mute flag 8 = bit 3 set = muted
        let input = "SEND\t1\t0\t8\t0.300000\t-0.500000\t24";
        let (vol, mute, pan) = parse_send_state(input).unwrap();
        assert!((vol - 0.3).abs() < 0.001);
        assert!(mute);
        assert!((pan - (-0.5)).abs() < 0.001);
    }

    #[test]
    fn test_parse_send_state_invalid() {
        assert!(parse_send_state("TRACK\t1\tname").is_none());
        assert!(parse_send_state("").is_none());
    }

    #[test]
    fn test_reaper_url_get_send_state_format() {
        let url = reaper_api::get_send_state("http://127.0.0.1:8080", 1, 2);
        assert_eq!(url, "http://127.0.0.1:8080/_/GET/TRACK/1/SEND/2");
        assert!(url.contains("/_/"), "get_send_state must use /_/ prefix");
    }

    // ================================================================
    // Volume roundtrip precision tests (echo suppression depends on this)
    // ================================================================

    #[test]
    fn test_db_roundtrip_precision_full_range() {
        // With quantize_02, the cache pre-write stores quantize_02(reaper_vol_to_db(vol)).
        // The poller also quantizes. Both should produce identical values.
        for db_5x in (-300..=60).step_by(1) {
            let db = db_5x as f32 / 5.0; // 0.2 dB steps
            let vol = db_to_reaper_vol(db);
            let cached = quantize_02(reaper_vol_to_db(vol));
            let polled = quantize_02(reaper_vol_to_db(vol));
            assert_eq!(
                cached, polled,
                "Cache and poller disagree at {:.1}dB: cached={:.4}, polled={:.4}",
                db, cached, polled
            );
        }
    }

    #[test]
    fn test_cache_prewrite_prevents_diff_detection() {
        // Simulate cache pre-write with quantize_02:
        // 1. User sends SetLevel { level_db: -12.0 }
        // 2. Cache stores quantize_02(reaper_vol_to_db(db_to_reaper_vol(-12.0)))
        // 3. Poller reads REAPER value, stores quantize_02(reaper_vol_to_db(vol))
        // 4. Diff should be 0.0 (identical values, no broadcast)
        let user_db = -12.0_f32;
        let reaper_vol = db_to_reaper_vol(user_db);
        let cached_db = quantize_02(reaper_vol_to_db(reaper_vol));
        let polled_db = quantize_02(reaper_vol_to_db(reaper_vol));

        let diff = (cached_db - polled_db).abs();
        assert!(
            diff < 0.05,
            "Cache pre-write value ({}) and polled value ({}) differ by {} dB (threshold 0.05)",
            cached_db,
            polled_db,
            diff
        );
    }

    // ================================================================
    // Track categorization regression tests - HAND tracks must be tech
    // ================================================================

    #[test]
    fn test_categorize_hand_mic_as_tech() {
        // Bug: "HAND1 mic" contains "mic" → wrongly categorized as "mics"
        // HAND tracks must always be "tech", even though they have "mic" in the name
        let (cat, _, _) = categorize_track("HAND1 mic");
        assert_eq!(cat, "tech", "HAND1 mic must be tech, not mics");

        let (cat, _, _) = categorize_track("HAND2 mic");
        assert_eq!(cat, "tech");

        let (cat, _, _) = categorize_track("HAND3 mic");
        assert_eq!(cat, "tech");

        let (cat, _, _) = categorize_track("HAND4 mic");
        assert_eq!(cat, "tech");

        let (cat, _, _) = categorize_track("ENGINEER mic");
        assert_eq!(cat, "tech", "ENGINEER mic must be tech, not mics");
    }

    #[test]
    fn test_build_channel_templates_uses_config_category() {
        // When InputTrack has Some(category), it wins over name-based derivation.
        let inputs = vec![iem_core::config::InputTrack {
            name: "MEMBER7 kl L".to_string(),
            dante_input: 113,
            default_level_db: 0.0,
            category: Some("mics".to_string()),
            stereo_pair: Some("member7 kl".to_string()),
        }];
        let channels = build_channel_templates(&inputs, None);
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].category, "mics");
        assert_eq!(channels[0].stereo_pair, Some("member7 kl".to_string()));
        assert_eq!(channels[0].stereo_side, Some("L".to_string()));
    }

    #[test]
    fn test_build_channel_templates_fallback_when_no_config_category() {
        // When InputTrack has None category, falls back to categorize_track().
        let inputs = vec![iem_core::config::InputTrack {
            name: "MEMBER3 mic".to_string(),
            dante_input: 105,
            default_level_db: 0.0,
            category: None,
            stereo_pair: None,
        }];
        let channels = build_channel_templates(&inputs, None);
        assert_eq!(channels[0].category, "mics");
        assert_eq!(channels[0].stereo_pair, None);
        assert_eq!(channels[0].stereo_side, None);
    }

    #[test]
    fn test_derive_stereo_side() {
        assert_eq!(derive_stereo_side("MEMBER7 kl L"), Some("L".to_string()));
        assert_eq!(derive_stereo_side("MEMBER7 kl R"), Some("R".to_string()));
        assert_eq!(derive_stereo_side("MEMBER7 kl"), None);
        assert_eq!(derive_stereo_side("MEMBER3 mic"), None);
        assert_eq!(derive_stereo_side("DRUMS L"), Some("L".to_string()));
    }

    /// Regression test for reaperiem#179: MEMBER7 kl was added to the REAPER project at
    /// track index 44 (after 10 inears + TRANSLATOR + 10 stems) but it is
    /// the 23rd entry in config.inputs. The old validator used
    /// `ti <= inputs.len()` (23), which silently rejected every SetMute /
    /// SetLevel / SetPan for MEMBER7 kl during a live service — members could
    /// not mute or adjust the keyboard, it stayed audibly pinned at unity.
    /// The fix validates against REAPER-resolved indices instead of the
    /// input count.
    #[test]
    fn test_collect_valid_input_indices_accepts_out_of_position_reaper_index() {
        let inputs = vec![
            iem_core::config::InputTrack {
                name: "OLDMEMBER1 mic".to_string(),
                dante_input: 101,
                default_level_db: 0.0,
                category: None,
                stereo_pair: None,
            },
            iem_core::config::InputTrack {
                name: "MEMBER7 kl".to_string(),
                dante_input: 113,
                default_level_db: 0.0,
                category: Some("mics".to_string()),
                stereo_pair: None,
            },
        ];
        let mut resolved = HashMap::new();
        resolved.insert("OLDMEMBER1 mic".to_string(), 1);
        resolved.insert("MEMBER7 kl".to_string(), 44);

        let valid = collect_valid_input_indices(&inputs, &resolved);

        assert!(
            valid.contains(&1),
            "OLDMEMBER1 mic at REAPER track 1 must be valid"
        );
        assert!(
            valid.contains(&44),
            "MEMBER7 kl at REAPER track 44 must be valid — regression from #179"
        );
        assert!(
            !valid.contains(&2),
            "REAPER track 2 (not an input track in this setup) must NOT be valid"
        );
        // A validator that used `ti <= inputs.len()` would accept 2 and
        // reject 44 — exact inverse of what's correct. Guard against that:
        assert!(
            valid.len() == 2 && valid.contains(&1) && valid.contains(&44),
            "validator must match discovered REAPER indices, not config position count"
        );
    }

    /// cargo-mutants MISSED: `|| → &&` in is_valid_track's OR-clause. This
    /// set of tests pins down the exact truth table so any single-operator
    /// mutation flips at least one case.
    #[test]
    fn test_is_valid_track_index_input_only() {
        let mut inputs = std::collections::HashSet::new();
        inputs.insert(5usize);
        let mixes: Vec<usize> = vec![];
        assert!(is_valid_track_index(5, &inputs, &mixes));
        assert!(!is_valid_track_index(6, &inputs, &mixes));
    }

    #[test]
    fn test_is_valid_track_index_mix_only() {
        let inputs = std::collections::HashSet::new();
        let mixes: Vec<usize> = vec![42, 43];
        assert!(is_valid_track_index(42, &inputs, &mixes));
        assert!(!is_valid_track_index(5, &inputs, &mixes));
    }

    #[test]
    fn test_is_valid_track_index_or_not_and() {
        // Regression guard for cargo-mutants `|| → &&`: with && the truth
        // table flips — a track in ONLY the input set would be rejected.
        // MEMBER7 kl (track 44) is in valid_input_indices but NOT in
        // mix_track_indices for a regular member; the fix MUST accept it.
        let mut inputs = std::collections::HashSet::new();
        inputs.insert(44usize);
        let mixes: Vec<usize> = vec![50, 51];
        assert!(
            is_valid_track_index(44, &inputs, &mixes),
            "MEMBER7 kl in inputs only MUST be accepted — && instead of || would reject it"
        );
        assert!(
            is_valid_track_index(50, &inputs, &mixes),
            "mix track 50 in mixes only MUST be accepted"
        );
        assert!(
            !is_valid_track_index(99, &inputs, &mixes),
            "track 99 in neither set MUST be rejected"
        );
    }

    /// cargo-mutants MISSED: `== → !=` in the `find` predicate of the
    /// track_name reverse-lookup. With `!=`, find returns the first
    /// non-matching entry — a wrong name that flows into tracing logs.
    #[test]
    fn test_lookup_input_name_returns_correct_name_for_reaper_index() {
        let mut resolved = HashMap::new();
        resolved.insert("MEMBER1 mic".to_string(), 1);
        resolved.insert("MEMBER7 kl".to_string(), 44);
        resolved.insert("ENGINEER mic".to_string(), 22);
        let inputs: Vec<iem_core::config::InputTrack> = vec![];

        assert_eq!(
            lookup_input_name(&resolved, 44, &inputs),
            Some("MEMBER7 kl".to_string()),
            "must return exact match for track 44 — `!=` mutation returns a different entry"
        );
        assert_eq!(
            lookup_input_name(&resolved, 1, &inputs),
            Some("MEMBER1 mic".to_string())
        );
        assert_eq!(
            lookup_input_name(&resolved, 22, &inputs),
            Some("ENGINEER mic".to_string())
        );
    }

    #[test]
    fn test_lookup_input_name_falls_back_to_position_when_not_resolved() {
        let resolved = HashMap::new();
        let inputs = vec![
            iem_core::config::InputTrack {
                name: "A".to_string(),
                dante_input: 101,
                default_level_db: 0.0,
                category: None,
                stereo_pair: None,
            },
            iem_core::config::InputTrack {
                name: "B".to_string(),
                dante_input: 102,
                default_level_db: 0.0,
                category: None,
                stereo_pair: None,
            },
        ];
        assert_eq!(
            lookup_input_name(&resolved, 1, &inputs),
            Some("A".to_string())
        );
        assert_eq!(
            lookup_input_name(&resolved, 2, &inputs),
            Some("B".to_string())
        );
        assert_eq!(lookup_input_name(&resolved, 99, &inputs), None);
    }

    /// cargo-mutants MISSED: `!` deletion in validate_track_index — the
    /// inverted branch would reject every valid index and accept every
    /// invalid one. Test both branches to pin the `!`.
    #[test]
    fn test_validate_track_index_accepts_member_and_rejects_non_member() {
        let mut valid = std::collections::HashSet::new();
        valid.insert(44usize);
        assert!(
            validate_track_index(44, &valid).is_ok(),
            "track 44 in the valid set must validate OK — `!` deletion would return Err"
        );
        assert!(
            validate_track_index(99, &valid).is_err(),
            "track 99 not in the valid set must validate Err — `!` deletion would return Ok"
        );
    }

    #[test]
    fn test_collect_valid_input_indices_falls_back_to_position_when_unresolved() {
        // Before the poller has populated input_track_indices, inputs
        // resolve to their 1-based config position (matches
        // build_channel_templates fallback).
        let inputs = vec![
            iem_core::config::InputTrack {
                name: "OLDMEMBER1 mic".to_string(),
                dante_input: 101,
                default_level_db: 0.0,
                category: None,
                stereo_pair: None,
            },
            iem_core::config::InputTrack {
                name: "MEMBER2 mic".to_string(),
                dante_input: 102,
                default_level_db: 0.0,
                category: None,
                stereo_pair: None,
            },
        ];
        let resolved = HashMap::new();
        let valid = collect_valid_input_indices(&inputs, &resolved);
        assert!(valid.contains(&1));
        assert!(valid.contains(&2));
        assert_eq!(valid.len(), 2);
    }

    #[test]
    fn test_categorize_regular_mic_still_mics() {
        // Regular member mics must still be categorized as "mics"
        let (cat, _, _) = categorize_track("OLDMEMBER1 mic");
        assert_eq!(cat, "mics");

        let (cat, _, _) = categorize_track("MEMBER2 mic");
        assert_eq!(cat, "mics");

        let (cat, _, _) = categorize_track("MEMBER3 gtr");
        assert_eq!(cat, "mics");
    }

    /// Helper: parse stereo meters from NTRACK text (mirrors poll_mixer_state logic)
    fn parse_meters_from_ntrack(text: &str) -> HashMap<usize, [f32; 2]> {
        let mut meters = HashMap::new();
        for line in text.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.first() == Some(&"TRACK")
                && parts.len() >= 14
                && let Ok(track_idx) = parts[1].parse::<usize>()
                && let (Ok(peak_db10), Ok(pos_db10)) =
                    (parts[6].parse::<f32>(), parts[7].parse::<f32>())
            {
                let db10_to_linear = |v: f32| -> f32 {
                    if v <= -1500.0 {
                        0.0
                    } else {
                        10.0_f32.powf(v / 10.0 / 20.0)
                    }
                };
                meters.insert(
                    track_idx,
                    [db10_to_linear(peak_db10), db10_to_linear(pos_db10)],
                );
            }
        }
        meters
    }

    /// 14-field TRACK line should produce meter data with correct dB×10 conversion
    #[test]
    fn test_ntrack_14_fields_produces_meter() {
        // -100 dB×10 = -10 dB, -80 dB×10 = -8 dB
        let line =
            "TRACK\t1\tOLDMEMBER1 mic\t0\t1.000000\t0.000000\t-100\t-80\t1.000000\t0\t9\t0\t0\t0";
        let meters = parse_meters_from_ntrack(line);
        assert!(meters.contains_key(&1));
        let [left, right] = meters[&1];
        // -100 dB×10 = -10.0 dB → 10^(-10/20) ≈ 0.3162
        assert!(
            (left - 0.3162).abs() < 0.01,
            "-100 (dB×10) L should be ~0.3162 linear, got {}",
            left
        );
        // -80 dB×10 = -8.0 dB → 10^(-8/20) ≈ 0.3981
        assert!(
            (right - 0.3981).abs() < 0.01,
            "-80 (dB×10) R should be ~0.3981 linear, got {}",
            right
        );
    }

    /// 12-field TRACK line (no meter fields) must NOT produce meter data.
    /// Without meter fields, field[6] is width (1.000000), not a dB value.
    #[test]
    fn test_ntrack_12_fields_no_meter() {
        let line = "TRACK\t1\tOLDMEMBER1 mic\t0\t1.000000\t0.000000\t1.000000\t0\t9\t0\t0\t0";
        let meters = parse_meters_from_ntrack(line);
        assert!(
            !meters.contains_key(&1),
            "12-field line must NOT produce meter — field[6] is width, not meter"
        );
    }

    /// Regression: width value 1.000000 must NOT be parsed as meter data
    #[test]
    fn test_ntrack_width_not_parsed_as_meter() {
        let text = "\
NTRACK\t3
TRACK\t1\tOLDMEMBER1 mic\t0\t1.000000\t0.000000\t1.000000\t0\t9\t0\t0\t0
TRACK\t2\tMEMBER2 mic\t0\t1.000000\t0.000000\t1.000000\t0\t9\t0\t0\t0
TRACK\t3\tMEMBER3 mic\t0\t1.000000\t0.000000\t1.000000\t0\t9\t0\t0\t0";
        let meters = parse_meters_from_ntrack(text);
        assert!(
            meters.is_empty(),
            "No 12-field tracks should produce meters, got {} entries",
            meters.len()
        );
    }

    /// REAPER meter floor (-1500 = -150 dB) must produce 0.0 (silence)
    #[test]
    fn test_reaper_meter_floor_is_silence() {
        let line = "TRACK\t1\tOLDMEMBER1 mic\t192\t1.000000\t0.000000\t-1500\t-1500\t1.000000\t3\t9\t0\t0\t24421844";
        let meters = parse_meters_from_ntrack(line);
        assert_eq!(
            meters.get(&1),
            Some(&[0.0, 0.0]),
            "-1500 (dB×10 = -150 dB) must be silence"
        );
    }

    /// Values above REAPER meter floor should produce signal
    #[test]
    fn test_reaper_above_floor_shows_signal() {
        // -140 dB×10 = -14 dB, -120 dB×10 = -12 dB
        let line = "TRACK\t1\tOLDMEMBER1 mic\t192\t1.000000\t0.000000\t-140\t-120\t1.000000\t3\t9\t0\t0\t24421844";
        let meters = parse_meters_from_ntrack(line);
        let [left, right] = meters[&1];
        assert!(
            left > 0.0,
            "-140 (dB×10) L should show signal, got {}",
            left
        );
        assert!(
            right > 0.0,
            "-120 (dB×10) R should show signal, got {}",
            right
        );
    }

    /// Parse captured live NTRACK response — all tracks silent at -1500 floor
    /// Data captured 2026-02-28 from: curl -s "http://127.0.0.1:8080/_/NTRACK;TRACK"
    #[test]
    fn test_reaper_captured_ntrack_all_silent() {
        let text = "\
NTRACK\t33
TRACK\t1\tOLDMEMBER1 mic\t192\t1.000000\t0.000000\t-1500\t-1500\t1.000000\t3\t9\t0\t0\t24421844
TRACK\t2\tMEMBER2 mic\t192\t1.000000\t0.000000\t-1500\t-1500\t1.000000\t3\t9\t0\t0\t24421844
TRACK\t3\tMEMBER3 mic\t192\t1.000000\t0.000000\t-1500\t-1500\t1.000000\t3\t9\t0\t0\t24421844";
        let meters = parse_meters_from_ntrack(text);
        for (idx, [left, right]) in &meters {
            assert_eq!(*left, 0.0, "Track {} L should be silent, got {}", idx, left);
            assert_eq!(
                *right, 0.0,
                "Track {} R should be silent, got {}",
                idx, right
            );
        }
    }

    // ================================================================
    // build_mix_channel_templates tests (v1.49.0 Engineer Mixes Tab)
    // ================================================================

    fn make_discovered_members() -> Vec<iem_core::DiscoveredMember> {
        vec![
            iem_core::DiscoveredMember {
                name: "MEMBER1".to_string(),
                track_index: 23,
                dante_output_l: 71,
                dante_output_r: 72,
                send_index: 0,
                mix_send_index: Some(1),
                mix_send_indices: std::collections::HashMap::new(),
            },
            iem_core::DiscoveredMember {
                name: "MEMBER2".to_string(),
                track_index: 24,
                dante_output_l: 73,
                dante_output_r: 74,
                send_index: 1,
                mix_send_index: Some(1),
                mix_send_indices: std::collections::HashMap::new(),
            },
            iem_core::DiscoveredMember {
                name: "MEMBER3".to_string(),
                track_index: 25,
                dante_output_l: 75,
                dante_output_r: 76,
                send_index: 2,
                mix_send_index: Some(1),
                mix_send_indices: std::collections::HashMap::new(),
            },
            iem_core::DiscoveredMember {
                name: "ENGINEER".to_string(),
                track_index: 32,
                dante_output_l: 91,
                dante_output_r: 92,
                send_index: 9,
                mix_send_index: None,
                mix_send_indices: std::collections::HashMap::new(),
            },
        ]
    }

    #[test]
    fn test_mix_channels_exclude_engineer() {
        let discovered = make_discovered_members();
        let mix = build_mix_channel_templates(&discovered, "engineer");
        // Should have 3 channels (MEMBER1, MEMBER2, MEMBER3) — engineer excluded
        assert_eq!(mix.len(), 3);
        assert!(
            !mix.iter().any(|ch| ch.name == "ENGINEER"),
            "Engineer should be excluded from mix channels"
        );
    }

    #[test]
    fn test_mix_channels_have_correct_category() {
        let discovered = make_discovered_members();
        let mix = build_mix_channel_templates(&discovered, "engineer");
        for ch in &mix {
            assert_eq!(
                ch.category, "mixes",
                "Mix channels must have category 'mixes'"
            );
        }
    }

    #[test]
    fn test_mix_channels_use_member_track_index() {
        let discovered = make_discovered_members();
        let mix = build_mix_channel_templates(&discovered, "engineer");
        assert_eq!(mix[0].track_index, 23, "MEMBER1 track_index");
        assert_eq!(mix[1].track_index, 24, "MEMBER2 track_index");
        assert_eq!(mix[2].track_index, 25, "MEMBER3 track_index");
    }

    #[test]
    fn test_mix_channels_name_strips_inear_suffix() {
        // DiscoveredMember.name already excludes " inear" — just the uppercase name
        let discovered = make_discovered_members();
        let mix = build_mix_channel_templates(&discovered, "engineer");
        assert_eq!(mix[0].name, "MEMBER1");
        assert_eq!(mix[1].name, "MEMBER2");
        assert_eq!(mix[2].name, "MEMBER3");
    }

    #[test]
    fn test_mix_channels_default_values() {
        let discovered = make_discovered_members();
        let mix = build_mix_channel_templates(&discovered, "engineer");
        for ch in &mix {
            assert_eq!(ch.level_db, 0.0, "Default level should be 0 dB");
            assert_eq!(ch.pan, 0.5, "Default pan should be 0.5 (center)");
            assert!(ch.muted, "Default muted should be true (safe default)");
            assert!(ch.stereo_pair.is_none(), "No stereo pair");
            assert!(ch.stereo_side.is_none(), "No stereo side");
        }
    }

    #[test]
    fn test_mix_channels_empty_when_no_members() {
        let mix = build_mix_channel_templates(&[], "engineer");
        assert!(mix.is_empty());
    }

    #[test]
    fn test_mix_channels_empty_when_only_engineer() {
        let discovered = vec![iem_core::DiscoveredMember {
            name: "ENGINEER".to_string(),
            track_index: 32,
            dante_output_l: 91,
            dante_output_r: 92,
            send_index: 9,
            mix_send_index: None,
            mix_send_indices: std::collections::HashMap::new(),
        }];
        let mix = build_mix_channel_templates(&discovered, "engineer");
        assert!(mix.is_empty(), "No mix channels when only engineer exists");
    }

    // ================================================================
    // Send destination parsing tests — CRITICAL for engineer mute safety
    // ================================================================

    #[test]
    fn test_parse_send_destination_member_to_engineer() {
        // Real REAPER response: member inear track send to engineer (track 32)
        let response = "SEND\t23\t1\t0\t1.00000000\t0.00000000\t32\n";
        assert_eq!(
            parse_send_destination(response),
            Some(32_i32),
            "Should parse destination track 32 (engineer)"
        );
    }

    #[test]
    fn test_parse_send_destination_hw_output_negative() {
        // Real REAPER response: hardware output send has destination -1
        // This MUST return Some(-1), NOT None — None means "send doesn't exist"
        let response = "SEND\t23\t0\t0\t1.00000000\t0.00000000\t-1\n";
        assert_eq!(
            parse_send_destination(response),
            Some(-1_i32),
            "Hardware output destination -1 must parse as Some(-1), not None"
        );
    }

    #[test]
    fn test_parse_send_destination_invalid() {
        assert_eq!(parse_send_destination("TRACK\t1\tname"), None);
        assert_eq!(parse_send_destination(""), None);
        assert_eq!(parse_send_destination("SEND\t1\t0\t0\t1.0\t0.0"), None); // Too few fields
    }

    #[test]
    fn test_parse_send_destination_multiline() {
        // Response might have extra lines
        let response = "SEND\t23\t1\t8\t0.50000000\t0.00000000\t32\nSOMETHING\telse\n";
        assert_eq!(parse_send_destination(response), Some(32_i32));
    }

    #[test]
    fn test_parse_send_destination_none_means_no_send() {
        // Empty response = send doesn't exist (REAPER returned nothing)
        assert_eq!(parse_send_destination(""), None);
        // Non-SEND response = not a send query result
        assert_eq!(parse_send_destination("TRACK\t1\tname"), None);
    }

    // ================================================================
    // Mix send index usage tests — ensures hardcoded 0 is never used
    // ================================================================

    #[test]
    fn test_send_index_for_mix_channel_uses_discovered_index() {
        // Simulate the send_index_for logic with discovered members
        let discovered = make_discovered_members();
        let member_index = 9_usize; // engineer's send_index

        // For a mix track (member inear), should use mix_send_index, not 0
        let mix_track_indices: Vec<usize> = discovered
            .iter()
            .filter(|m| m.id() != "engineer")
            .map(|m| m.track_index)
            .collect();

        let send_index_for = |ti: usize| -> Option<usize> {
            if mix_track_indices.contains(&ti) {
                discovered
                    .iter()
                    .find(|m| m.track_index == ti)
                    .and_then(|m| m.mix_send_index)
            } else {
                Some(member_index)
            }
        };

        // MEMBER1 inear (track 23) → mix_send_index = 1 (NOT 0!)
        assert_eq!(
            send_index_for(23),
            Some(1),
            "Mix channel must use mix_send_index (1), not hardcoded 0"
        );
        // Regular input track (track 5) → member's send_index
        assert_eq!(
            send_index_for(5),
            Some(member_index),
            "Input track should use member's send_index"
        );
    }

    // ================================================================
    // reaperiem#204: the SHARED resolver used by the WS write path AND both REST
    // restore handlers. Before the fix, snapshot_routes/preset_routes
    // applied the member's own send_index to every track, writing mix
    // channels to the wrong send.
    // ================================================================

    #[test]
    fn test_resolve_send_index_mix_channel_uses_discovered_index() {
        // (track_index, mix_send_index) — as compute_mix_members builds it.
        let mix_members = vec![(23_usize, Some(1_usize)), (24, Some(1)), (25, Some(1))];
        let member_index = 0_usize; // e.g. member1's own send index

        // Mix channel (track 23) MUST resolve to its discovered mix_send_index (1),
        // NOT the member's own send index (0) — this is the reaperiem#204 regression.
        assert_eq!(
            resolve_send_index(23, member_index, &mix_members),
            Ok(1),
            "mix channel must use discovered mix_send_index, not member send_index (#204)"
        );
        // Regular input track (5) is not a mix member → member's own send index.
        assert_eq!(
            resolve_send_index(5, member_index, &mix_members),
            Ok(member_index),
            "regular input track uses the member's send index"
        );
        // Missing mix_send_index → SAFETY error, never a silent fallback.
        assert!(
            resolve_send_index(23, member_index, &[(23_usize, None)]).is_err(),
            "missing mix_send_index must be a SAFETY error, not a fallback (#204)"
        );
    }

    #[test]
    fn test_compute_mix_members_engineer_and_regular() {
        let discovered = make_discovered_members();
        // Engineer sees every other member's inear track + its send to engineer.
        let eng = compute_mix_members(&discovered, "engineer");
        assert_eq!(
            eng.len(),
            3,
            "engineer has 3 mix channels (member1/member2/member3)"
        );
        assert!(
            eng.iter().all(|(_, si)| si.is_some()),
            "each mix channel carries a discovered mix_send_index"
        );
        // A regular member has no mix channels.
        assert!(
            compute_mix_members(&discovered, "member2").is_empty(),
            "regular member has no mix channels"
        );
    }

    #[test]
    fn test_compute_mix_members_elevated_uses_mix_send_indices() {
        use std::collections::HashMap;
        // Elevated member (member1) with per-source send indices to HER inear.
        let mut member1_idx = HashMap::new();
        member1_idx.insert("member2".to_string(), 3_usize);
        member1_idx.insert("member3".to_string(), 4_usize);
        let discovered = vec![
            iem_core::DiscoveredMember {
                name: "MEMBER1".to_string(),
                track_index: 23,
                dante_output_l: 71,
                dante_output_r: 72,
                send_index: 0,
                mix_send_index: Some(1),
                mix_send_indices: member1_idx,
            },
            iem_core::DiscoveredMember {
                name: "MEMBER2".to_string(),
                track_index: 24,
                dante_output_l: 73,
                dante_output_r: 74,
                send_index: 1,
                mix_send_index: Some(1),
                mix_send_indices: HashMap::new(),
            },
            iem_core::DiscoveredMember {
                name: "MEMBER3".to_string(),
                track_index: 25,
                dante_output_l: 75,
                dante_output_r: 76,
                send_index: 2,
                mix_send_index: Some(1),
                mix_send_indices: HashMap::new(),
            },
            iem_core::DiscoveredMember {
                name: "ENGINEER".to_string(),
                track_index: 32,
                dante_output_l: 91,
                dante_output_r: 92,
                send_index: 9,
                mix_send_index: None,
                mix_send_indices: HashMap::new(),
            },
        ];
        let mix = compute_mix_members(&discovered, "member1");
        // Excludes the elevated member's OWN track and the engineer track (reaperiem#204).
        assert!(
            !mix.iter().any(|(t, _)| *t == 23),
            "elevated member's own inear track must be excluded"
        );
        assert!(
            !mix.iter().any(|(t, _)| *t == 32),
            "engineer track must be excluded"
        );
        // Each other member's send index comes from THIS elevated member's map.
        assert_eq!(
            mix.iter().find(|(t, _)| *t == 24).and_then(|(_, s)| *s),
            Some(3),
            "member2's inear routes to member1 via send 3"
        );
        assert_eq!(
            mix.iter().find(|(t, _)| *t == 25).and_then(|(_, s)| *s),
            Some(4),
            "member3's inear routes to member1 via send 4"
        );
        assert_eq!(
            mix.len(),
            2,
            "exactly the two non-self, non-engineer members"
        );
    }

    #[test]
    fn test_build_eq_bands_map_drops_empty_and_none_when_all_empty() {
        let band = || iem_core::EqBand {
            band_type: "band".to_string(),
            freq_hz: 1000.0,
            gain_db: 0.0,
            bw: 1.0,
            freq_norm: 0.5,
            gain_norm: 0.25,
            bw_norm: 0.5,
            gain_db_min: -12.0,
            gain_db_max: 12.0,
            enabled: true,
        };
        // Track 5 has EQ, track 7 has none → only 5 survives (reaperiem#205).
        let out = build_eq_bands_map(vec![(5_usize, vec![band()]), (7_usize, vec![])])
            .expect("a non-empty track must yield Some");
        assert_eq!(out.len(), 1, "empty band lists must be dropped");
        assert!(out.contains_key(&5), "the track with EQ is kept");
        assert!(!out.contains_key(&7), "the empty track is dropped");
        // Nothing / all-empty → None (never Some(empty map)).
        assert!(build_eq_bands_map(vec![]).is_none());
        assert!(
            build_eq_bands_map(vec![(7_usize, vec![])]).is_none(),
            "all-empty input must be None, not an empty map"
        );
    }

    #[test]
    fn test_mute_all_uses_mix_send_index_not_zero() {
        // Simulate the MuteAll URL generation for engineer
        let discovered = make_discovered_members();
        let reaper_url = "http://127.0.0.1:8080";

        let mix_urls: Vec<String> = discovered
            .iter()
            .filter(|m| m.id() != "engineer")
            .filter_map(|m| {
                m.mix_send_index
                    .map(|si| reaper_api::set_send_mute(reaper_url, m.track_index, si, 1))
            })
            .collect();

        assert_eq!(mix_urls.len(), 3, "Should have 3 mix mute URLs");

        // ALL URLs must use SEND/1 (mix_send_index), NOT SEND/0 (hw output)
        for url in &mix_urls {
            assert!(
                url.contains("/SEND/1/"),
                "Mute URL must use SEND/1 (engineer send), not SEND/0 (hw out): {}",
                url
            );
            assert!(
                !url.contains("/SEND/0/"),
                "SEND/0 is the hardware output — muting it kills member audio: {}",
                url
            );
        }
    }

    #[test]
    fn test_mute_all_skips_members_without_mix_send_index() {
        // If a member doesn't have mix_send_index, it should be skipped
        let mut discovered = make_discovered_members();
        // Remove mix_send_index from MEMBER1
        discovered[0].mix_send_index = None;

        let mix_urls: Vec<String> = discovered
            .iter()
            .filter(|m| m.id() != "engineer")
            .filter_map(|m| {
                m.mix_send_index.map(|si| {
                    reaper_api::set_send_mute("http://127.0.0.1:8080", m.track_index, si, 1)
                })
            })
            .collect();

        // Only MEMBER2 and MEMBER3 should have URLs (MEMBER1 skipped)
        assert_eq!(
            mix_urls.len(),
            2,
            "Members without mix_send_index must be skipped"
        );
    }

    // ================================================================
    // build_channel_templates resolved indices tests (Issue reaperiem#85)
    // ================================================================

    fn make_test_inputs() -> Vec<iem_core::config::InputTrack> {
        vec![
            iem_core::config::InputTrack {
                name: "OLDMEMBER1 mic".to_string(),
                dante_input: 101,
                default_level_db: 0.0,
                category: None,
                stereo_pair: None,
            },
            iem_core::config::InputTrack {
                name: "MEMBER2 mic".to_string(),
                dante_input: 102,
                default_level_db: 0.0,
                category: None,
                stereo_pair: None,
            },
            iem_core::config::InputTrack {
                name: "DRUMS".to_string(),
                dante_input: 103,
                default_level_db: 0.0,
                category: None,
                stereo_pair: None,
            },
        ]
    }

    #[test]
    fn test_build_channel_templates_fallback_without_resolved() {
        // No resolved map (None) → sequential i+1 (backward compat)
        let inputs = make_test_inputs();
        let channels = build_channel_templates(&inputs, None);
        assert_eq!(channels[0].track_index, 1);
        assert_eq!(channels[1].track_index, 2);
        assert_eq!(channels[2].track_index, 3);
    }

    #[test]
    fn test_build_channel_templates_with_resolved_indices() {
        // Resolved map shifts indices (e.g., track inserted at position 1)
        let inputs = make_test_inputs();
        let mut resolved = std::collections::HashMap::new();
        resolved.insert("OLDMEMBER1 mic".to_string(), 2_usize);
        resolved.insert("MEMBER2 mic".to_string(), 3);
        resolved.insert("DRUMS".to_string(), 4);
        let channels = build_channel_templates(&inputs, Some(&resolved));
        assert_eq!(channels[0].track_index, 2, "OLDMEMBER1 mic should be at 2");
        assert_eq!(channels[1].track_index, 3, "MEMBER2 mic should be at 3");
        assert_eq!(channels[2].track_index, 4, "DRUMS should be at 4");
    }

    #[test]
    fn test_build_channel_templates_partial_resolution() {
        // Some names resolved, others fall back to sequential
        let inputs = make_test_inputs();
        let mut resolved = std::collections::HashMap::new();
        resolved.insert("OLDMEMBER1 mic".to_string(), 5_usize);
        // MEMBER2 mic and DRUMS not in resolved map
        let channels = build_channel_templates(&inputs, Some(&resolved));
        assert_eq!(channels[0].track_index, 5, "OLDMEMBER1 mic resolved to 5");
        assert_eq!(
            channels[1].track_index, 2,
            "MEMBER2 mic falls back to i+1=2"
        );
        assert_eq!(channels[2].track_index, 3, "DRUMS falls back to i+1=3");
    }

    // ================================================================
    // EQ parameter parsing tests
    // ================================================================

    #[test]
    fn test_parse_eq_band_lowshelf_with_formatted_values() {
        // New format with REAPER-formatted display values (fh, gd, bo)
        let s = "b0:lowshelf,fn=0.283000,gn=0.183911,bn=0.295000,fh=250.0,gd=-2.7,bo=1.18";
        let band = parse_eq_band(s).unwrap();
        assert_eq!(band.band_type, "lowshelf");
        assert!((band.freq_norm - 0.283).abs() < 0.001);
        assert!((band.gain_norm - 0.183911).abs() < 0.001);
        assert!((band.bw_norm - 0.295).abs() < 0.001);
        // Display values come from REAPER-formatted fields (accurate)
        assert!(
            (band.freq_hz - 250.0).abs() < 0.1,
            "freq_hz should use fh= value"
        );
        assert!(
            (band.gain_db - -2.7).abs() < 0.1,
            "gain_db should use gd= value"
        );
        assert!((band.bw - 1.18).abs() < 0.01, "bw should use bo= value");
    }

    #[test]
    fn test_parse_eq_band_lowshelf_missing_gd_returns_none() {
        // gd= is mandatory; old format without it must fail closed, not approximate.
        let s = "b0:lowshelf,fn=0.283000,gn=0.184000,bn=0.295000";
        assert!(
            parse_eq_band(s).is_none(),
            "missing gd= must return None (malformed input)"
        );
    }

    #[test]
    fn test_parse_eq_band_regular_with_formatted() {
        let s = "b1:band,fn=0.500000,gn=0.250000,bn=0.500000,fh=632.5,gd=0.0,bo=2.00";
        let band = parse_eq_band(s).unwrap();
        assert_eq!(band.band_type, "band");
        assert!((band.freq_hz - 632.5).abs() < 0.1);
        assert!((band.gain_db - 0.0).abs() < 0.01);
        assert!((band.bw - 2.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_eq_band_regular_missing_gd_returns_none() {
        // gd= is mandatory; old format without it must fail closed.
        let s = "b1:band,fn=0.500000,gn=0.250000,bn=0.500000";
        assert!(
            parse_eq_band(s).is_none(),
            "missing gd= must return None (malformed input)"
        );
    }

    #[test]
    fn test_parse_eq_band_enabled_field() {
        let s = "b4:highpass,fn=0.240000,gn=0.250000,bn=0.500000,fh=212.7,gd=0.0,bo=2.00,en=1";
        let band = parse_eq_band(s).unwrap();
        assert!(band.enabled);

        let s = "b4:highpass,fn=0.240000,gn=0.250000,bn=0.500000,fh=212.7,gd=0.0,bo=2.00,en=0";
        let band = parse_eq_band(s).unwrap();
        assert!(!band.enabled);
    }

    #[test]
    fn test_parse_eq_band_missing_gd_returns_none_for_highpass() {
        // gd= is mandatory; any input without it must fail closed.
        let s = "b4:highpass,fn=0.500000,gn=1.000000,bn=0.500000";
        assert!(
            parse_eq_band(s).is_none(),
            "missing gd= must return None (malformed input)"
        );
    }

    #[test]
    fn test_parse_eq_params_response_ok() {
        let value = "OK:track=3,name=MEMBER3 mic,fx=1,bands=2,gg=0.0dB,bypass=0|b0:lowshelf,fn=0.283000,gn=0.184000,bn=0.295000,fh=250.0,gd=-2.7,bo=1.18|b1:band,fn=0.500000,gn=0.250000,bn=0.500000,fh=632.5,gd=0.0,bo=2.00";
        let msg = parse_eq_params_response(3, value).unwrap();
        match msg {
            iem_core::ServerMsg::EqParams {
                track_index,
                track_name,
                bands,
            } => {
                assert_eq!(track_index, 3);
                assert_eq!(track_name, "MEMBER3 mic");
                assert_eq!(bands.len(), 2);
                assert_eq!(bands[0].band_type, "lowshelf");
                assert_eq!(bands[1].band_type, "band");
            }
            _ => panic!("Expected EqParams"),
        }
    }

    #[test]
    fn test_parse_eq_params_response_no_eq() {
        let value = "NO_EQ:MEMBER3 mic";
        let msg = parse_eq_params_response(3, value).unwrap();
        match msg {
            iem_core::ServerMsg::EqParams {
                track_index,
                track_name,
                bands,
            } => {
                assert_eq!(track_index, 3);
                assert_eq!(track_name, "MEMBER3 mic");
                assert!(bands.is_empty());
            }
            _ => panic!("Expected EqParams"),
        }
    }

    #[test]
    fn test_parse_eq_params_response_error() {
        let value = "ERROR:track_not_found:99";
        assert!(parse_eq_params_response(99, value).is_none());
    }

    #[test]
    fn test_reaper_url_set_extstate_format() {
        let url =
            reaper_api::set_extstate("http://127.0.0.1:8080", "reaperiem", "eq_read_track", "3");
        assert_eq!(
            url,
            "http://127.0.0.1:8080/_/SET/EXTSTATE/reaperiem/eq_read_track/3"
        );
        assert!(url.contains("/_/"), "set_extstate must use /_/ prefix");
    }

    // ---- /api/client-error tests (reaperiem#153) ----

    #[tokio::test]
    async fn client_error_accepts_minimal_body() {
        let report = ClientErrorReport {
            panic_message: "boom".to_string(),
            version: None,
            git_hash: None,
            url: None,
            user_agent: None,
            location: None,
            backtrace: None,
        };
        let status = client_error(axum::Json(report)).await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn client_error_accepts_full_body() {
        let report = ClientErrorReport {
            panic_message: "assertion failed at line 42".to_string(),
            version: Some("1.142.0".to_string()),
            git_hash: Some("abc1234".to_string()),
            url: Some("/engineer".to_string()),
            user_agent: Some("Mozilla/5.0".to_string()),
            location: Some("iem-ui/src/pages/mixer.rs:456:9".to_string()),
            backtrace: Some("  at foo\n  at bar".to_string()),
        };
        let status = client_error(axum::Json(report)).await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT);
    }

    #[test]
    fn client_error_report_deserialize_minimal() {
        let json = r#"{"panic_message":"boom"}"#;
        let report: ClientErrorReport = serde_json::from_str(json).expect("parse");
        assert_eq!(report.panic_message, "boom");
        assert!(report.version.is_none());
        assert!(report.git_hash.is_none());
    }

    #[test]
    fn client_error_report_deserialize_missing_panic_message_fails() {
        let json = r#"{"version":"1.142.0"}"#;
        let result: Result<ClientErrorReport, _> = serde_json::from_str(json);
        assert!(result.is_err(), "should reject body with no panic_message");
    }

    #[test]
    fn client_error_report_deserialize_full_body() {
        let json = r#"{
            "panic_message":"boom",
            "version":"1.142.0",
            "git_hash":"abc",
            "url":"/engineer",
            "user_agent":"UA",
            "location":"file:1:1",
            "backtrace":"trace"
        }"#;
        let report: ClientErrorReport = serde_json::from_str(json).expect("parse");
        assert_eq!(report.panic_message, "boom");
        assert_eq!(report.version.as_deref(), Some("1.142.0"));
        assert_eq!(report.git_hash.as_deref(), Some("abc"));
    }

    // ---- Router-layer integration tests for /api/client-error (reaperiem#153) ----
    //
    // These tests wire the real handler through an axum Router with the same
    // DefaultBodyLimit layer used in routes.rs, so the body size gate is
    // actually exercised. A handler-only unit test would bypass the layer.

    fn client_error_test_router() -> axum::Router {
        use axum::routing::post;
        axum::Router::new().route(
            "/api/client-error",
            post(client_error).layer(axum::extract::DefaultBodyLimit::max(10_240)),
        )
    }

    #[tokio::test]
    async fn client_error_router_accepts_minimal_body() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        let router = client_error_test_router();
        let req = Request::builder()
            .method("POST")
            .uri("/api/client-error")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"panic_message":"router-min"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn client_error_router_rejects_oversize_body() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        // 11 KiB payload — exceeds the 10_240 byte limit on the layer.
        let big = "x".repeat(11 * 1024);
        let body_json = format!(r#"{{"panic_message":"{}"}}"#, big);
        assert!(
            body_json.len() > 10_240,
            "test payload must be larger than the limit"
        );

        let router = client_error_test_router();
        let req = Request::builder()
            .method("POST")
            .uri("/api/client-error")
            .header("content-type", "application/json")
            .body(Body::from(body_json))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn client_error_router_rejects_body_just_under_large() {
        // Sanity: a body just under the 10 KiB limit must still be accepted.
        // Kills the mutation where `DefaultBodyLimit::max(10_240)` could be
        // changed to a smaller literal without any test noticing.
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        // 9 KiB payload inside a JSON envelope — total body ~9250 bytes.
        let payload = "y".repeat(9 * 1024);
        let body_json = format!(r#"{{"panic_message":"{}"}}"#, payload);
        assert!(
            body_json.len() < 10_240,
            "test payload must be smaller than the limit"
        );

        let router = client_error_test_router();
        let req = Request::builder()
            .method("POST")
            .uri("/api/client-error")
            .header("content-type", "application/json")
            .body(Body::from(body_json))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    // Tracing capture test — verifies the handler emits a structured warn
    // event with the expected fields. Uses tracing-test's #[traced_test]
    // which installs a thread-local subscriber and provides logs_contain.
    //
    // Kills mutations on the tracing::warn! macro body that cargo-mutants
    // would otherwise leave uncaught (e.g. the level check conditional).
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn client_error_emits_structured_warn_event() {
        let report = ClientErrorReport {
            panic_message: "tracing-capture-marker-42".to_string(),
            version: Some("9.9.9".to_string()),
            git_hash: Some("deadbee".to_string()),
            url: Some("/trace-test".to_string()),
            user_agent: Some("TraceUA".to_string()),
            location: Some("trace:1:1".to_string()),
            backtrace: None,
        };
        let status = client_error(axum::Json(report)).await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT);

        // Captured logs must contain the structured fields we care about.
        assert!(
            logs_contain("client_error"),
            "expected 'client_error' message in captured logs",
        );
        assert!(
            logs_contain("tracing-capture-marker-42"),
            "expected panic_message to appear in captured logs",
        );
        assert!(
            logs_contain("9.9.9"),
            "expected version to appear in captured logs",
        );
        assert!(
            logs_contain("/trace-test"),
            "expected url to appear in captured logs",
        );
    }

    #[tokio::test]
    #[tracing_test::traced_test]
    async fn client_error_emits_backtrace_log_when_present() {
        let report = ClientErrorReport {
            panic_message: "with-bt".to_string(),
            version: None,
            git_hash: None,
            url: None,
            user_agent: None,
            location: None,
            backtrace: Some("trace-capture-bt-marker-99".to_string()),
        };
        let _ = client_error(axum::Json(report)).await;
        assert!(
            logs_contain("client_error_backtrace"),
            "expected 'client_error_backtrace' message in captured logs",
        );
        assert!(
            logs_contain("trace-capture-bt-marker-99"),
            "expected backtrace body to appear in captured logs",
        );
    }

    // ================================================================
    // Limiter activity ms → seconds conversion (reaperiem#145)
    // ================================================================

    #[test]
    fn test_limiter_ms_to_seconds_zero() {
        assert_eq!(limiter_ms_to_seconds(0), 0.0);
    }

    #[test]
    fn test_limiter_ms_to_seconds_exact() {
        // 83_500 ms = 83.5 s; division (not multiplication or modulo)
        let result = limiter_ms_to_seconds(83_500);
        assert!((result - 83.5).abs() < 1e-9, "expected 83.5, got {result}");
    }

    #[test]
    fn test_limiter_ms_to_seconds_one_second() {
        assert_eq!(limiter_ms_to_seconds(1000), 1.0);
    }

    // ================================================================
    // Limiter authorization (reaperiem#145 / reaperiem#156)
    // ================================================================

    #[test]
    fn test_check_owns_limiter_track_engineer_any_track() {
        let indices = std::collections::HashMap::new();
        // Engineer is allowed on any track, even ones not in the index map.
        assert!(check_owns_limiter_track(true, "engineer", &indices, 1));
        assert!(check_owns_limiter_track(true, "engineer", &indices, 99));
    }

    #[test]
    fn test_check_owns_limiter_track_member_own_track() {
        let mut indices = std::collections::HashMap::new();
        indices.insert("member1".to_string(), 23);
        assert!(check_owns_limiter_track(false, "member1", &indices, 23));
    }

    #[test]
    fn test_check_owns_limiter_track_member_other_track() {
        let mut indices = std::collections::HashMap::new();
        indices.insert("member1".to_string(), 23);
        indices.insert("member2".to_string(), 24);
        // Member1 (track 23) must NOT be allowed on Member2's track (24).
        assert!(!check_owns_limiter_track(false, "member1", &indices, 24));
    }

    #[test]
    fn test_check_owns_limiter_track_member_unknown_id() {
        let indices = std::collections::HashMap::new();
        // Unknown member id with empty indices → denied.
        assert!(!check_owns_limiter_track(false, "nobody", &indices, 23));
    }

    #[test]
    fn test_check_owns_limiter_track_engineer_trumps_missing_entry() {
        // Engineer id isn't in the output_track_indices map, but is_engineer=true
        // overrides that check.
        let indices = std::collections::HashMap::new();
        assert!(check_owns_limiter_track(true, "engineer", &indices, 32));
    }

    /// End-to-end regression test for reaperiem#179 against the real `apply_command_to_cache`.
    /// Builds a minimal AppState that reproduces the live-service topology:
    ///   - config.inputs has 23 entries including "MEMBER7 kl"
    ///   - mixer_cache.valid_input_track_indices includes REAPER track 44
    ///   - discovered_members contains member2 at send_index 1
    /// Then sends a `SetMute { track_index: 44, muted: true }` and asserts
    /// the handler returns Ok with a REAPER URL targeting TRACK/44/SEND/1/MUTE/1.
    ///
    /// Before the fix: the old `ti <= inputs.len()` (23) validator rejected
    /// track 44 and this test would fail with "track_index 44 out of range"
    /// — the exact failure mode members saw during the live service.
    #[tokio::test]
    async fn test_apply_command_to_cache_mutes_member7_kl_at_reaper_track_44() {
        use crate::AppState;
        use iem_core::config::{Config, InputTrack};
        use iem_core::{ClientMsg, DiscoveredMember};
        use std::collections::{HashMap, HashSet};

        // Minimal config with 23 inputs — matching live-service shape where
        // MEMBER7 kl is the 23rd entry and therefore would be index 23 under
        // the buggy `inputs.len()` validator, but is actually at REAPER
        // track 44.
        let mut config = Config {
            inputs: (0..22)
                .map(|i| InputTrack {
                    name: format!("MIC{} mic", i + 1),
                    dante_input: (101 + i) as u8,
                    default_level_db: 0.0,
                    category: None,
                    stereo_pair: None,
                })
                .collect(),
            ..Config::default()
        };
        config.inputs.push(InputTrack {
            name: "MEMBER7 kl".to_string(),
            dante_input: 113,
            default_level_db: 0.0,
            category: Some("mics".to_string()),
            stereo_pair: None,
        });

        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(config, dir.path());

        // Populate the cache exactly as the poller would after discovering
        // MEMBER7 kl at REAPER track 44.
        {
            let mut cache = state.mixer_cache.write().await;
            let mut input_track_indices: HashMap<String, usize> = (0..22)
                .map(|i| (format!("MIC{} mic", i + 1), i + 1))
                .collect();
            input_track_indices.insert("MEMBER7 kl".to_string(), 44);
            cache.input_track_indices = input_track_indices;

            let mut valid: HashSet<usize> = (1..=22).collect();
            valid.insert(44);
            cache.valid_input_track_indices = valid;

            // The WS handler looks up channels by track_index in member_states
            // to update cached state alongside the REAPER call — seed one
            // channel for MEMBER7 kl on member2's state so the handler reaches
            // the REAPER-URL branch, not an early-exit.
            cache.member_states.insert(
                "member2".to_string(),
                vec![iem_core::Channel {
                    track_index: 44,
                    name: "MEMBER7 kl".to_string(),
                    level_db: 0.0,
                    pan: 0.5,
                    muted: false,
                    category: "mics".to_string(),
                    stereo_pair: None,
                    stereo_side: None,
                }],
            );
        }

        // member2 at send_index=1 — matches live routing (send 0 → MEMBER1,
        // send 1 → MEMBER2, etc.)
        {
            let mut discovered = state.discovered_members.write().await;
            discovered.push(DiscoveredMember {
                name: "MEMBER2".to_string(),
                track_index: 24,
                dante_output_l: 73,
                dante_output_r: 74,
                send_index: 1,
                mix_send_index: Some(1),
                mix_send_indices: std::collections::HashMap::new(),
            });
        }

        let cmd = ClientMsg::SetMute {
            track_index: 44,
            muted: true,
        };
        let result = apply_command_to_cache(&state, "member2", &cmd).await;

        assert!(
            result.is_ok(),
            "apply_command_to_cache must ACCEPT SetMute for MEMBER7 kl (track 44). \
             Got Err: {:?}. Under the pre-#179 validator, this returned \
             'track_index 44 out of range 1..23'.",
            result.as_ref().err()
        );
        let (url, _event) = result.unwrap();
        assert!(
            url.contains("/SET/TRACK/44/SEND/1/MUTE/1"),
            "Expected REAPER URL to target TRACK/44/SEND/1/MUTE/1 — got: {}",
            url
        );
    }

    /// Inverse guard: if cache's valid_input_track_indices does NOT contain
    /// the track, the handler must reject — proves validation is still
    /// active, not stripped out by a future refactor.
    #[tokio::test]
    async fn test_apply_command_to_cache_rejects_unknown_track() {
        use crate::AppState;
        use iem_core::ClientMsg;
        use iem_core::config::Config;

        let config = Config::default();
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(config, dir.path());

        // Seed a discovered member so we don't trip the "unknown member"
        // early-exit; the test is about track_index rejection specifically.
        {
            let mut discovered = state.discovered_members.write().await;
            discovered.push(iem_core::DiscoveredMember {
                name: "MEMBER2".to_string(),
                track_index: 24,
                dante_output_l: 73,
                dante_output_r: 74,
                send_index: 1,
                mix_send_index: Some(1),
                mix_send_indices: std::collections::HashMap::new(),
            });
        }

        let cmd = ClientMsg::SetMute {
            track_index: 999,
            muted: true,
        };
        let result = apply_command_to_cache(&state, "member2", &cmd).await;
        assert!(
            result.is_err(),
            "track_index 999 (not in valid set) must be rejected — got Ok: {:?}",
            result.ok()
        );
    }

    #[test]
    fn test_parse_eq_band_default_gd_min_max_when_missing() {
        // Mutation killer for line ~2573 (unwrap_or(-12.0)) and ~2574 (unwrap_or(12.0)):
        // when an old ReaScript response lacks gd_min=/gd_max= fields, the parsed
        // band must default to -12.0 / +12.0, NOT 12.0 / -12.0.
        let band_str = "b0:band,fn=0.5,gn=0.25,bn=0.5,fh=1000,gd=0.0,bo=1.0,en=1";
        let band = parse_eq_band(band_str).expect("parse must succeed");
        assert_eq!(band.gain_db_min, -12.0);
        assert_eq!(band.gain_db_max, 12.0);
    }

    #[test]
    fn test_parse_eq_band_returns_none_when_gd_missing() {
        // gd= is mandatory in current ReaScript output; if absent the input is malformed
        // and parse must fail closed (not silently approximate).
        let band_str = "b0:band,fn=0.5,gn=0.25,bn=0.5,fh=1000,bo=1.0,en=1";
        assert!(parse_eq_band(band_str).is_none());
    }

    #[test]
    fn test_parse_eq_band_returns_none_when_fh_missing() {
        // fh= is mandatory in current ReaScript output; if absent the input is malformed.
        let band_str = "b0:band,fn=0.5,gn=0.25,bn=0.5,gd=0.0,bo=1.0,en=1";
        assert!(parse_eq_band(band_str).is_none());
    }

    #[test]
    fn test_parse_eq_band_returns_none_when_bo_missing() {
        // bo= is mandatory in current ReaScript output; if absent the input is malformed.
        let band_str = "b0:band,fn=0.5,gn=0.25,bn=0.5,fh=1000,gd=0.0,en=1";
        assert!(parse_eq_band(band_str).is_none());
    }
}
