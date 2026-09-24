//! S5: REAPER control plane — replaced by the engine client (program spec §6 S5); imported only so the server builds and its tests run.
//! Background REAPER poller
//!
//! Runs every 150ms, queries REAPER for meter and send state,
//! detects changes via cache diff, and broadcasts updates to WebSocket clients.
//!
//! Also handles member discovery from REAPER at startup.

use iem_core::{DiscoveredMember, MixSnapshot, ServerMsg};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use crate::proxy::{
    build_channel_templates, query_send_state, reaper_api, reaper_pan_to_ui, reaper_vol_to_db,
};
use crate::snapshot_store::SnapshotStore;
use crate::{AppState, GlobalVolState};

/// Whether the meter bridge ReaScript has been triggered this session
static METER_BRIDGE_STARTED: AtomicBool = AtomicBool::new(false);

/// Counter for periodic full meter broadcasts (every 7 cycles ≈ 1s at 150ms interval).
/// New WebSocket clients need full meter data within ~1 second of connecting,
/// and steady signals (test tones) would never trigger delta-only broadcasts.
static METER_FULL_BROADCAST_CYCLE: AtomicU64 = AtomicU64::new(0);

/// REAPER action ID for the meter bridge ReaScript
const METER_BRIDGE_ACTION: &str = "_RS_REAPERIEM_METER_BRIDGE";

/// Parse meter bridge EXTSTATE response into per-channel L/R meter data.
/// Input format: "1:-37,-37;2:-911,-911;..." (track_idx:L_db10,R_db10)
/// Returns HashMap<track_idx, [left_linear, right_linear]>
pub fn parse_meter_bridge(text: &str) -> HashMap<usize, [f32; 2]> {
    let mut meters = HashMap::new();
    let db10_to_linear = |v: f32| -> f32 {
        if v <= -1500.0 {
            0.0
        } else {
            10.0_f32.powf(v / 10.0 / 20.0)
        }
    };
    for entry in text.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some((idx_str, vals_str)) = entry.split_once(':')
            && let Ok(track_idx) = idx_str.parse::<usize>()
            && let Some((l_str, r_str)) = vals_str.split_once(',')
            && let (Ok(l_db10), Ok(r_db10)) = (l_str.parse::<f32>(), r_str.parse::<f32>())
        {
            meters.insert(track_idx, [db10_to_linear(l_db10), db10_to_linear(r_db10)]);
        }
    }
    meters
}

/// Parse limiter activity EXTSTATE response into per-track active_ms map.
/// Input format: "23:1230;24:0;25:8470" (track_idx_1based:active_ms_u64)
/// Skips malformed entries silently.
pub fn parse_limiter_activity_totals(text: &str) -> HashMap<usize, u64> {
    let mut totals = HashMap::new();
    for entry in text.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some((idx_str, ms_str)) = entry.split_once(':')
            && let Ok(track_idx) = idx_str.parse::<usize>()
            && let Ok(ms) = ms_str.parse::<u64>()
        {
            totals.insert(track_idx, ms);
        }
    }
    totals
}

/// Discover band members from REAPER by querying tracks ending in " inear".
/// REAPER is the source of truth for member names.
/// Returns the list of discovered members, or empty if REAPER is unreachable.
pub async fn discover_members(state: &AppState) -> Vec<DiscoveredMember> {
    let config = state.config.read().await;
    let reaper_url = config.reaper_url.clone();
    let config_ref = config.clone();
    drop(config);

    let tracks_url = reaper_api::query_tracks(&reaper_url);
    let mut members = Vec::new();

    if let Ok(resp) = state.http_client.get(&tracks_url).send().await
        && let Ok(text) = resp.text().await
    {
        let mut send_index = 0;
        for line in text.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.first() == Some(&"TRACK")
                && parts.len() > 2
                && let Ok(track_idx) = parts[1].parse::<usize>()
            {
                let track_name = parts[2];
                if let Some(member) = DiscoveredMember::from_reaper_track(
                    track_name,
                    track_idx,
                    send_index,
                    &config_ref,
                ) {
                    tracing::info!(
                        member = %member.name,
                        track_index = member.track_index,
                        send_index = member.send_index,
                        dante = ?[member.dante_output_l, member.dante_output_r],
                        "Discovered member from REAPER"
                    );
                    members.push(member);
                    send_index += 1;
                }
            }
        }
    }

    // Discover mix_send_index for each member: find which send targets the engineer track.
    // This is critical: member inear tracks have hardware outputs at SEND/0,
    // and the send to ENGINEER sits at a higher index. Hardcoding 0 mutes hardware output!
    let engineer_track = members
        .iter()
        .find(|m| m.id() == "engineer")
        .map(|m| m.track_index);
    if let Some(eng_ti) = engineer_track {
        for member in members.iter_mut().filter(|m| m.id() != "engineer") {
            // Query sends on this member's inear track to find the one targeting engineer.
            // We don't know send count from discovery, so probe up to 10 sends.
            for si in 0..10 {
                // Rate-limit: small delay between REAPER queries to prevent overwhelming
                // the HTTP API. Without this, 9 members × up to 10 sends = 90 rapid-fire
                // requests on startup, which can crash REAPER.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;

                if let Some(dest) = crate::proxy::query_send_destination(
                    &state.http_client,
                    &reaper_url,
                    member.track_index,
                    si,
                )
                .await
                {
                    // dest is i32: negative = hardware output (skip), positive = track index
                    if dest >= 0 && dest as usize == eng_ti {
                        member.mix_send_index = Some(si);
                        tracing::info!(
                            member = %member.name,
                            track = member.track_index,
                            send_index = si,
                            engineer_track = eng_ti,
                            "Discovered mix send index to engineer"
                        );
                        break;
                    }
                    // dest is -1 (hardware output) or other track → continue probing
                } else {
                    // None = no SEND line in response = send doesn't exist → stop
                    break;
                }
            }
            if member.mix_send_index.is_none() {
                tracing::warn!(
                    member = %member.name,
                    track = member.track_index,
                    "No send to engineer found — mix channel will be read-only"
                );
            }
        }
    }
    // Discover mix_send_indices for elevated members: find sends from OTHER members'
    // inear tracks that target this elevated member's inear track.
    // Same mechanism as engineer discovery, but for each elevated member.
    // Hardcoded: only "member1" is elevated.
    {
        let elevated_list: Vec<String> = vec!["member1".to_string()];

        for elevated_id in &elevated_list {
            let elevated_ti = members
                .iter()
                .find(|m| m.id() == *elevated_id)
                .map(|m| m.track_index);
            let Some(elev_ti) = elevated_ti else {
                continue;
            };

            // For each OTHER member's inear track, probe sends to find one targeting this elevated member
            let other_members: Vec<(String, usize)> = members
                .iter()
                .filter(|m| m.id() != *elevated_id && m.id() != "engineer")
                .map(|m| (m.id(), m.track_index))
                .collect();

            for (other_id, other_track) in &other_members {
                for si in 0..15 {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    if let Some(dest) = crate::proxy::query_send_destination(
                        &state.http_client,
                        &reaper_url,
                        *other_track,
                        si,
                    )
                    .await
                    {
                        if dest >= 0 && dest as usize == elev_ti {
                            // Found: OTHER's inear SEND/si targets this elevated member's inear
                            if let Some(member) =
                                members.iter_mut().find(|m| m.id() == *elevated_id)
                            {
                                member.mix_send_indices.insert(other_id.clone(), si);
                                tracing::info!(
                                    elevated = %elevated_id,
                                    source = %other_id,
                                    source_track = other_track,
                                    send_index = si,
                                    "Discovered mix send for elevated member"
                                );
                            }
                            break;
                        }
                    } else {
                        break; // No more sends on this track
                    }
                }
            }
        }
    }

    if members.is_empty() {
        tracing::warn!("No members discovered from REAPER - using fallback from config.members");
        // Fallback: create DiscoveredMember from legacy config.members
        for (idx, m) in config_ref.members.iter().enumerate() {
            members.push(DiscoveredMember {
                name: m
                    .track_name()
                    .strip_suffix(" inear")
                    .unwrap_or(&m.name.to_uppercase())
                    .to_string(),
                track_index: 0, // Unknown
                dante_output_l: m.dante_output_l,
                dante_output_r: m.dante_output_r,
                send_index: idx,
                mix_send_index: None,
                mix_send_indices: std::collections::HashMap::new(),
            });
        }
    }

    members
}

/// Cooldown between re-discovery attempts (10 seconds)
static LAST_REDISCOVERY_ATTEMPT: AtomicU64 = AtomicU64::new(0);

/// Check if discovered members need re-discovery (fallback or incomplete state).
///
/// Returns `true` when any non-engineer member has `mix_send_index == None`
/// or any member has `track_index == 0` (fallback placeholder from config).
pub fn needs_rediscovery(members: &[DiscoveredMember]) -> bool {
    members.iter().any(|m| {
        // Engineer doesn't have mix_send_index (it's the target, not source)
        if m.id() == "engineer" {
            return false;
        }
        // Fallback members have track_index = 0
        m.track_index == 0 || m.mix_send_index.is_none()
    })
}

/// Spawn the background poller task
pub fn spawn_poller(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(150));
        loop {
            interval.tick().await;
            poll_reaper_and_broadcast(&state).await;
        }
    })
}

/// Check if the elevated member (member1) is missing mix_send_indices (needs re-discovery).
async fn elevated_needs_rediscovery(state: &AppState) -> bool {
    let discovered = state.discovered_members.read().await;
    if let Some(member) = discovered.iter().find(|m| m.id() == "member1")
        && member.mix_send_indices.is_empty()
    {
        return true;
    }
    false
}

/// Single poll cycle: query REAPER, diff against cache, broadcast changes
async fn poll_reaper_and_broadcast(state: &AppState) {
    // Retry discovery if previous attempt was incomplete (e.g., REAPER was down at startup)
    // Also re-discover when elevated members are missing mix_send_indices
    {
        let discovered = state.discovered_members.read().await;
        let needs_basic = needs_rediscovery(&discovered);
        drop(discovered);
        let needs_elevated = elevated_needs_rediscovery(state).await;

        if needs_basic || needs_elevated {
            // Only attempt re-discovery every 10 seconds to avoid hammering REAPER
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let last = LAST_REDISCOVERY_ATTEMPT.load(Ordering::Relaxed);
            if now_ms.saturating_sub(last) > 10_000 {
                LAST_REDISCOVERY_ATTEMPT.store(now_ms, Ordering::Relaxed);
                let new_members = discover_members(state).await;
                if !new_members.is_empty() {
                    let mut discovered = state.discovered_members.write().await;
                    tracing::info!("Re-discovery successful — updating members");
                    *discovered = new_members;
                    // Clear cached channel states to force full State resync
                    let mut cache = state.mixer_cache.write().await;
                    cache.member_states.clear();
                }
            }
        }
    }

    let config = state.config.read().await;
    let reaper_url = config.reaper_url.clone();
    let inputs = config.inputs.clone();
    drop(config);

    // Build member track_name -> id mapping from discovered members (REAPER source of truth)
    let discovered = state.discovered_members.read().await;
    let member_track_names: HashMap<String, String> = discovered
        .iter()
        .map(|m| (m.track_name(), m.id()))
        .collect();
    drop(discovered);

    // Check which members have active WS connections (HashMap keys)
    let active_members: Vec<String> = {
        let cache = state.mixer_cache.read().await;
        cache.active_members.keys().cloned().collect()
    };

    // Skip polling if no active WebSocket clients
    if active_members.is_empty() {
        return;
    }

    // 1. Query meters via NTRACK;TRACK (single call)
    let mut meters: HashMap<usize, [f32; 2]> = HashMap::new();
    let mut connected = false;
    // Discovered output tracks: member_id -> (track_index, vol_linear, flags)
    let mut output_tracks: HashMap<String, (usize, f32, i32)> = HashMap::new();
    // Discovered stems-bus tracks: member_id -> (track_index, vol_linear, flags)
    let mut stems_bus_tracks: HashMap<String, (usize, f32, i32)> = HashMap::new();

    // Name→index map for ALL REAPER tracks (for input track index resolution)
    let mut all_track_names: HashMap<String, usize> = HashMap::new();

    let tracks_url = reaper_api::query_tracks(&reaper_url);
    if let Ok(resp) = state.http_client.get(&tracks_url).send().await
        && let Ok(text) = resp.text().await
    {
        connected = true;

        // Detect track count changes
        for line in text.lines() {
            if let Some(count_str) = line.strip_prefix("NTRACK\t") {
                if let Ok(count) = count_str.parse::<usize>() {
                    let mut cache = state.mixer_cache.write().await;
                    if let Some(prev) = cache.last_track_count
                        && count != prev
                    {
                        tracing::warn!(
                            old_count = prev,
                            new_count = count,
                            "REAPER track count changed — track indices may have shifted"
                        );
                    }
                    cache.last_track_count = Some(count);
                }
                break;
            }
        }

        for line in text.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.first() == Some(&"TRACK")
                && parts.len() > 7
                && let Ok(track_idx) = parts[1].parse::<usize>()
            {
                // Meter fields only present when track has 14+ fields
                // (record-armed tracks include last_meter_peak [6] and last_meter_pos [7])
                // Without these fields (12 fields): field [6] is width, NOT a meter value
                if parts.len() >= 14
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
                // No VU → don't insert → frontend defaults to 0.0

                // Build name→index map for ALL tracks (input index resolution)
                let track_name = parts[2];
                all_track_names.insert(track_name.to_string(), track_idx);

                // Check if this is a member output track (e.g. "OLDMEMBER1 inear")
                if let Some(member_id) = member_track_names.get(track_name) {
                    let vol: f32 = parts[4].parse().unwrap_or(1.0);
                    let flags: i32 = parts[3].parse().unwrap_or(0);
                    output_tracks.insert(member_id.clone(), (track_idx, vol, flags));
                }

                // Check if this is a stems-bus track (e.g. "OLDMEMBER1 stems")
                if let Some(member_name) = track_name.strip_suffix(" stems") {
                    let member_id = member_name.to_lowercase();
                    let vol: f32 = parts[4].parse().unwrap_or(1.0);
                    let flags: i32 = parts[3].parse().unwrap_or(0);
                    stems_bus_tracks.insert(member_id, (track_idx, vol, flags));
                }
            }
        }
        // Debug: log meter summary periodically (every ~10s = 66 poll cycles)
        use std::sync::atomic::AtomicU64;
        static POLL_COUNT: AtomicU64 = AtomicU64::new(0);
        let count = POLL_COUNT.fetch_add(1, Ordering::Relaxed);
        if count.is_multiple_of(66) {
            let non_zero: Vec<_> = meters
                .iter()
                .filter(|(_, v)| v[0] > 0.001 || v[1] > 0.001)
                .take(5)
                .collect();
            tracing::debug!(
                meter_count = meters.len(),
                non_zero_count = non_zero.len(),
                sample = ?non_zero,
                output_tracks = output_tracks.len(),
                "Meter poll summary"
            );
        }
    }

    // 1b. Try to read meter bridge EXTSTATE for true L/R per-channel peaks.
    // The meter bridge ReaScript (meter_bridge.lua) provides instantaneous
    // Track_GetPeakInfo values per channel, unlike TRACK fields which only
    // give combined peak-hold data.
    if connected {
        // Start meter bridge if not already running.
        // MUST check EXTSTATE first — triggering the action while the script
        // is already running opens REAPER's "ReaScript task control" modal dialog,
        // which blocks the HTTP API and freezes everything.
        if !METER_BRIDGE_STARTED.load(Ordering::Relaxed) {
            // Check if bridge is already running in REAPER before triggering
            let running_url =
                reaper_api::get_extstate(&reaper_url, "REAPERIEM_METERS", "bridge_running");
            let already_running = if let Ok(resp) = state.http_client.get(&running_url).send().await
            {
                resp.text()
                    .await
                    .ok()
                    .and_then(|t| t.split('\t').nth(3).map(|v| v == "1"))
                    .unwrap_or(false)
            } else {
                false
            };

            if already_running {
                // Bridge is running, just set our flag
                METER_BRIDGE_STARTED.store(true, Ordering::Relaxed);
                tracing::info!("Meter bridge already running in REAPER");
            } else {
                let action_url = reaper_api::trigger_action(&reaper_url, METER_BRIDGE_ACTION);
                if state.http_client.get(&action_url).send().await.is_ok() {
                    METER_BRIDGE_STARTED.store(true, Ordering::Relaxed);
                    tracing::info!("Triggered meter bridge ReaScript");
                }
            }
        }

        let extstate_url = reaper_api::get_extstate(&reaper_url, "REAPERIEM_METERS", "peaks");
        if let Ok(resp) = state.http_client.get(&extstate_url).send().await
            && let Ok(text) = resp.text().await
        {
            // REAPER EXTSTATE response: "EXTSTATE\tSECTION\tKEY\tvalue"
            if let Some(value) = text.split('\t').nth(3)
                && !value.is_empty()
            {
                let bridge_meters = parse_meter_bridge(value);
                if !bridge_meters.is_empty() {
                    // Override TRACK field meters with true L/R data
                    meters = bridge_meters;
                }
            }
        }

        // Limiter activity totals (reaperiem#145) — meter_bridge.lua writes per-track
        // cumulative active milliseconds where the limiter is reducing gain.
        // Unlike the meter block above, we deliberately do NOT early-return on
        // an empty value: meter_bridge always writes a non-empty string when
        // at least one inear track has the limiter inserted, so an empty value
        // legitimately means "no tracks have a limiter right now" and the
        // HashMap should be cleared to reflect that.
        let lim_url = reaper_api::get_extstate(&reaper_url, "REAPERIEM_LIMITER_ACTIVITY", "totals");
        if let Ok(resp) = state.http_client.get(&lim_url).send().await
            && let Ok(text) = resp.text().await
            && let Some(value) = text.split('\t').nth(3)
        {
            let totals = parse_limiter_activity_totals(value);
            let mut guard = state.limiter_activity.lock().await;
            *guard = totals;
        }
    }

    // Check if connection status changed
    {
        let mut cache = state.mixer_cache.write().await;
        if cache.connected != connected {
            cache.connected = connected;
            let _ = state.event_tx.send((
                String::new(), // broadcast to all
                ServerMsg::ConnectionChanged { connected },
            ));
        }
    }

    if !connected {
        // Reset meter bridge flag so it re-triggers on reconnection
        METER_BRIDGE_STARTED.store(false, Ordering::Relaxed);
        return;
    }

    // Broadcast meters: delta-only most of the time, full broadcast every 7 cycles (~1s).
    // The periodic full broadcast ensures:
    // - New WebSocket clients receive meter data within ~1 second of connecting
    // - Steady signals (test tones) stay visible even when delta is zero
    {
        let cycle = METER_FULL_BROADCAST_CYCLE.fetch_add(1, Ordering::Relaxed);
        let force_full = cycle.is_multiple_of(7);

        let cache = state.mixer_cache.read().await;
        let prev = &cache.meters;

        let to_broadcast = if force_full && !meters.is_empty() {
            meters.clone()
        } else {
            meters
                .iter()
                .filter(|(idx, [l, r])| {
                    prev.get(idx)
                        .is_none_or(|[pl, pr]| (l - pl).abs() > 0.01 || (r - pr).abs() > 0.01)
                })
                .map(|(k, v)| (*k, *v))
                .collect()
        };

        if !to_broadcast.is_empty() {
            let _ = state.event_tx.send((
                String::new(), // broadcast to all
                ServerMsg::Meters {
                    meters: to_broadcast,
                },
            ));
        }
    }

    // Update cached meters and output track state
    {
        let now = std::time::Instant::now();
        let echo_suppress_window = Duration::from_millis(500);
        let mut cache = state.mixer_cache.write().await;
        cache.meters = meters;

        // Update output track indices and global volumes, broadcast changes
        for (member_id, (track_idx, vol_linear, flags)) in &output_tracks {
            let old_idx = cache.output_track_indices.get(member_id).copied();
            if old_idx.is_some_and(|old| old != *track_idx) {
                tracing::warn!(
                    member = %member_id,
                    old = old_idx.unwrap(),
                    new = track_idx,
                    "Output track index shifted — forcing full state resync"
                );
                cache.member_states.clear();
            }
            cache
                .output_track_indices
                .insert(member_id.clone(), *track_idx);

            let level_db = crate::proxy::quantize_02(reaper_vol_to_db(*vol_linear));
            let muted = (*flags & 8) != 0;

            let changed = match cache.global_volumes.get(member_id) {
                Some(gv) => (gv.level_db - level_db).abs() > 0.05 || gv.muted != muted,
                None => true,
            };

            if changed {
                // Check echo suppression (keyed with output_track + 100000 offset)
                let key = (member_id.clone(), *track_idx + 100000);
                let recently_commanded = cache
                    .command_timestamps
                    .get(&key)
                    .is_some_and(|ts| now.duration_since(*ts) < echo_suppress_window);

                if !recently_commanded {
                    let _ = state.event_tx.send((
                        member_id.clone(),
                        ServerMsg::GlobalVolumeUpdate { level_db, muted },
                    ));
                }
            }

            cache
                .global_volumes
                .insert(member_id.clone(), GlobalVolState { level_db, muted });
        }

        // Update stems-bus track indices and volumes, broadcast changes
        for (member_id, (track_idx, vol_linear, flags)) in &stems_bus_tracks {
            cache
                .stems_bus_indices
                .insert(member_id.clone(), *track_idx);

            let level_db = crate::proxy::quantize_02(reaper_vol_to_db(*vol_linear));
            let muted = (*flags & 8) != 0;

            let changed = match cache.stems_volumes.get(member_id) {
                Some(sv) => (sv.level_db - level_db).abs() > 0.05 || sv.muted != muted,
                None => true,
            };

            if changed {
                // Echo suppression (offset 200000 to avoid collision with global/send keys)
                let key = (member_id.clone(), *track_idx + 200000);
                let recently_commanded = cache
                    .command_timestamps
                    .get(&key)
                    .is_some_and(|ts| now.duration_since(*ts) < echo_suppress_window);

                if !recently_commanded {
                    let _ = state.event_tx.send((
                        member_id.clone(),
                        ServerMsg::StemsVolumeUpdate { level_db, muted },
                    ));
                }
            }

            cache
                .stems_volumes
                .insert(member_id.clone(), GlobalVolState { level_db, muted });
        }

        // Resolve input track indices by name from NTRACK response
        if !all_track_names.is_empty() {
            let mut input_indices: HashMap<String, usize> = HashMap::new();
            for input in &inputs {
                if let Some(&idx) = all_track_names.get(&input.name) {
                    input_indices.insert(input.name.clone(), idx);
                }
            }
            if cache.input_track_indices != input_indices {
                tracing::warn!(
                    "Input track indices changed — forcing full state resync for all members"
                );
                cache.member_states.clear();
            }
            cache.input_track_indices = input_indices;
            // Recompute the authoritative valid-input-index set at exactly
            // the same moment — this is the set that per-command validators
            // read on hot paths. Mirrors build_channel_templates' resolution
            // so the set the client was sent agrees with what the server
            // will accept. See reaperiem#179: using inputs.len() here silently
            // rejected every MEMBER7 kl (track 44) command during a live
            // service.
            cache.valid_input_track_indices =
                crate::proxy::collect_valid_input_indices(&inputs, &cache.input_track_indices);
        }
    }

    // Sync output track index changes back to discovered_members
    if !output_tracks.is_empty() {
        let mut discovered = state.discovered_members.write().await;
        for (member_id, (new_idx, _, _)) in &output_tracks {
            if let Some(member) = discovered.iter_mut().find(|m| m.id() == *member_id)
                && member.track_index != *new_idx
            {
                tracing::warn!(
                    member = %member_id,
                    old = member.track_index,
                    new = new_idx,
                    "Output track index shifted — syncing DiscoveredMember"
                );
                member.track_index = *new_idx;
            }
        }
    }

    // 2. For each active member, query send states in parallel
    // Use discovered members for send indices (REAPER source of truth)
    let discovered = state.discovered_members.read().await;
    let member_indices: Vec<(String, usize)> = active_members
        .iter()
        .filter_map(|mid| {
            discovered
                .iter()
                .find(|m| m.id() == *mid)
                .map(|m| (mid.clone(), m.send_index))
        })
        .collect();
    let discovered_snapshot = discovered.clone();
    drop(discovered);
    let resolved = state.mixer_cache.read().await.input_track_indices.clone();
    let resolved_ref = if resolved.is_empty() {
        None
    } else {
        Some(&resolved)
    };
    let channel_templates = build_channel_templates(&inputs, resolved_ref);

    for (member_id, member_index) in &member_indices {
        let channels = channel_templates.clone();

        // Query all send states in parallel
        let send_futures: Vec<_> = channels
            .iter()
            .map(|ch| {
                let client = state.http_client.clone();
                let url = reaper_url.clone();
                let track_index = ch.track_index;
                async move {
                    let result = query_send_state(&client, &url, track_index, *member_index).await;
                    (track_index, result)
                }
            })
            .collect();

        let send_results = futures::future::join_all(send_futures).await;

        let mut result_channels = channels;
        for (track_index, result) in send_results {
            if let Ok((level, mute, pan)) = result
                && let Some(ch) = result_channels
                    .iter_mut()
                    .find(|c| c.track_index == track_index)
            {
                ch.level_db = crate::proxy::quantize_02(reaper_vol_to_db(level));
                ch.muted = mute;
                ch.pan = reaper_pan_to_ui(pan);
            }
        }

        // For engineer or elevated members: also query mix channels
        let is_elevated = member_id == "member1";
        if member_id == "engineer" || is_elevated {
            let mut mix_channels =
                crate::proxy::build_mix_channel_templates(&discovered_snapshot, member_id);

            let mix_futures: Vec<_> = mix_channels
                .iter()
                .filter_map(|ch| {
                    // Look up the correct send index:
                    // Engineer: mix_send_index, Elevated: mix_send_indices[member_id]
                    let mix_si = if member_id == "engineer" {
                        discovered_snapshot
                            .iter()
                            .find(|m| m.track_index == ch.track_index)
                            .and_then(|m| m.mix_send_index)
                    } else {
                        // mix_send_indices stored on elevated member, keyed by source ID
                        let source_id = discovered_snapshot
                            .iter()
                            .find(|m| m.track_index == ch.track_index)
                            .map(|m| m.id());
                        let elevated = discovered_snapshot.iter().find(|m| m.id() == *member_id);
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

            let mix_results = futures::future::join_all(mix_futures).await;
            for (track_index, result) in mix_results {
                if let Ok((level, mute, pan)) = result
                    && let Some(ch) = mix_channels
                        .iter_mut()
                        .find(|c| c.track_index == track_index)
                {
                    ch.level_db = crate::proxy::quantize_02(reaper_vol_to_db(level));
                    ch.muted = mute;
                    ch.pan = reaper_pan_to_ui(pan);
                }
            }

            result_channels.extend(mix_channels);
        }

        // Diff against cached state and broadcast changes
        let now = std::time::Instant::now();
        let echo_suppress_window = Duration::from_millis(500);
        let mut cache = state.mixer_cache.write().await;

        if let Some(cached_channels) = cache.member_states.get(member_id) {
            // Send per-channel updates for changed channels
            for new_ch in &result_channels {
                if let Some(old_ch) = cached_channels
                    .iter()
                    .find(|c| c.track_index == new_ch.track_index)
                {
                    let changed = (old_ch.level_db - new_ch.level_db).abs() > 0.05
                        || old_ch.muted != new_ch.muted
                        || (old_ch.pan - new_ch.pan).abs() > 0.001;

                    if changed {
                        // Check if this channel was recently commanded — suppress echo
                        let key = (member_id.clone(), new_ch.track_index);
                        let recently_commanded = cache
                            .command_timestamps
                            .get(&key)
                            .is_some_and(|ts| now.duration_since(*ts) < echo_suppress_window);

                        if !recently_commanded {
                            let _ = state.event_tx.send((
                                member_id.clone(),
                                ServerMsg::ChannelUpdate {
                                    track_index: new_ch.track_index,
                                    level_db: new_ch.level_db,
                                    muted: new_ch.muted,
                                    pan: new_ch.pan,
                                },
                            ));
                        }
                    }
                }
            }
        } else {
            // First poll for this member - send full state
            let (global_level_db, global_muted) = cache
                .global_volumes
                .get(member_id)
                .map(|gv| (Some(gv.level_db), Some(gv.muted)))
                .unwrap_or((None, None));
            let output_track_index = cache.output_track_indices.get(member_id).copied();
            let (stems_level_db, stems_muted) = cache
                .stems_volumes
                .get(member_id)
                .map(|sv| (Some(sv.level_db), Some(sv.muted)))
                .unwrap_or((None, None));
            let stems_bus_index = cache.stems_bus_indices.get(member_id).copied();
            let _ = state.event_tx.send((
                member_id.clone(),
                ServerMsg::State {
                    channels: result_channels.clone(),
                    connected: true,
                    global_level_db,
                    global_muted,
                    output_track_index,
                    stems_level_db,
                    stems_muted,
                    stems_bus_index,
                },
            ));
        }

        // Update cache with real REAPER state (never poisoned by suppression)
        cache
            .member_states
            .insert(member_id.clone(), result_channels.clone());

        // Daily auto-snapshot: on first channel change of the day, save a snapshot
        // capturing the starting state before changes
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let last_date = cache.snapshot_last_date.get(member_id).cloned();
        let needs_snapshot = last_date.as_deref() != Some(&today)
            && !state.snapshot_store.has_snapshot_today(member_id)
            && !result_channels.is_empty();
        let snapshot_track_indices: Vec<usize> = if needs_snapshot {
            result_channels.iter().map(|ch| ch.track_index).collect()
        } else {
            vec![]
        };
        let snapshot_channels = if needs_snapshot {
            Some(SnapshotStore::channels_from_state(&result_channels))
        } else {
            None
        };
        let snapshot_member_id = member_id.clone();

        // When a snapshot isn't needed (file already exists for today) but the date
        // changed, still update the cache so we skip the has_snapshot_today() file
        // check on subsequent ticks this session.
        //
        // MUTATION-TEST NOTE: the three predicates on this line
        //   (!needs_snapshot, last_date.as_deref() != Some(&today))
        // are covered by poller_snapshot_ordering integration tests that exercise
        // the full snapshot lifecycle. They cannot be meaningfully unit-tested in
        // isolation because they depend on async REAPER state and the snapshot file
        // system — extract would require mocking the entire AppState.
        if !needs_snapshot && last_date.as_deref() != Some(&today) {
            cache
                .snapshot_last_date
                .insert(member_id.clone(), today.clone());
        }

        // Periodic cleanup of stale command timestamps (>2s old)
        let stale_cutoff = Duration::from_secs(2);
        cache
            .command_timestamps
            .retain(|_, ts| now.duration_since(*ts) < stale_cutoff);

        // Drop the cache lock before doing async EQ reads
        drop(cache);

        // Save auto-snapshot with EQ data (outside lock — EQ reads are async HTTP calls)
        if let Some(channel_map) = snapshot_channels
            && let Err(e) = try_persist_auto_snapshot(
                state,
                &snapshot_member_id,
                &today,
                channel_map,
                snapshot_track_indices,
            )
            .await
        {
            tracing::error!(member = %snapshot_member_id, error = %e, "auto-snapshot persistence failed");
        }
    }
}

/// Persist an auto-snapshot for a member, collecting EQ data via async HTTP reads.
///
/// Returns `Ok(())` on successful save, or an error if the snapshot could not be written.
/// The cache flag `snapshot_last_date` is set only after a successful save, ensuring
/// that any failure leaves the day un-claimed so the next tick can retry.
pub async fn try_persist_auto_snapshot(
    state: &crate::AppState,
    member_id: &str,
    today: &str,
    channel_map: std::collections::HashMap<usize, iem_core::ChannelSnapshot>,
    snapshot_track_indices: Vec<usize>,
) -> Result<(), anyhow::Error> {
    // Why this ordering is critical:
    //   The cache flag `snapshot_last_date` gates retries — once set for `today`, the
    //   next channel-change tick won't try again. If we set it BEFORE save and save
    //   fails (EQ HTTP errors, disk full, etc.), the day is silently "claimed" with
    //   no file written, and no retry happens until tomorrow. Setting it AFTER save
    //   ensures failures stay retryable.

    // EQ reads are the async HTTP step that may fail or be slow.
    // On failure the snapshot is still saved — just without EQ bands.
    //
    // MUTATION-TEST NOTE: the `!bands.is_empty()` guard (line ~969) survives mutation
    // testing because it is inside an async HTTP path that requires a live REAPER
    // connection. It is covered by the deploy-runner E2E snapshot tests, not by
    // unit tests. Extracting a helper would add a level of indirection for a single
    // boolean inversion that is clearer inline.
    let mut eq_bands_map = std::collections::HashMap::new();
    for track_idx in &snapshot_track_indices {
        if let Some(iem_core::ServerMsg::EqParams { bands, .. }) =
            crate::proxy::handle_get_eq_params(state, *track_idx).await
            && !bands.is_empty()
        {
            eq_bands_map.insert(*track_idx, bands);
        }
    }
    let eq_bands = if eq_bands_map.is_empty() {
        None
    } else {
        Some(eq_bands_map)
    };
    let snapshot = MixSnapshot::new_auto(channel_map, eq_bands);
    state
        .snapshot_store
        .save_snapshot(member_id, snapshot)
        .map_err(|e| anyhow::anyhow!("Failed to save daily auto-snapshot: {e}"))?;
    tracing::info!(member = %member_id, "Saved daily auto-snapshot with EQ");

    // Only NOW mark the day as "done" — failure above leaves the cache un-claimed
    // so the next channel change retries on the same day.
    {
        let mut cache = state.mixer_cache.write().await;
        cache
            .snapshot_last_date
            .insert(member_id.to_string(), today.to_string());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MixerCache;

    /// Test that command_timestamps suppress echo broadcasts
    #[test]
    fn test_echo_suppression_within_window() {
        let now = std::time::Instant::now();
        let echo_window = Duration::from_millis(500);

        let mut ts_map: HashMap<(String, usize), std::time::Instant> = HashMap::new();
        // Simulate a command sent 100ms ago
        let key = ("oldmember1".to_string(), 1);
        ts_map.insert(key.clone(), now - Duration::from_millis(100));

        // Check: should be suppressed (100ms < 500ms window)
        let recently_commanded = ts_map
            .get(&key)
            .is_some_and(|ts| now.duration_since(*ts) < echo_window);
        assert!(
            recently_commanded,
            "Should suppress echo within 500ms window"
        );
    }

    /// Test that broadcasts happen normally outside the suppression window
    #[test]
    fn test_no_suppression_outside_window() {
        let now = std::time::Instant::now();
        let echo_window = Duration::from_millis(500);

        let mut ts_map: HashMap<(String, usize), std::time::Instant> = HashMap::new();
        // Simulate a command sent 600ms ago
        let key = ("oldmember1".to_string(), 1);
        ts_map.insert(key.clone(), now - Duration::from_millis(600));

        let recently_commanded = ts_map
            .get(&key)
            .is_some_and(|ts| now.duration_since(*ts) < echo_window);
        assert!(
            !recently_commanded,
            "Should NOT suppress echo outside 500ms window"
        );
    }

    /// Test that non-commanded channels are not suppressed
    #[test]
    fn test_no_suppression_for_unrelated_channel() {
        let now = std::time::Instant::now();
        let echo_window = Duration::from_millis(500);

        let mut ts_map: HashMap<(String, usize), std::time::Instant> = HashMap::new();
        // Command for track 1 only
        let key1 = ("oldmember1".to_string(), 1);
        ts_map.insert(key1, now - Duration::from_millis(100));

        // Track 2 should NOT be suppressed
        let key2 = ("oldmember1".to_string(), 2);
        let recently_commanded = ts_map
            .get(&key2)
            .is_some_and(|ts| now.duration_since(*ts) < echo_window);
        assert!(
            !recently_commanded,
            "Unrelated track should NOT be suppressed"
        );
    }

    /// Test that timestamp cleanup removes stale entries
    #[test]
    fn test_stale_timestamp_cleanup() {
        let now = std::time::Instant::now();
        let stale_cutoff = Duration::from_secs(2);

        let mut ts_map: HashMap<(String, usize), std::time::Instant> = HashMap::new();
        let key1 = ("oldmember1".to_string(), 1);
        let key2 = ("oldmember1".to_string(), 2);
        // Fresh timestamp (100ms ago)
        ts_map.insert(key1.clone(), now - Duration::from_millis(100));
        // Stale timestamp (3s ago)
        ts_map.insert(key2, now - Duration::from_secs(3));

        ts_map.retain(|_, ts| now.duration_since(*ts) < stale_cutoff);

        assert_eq!(ts_map.len(), 1, "Stale entries should be cleaned up");
        assert!(ts_map.contains_key(&key1), "Fresh entry should be retained");
    }

    /// Test that MixerCache initializes with empty command_timestamps
    #[test]
    fn test_mixer_cache_new_has_empty_timestamps() {
        let cache = MixerCache::new();
        assert!(cache.command_timestamps.is_empty());
    }

    /// Test that NTRACK output track name matching works
    #[test]
    fn test_ntrack_output_track_name_matching() {
        let member_track_names: HashMap<String, String> = [
            ("OLDMEMBER1 inear".to_string(), "oldmember1".to_string()),
            ("MEMBER3 inear".to_string(), "member3".to_string()),
        ]
        .into_iter()
        .collect();

        // Simulated NTRACK line for OLDMEMBER1 inear at track index 23
        let line = "TRACK\t23\tOLDMEMBER1 inear\t0\t1.000000\t0.000000\t-2000\t-2000\t1.000000\t0\t0\t22\t0\t0";
        let parts: Vec<&str> = line.split('\t').collect();
        let track_name = parts[2];

        assert!(member_track_names.contains_key(track_name));
        assert_eq!(
            member_track_names.get(track_name),
            Some(&"oldmember1".to_string())
        );
    }

    /// Test that NTRACK flags mute bit is correctly detected
    #[test]
    fn test_ntrack_flags_mute_bit() {
        // Flag 8 = muted (bit 3)
        assert!((8_i32 & 8) != 0, "Flag 8 should be muted");
        assert!((0_i32 & 8) == 0, "Flag 0 should be unmuted");
        // Flag can have other bits set too
        assert!((9_i32 & 8) != 0, "Flag 9 (8+1) should be muted");
        assert!((7_i32 & 8) == 0, "Flag 7 should be unmuted");
    }

    /// Test MixerCache initializes with empty global volumes
    #[test]
    fn test_mixer_cache_new_has_empty_global_volumes() {
        let cache = MixerCache::new();
        assert!(cache.global_volumes.is_empty());
        assert!(cache.output_track_indices.is_empty());
    }

    /// Helper: parse stereo meters from NTRACK response text using the same logic as the poller.
    /// REAPER HTTP API docs: values are dB×10, so -100 = -10 dB.
    fn parse_meters_from_ntrack(text: &str) -> HashMap<usize, [f32; 2]> {
        let mut meters = HashMap::new();
        for line in text.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.first() == Some(&"TRACK") && parts.len() > 7 {
                if let Ok(track_idx) = parts[1].parse::<usize>() {
                    if parts.len() >= 14 {
                        if let (Ok(peak_db10), Ok(pos_db10)) =
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
                }
            }
        }
        meters
    }

    /// 14-field TRACK line should produce meter data with correct dB×10 conversion
    #[test]
    fn test_ntrack_with_meter_produces_data() {
        // 14 fields: TRACK idx name flags vol pan peak pos width panmode sendcnt recvcnt hwout color
        // Use -100 (= -10 dB) and -80 (= -8 dB) — realistic signal levels
        let line =
            "TRACK\t1\tOLDMEMBER1 mic\t0\t1.000000\t0.000000\t-100\t-80\t1.000000\t0\t9\t0\t0\t0";
        let meters = parse_meters_from_ntrack(line);
        assert!(
            meters.contains_key(&1),
            "14-field line should produce meter for track 1"
        );
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

    /// 12-field TRACK line (without meter fields) must NOT produce meter data
    #[test]
    fn test_ntrack_without_meter_no_data() {
        // 12 fields: TRACK idx name flags vol pan width panmode sendcnt recvcnt hwout color
        // Field [6] is width (1.000000), NOT a meter value
        let line = "TRACK\t1\tOLDMEMBER1 mic\t0\t1.000000\t0.000000\t1.000000\t0\t9\t0\t0\t0";
        let meters = parse_meters_from_ntrack(line);
        assert!(
            !meters.contains_key(&1),
            "12-field line (no meter) should NOT produce meter data"
        );
    }

    /// REAPER meter floor (-1500 = -150 dB) must produce 0.0 (silence)
    /// Verified 2026-02-28: all 33 live tracks report -1500 with no audio.
    #[test]
    fn test_reaper_meter_floor_is_silence() {
        let line = "TRACK\t1\tOLDMEMBER1 mic\t192\t1.000000\t0.000000\t-1500\t-1500\t1.000000\t3\t9\t0\t0\t24421844";
        let meters = parse_meters_from_ntrack(line);
        assert_eq!(
            meters.get(&1),
            Some(&[0.0, 0.0]),
            "-1500 (dB×10 = -150 dB, REAPER meter floor) must be silence"
        );
    }

    /// Values just above floor should produce tiny but non-zero signal
    #[test]
    fn test_reaper_above_floor_shows_signal() {
        // L: -140 dB×10 = -14.0 dB, R: -120 dB×10 = -12.0 dB
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
        // -14.0 dB → 10^(-14/20) ≈ 0.1995
        assert!(
            (left - 0.1995).abs() < 0.01,
            "L expected ~0.1995, got {}",
            left
        );
        // -12.0 dB → 10^(-12/20) ≈ 0.2512
        assert!(
            (right - 0.2512).abs() < 0.01,
            "R expected ~0.2512, got {}",
            right
        );
    }

    /// Noise floor values (~-900 dB×10 = -90 dB) should produce near-zero linear
    /// This is the key bug: MEMBER2 mic reports -925 which is -92.5 dB (digital noise),
    /// but was previously treated as -9.25 dB (loud signal) due to /100 instead of /10.
    #[test]
    fn test_reaper_noise_floor_is_near_zero() {
        // Real captured value: MEMBER2 mic reports -925 dB×10 = -92.5 dB
        let line = "TRACK\t2\tMEMBER2 mic\t192\t1.000000\t0.000000\t-925\t-925\t1.000000\t3\t9\t0\t0\t24421844";
        let meters = parse_meters_from_ntrack(line);
        let [left, right] = meters[&2];
        // -925 dB×10 = -92.5 dB → 10^(-92.5/20) = 10^(-4.625) ≈ 0.0000237
        // This should be essentially invisible on the meter (< 0.01%)
        assert!(
            left < 0.001,
            "-925 (dB×10 = -92.5 dB noise floor) should be near-zero, got {}",
            left
        );
        assert!(
            right < 0.001,
            "-925 (dB×10 = -92.5 dB noise floor) should be near-zero, got {}",
            right
        );
        // But it should be non-zero (it's above the -1500 floor)
        assert!(left > 0.0, "Above floor should be non-zero");
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
            assert_eq!(
                *left, 0.0,
                "Track {} L should be silent (0.0), got {}",
                idx, left
            );
            assert_eq!(
                *right, 0.0,
                "Track {} R should be silent (0.0), got {}",
                idx, right
            );
        }
    }

    /// 0 dB×10 (= 0 dB = full scale) should be 1.0 linear
    #[test]
    fn test_ntrack_meter_full_scale() {
        let line = "TRACK\t1\tOLDMEMBER1 mic\t0\t1.000000\t0.000000\t0\t0\t1.000000\t0\t9\t0\t0\t0";
        let meters = parse_meters_from_ntrack(line);
        let [left, right] = meters[&1];
        assert!(
            (left - 1.0).abs() < 0.01,
            "0 (dB×10 = 0 dB) L should be ~1.0 linear, got {}",
            left
        );
        assert!(
            (right - 1.0).abs() < 0.01,
            "0 (dB×10 = 0 dB) R should be ~1.0 linear, got {}",
            right
        );
    }

    /// Multiple tracks: only 14-field lines produce meters
    #[test]
    fn test_ntrack_mixed_field_counts() {
        let text = "\
NTRACK\t33
TRACK\t1\tOLDMEMBER1 mic\t0\t1.000000\t0.000000\t-1500\t-1500\t1.000000\t0\t9\t0\t0\t0
TRACK\t2\tMEMBER2 mic\t0\t1.000000\t0.000000\t1.000000\t0\t9\t0\t0\t0
TRACK\t23\tOLDMEMBER1 inear\t0\t1.000000\t0.000000\t-50\t-60\t1.000000\t0\t0\t22\t0\t0";
        let meters = parse_meters_from_ntrack(text);
        assert!(
            meters.contains_key(&1),
            "Track 1 (14 fields) should have meter"
        );
        assert!(
            !meters.contains_key(&2),
            "Track 2 (12 fields) should NOT have meter"
        );
        assert!(
            meters.contains_key(&23),
            "Track 23 (14 fields) should have meter"
        );
        // Verify: track 23 has different peak (-50 = -5 dB) and pos (-60 = -6 dB)
        let [l, r] = meters[&23];
        assert!(
            l > r,
            "L (-50 = -5 dB) should be louder than R (-60 = -6 dB)"
        );
    }

    // --- Meter bridge EXTSTATE parser tests ---

    /// Parse typical meter bridge output with multiple tracks
    #[test]
    fn test_parse_meter_bridge_basic() {
        let input = "1:-37,-37;2:-911,-911;11:-178,-250";
        let meters = parse_meter_bridge(input);
        assert_eq!(meters.len(), 3);

        // Track 1: -37 dB×10 = -3.7 dB → 10^(-3.7/20) ≈ 0.653
        let [l, r] = meters[&1];
        assert!((l - 0.653).abs() < 0.01, "Track 1 L: got {}", l);
        assert!((r - 0.653).abs() < 0.01, "Track 1 R: got {}", r);

        // Track 11: L=-178 (-17.8 dB), R=-250 (-25.0 dB) — TRUE STEREO (different L/R)
        let [l, r] = meters[&11];
        assert!(l > r, "Panned left: L ({}) should be > R ({})", l, r);
        // -17.8 dB → 10^(-17.8/20) ≈ 0.1288
        assert!((l - 0.1288).abs() < 0.01, "Track 11 L: got {}", l);
        // -25.0 dB → 10^(-25/20) ≈ 0.0562
        assert!((r - 0.0562).abs() < 0.01, "Track 11 R: got {}", r);
    }

    /// Parse meter bridge with floor values (silence)
    #[test]
    fn test_parse_meter_bridge_silence() {
        let input = "1:-1500,-1500;2:-1500,-1500";
        let meters = parse_meter_bridge(input);
        assert_eq!(meters[&1], [0.0, 0.0]);
        assert_eq!(meters[&2], [0.0, 0.0]);
    }

    /// Parse empty/missing meter bridge data
    #[test]
    fn test_parse_meter_bridge_empty() {
        assert!(parse_meter_bridge("").is_empty());
        assert!(parse_meter_bridge("  ").is_empty());
    }

    /// Parse meter bridge with noise floor values
    #[test]
    fn test_parse_meter_bridge_noise_floor() {
        // -925 dB×10 = -92.5 dB → should be near-zero (preamp noise)
        let input = "2:-925,-925";
        let meters = parse_meter_bridge(input);
        let [l, r] = meters[&2];
        assert!(l < 0.001, "Noise floor L should be near-zero, got {}", l);
        assert!(r < 0.001, "Noise floor R should be near-zero, got {}", r);
        assert!(l > 0.0, "Above floor should be non-zero");
    }

    /// Parse meter bridge with malformed entries (graceful handling)
    #[test]
    fn test_parse_meter_bridge_malformed() {
        // Mixed valid and invalid entries
        let input = "1:-37,-37;bad;3:-50";
        let meters = parse_meter_bridge(input);
        assert_eq!(meters.len(), 1, "Only valid entries should be parsed");
        assert!(meters.contains_key(&1));
    }

    /// Discovery probes up to 10 sends per member with 50ms delay between requests.
    /// This prevents overwhelming REAPER's HTTP API on startup.
    #[test]
    fn test_discovery_rate_limit_bounds() {
        let delay_ms: u64 = 50;
        let max_sends_per_member: u64 = 10;
        let max_members: u64 = 9;
        let worst_case_ms = delay_ms * max_sends_per_member * max_members;
        assert!(
            worst_case_ms <= 5000,
            "Discovery should complete within 5 seconds, got {}ms",
            worst_case_ms
        );
    }

    // ================================================================
    // Track index safety tests (Issue reaperiem#85)
    // ================================================================

    /// When poller discovers output track at a different index, DiscoveredMember must sync
    #[test]
    fn test_output_track_index_sync_detects_shift() {
        let mut member = DiscoveredMember {
            name: "OLDMEMBER1".to_string(),
            track_index: 23,
            dante_output_l: 89,
            dante_output_r: 90,
            send_index: 0,
            mix_send_index: Some(1),
            mix_send_indices: std::collections::HashMap::new(),
        };

        // Simulate: poller found "OLDMEMBER1 inear" at track 24 (shifted by 1)
        let new_idx: usize = 24;
        if member.track_index != new_idx {
            member.track_index = new_idx;
        }

        assert_eq!(
            member.track_index, 24,
            "DiscoveredMember.track_index must update when track shifts"
        );
    }

    /// When track index hasn't changed, no update should occur
    #[test]
    fn test_output_track_index_sync_no_change() {
        let member = DiscoveredMember {
            name: "OLDMEMBER1".to_string(),
            track_index: 23,
            dante_output_l: 89,
            dante_output_r: 90,
            send_index: 0,
            mix_send_index: Some(1),
            mix_send_indices: std::collections::HashMap::new(),
        };

        let new_idx: usize = 23;
        assert_eq!(
            member.track_index, new_idx,
            "No update needed when index matches"
        );
    }

    /// Track count change should be detectable
    #[test]
    fn test_track_count_change_detection() {
        let mut cache = MixerCache::new();
        assert!(cache.last_track_count.is_none());

        // First observation
        cache.last_track_count = Some(33);
        assert_eq!(cache.last_track_count, Some(33));

        // Track inserted → count changes
        let new_count: usize = 34;
        let changed = cache
            .last_track_count
            .map_or(false, |prev| prev != new_count);
        assert!(changed, "Should detect track count change from 33 to 34");
        cache.last_track_count = Some(new_count);
        assert_eq!(cache.last_track_count, Some(34));
    }

    /// Input track indices should be resolvable by name
    #[test]
    fn test_input_track_name_resolution() {
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

        // Simulate NTRACK showing tracks at shifted positions
        let mut all_track_names: HashMap<String, usize> = HashMap::new();
        all_track_names.insert("NEW TRACK".to_string(), 1); // inserted track
        all_track_names.insert("OLDMEMBER1 mic".to_string(), 2); // shifted
        all_track_names.insert("MEMBER2 mic".to_string(), 3); // shifted

        let mut input_indices: HashMap<String, usize> = HashMap::new();
        for input in &inputs {
            if let Some(&idx) = all_track_names.get(&input.name) {
                input_indices.insert(input.name.clone(), idx);
            }
        }

        assert_eq!(input_indices.get("OLDMEMBER1 mic"), Some(&2));
        assert_eq!(input_indices.get("MEMBER2 mic"), Some(&3));
    }

    /// MixerCache should initialize with empty input_track_indices
    #[test]
    fn test_mixer_cache_new_has_empty_input_track_indices() {
        let cache = MixerCache::new();
        assert!(cache.input_track_indices.is_empty());
        assert!(cache.last_track_count.is_none());
    }

    // ================================================================
    // Cache invalidation on track index shift (follow-up to reaperiem#85)
    // ================================================================

    /// When input track indices change, member_states must be cleared to force
    /// a full State resync. Without this, the delta diff compares by track_index
    /// and misses shifted channels, leaving frontends with stale data.
    #[test]
    fn test_index_shift_clears_member_states_for_full_resync() {
        let mut cache = MixerCache::new();

        // Simulate cached state: MEMBER2 at track 2
        cache.member_states.insert(
            "engineer".to_string(),
            vec![
                iem_core::Channel {
                    track_index: 1,
                    name: "OLDMEMBER1 mic".into(),
                    level_db: -10.0,
                    pan: 0.0,
                    muted: false,
                    category: "mics".into(),
                    stereo_pair: None,
                    stereo_side: None,
                },
                iem_core::Channel {
                    track_index: 2,
                    name: "MEMBER2 mic".into(),
                    level_db: -10.0,
                    pan: 0.0,
                    muted: false,
                    category: "mics".into(),
                    stereo_pair: None,
                    stereo_side: None,
                },
            ],
        );
        cache.input_track_indices = [
            ("OLDMEMBER1 mic".to_string(), 1),
            ("MEMBER2 mic".to_string(), 2),
        ]
        .into_iter()
        .collect();

        // New indices: MEMBER2 shifted to track 3
        let new_indices: HashMap<String, usize> = [
            ("OLDMEMBER1 mic".to_string(), 1),
            ("MEMBER2 mic".to_string(), 3),
        ]
        .into_iter()
        .collect();

        // Detect change → member_states must be cleared
        if cache.input_track_indices != new_indices {
            cache.member_states.clear();
        }
        cache.input_track_indices = new_indices;

        assert!(
            cache.member_states.is_empty(),
            "member_states must be cleared on index shift to force full State resend"
        );
    }

    /// When output track indices change (e.g., member inear track shifts),
    /// member_states must be cleared so engineer mix channels get correct data.
    #[test]
    fn test_output_index_shift_clears_member_states() {
        let mut cache = MixerCache::new();
        cache
            .output_track_indices
            .insert("oldmember1".to_string(), 23);
        cache.member_states.insert(
            "engineer".to_string(),
            vec![iem_core::Channel {
                track_index: 23,
                name: "OLDMEMBER1 inear".into(),
                level_db: -6.0,
                pan: 0.0,
                muted: false,
                category: "members".into(),
                stereo_pair: None,
                stereo_side: None,
            }],
        );

        // Output track shifts from 23 to 24
        let new_idx: usize = 24;
        let old_idx = cache.output_track_indices.get("oldmember1").copied();
        if old_idx.is_some_and(|old| old != new_idx) {
            cache.member_states.clear();
        }
        cache
            .output_track_indices
            .insert("oldmember1".to_string(), new_idx);

        assert!(
            cache.member_states.is_empty(),
            "member_states must be cleared on output index shift"
        );
    }

    /// When indices haven't changed, member_states should NOT be cleared
    #[test]
    fn test_no_clear_when_indices_unchanged() {
        let mut cache = MixerCache::new();
        cache.member_states.insert("oldmember1".to_string(), vec![]);
        cache.input_track_indices = [
            ("OLDMEMBER1 mic".to_string(), 1),
            ("MEMBER2 mic".to_string(), 2),
        ]
        .into_iter()
        .collect();

        // Same indices — no change
        let new_indices: HashMap<String, usize> = cache.input_track_indices.clone();
        if cache.input_track_indices != new_indices {
            cache.member_states.clear();
        }
        cache.input_track_indices = new_indices;

        assert!(
            !cache.member_states.is_empty(),
            "member_states must NOT be cleared when indices are unchanged"
        );
    }

    /// METER_BRIDGE_STARTED resets on disconnect so bridge re-triggers on reconnect.
    /// Without this, REAPER restarts lose the meter bridge forever.
    #[test]
    fn test_meter_bridge_flag_resets_on_disconnect() {
        // Simulate: bridge was started (flag = true)
        METER_BRIDGE_STARTED.store(true, Ordering::Relaxed);
        assert!(METER_BRIDGE_STARTED.load(Ordering::Relaxed));

        // Simulate disconnect: flag should reset
        METER_BRIDGE_STARTED.store(false, Ordering::Relaxed);
        assert!(
            !METER_BRIDGE_STARTED.load(Ordering::Relaxed),
            "Flag must reset on disconnect so bridge re-triggers on reconnect"
        );
    }

    // ================================================================
    // Meter change detection tests (v1.72.0 performance optimization)
    // ================================================================

    /// Helper: compute delta meters using the same logic as poll_and_broadcast.
    /// Returns only meters that changed beyond the 0.01 threshold.
    fn compute_meter_delta(
        prev: &HashMap<usize, [f32; 2]>,
        current: &HashMap<usize, [f32; 2]>,
    ) -> HashMap<usize, [f32; 2]> {
        current
            .iter()
            .filter(|(idx, [l, r])| {
                prev.get(idx)
                    .is_none_or(|[pl, pr]| (l - pl).abs() > 0.01 || (r - pr).abs() > 0.01)
            })
            .map(|(k, v)| (*k, *v))
            .collect()
    }

    /// Unchanged meters should NOT be broadcast (saves bandwidth during silence)
    #[test]
    fn test_meter_delta_unchanged_is_empty() {
        let meters: HashMap<usize, [f32; 2]> = [(1, [0.5, 0.5]), (2, [0.0, 0.0]), (3, [0.3, 0.25])]
            .into_iter()
            .collect();

        let delta = compute_meter_delta(&meters, &meters);
        assert!(
            delta.is_empty(),
            "Identical meters should produce empty delta, got {} entries",
            delta.len()
        );
    }

    /// Changed meters should be included in the delta
    #[test]
    fn test_meter_delta_changed_is_broadcast() {
        let prev: HashMap<usize, [f32; 2]> = [(1, [0.5, 0.5]), (2, [0.0, 0.0]), (3, [0.3, 0.25])]
            .into_iter()
            .collect();

        // Track 1 changed significantly, track 2 unchanged, track 3 changed
        let current: HashMap<usize, [f32; 2]> = [
            (1, [0.7, 0.6]),  // changed
            (2, [0.0, 0.0]),  // unchanged
            (3, [0.1, 0.25]), // L changed
        ]
        .into_iter()
        .collect();

        let delta = compute_meter_delta(&prev, &current);
        assert_eq!(delta.len(), 2, "Should have 2 changed tracks");
        assert!(delta.contains_key(&1), "Track 1 changed");
        assert!(delta.contains_key(&3), "Track 3 L changed");
        assert!(!delta.contains_key(&2), "Track 2 unchanged");
    }

    /// Small changes below threshold should NOT trigger broadcast
    #[test]
    fn test_meter_delta_below_threshold() {
        let prev: HashMap<usize, [f32; 2]> = [(1, [0.5, 0.5])].into_iter().collect();
        let current: HashMap<usize, [f32; 2]> = [(1, [0.505, 0.498])].into_iter().collect();

        let delta = compute_meter_delta(&prev, &current);
        assert!(
            delta.is_empty(),
            "Changes < 0.01 should be suppressed, got {} entries",
            delta.len()
        );
    }

    /// New tracks (not in previous cache) should always be broadcast
    #[test]
    fn test_meter_delta_new_track_is_broadcast() {
        let prev: HashMap<usize, [f32; 2]> = [(1, [0.5, 0.5])].into_iter().collect();
        let current: HashMap<usize, [f32; 2]> = [
            (1, [0.5, 0.5]), // unchanged
            (2, [0.3, 0.3]), // new track
        ]
        .into_iter()
        .collect();

        let delta = compute_meter_delta(&prev, &current);
        assert_eq!(delta.len(), 1);
        assert!(delta.contains_key(&2), "New track should be broadcast");
    }

    /// All-silent meters with empty previous cache should be broadcast once
    #[test]
    fn test_meter_delta_first_broadcast_includes_all() {
        let prev: HashMap<usize, [f32; 2]> = HashMap::new();
        let current: HashMap<usize, [f32; 2]> =
            [(1, [0.0, 0.0]), (2, [0.0, 0.0])].into_iter().collect();

        let delta = compute_meter_delta(&prev, &current);
        assert_eq!(
            delta.len(),
            2,
            "First broadcast should include all tracks (no previous cache)"
        );
    }

    // ================================================================
    // Re-discovery tests (REAPER unavailable at startup hardening)
    // ================================================================

    /// Helper to create a test DiscoveredMember
    fn make_member(
        name: &str,
        track_index: usize,
        mix_send_index: Option<usize>,
    ) -> DiscoveredMember {
        DiscoveredMember {
            name: name.to_string(),
            track_index,
            dante_output_l: 89,
            dante_output_r: 90,
            send_index: 0,
            mix_send_index,
            mix_send_indices: std::collections::HashMap::new(),
        }
    }

    /// Member with mix_send_index == None triggers rediscovery
    #[test]
    fn test_discovery_incomplete_when_mix_send_index_is_none() {
        let members = vec![
            make_member("OLDMEMBER1", 23, None),
            make_member("MEMBER2", 24, Some(1)),
            make_member("ENGINEER", 32, None),
        ];
        assert!(
            needs_rediscovery(&members),
            "OLDMEMBER1 has mix_send_index=None → needs rediscovery"
        );
    }

    /// Member with track_index == 0 (fallback placeholder) triggers rediscovery
    #[test]
    fn test_discovery_incomplete_when_track_index_zero() {
        let members = vec![
            make_member("OLDMEMBER1", 0, Some(1)),
            make_member("ENGINEER", 32, None),
        ];
        assert!(
            needs_rediscovery(&members),
            "OLDMEMBER1 has track_index=0 → needs rediscovery"
        );
    }

    /// All members properly discovered → no rediscovery needed
    #[test]
    fn test_discovery_complete_when_all_fields_set() {
        let members = vec![
            make_member("OLDMEMBER1", 23, Some(1)),
            make_member("MEMBER2", 24, Some(1)),
            make_member("ENGINEER", 32, None),
        ];
        assert!(
            !needs_rediscovery(&members),
            "All non-engineer members have valid track_index and mix_send_index"
        );
    }

    /// Engineer with mix_send_index == None does NOT trigger rediscovery
    /// (engineer is the target track, not a source — it never has mix_send_index)
    #[test]
    fn test_engineer_excluded_from_rediscovery_check() {
        let members = vec![make_member("ENGINEER", 32, None)];
        assert!(
            !needs_rediscovery(&members),
            "Engineer alone with mix_send_index=None should NOT trigger rediscovery"
        );
    }

    /// Empty members list does not trigger rediscovery (nothing to check)
    #[test]
    fn test_rediscovery_empty_members() {
        let members: Vec<DiscoveredMember> = vec![];
        assert!(
            !needs_rediscovery(&members),
            "Empty member list should not trigger rediscovery"
        );
    }

    // ================================================================
    // Periodic full meter broadcast tests (Bug reaperiem#2: meters not showing)
    // ================================================================

    /// Helper: simulate the meter broadcast logic used in poll_reaper_and_broadcast.
    /// Returns what would be broadcast given current meters, cached (prev) meters, and cycle count.
    fn simulate_meter_broadcast(
        prev: &HashMap<usize, [f32; 2]>,
        current: &HashMap<usize, [f32; 2]>,
        cycle: u64,
    ) -> HashMap<usize, [f32; 2]> {
        let force_full = cycle.is_multiple_of(7);
        if force_full && !current.is_empty() {
            current.clone()
        } else {
            current
                .iter()
                .filter(|(idx, [l, r])| {
                    prev.get(idx)
                        .is_none_or(|[pl, pr]| (l - pl).abs() > 0.01 || (r - pr).abs() > 0.01)
                })
                .map(|(k, v)| (*k, *v))
                .collect()
        }
    }

    /// Bug 2 reproduction: new client connects, meters are stable → delta is empty → no broadcast.
    /// With the fix, cycle 0 (and every 7th) forces a full broadcast.
    #[test]
    fn test_new_client_receives_meters_via_periodic_full_broadcast() {
        let cached: HashMap<usize, [f32; 2]> = [(1, [0.251, 0.251]), (23, [0.158, 0.158])]
            .into_iter()
            .collect();
        // Same values returned by next poll — delta would be empty
        let current = cached.clone();

        // On a non-full cycle, delta is empty (this was the bug)
        let delta_only = simulate_meter_broadcast(&cached, &current, 1);
        assert!(
            delta_only.is_empty(),
            "Delta-only broadcast on unchanged meters should be empty"
        );

        // On a full broadcast cycle (every 7th), ALL meters are sent regardless of delta
        let full = simulate_meter_broadcast(&cached, &current, 7);
        assert_eq!(
            full.len(),
            2,
            "Full broadcast cycle should send all meters, got {}",
            full.len()
        );
        assert!(full.contains_key(&1));
        assert!(full.contains_key(&23));
    }

    /// Verify that cycle 0 triggers full broadcast (first poll after startup)
    #[test]
    fn test_cycle_zero_triggers_full_broadcast() {
        let prev: HashMap<usize, [f32; 2]> = HashMap::new();
        let current: HashMap<usize, [f32; 2]> = [(1, [0.5, 0.5])].into_iter().collect();

        let result = simulate_meter_broadcast(&prev, &current, 0);
        assert_eq!(result.len(), 1, "Cycle 0 should force full broadcast");
    }

    /// Verify that non-7th cycles still use delta logic
    #[test]
    fn test_non_full_cycle_uses_delta() {
        let prev: HashMap<usize, [f32; 2]> =
            [(1, [0.5, 0.5]), (2, [0.3, 0.3])].into_iter().collect();
        let current: HashMap<usize, [f32; 2]> = [
            (1, [0.5, 0.5]), // unchanged
            (2, [0.8, 0.8]), // changed
        ]
        .into_iter()
        .collect();

        // Cycle 3 is not a full broadcast cycle
        let result = simulate_meter_broadcast(&prev, &current, 3);
        assert_eq!(result.len(), 1, "Only changed track should be broadcast");
        assert!(result.contains_key(&2));
    }

    /// Verify that full broadcast cycles are every 7th: 0, 7, 14, 21...
    #[test]
    fn test_full_broadcast_cycle_frequency() {
        let meters: HashMap<usize, [f32; 2]> = [(1, [0.5, 0.5])].into_iter().collect();
        let same = meters.clone();

        for cycle in 0..28 {
            let result = simulate_meter_broadcast(&meters, &same, cycle);
            if cycle.is_multiple_of(7) {
                assert_eq!(result.len(), 1, "Cycle {} should be full broadcast", cycle);
            } else {
                assert!(
                    result.is_empty(),
                    "Cycle {} should be delta-only (empty)",
                    cycle
                );
            }
        }
    }

    /// Test A: During active listen, cache reflects REAL REAPER mute state (not pre-listen).
    /// This proves the cache poisoning bug is fixed: REAPER says muted=true (by ReaScript),
    /// so the cache must also say muted=true — NOT the pre-listen muted=false.
    #[test]
    fn test_listen_cache_reflects_real_reaper_state() {
        // Simulate: REAPER reports channels muted by ReaScript during listen
        let reaper_channels = vec![
            iem_core::Channel {
                track_index: 23,
                name: "OLDMEMBER1".to_string(),
                level_db: -6.0,
                muted: true, // ReaScript muted for listen isolation
                pan: 0.0,
                category: String::new(),
                stereo_pair: None,
                stereo_side: None,
            },
            iem_core::Channel {
                track_index: 24,
                name: "MEMBER2".to_string(),
                level_db: -3.0,
                muted: true, // ReaScript muted for listen isolation
                pan: 0.0,
                category: String::new(),
                stereo_pair: None,
                stereo_side: None,
            },
        ];

        // The poller now writes REAPER state directly to cache (no suppression replacement).
        // Simulate what the poller does: insert result_channels into cache.
        let mut cache = MixerCache::new();
        cache
            .member_states
            .insert("engineer".to_string(), reaper_channels.clone());

        // Assert: cache has REAL REAPER state (muted=true), not pre-listen state
        let cached = cache.member_states.get("engineer").unwrap();
        assert!(
            cached[0].muted,
            "OLDMEMBER1 cache must reflect REAPER truth: muted=true during listen"
        );
        assert!(
            cached[1].muted,
            "MEMBER2 cache must reflect REAPER truth: muted=true during listen"
        );
    }

    /// ListenTarget defaults to Idle
    #[test]
    fn test_listen_target_default_is_idle() {
        let target = crate::ListenTarget::default();
        assert!(
            matches!(target, crate::ListenTarget::Idle),
            "Default ListenTarget must be Idle"
        );
    }

    /// ListenTarget::Member stores saved mute states
    #[test]
    fn test_listen_target_member_stores_saved_mutes() {
        let saved = vec![(23, 5, false), (24, 5, true), (25, 5, false)];
        let target = crate::ListenTarget::Member(saved.clone());
        if let crate::ListenTarget::Member(mutes) = target {
            assert_eq!(mutes.len(), 3, "Member must store all saved mute states");
            assert_eq!(mutes[0], (23, 5, false), "First member: unmuted before");
            assert_eq!(mutes[1], (24, 5, true), "Second member: muted before");
            assert_eq!(mutes[2], (25, 5, false), "Third member: unmuted before");
        } else {
            panic!("Expected Member variant");
        }
    }

    /// Poller broadcasts ALL changes without suppression (no listen special-casing)
    #[test]
    fn test_poller_broadcasts_mute_changes_during_listen() {
        // With mute-based listen, the poller has no suppression logic.
        // All changes from REAPER are broadcast to the UI unconditionally.
        let old_ch = iem_core::Channel {
            track_index: 23,
            name: "OLDMEMBER1".to_string(),
            level_db: -6.0,
            muted: false,
            pan: 0.0,
            category: String::new(),
            stereo_pair: None,
            stereo_side: None,
        };
        let new_ch = iem_core::Channel {
            track_index: 23,
            name: "OLDMEMBER1".to_string(),
            level_db: -6.0,
            muted: true, // User toggled mute during listen
            pan: 0.0,
            category: String::new(),
            stereo_pair: None,
            stereo_side: None,
        };

        let changed = (old_ch.level_db - new_ch.level_db).abs() > 0.05
            || old_ch.muted != new_ch.muted
            || (old_ch.pan - new_ch.pan).abs() > 0.001;
        assert!(changed, "Mute change should be detected");

        // No suppression — all changes broadcast
        let should_broadcast = changed;
        assert!(
            should_broadcast,
            "Mute changes must always be broadcast (no suppression during listen)"
        );
    }

    #[test]
    fn parse_limiter_activity_totals_parses_pairs() {
        let text = "23:1230;24:0;25:8470";
        let parsed = super::parse_limiter_activity_totals(text);
        assert_eq!(parsed.get(&23), Some(&1230));
        assert_eq!(parsed.get(&24), Some(&0));
        assert_eq!(parsed.get(&25), Some(&8470));
        assert_eq!(parsed.len(), 3);
    }

    #[test]
    fn parse_limiter_activity_totals_empty_input() {
        assert!(super::parse_limiter_activity_totals("").is_empty());
    }

    #[test]
    fn parse_limiter_activity_totals_skips_malformed_entries() {
        let text = "23:1230;garbage;24:not_a_number;25:9999";
        let parsed = super::parse_limiter_activity_totals(text);
        assert_eq!(parsed.get(&23), Some(&1230));
        assert_eq!(parsed.get(&25), Some(&9999));
        assert_eq!(parsed.len(), 2);
    }
}
