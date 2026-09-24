//! Mixer page with faders, meters, categories, and presets
//!
//! Uses WebSocket for real-time bidirectional communication with REAPER.

use crate::components::alert_toast::AlertToast;
use crate::components::category_tabs::{Category, CategoryTabs};
use crate::components::eq_modal::EQModal;
use crate::components::limiter_modal::LimiterModal;
use crate::components::pin_change_modal::PinChangeModal;
use crate::components::preset_modal::PresetModal;
use crate::components::settings_modal::SettingsModal;
use crate::components::snapshot_modal::SnapshotModal;
use crate::components::toolbar::Toolbar;
use crate::components::tunnel_status::{LanHint, TunnelBanner, TunnelIndicator};
use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_params_map};

mod components;
mod connection;
mod handlers;
mod helpers;
pub mod push;
mod state;

use components::{ChannelList, GlobalVolumeFader, StemsVolumeFader};
use connection::setup_connection;
use helpers::*;
use push::subscribe_to_push;
use state::MixerState;

/// Mixer page for a specific member
#[component]
pub fn MixerPage() -> impl IntoView {
    let params = use_params_map();
    let navigate_back = use_navigate();
    let navigate_to_login = use_navigate();

    // Auth guard: check token expiry AND cross-member access
    Effect::new(move |_| {
        let member = params
            .get()
            .get("member")
            .map(|s| s.to_string())
            .unwrap_or_default();

        if crate::auth::is_token_expired() {
            // Token expired or missing - redirect to login
            if !member.is_empty() {
                let login_url = format!("/login?member={}&next=/{}", member, member);
                navigate_to_login(&login_url, Default::default());
            } else {
                navigate_to_login("/", Default::default());
            }
            return;
        }

        // Cross-member access check: only allow access to own mixer (or engineer)
        if let Some(auth) = crate::auth::get_auth() {
            if !auth.engineer && auth.member != member && !member.is_empty() {
                // Clear stale auth and redirect to login for the target member
                crate::auth::clear_auth();
                let login_url = format!("/login?member={}&next=/{}", member, member);
                navigate_to_login(&login_url, Default::default());
            }
        }
    });

    // Get member ID from route params
    let member_id = move || {
        params
            .get()
            .get("member")
            .map(|s| s.to_string())
            .unwrap_or_default()
    };

    let state = MixerState::new(&member_id());

    // Destructure signals used in the view template.
    // Read-only signals use .0; read+write use tuple destructuring.
    // Signals only used by ConnectionManager or MixerState methods are not listed.
    let (channels, set_channels) = state.channels;
    let meters = state.meters.0;
    let connected = state.connected.0;
    let active_category = state.active_category.0;
    let preset_modal_visible = state.preset_modal_visible.0;
    let pin_modal_visible = state.pin_modal_visible.0;
    let settings_modal_visible = state.settings_modal_visible.0;
    let snapshot_modal_visible = state.snapshot_modal_visible.0;
    let (has_photo, set_has_photo) = state.has_photo;
    let (double_tap_fader, set_double_tap_fader) = state.double_tap_fader;
    let set_fader_touched = state.fader_touched.1;
    let loading = state.loading.0;
    // reaperiem#186: 3s-debounced disconnect — banner only shows for sustained disconnects.
    // Underlying `connected` signal stays untouched so other instant-feedback UI
    // (status dot, channel disabled styling) keeps current behavior.
    let show_reconnecting = crate::lifecycle::debounced_disconnect(connected, 3000);
    let (soloed, set_soloed) = state.soloed;
    let (pre_solo_mutes, set_pre_solo_mutes) = state.pre_solo_mutes;
    let data_pulse = state.data_pulse.0;
    let (global_level, set_global_level) = state.global_level;
    let (global_muted, set_global_muted) = state.global_muted;
    let set_global_touched = state.global_touched.1;
    let (stems_level, set_stems_level) = state.stems_level;
    let (stems_muted, set_stems_muted) = state.stems_muted;
    let set_stems_touched = state.stems_touched.1;
    let stems_bus_idx = state.stems_bus_idx.0;
    let (eq_open, set_eq_open) = state.eq_open;
    let (eq_bands, set_eq_bands) = state.eq_bands;
    let (eq_loading, set_eq_loading) = state.eq_loading;
    let (limiter_open, set_limiter_open) = state.limiter_open;
    let limiter_limit_db = state.limiter_limit_db.0;
    let limiter_limit_norm = state.limiter_limit_norm.0;
    let limiter_enabled = state.limiter_enabled.0;
    let (limiter_loading, set_limiter_loading) = state.limiter_loading;
    let limiter_active_seconds = state.limiter_active_seconds.0;
    let (pinned_channels, set_pinned_channels) = state.pinned_channels;
    let (hidden_channels, set_hidden_channels) = state.hidden_channels;
    let network_mode = state.network_mode.0;
    let output_track_idx = state.output_track_idx.0;
    let alert_data = state.alert_data.0;
    let alert_active = state.alert_active.0;
    let (talk_state, set_talk_state) = state.talk_state;
    let engineer_talking = state.engineer_talking.0;
    let tunnel = state.tunnel.0;
    let ws = state.ws.0;

    // Check if member has photo on mount (reaperiem#16)
    // Use try_set (via state method) to guard against disposal race — if the
    // user navigates away while `get_members` is still in flight, the await
    // resumes on a disposed signal and Leptos panics. See reaperiem#153.
    {
        let mid = member_id();
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(members) = crate::api::get_members().await {
                if let Some(m) = members.iter().find(|m| m.id == mid) {
                    state.update_has_photo(m.has_photo);
                }
            }
        });
    }

    // Set up all background tasks (WS connect, reconnect, watchdog, token-expiry).
    // Registers on_cleanup internally to clear JS intervals on scope disposal.
    // Background closures check disposal_guard + try_get_untracked for safety.
    setup_connection(state, member_id.clone());

    // Signal::derive wraps the member ID so it can be passed into Memo/Callback
    // closures that require Send + Sync bounds, while staying reactive (tracks
    // route params changes — StoredValue was a snapshot that missed late params).
    let member_id_signal = Signal::derive(member_id.clone());

    // Handle back button
    let on_back = move |_| {
        navigate_back("/", Default::default());
    };

    // Process channels for display (handle stereo pairs, pin/hide)
    // Memoized to avoid recomputation on every meter update
    let display_channels = handlers::make_display_channels(
        channels,
        member_id_signal,
        active_category,
        pinned_channels,
        hidden_channels,
    );

    // Preset handlers
    let get_current_state =
        handlers::make_get_current_state(channels, stems_bus_idx, stems_level, eq_bands, eq_open);

    let on_load_preset = handlers::make_on_load_preset(connected, set_channels, ws);

    // Toolbar callbacks
    // Show engineer toolbar (Mute All + Listen) on any page when logged in as engineer
    let is_engineer = crate::auth::get_auth().map(|a| a.engineer).unwrap_or(false);
    // Mute All only on /engineer (engineer's own mixer)
    let is_engineer_own_mixer = is_engineer && member_id() == "engineer";

    // Subscribe to Web Push for SOS alerts (engineer only, one-time) (reaperiem#133)
    if is_engineer {
        subscribe_to_push();
    }

    let on_presets = Callback::new(move |_: ()| {
        state.open_preset_modal();
    });

    let on_history = Callback::new(move |_: ()| {
        state.open_snapshot_modal();
    });

    let on_mute_all = handlers::make_on_mute_all(member_id_signal);

    let on_close_modal = Callback::new(move |_: ()| {
        state.close_preset_modal();
    });

    view! {
        <div class="app mixer">
            <header class="mixer-header">
                <button class="back-btn" on:click=on_back>
                    "\u{2190}"
                </button>
                <h1>{move || {
                    let m = member_id();
                    let mut chars = m.chars();
                    match chars.next() {
                        None => String::new(),
                        Some(c) => c.to_uppercase().chain(chars).collect(),
                    }
                }}</h1>
                <Show
                    when=move || !soloed.get().is_empty()
                    fallback=|| view! {
                        <div class="header-version">
                            <span class="header-version-number">{iem_core::version_label()}</span>
                            <span class="header-version-date">{iem_core::build_datetime()}</span>
                        </div>
                    }
                >
                    <button
                        class="header-solo-btn"
                        aria-label="Clear solo"
                        on:click=move |_| {
                            if !connected.get() {
                                return;
                            }
                            // Optimistic UI: restore pre-solo mutes locally
                            state.clear_solo();
                            // Send empty SetSolo — server restores REAPER mutes and broadcasts
                            ws_send(ws, &iem_core::ClientMsg::SetSolo { soloed: vec![] });
                        }
                    >
                        "SOLO"
                        <span class="solo-close">"\u{2715}"</span>
                    </button>
                </Show>
                <button class="settings-btn" on:click=move |_| { state.open_settings_modal(); }>
                    "\u{2699}"
                </button>
                <Show
                    when=move || !network_mode.get().is_empty()
                    fallback=|| ()
                >
                    <div class=move || {
                        if network_mode.get() == "local" {
                            "network-indicator local"
                        } else {
                            "network-indicator remote"
                        }
                    }>
                        {move || if network_mode.get() == "local" { "LAN" } else { "WAN" }}
                    </div>
                </Show>
                <div class=move || {
                    let base = if connected.get() { "status-dot connected" } else { "status-dot disconnected" };
                    if connected.get() && data_pulse.get() {
                        format!("{} pulse-a", base)
                    } else if connected.get() {
                        format!("{} pulse-b", base)
                    } else {
                        base.to_string()
                    }
                }/>
            </header>

            // Internet access (Cloudflare tunnel) status (reaperiem#202): indicator for
            // the engineer, "use the LAN address" banner for band members.
            {is_engineer.then(|| view! { <TunnelIndicator status=tunnel /> })}
            {(!is_engineer).then(|| view! { <TunnelBanner status=tunnel /> })}

            <CategoryTabs
                active=active_category.into()
                on_select=move |cat| { state.select_category(cat); }
                show_hidden=Signal::derive(move || !hidden_channels.get().is_empty())
                show_mixes=Signal::derive(move || channels.get().iter().any(|ch| ch.category == "mixes"))
            />

            <Show
                when=move || show_reconnecting.get() && !loading.get()
                fallback=|| ()
            >
                <div class="disconnected-banner">
                    "Reconnecting to REAPER..."
                    <LanHint />
                </div>
            </Show>

            <Show
                when=move || !loading.get()
                fallback=|| view! {
                    <div class="loading">
                        <div class="spinner"></div>
                    </div>
                }
            >
                <div class="channels-scroll">
                    <div class="channels-grid">
                        <Show
                            when=move || active_category.get() == Category::Main
                            fallback=|| ()
                        >
                            <GlobalVolumeFader
                                level=global_level
                                set_level=set_global_level
                                muted=global_muted
                                set_muted=set_global_muted
                                set_global_touched=set_global_touched
                                connected=connected
                                ws=ws
                                meters=meters.into()
                                output_track_idx=output_track_idx
                                set_eq_open=set_eq_open
                                set_eq_bands=set_eq_bands
                                set_eq_loading=set_eq_loading
                                set_limiter_open=set_limiter_open
                                set_limiter_loading=set_limiter_loading
                            />
                        </Show>
                        <Show
                            when=move || active_category.get() == Category::Stems
                            fallback=|| ()
                        >
                            <StemsVolumeFader
                                level=stems_level
                                set_level=set_stems_level
                                muted=stems_muted
                                set_muted=set_stems_muted
                                set_stems_touched=set_stems_touched
                                connected=connected
                                ws=ws
                                meters=meters.into()
                                stems_bus_idx=stems_bus_idx
                                set_eq_open=set_eq_open
                                set_eq_bands=set_eq_bands
                                set_eq_loading=set_eq_loading
                            />
                        </Show>
                        <ChannelList
                            display_channels=display_channels.into()
                            meters=meters.into()
                            channels=channels
                            set_channels=set_channels
                            set_fader_touched=set_fader_touched
                            soloed=soloed
                            set_soloed=set_soloed
                            pre_solo_mutes=pre_solo_mutes
                            set_pre_solo_mutes=set_pre_solo_mutes
                            connected=connected
                            ws=ws
                            double_tap_fader=double_tap_fader
                            pinned_channels=pinned_channels
                            set_pinned_channels=set_pinned_channels
                            hidden_channels=hidden_channels
                            set_hidden_channels=set_hidden_channels
                            active_category=active_category
                            set_eq_open=set_eq_open
                            set_eq_bands=set_eq_bands
                            set_eq_loading=set_eq_loading
                            member_id=member_id()
                            is_engineer=is_engineer
                        />
                        <Show
                            when=move || active_category.get() == Category::Main
                            fallback=|| ()
                        >
                            <StemsVolumeFader
                                level=stems_level
                                set_level=set_stems_level
                                muted=stems_muted
                                set_muted=set_stems_muted
                                set_stems_touched=set_stems_touched
                                connected=connected
                                ws=ws
                                meters=meters.into()
                                stems_bus_idx=stems_bus_idx
                                set_eq_open=set_eq_open
                                set_eq_bands=set_eq_bands
                                set_eq_loading=set_eq_loading
                            />
                        </Show>
                    </div>
                </div>
            </Show>

            <Toolbar
                on_presets=on_presets
                on_history=on_history
                is_engineer=is_engineer
                on_mute_all=on_mute_all
                is_engineer_own_mixer=is_engineer_own_mixer
                member_id=member_id()
                ws=ws
                alert_active=alert_active
                talk_state=talk_state
                set_talk_state=set_talk_state
            />

            {is_engineer.then(|| view! {
                <AlertToast alert=alert_data ws=ws />
            })}

            // "ENGINEER SPEAKING" banner for band members (reaperiem#123)
            <Show when=move || engineer_talking.get()>
                <div class="engineer-speaking-banner">"ENGINEER SPEAKING"</div>
            </Show>

            <PresetModal
                visible=preset_modal_visible.into()
                member_id=member_id()
                connected=connected
                on_close=on_close_modal
                on_load=on_load_preset
                get_current_state=get_current_state
            />

            <SettingsModal
                visible=settings_modal_visible.into()
                on_close=Callback::new(move |_: ()| { state.close_settings_modal(); })
                on_open_pin_change=Callback::new(move |_: ()| { state.open_pin_change_modal(); })
                double_tap_fader=double_tap_fader
                set_double_tap_fader=set_double_tap_fader
                member_id=member_id()
                is_engineer=is_engineer_own_mixer
                has_photo=has_photo.into()
                set_has_photo=set_has_photo
            />

            <PinChangeModal
                visible=pin_modal_visible.into()
                on_close=Callback::new(move |_: ()| { state.close_pin_change_modal(); })
                member_id=member_id()
            />

            <SnapshotModal
                visible=snapshot_modal_visible.into()
                member_id=member_id()
                on_close=Callback::new(move |_: ()| { state.close_snapshot_modal(); })
            />

            // EQ Modal (full-screen, shown when eq_open is Some)
            <Show when=move || eq_open.get().is_some() fallback=|| ()>
                {move || {
                    let (track_idx, track_name) = eq_open.get().unwrap();
                    let ws_for_eq = ws;
                    view! {
                        <EQModal
                            track_index=track_idx
                            track_name=track_name
                            bands=eq_bands
                            loading=eq_loading
                            on_param_change=Callback::new(move |(band, param, value): (u8, String, f32)| {
                                if let Some((ti, _)) = eq_open.get_untracked() {
                                    ws_send(ws_for_eq, &iem_core::ClientMsg::SetEqBand {
                                        track_index: ti,
                                        band,
                                        param,
                                        value,
                                    });
                                }
                            })
                            on_close=Callback::new(move |_: ()| {
                                // Defer to next macrotask via setTimeout(0) — setting eq_open=None
                                // destroys the EQ modal DOM. This must happen AFTER the current
                                // event handler stack fully unwinds (including microtasks), or
                                // Leptos reactive graph teardown hits dropped closures.
                                state.close_eq();
                            })
                        />
                    }
                }}
            </Show>

            // Limiter Modal (reaperiem#72)
            <Show when=move || limiter_open.get().is_some() fallback=|| ()>
                {move || {
                    let (_track_idx, track_name) = limiter_open.get().unwrap();
                    let ws_for_lim = ws;
                    view! {
                        <LimiterModal
                            track_name=track_name
                            limit_db=limiter_limit_db
                            limit_norm=limiter_limit_norm
                            enabled=limiter_enabled
                            loading=limiter_loading
                            active_seconds=limiter_active_seconds
                            on_reset=Callback::new(move |_: ()| {
                                if let Some((ti, _)) = limiter_open.get_untracked() {
                                    ws_send(ws_for_lim, &iem_core::ClientMsg::ResetLimiterActivity {
                                        track_index: ti,
                                    });
                                    // Optimistic local update so the user sees "never" immediately.
                                    state.reset_limiter_activity();
                                }
                            })
                            on_param_change=Callback::new(move |(param, value): (String, f32)| {
                                if let Some((ti, _)) = limiter_open.get_untracked() {
                                    // Optimistic local update (no server echo)
                                    state.set_limiter_param(value);
                                    ws_send(ws_for_lim, &iem_core::ClientMsg::SetLimiterParam {
                                        track_index: ti,
                                        param,
                                        value,
                                    });
                                }
                            })
                            on_enabled_change=Callback::new(move |en: bool| {
                                if let Some((ti, _)) = limiter_open.get_untracked() {
                                    ws_send(ws_for_lim, &iem_core::ClientMsg::SetLimiterEnabled {
                                        track_index: ti,
                                        enabled: en,
                                    });
                                    state.set_limiter_enabled_state(en);
                                }
                            })
                            on_close=Callback::new(move |_: ()| {
                                state.close_limiter();
                            })
                        />
                    }
                }}
            </Show>
        </div>
    }
}
