//! WebSocket connection manager with deterministic disposal.
//!
//! Owns all background tasks (reconnect, watchdog, token-expiry intervals)
//! and tears them down via `on_cleanup`. Background closures check
//! `disposal_guard` (an `Arc<AtomicBool>`) before touching reactive state.

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;
use std::collections::{HashMap, HashSet};
use wasm_bindgen::prelude::*;

use iem_core::Channel;

use crate::components::eq_modal::EqBandState;
use crate::components::talk_button::TalkState;

use super::helpers::{MAX_WS_FAILURES, WsClosureStore, WsFailCounter};
use super::state::MixerState;

/// Set up all background tasks (WS connect, reconnect, watchdog, token-expiry)
/// and register an `on_cleanup` callback that tears them all down when the
/// component scope is disposed.
///
/// Background closures (`Closure::forget`) check `disposal_guard` before
/// touching any reactive state — once cleanup fires, they no-op.
///
/// This is a free function (not a struct with Drop) because Leptos `on_cleanup`
/// requires `Send` closures. We capture the i32 interval IDs and the
/// `Arc<AtomicBool>` guard in a single cleanup closure.
pub(super) fn setup_connection(
    state: MixerState,
    member_id: impl Fn() -> String + Clone + 'static,
) {
    let disposal_guard = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // --- WS closure storage, fail counter, page visibility ---

    // Closure storage: keeps WS callbacks alive without Closure::forget() leak
    let ws_closures: WsClosureStore = std::rc::Rc::new(std::cell::RefCell::new(None));

    // WS failure counter: tracks consecutive failures without receiving data
    let ws_fail_count: WsFailCounter = std::rc::Rc::new(std::cell::Cell::new(0));

    // Track page visibility — skip meter updates when backgrounded.
    // Created once in component body (NOT in connect_websocket) to avoid stacking listeners.
    let page_visible = std::rc::Rc::new(std::cell::Cell::new(true));
    {
        let pv = page_visible.clone();
        let vis_closure = Closure::wrap(Box::new(move || {
            if let Some(w) = web_sys::window()
                && let Some(doc) = w.document()
            {
                pv.set(!doc.hidden());
            }
        }) as Box<dyn FnMut()>);
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            let _ = doc.add_event_listener_with_callback(
                "visibilitychange",
                vis_closure.as_ref().unchecked_ref(),
            );
        }
        vis_closure.forget(); // Lives for component lifetime — one-time registration
    }

    // --- Watchdog + backoff state ---

    // Watchdog + backoff state (reaperiem#153). Lives in MixerPage so it survives across
    // reconnects — connect_websocket is called repeatedly by the reconnect loop
    // below, so these cells must NOT live inside that helper.
    // - last_frame_at: updated in onmessage, checked by the 5s watchdog
    // - reconnect_attempt: incremented in onclose, reset in onmessage, drives backoff
    // - last_reconnect_attempt_at: timestamp of last reconnect action, gates the
    //   backoff schedule inside the reconnect closure below
    let last_frame_at = std::rc::Rc::new(std::cell::Cell::new(js_sys::Date::now()));
    let reconnect_attempt = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let last_reconnect_attempt_at = std::rc::Rc::new(std::cell::Cell::new(0.0_f64));

    // --- Initial connect Effect ---

    let ws_member_id = member_id.clone();
    let ws_closures_effect = ws_closures.clone();
    let ws_fail_count_effect = ws_fail_count.clone();
    let page_visible_effect = page_visible.clone();
    let last_frame_at_effect = last_frame_at.clone();
    let reconnect_attempt_effect = reconnect_attempt.clone();
    let disposal_guard_effect = disposal_guard.clone();
    Effect::new(move |_| {
        let member = ws_member_id();
        if member.is_empty() {
            return;
        }

        connect_websocket(
            &member,
            last_frame_at_effect.clone(),
            reconnect_attempt_effect.clone(),
            &state,
            ws_closures_effect.clone(),
            ws_fail_count_effect.clone(),
            page_visible_effect.clone(),
            disposal_guard_effect.clone(),
        );
    });

    // Network mode (LAN/WAN) is now sent via WebSocket on every connect/reconnect,
    // so it automatically updates when switching between WiFi and mobile data.

    // --- Reconnect closure + setInterval ---

    let (ws, _set_ws) = state.ws;
    let reconnect_member_id = member_id.clone();
    let navigate_auth_fail = use_navigate();
    let reconnect_attempt_tick = reconnect_attempt.clone();
    let last_reconnect_attempt_at_tick = last_reconnect_attempt_at.clone();
    let last_frame_at_tick = last_frame_at.clone();
    let disposal_guard_reconnect = disposal_guard.clone();
    let reconnect_closure = Closure::wrap(Box::new(move || {
        if disposal_guard_reconnect.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }

        // Exponential backoff gate: skip this tick if the scheduled delay
        // hasn't elapsed since the last reconnect attempt. reaperiem#153
        let now_ms = js_sys::Date::now();
        let attempt = reconnect_attempt_tick.get();
        let delay_ms = crate::lifecycle::backoff_delay_ms(attempt) as f64;
        let last_attempt = last_reconnect_attempt_at_tick.get();
        if last_attempt > 0.0 && (now_ms - last_attempt) < delay_ms {
            return;
        }

        let needs_reconnect = match ws.try_get_untracked() {
            // Disposal: scope gone → stop reconnecting.
            None => return,
            // Signal alive, inner Option empty → nothing to reconnect yet.
            Some(None) => false,
            Some(Some(ref w)) => w.ready_state() == web_sys::WebSocket::CLOSED,
        };
        if needs_reconnect {
            let member = reconnect_member_id();
            if member.is_empty() {
                return;
            }

            // After MAX_WS_FAILURES consecutive failures, check if token is invalid
            if ws_fail_count.get() >= MAX_WS_FAILURES {
                let nav = navigate_auth_fail.clone();
                let m = member.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    if !crate::api::verify_token_valid(&m).await {
                        // Token rejected by server — clear auth and redirect to login
                        crate::auth::clear_auth();
                        let url = format!("/login?member={}&next=/{}", m, m);
                        nav(&url, Default::default());
                    }
                });
                return;
            }

            last_reconnect_attempt_at_tick.set(now_ms);
            connect_websocket(
                &member,
                last_frame_at_tick.clone(),
                reconnect_attempt_tick.clone(),
                &state,
                ws_closures.clone(),
                ws_fail_count.clone(),
                page_visible.clone(),
                disposal_guard_reconnect.clone(),
            );
        }
    }) as Box<dyn FnMut()>);
    let reconnect_interval_id = web_sys::window()
        .unwrap()
        .set_interval_with_callback_and_timeout_and_arguments_0(
            reconnect_closure.as_ref().unchecked_ref(),
            2000,
        )
        .unwrap();
    reconnect_closure.forget();

    // --- Watchdog closure + setInterval ---

    // Watchdog (reaperiem#153): every 5s, check whether the socket has received any
    // frame in the last 30s. If not, force-close it — onclose fires,
    // connected=false, the existing .disconnected-banner appears, and the
    // reconnect loop opens a new socket. Catches zombie sockets where
    // ready_state == OPEN but no data flows.
    let last_frame_at_watch = last_frame_at.clone();
    let ws_watch = ws;
    let disposal_guard_watchdog = disposal_guard.clone();
    let watchdog_closure = Closure::wrap(Box::new(move || {
        if disposal_guard_watchdog.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }

        // Double-Option: outer = disposal guard, inner = signal value.
        let Some(Some(socket)) = ws_watch.try_get_untracked() else {
            return;
        };
        if socket.ready_state() != web_sys::WebSocket::OPEN {
            return;
        }
        let now = js_sys::Date::now();
        if crate::lifecycle::is_stale(
            last_frame_at_watch.get(),
            now,
            crate::lifecycle::WS_STALENESS_THRESHOLD_MS,
        ) {
            web_sys::console::warn_1(
                &"WS watchdog: no frames for >30s, force-closing socket".into(),
            );
            let _ = socket.close();
        }
    }) as Box<dyn FnMut()>);

    let watchdog_interval_id = web_sys::window()
        .unwrap()
        .set_interval_with_callback_and_timeout_and_arguments_0(
            watchdog_closure.as_ref().unchecked_ref(),
            crate::lifecycle::WS_WATCHDOG_INTERVAL_MS as i32,
        )
        .unwrap();
    watchdog_closure.forget();

    // --- Token expiry closure + setInterval ---

    let navigate_expired = use_navigate();
    let member_for_expiry = member_id.clone();
    let disposal_guard_expiry = disposal_guard.clone();
    let expiry_closure = Closure::wrap(Box::new(move || {
        if disposal_guard_expiry.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }

        if crate::auth::is_token_expired() {
            crate::auth::clear_auth();
            let m = member_for_expiry();
            if !m.is_empty() {
                let url = format!("/login?member={}&next=/{}", m, m);
                navigate_expired(&url, Default::default());
            } else {
                navigate_expired("/", Default::default());
            }
        }
    }) as Box<dyn FnMut()>);
    let expiry_interval_id = web_sys::window()
        .unwrap()
        .set_interval_with_callback_and_timeout_and_arguments_0(
            expiry_closure.as_ref().unchecked_ref(),
            60_000, // Check every 60 seconds
        )
        .unwrap();
    expiry_closure.forget();

    // Register cleanup: set the disposal guard and clear all JS intervals
    // when the component scope is disposed. Arc<AtomicBool> is Send so it
    // can be captured directly in the on_cleanup closure.
    let guard_for_cleanup = disposal_guard.clone();
    on_cleanup(move || {
        guard_for_cleanup.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(w) = web_sys::window() {
            w.clear_interval_with_handle(reconnect_interval_id);
            w.clear_interval_with_handle(watchdog_interval_id);
            w.clear_interval_with_handle(expiry_interval_id);
        }
    });
}

/// Minimum spacing (ms) between applied meter updates: the server sends every
/// 150 ms, but network jitter can bunch messages; this caps reactive updates
/// at ~20/s.
const METER_THROTTLE_MS: f64 = 50.0;

/// Whether a meter frame arriving at `now` (ms) is applied, given when the
/// last one was.
fn meter_update_due(now: f64, last_applied: f64) -> bool {
    now - last_applied >= METER_THROTTLE_MS
}

/// Whether a server `ChannelUpdate` for `track_index` applies: never while the
/// user is touching that fader (their gesture wins).
fn channel_update_applies(touched: &HashMap<usize, bool>, track_index: usize) -> bool {
    !touched.get(&track_index).copied().unwrap_or(false)
}

/// Write a server `ChannelUpdate` into the matching channel.
fn update_channel(chs: &mut [Channel], track_index: usize, level_db: f32, muted: bool, pan: f32) {
    if let Some(ch) = chs.iter_mut().find(|c| c.track_index == track_index) {
        ch.level_db = level_db;
        ch.muted = muted;
        ch.pan = pan;
    }
}

/// What a remote `SoloUpdate` changes locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SoloChange {
    /// The echo of our own command: nothing to do.
    Unchanged,
    /// Remote un-soloed everything: forget the pre-solo mutes.
    Cleared,
    /// Remote entered solo: save the current mutes for the restore.
    Entered,
    /// Remote switched the exclusive solo: update the mute display (reaperiem#131).
    Switched,
}

fn solo_change(current: &HashSet<usize>, new: &HashSet<usize>) -> SoloChange {
    if new == current {
        SoloChange::Unchanged
    } else if new.is_empty() {
        SoloChange::Cleared
    } else if current.is_empty() {
        SoloChange::Entered
    } else {
        SoloChange::Switched
    }
}

/// Exclusive solo: every channel outside `soloed` shows as muted.
fn show_exclusive_solo(chs: &mut [Channel], soloed: &HashSet<usize>) {
    for c in chs.iter_mut() {
        c.muted = !soloed.contains(&c.track_index);
    }
}

/// Whether an `AlertCleared` for `cleared` removes the alert on screen.
fn clears_shown_alert(shown: Option<&(String, String)>, cleared: &str) -> bool {
    shown.is_some_and(|(member, _)| member.as_str() == cleared)
}

/// Create and connect a WebSocket, wiring up message handlers to signals
// Eight parameters: the Rc/Arc handles are created once by `setup_connection`
// and shared by its two call sites (initial connect and the reconnect loop);
// bundling them into a struct is a refactor of imported code outside S0.
#[allow(clippy::too_many_arguments)]
fn connect_websocket(
    member: &str,
    last_frame_at: std::rc::Rc<std::cell::Cell<f64>>,
    reconnect_attempt: std::rc::Rc<std::cell::Cell<u32>>,
    state: &MixerState,
    ws_closures: WsClosureStore,
    ws_fail_count: WsFailCounter,
    page_visible: std::rc::Rc<std::cell::Cell<bool>>,
    disposal_guard: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    // Destructure state into local signal bindings (ReadSignal/WriteSignal are Copy)
    let (ws, set_ws) = state.ws;
    let set_channels = state.channels.1;
    let set_meters = state.meters.1;
    let set_connected = state.connected.1;
    let set_loading = state.loading.1;
    let fader_touched = state.fader_touched.0;
    let set_global_level = state.global_level.1;
    let set_global_muted = state.global_muted.1;
    let global_touched = state.global_touched.0;
    let set_data_pulse = state.data_pulse.1;
    let set_pinned_channels = state.pinned_channels.1;
    let set_hidden_channels = state.hidden_channels.1;
    let set_network_mode = state.network_mode.1;
    let set_tunnel = state.tunnel.1;
    let set_output_track_idx = state.output_track_idx.1;
    let set_soloed = state.soloed.1;
    let set_pre_solo_mutes = state.pre_solo_mutes.1;
    let channels = state.channels.0;
    let soloed = state.soloed.0;
    let set_stems_level = state.stems_level.1;
    let set_stems_muted = state.stems_muted.1;
    let stems_touched = state.stems_touched.0;
    let set_stems_bus_idx = state.stems_bus_idx.1;
    let set_eq_bands = state.eq_bands.1;
    let set_eq_loading = state.eq_loading.1;
    let set_limiter_limit_db = state.limiter_limit_db.1;
    let set_limiter_limit_norm = state.limiter_limit_norm.1;
    let set_limiter_enabled = state.limiter_enabled.1;
    let set_limiter_loading = state.limiter_loading.1;
    let set_limiter_active_seconds = state.limiter_active_seconds.1;
    let set_alert_data = state.alert_data.1;
    let alert_data = state.alert_data.0;
    let set_alert_active = state.alert_active.1;
    let set_talk_state = state.talk_state.1;
    let set_engineer_talking = state.engineer_talking.1;
    // Close previous WebSocket if exists (prevents closure leak on reconnect)
    if let Some(Some(old_ws)) = ws.try_get_untracked() {
        old_ws.set_onmessage(None);
        old_ws.set_onclose(None);
        old_ws.set_onerror(None);
        let _ = old_ws.close();
    }

    // Initialize last_frame_at to now — the onmessage handler will refresh it
    // on every received frame, and the watchdog (in MixerPage) force-closes the
    // socket if it falls more than 30s behind. reaperiem#153
    last_frame_at.set(js_sys::Date::now());

    let location = web_sys::window().unwrap().location();
    let host = location.host().unwrap_or_default();
    let protocol = if location.protocol().unwrap_or_default() == "https:" {
        "wss"
    } else {
        "ws"
    };
    // Include JWT token in WebSocket URL for authentication
    let token = crate::auth::get_token().unwrap_or_default();
    let ws_url = format!("{}://{}/ws/{}?token={}", protocol, host, member, token);

    let ws = match web_sys::WebSocket::new(&ws_url) {
        Ok(ws) => ws,
        Err(e) => {
            web_sys::console::error_1(&format!("WS connect error: {:?}", e).into());
            return;
        }
    };

    let _ = set_ws.try_set(Some(ws.clone()));

    // Expose WS to window for E2E test meter injection
    let _ = js_sys::Reflect::set(
        &web_sys::window().unwrap(),
        &wasm_bindgen::JsValue::from_str("__iem_ws"),
        &ws,
    );

    // Meter update throttle: skip updates arriving faster than 50ms apart.
    // Server sends every 150ms but network jitter can bunch messages.
    // This caps reactive signal storms at ~20/sec instead of unbounded.
    let last_meter_time = std::cell::Cell::new(0.0_f64);

    // Clone fail counter for use in closures
    let fail_count_msg = ws_fail_count.clone();
    let fail_count_close = ws_fail_count;

    let last_frame_at_msg = last_frame_at.clone();
    let reconnect_attempt_msg = reconnect_attempt.clone();

    let disposal_guard_msg = disposal_guard.clone();

    // Handle incoming messages
    let onmessage = Closure::wrap(Box::new(move |e: web_sys::MessageEvent| {
        // Disposal guard: if the ConnectionManager has been dropped, no-op.
        if disposal_guard_msg.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }

        // Any received frame counts as "alive" — refresh watchdog and reset
        // the reconnect backoff counter. This runs on every frame (not just
        // the first); the Cell write is a no-op after the first reset, so
        // the extra cost is negligible and the code stays branch-free.
        last_frame_at_msg.set(js_sys::Date::now());
        reconnect_attempt_msg.set(0);
        if let Some(text) = e.data().as_string()
            && let Ok(msg) = serde_json::from_str::<iem_core::ServerMsg>(&text)
        {
            // Scope may already be disposed (e.g. frame delivered after
            // navigate-back tore the mixer down). If so, drop the whole
            // message — all downstream signal accesses would panic.
            let Some(touched) = fader_touched.try_get_untracked() else {
                return;
            };
            match msg {
                iem_core::ServerMsg::State {
                    channels: new_chs,
                    connected: conn,
                    global_level_db,
                    global_muted,
                    output_track_index,
                    stems_level_db,
                    stems_muted,
                    stems_bus_index,
                } => {
                    // Successfully received data — reset failure counter
                    fail_count_msg.set(0);
                    let _ = set_channels.try_update(|chs| {
                        let touched_snapshot: std::collections::HashMap<usize, bool> =
                            touched.iter().map(|(k, v)| (*k, *v)).collect();
                        iem_core::merge_or_replace_channels(chs, new_chs, &touched_snapshot);
                    });
                    // Update global volume from initial state
                    if let Some(lvl) = global_level_db {
                        let _ = set_global_level.try_set(lvl);
                    }
                    if let Some(muted) = global_muted {
                        let _ = set_global_muted.try_set(muted);
                    }
                    if let Some(idx) = output_track_index {
                        let _ = set_output_track_idx.try_set(Some(idx));
                    }
                    if let Some(lvl) = stems_level_db {
                        let _ = set_stems_level.try_set(lvl);
                    }
                    if let Some(muted) = stems_muted {
                        let _ = set_stems_muted.try_set(muted);
                    }
                    if let Some(idx) = stems_bus_index {
                        let _ = set_stems_bus_idx.try_set(Some(idx));
                    }
                    let _ = set_connected.try_set(conn);
                    let _ = set_loading.try_set(false);
                }
                iem_core::ServerMsg::Meters { meters: m } => {
                    if !page_visible.get() {
                        return; // Skip meter updates when backgrounded
                    }
                    // Throttle: skip if less than 50ms since last meter update
                    let now = js_sys::Date::now();
                    if meter_update_due(now, last_meter_time.get()) {
                        last_meter_time.set(now);
                        // Merge delta meters into existing map (server sends only changed values)
                        let _ = set_meters.try_update(|existing| {
                            for (k, v) in m {
                                existing.insert(k, v);
                            }
                        });
                        let _ = set_data_pulse.try_update(|v| *v = !*v);
                    }
                }
                iem_core::ServerMsg::ChannelUpdate {
                    track_index,
                    level_db,
                    muted,
                    pan,
                } => {
                    if channel_update_applies(&touched, track_index) {
                        let _ = set_channels.try_update(|chs| {
                            update_channel(chs, track_index, level_db, muted, pan);
                        });
                    }
                }
                iem_core::ServerMsg::GlobalVolumeUpdate { level_db, muted } => {
                    if !global_touched.try_get_untracked().unwrap_or(true) {
                        let _ = set_global_level.try_set(level_db);
                        let _ = set_global_muted.try_set(muted);
                    }
                }
                iem_core::ServerMsg::StemsVolumeUpdate { level_db, muted } => {
                    if !stems_touched.try_get_untracked().unwrap_or(true) {
                        let _ = set_stems_level.try_set(level_db);
                        let _ = set_stems_muted.try_set(muted);
                    }
                }
                iem_core::ServerMsg::ConnectionChanged { connected: conn } => {
                    let _ = set_connected.try_set(conn);
                }
                iem_core::ServerMsg::CustomizationUpdate { pinned, hidden } => {
                    let _ = set_pinned_channels.try_set(pinned);
                    let _ = set_hidden_channels.try_set(hidden);
                }
                iem_core::ServerMsg::NetworkMode { mode } => {
                    let _ = set_network_mode.try_set(mode);
                }
                iem_core::ServerMsg::TunnelStatus(info) => {
                    let _ = set_tunnel.try_set(Some(info));
                }
                iem_core::ServerMsg::SoloUpdate { soloed: new_solo } => {
                    let new_soloed: HashSet<usize> = new_solo.into_iter().collect();
                    let Some(current) = soloed.try_get_untracked() else {
                        return;
                    };
                    match solo_change(&current, &new_soloed) {
                        SoloChange::Unchanged => return,
                        SoloChange::Cleared => {
                            let _ = set_pre_solo_mutes.try_set(HashMap::new());
                        }
                        SoloChange::Entered => {
                            let chs = channels.try_get_untracked().unwrap_or_default();
                            let mut saved = HashMap::new();
                            for ch in &chs {
                                saved.insert(ch.track_index, ch.muted);
                            }
                            let _ = set_pre_solo_mutes.try_set(saved);
                        }
                        SoloChange::Switched => {
                            let _ = set_channels
                                .try_update(|chs| show_exclusive_solo(chs, &new_soloed));
                        }
                    }
                    let _ = set_soloed.try_set(new_soloed);
                }
                iem_core::ServerMsg::AudioStatus { .. } => {
                    // Audio status handled by ListenButton's own audio WebSocket
                }
                iem_core::ServerMsg::EngineerAlert {
                    from_member,
                    from_name,
                } => {
                    let _ = set_alert_data.try_set(Some((from_member.clone(), from_name)));
                    let _ = set_alert_active.try_set(true);
                }
                iem_core::ServerMsg::AlertCleared { member_id: cleared } => {
                    let _ = set_alert_active.try_set(false);
                    // Double-nested Option: outer is try_get_untracked
                    // disposal guard, inner is the signal's own
                    // Option<(String, String)>.
                    if clears_shown_alert(
                        alert_data.try_get_untracked().flatten().as_ref(),
                        &cleared,
                    ) {
                        let _ = set_alert_data.try_set(None);
                    }
                }
                iem_core::ServerMsg::ActiveAlerts { alerts } => {
                    if let Some(first) = alerts.first() {
                        let _ = set_alert_data
                            .try_set(Some((first.from_member.clone(), first.from_name.clone())));
                    }
                }
                iem_core::ServerMsg::TalkAcquired => {
                    let _ = set_talk_state.try_set(TalkState::Live);
                }
                iem_core::ServerMsg::TalkBusy { .. } => {
                    let _ = set_talk_state.try_set(TalkState::InUse);
                }
                iem_core::ServerMsg::TalkReleased => {
                    let _ = set_talk_state.try_set(TalkState::Idle);
                }
                iem_core::ServerMsg::EngineerTalking { active } => {
                    // Red page overlay on band member devices (no vibration)
                    if let Some(window) = web_sys::window()
                        && let Some(doc) = window.document()
                        && let Some(body) = doc.body()
                    {
                        if active {
                            let _ = body.class_list().add_1("talk-live-overlay");
                        } else {
                            let _ = body.class_list().remove_1("talk-live-overlay");
                        }
                    }
                    let _ = set_engineer_talking.try_set(active);
                }
                iem_core::ServerMsg::EqParams {
                    track_index: _,
                    track_name: _,
                    bands,
                } => {
                    let _ = set_eq_bands.try_set(
                        bands
                            .into_iter()
                            .map(|b| EqBandState {
                                band_type: b.band_type,
                                freq_hz: b.freq_hz,
                                gain_db: b.gain_db,
                                bw: b.bw,
                                freq_norm: b.freq_norm,
                                gain_norm: b.gain_norm,
                                bw_norm: b.bw_norm,
                                enabled: b.enabled,
                            })
                            .collect(),
                    );
                    let _ = set_eq_loading.try_set(false);
                }
                iem_core::ServerMsg::EqParamsMulti { .. } => {
                    // Handled by preset modal (future integration)
                }
                iem_core::ServerMsg::LimiterParams {
                    track_index: _,
                    track_name: _,
                    limit_db,
                    limit_norm,
                    enabled,
                    active_seconds,
                } => {
                    let _ = set_limiter_limit_db.try_set(limit_db);
                    let _ = set_limiter_limit_norm.try_set(limit_norm);
                    let _ = set_limiter_enabled.try_set(enabled);
                    let _ = set_limiter_active_seconds.try_set(active_seconds);
                    let _ = set_limiter_loading.try_set(false);
                }
            }
        }
    }) as Box<dyn FnMut(web_sys::MessageEvent)>);
    ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

    let reconnect_attempt_close = reconnect_attempt.clone();

    // Handle close — mark disconnected and increment failure counter
    let onclose = Closure::wrap(Box::new(move |_: web_sys::CloseEvent| {
        let _ = set_connected.try_set(false);
        fail_count_close.set(fail_count_close.get() + 1);
        reconnect_attempt_close.set(reconnect_attempt_close.get() + 1);
    }) as Box<dyn FnMut(web_sys::CloseEvent)>);
    ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));

    // Store closures so they stay alive (preventing JS callback invalidation)
    // and get dropped on next reconnect (preventing memory leak from Closure::forget)
    *ws_closures.borrow_mut() = Some((onmessage, onclose));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(track_index: usize, muted: bool) -> Channel {
        Channel {
            track_index,
            name: format!("In {track_index}"),
            level_db: 0.0,
            pan: 0.5,
            muted,
            category: String::new(),
            stereo_pair: None,
            stereo_side: None,
        }
    }

    fn set(tracks: &[usize]) -> HashSet<usize> {
        tracks.iter().copied().collect()
    }

    #[test]
    fn meter_updates_are_spaced_by_fifty_milliseconds() {
        assert!(meter_update_due(100.0, 0.0));
        assert!(!meter_update_due(149.9, 100.0));
        assert!(meter_update_due(150.0, 100.0));
    }

    #[test]
    fn a_touched_fader_ignores_server_updates() {
        let touched = HashMap::from([(3, true), (4, false)]);
        assert!(!channel_update_applies(&touched, 3));
        assert!(channel_update_applies(&touched, 4));
        assert!(channel_update_applies(&touched, 5));
    }

    #[test]
    fn a_channel_update_writes_only_its_channel() {
        let mut chs = vec![channel(1, false), channel(2, false)];
        update_channel(&mut chs, 2, -6.0, true, 0.25);
        assert_eq!(chs[0], channel(1, false));
        assert_eq!(
            (chs[1].level_db, chs[1].muted, chs[1].pan),
            (-6.0, true, 0.25)
        );
    }

    #[test]
    fn solo_changes_are_classified() {
        assert_eq!(solo_change(&set(&[2]), &set(&[2])), SoloChange::Unchanged);
        assert_eq!(solo_change(&set(&[]), &set(&[])), SoloChange::Unchanged);
        assert_eq!(solo_change(&set(&[2]), &set(&[])), SoloChange::Cleared);
        assert_eq!(solo_change(&set(&[]), &set(&[2])), SoloChange::Entered);
        assert_eq!(solo_change(&set(&[2]), &set(&[3])), SoloChange::Switched);
    }

    #[test]
    fn an_exclusive_solo_mutes_every_other_channel() {
        let mut chs = vec![channel(1, false), channel(2, true), channel(3, false)];
        show_exclusive_solo(&mut chs, &set(&[2]));
        let muted: Vec<bool> = chs.iter().map(|c| c.muted).collect();
        assert_eq!(muted, [true, false, true]);
    }

    #[test]
    fn only_the_shown_members_alert_is_cleared() {
        let shown = ("member1".to_string(), "Member1".to_string());
        assert!(clears_shown_alert(Some(&shown), "member1"));
        assert!(!clears_shown_alert(Some(&shown), "member2"));
        assert!(!clears_shown_alert(None, "member1"));
    }
}
