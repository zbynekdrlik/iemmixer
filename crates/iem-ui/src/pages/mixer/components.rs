//! Sub-components for the mixer page: GlobalVolumeFader, StemsVolumeFader, ChannelList.

use leptos::prelude::*;
use std::collections::HashMap;
use wasm_bindgen::prelude::*;

use crate::api::Channel;
use crate::components::category_tabs::Category;
use crate::components::eq_modal::EqBandState;
use crate::components::fader::Fader;
use crate::components::meter::Meter;
use crate::components::pan::PanKnob;

use super::helpers::{
    DisplayChannel, POST_RELEASE_GUARD_MS, THROTTLE_INTERVAL_MS, format_db, parse_track_name,
    ws_send,
};

/// Global IEM volume fader rendered on the Main tab
#[component]
pub(super) fn GlobalVolumeFader(
    level: ReadSignal<f32>,
    set_level: WriteSignal<f32>,
    muted: ReadSignal<bool>,
    set_muted: WriteSignal<bool>,
    set_global_touched: WriteSignal<bool>,
    connected: ReadSignal<bool>,
    ws: ReadSignal<Option<web_sys::WebSocket>>,
    meters: ReadSignal<HashMap<usize, [f32; 2]>>,
    output_track_idx: ReadSignal<Option<usize>>,
    set_eq_open: WriteSignal<Option<(usize, String)>>,
    set_eq_bands: WriteSignal<Vec<EqBandState>>,
    set_eq_loading: WriteSignal<bool>,
    set_limiter_open: WriteSignal<Option<(usize, String)>>,
    set_limiter_loading: WriteSignal<bool>,
) -> impl IntoView {
    let (is_fader_active, set_is_fader_active) = signal(false);

    // Guard timeout for post-release protection
    let (guard_id, set_guard_id) = signal(Option::<i32>::None);

    // Throttle state
    let (last_send_time, set_last_send_time) = signal(0.0_f64);
    let (pending_value, set_pending_value) = signal(Option::<f32>::None);
    let (pending_timeout, set_pending_timeout) = signal(Option::<i32>::None);

    let cancel_guard = move || {
        if let Some(id) = guard_id.get_untracked() {
            if let Some(w) = web_sys::window() {
                w.clear_timeout_with_handle(id);
            }
            let _ = set_guard_id.try_set(None);
        }
    };

    let set_guard = move || {
        cancel_guard();
        let cb = Closure::once_into_js(move || {
            let _ = set_guard_id.try_set(None);
            let _ = set_global_touched.try_set(false);
        });
        if let Some(w) = web_sys::window() {
            if let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.unchecked_ref(),
                POST_RELEASE_GUARD_MS,
            ) {
                let _ = set_guard_id.try_set(Some(id));
            }
        }
    };

    let cancel_pending = move || {
        if let Some(id) = pending_timeout.get_untracked() {
            if let Some(w) = web_sys::window() {
                w.clear_timeout_with_handle(id);
            }
            let _ = set_pending_timeout.try_set(None);
        }
    };

    let on_level_change = Callback::new(move |new_level: f32| {
        let _ = set_level.try_set(new_level); // Optimistic update — prevents snap-back
        if !connected.get() {
            return;
        }

        // Throttled WebSocket send
        let now = js_sys::Date::now();
        let last = last_send_time.get_untracked();

        if now - last >= THROTTLE_INTERVAL_MS {
            let _ = set_last_send_time.try_set(now);
            let _ = set_pending_value.try_set(None);
            cancel_pending();
            ws_send(
                ws,
                &iem_core::ClientMsg::SetGlobalLevel {
                    level_db: new_level,
                },
            );
        } else {
            let _ = set_pending_value.try_set(Some(new_level));
            cancel_pending();
            let cb = Closure::once_into_js(move || {
                let pending = pending_value.get_untracked();
                if let Some(val) = pending {
                    let _ = set_last_send_time.try_set(js_sys::Date::now());
                    let _ = set_pending_value.try_set(None);
                    let _ = set_pending_timeout.try_set(None);
                    ws_send(ws, &iem_core::ClientMsg::SetGlobalLevel { level_db: val });
                }
            });
            if let Some(w) = web_sys::window() {
                if let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                    cb.unchecked_ref(),
                    THROTTLE_INTERVAL_MS as i32,
                ) {
                    let _ = set_pending_timeout.try_set(Some(id));
                }
            }
        }
    });

    let on_touch_state = Callback::new(move |touching: bool| {
        if touching {
            cancel_guard();
            let _ = set_global_touched.try_set(true);
        } else {
            // Flush pending
            let pending = pending_value.get_untracked();
            if let Some(val) = pending {
                let _ = set_last_send_time.try_set(js_sys::Date::now());
                let _ = set_pending_value.try_set(None);
                cancel_pending();
                ws_send(ws, &iem_core::ClientMsg::SetGlobalLevel { level_db: val });
            }
            set_guard();
        }
    });

    let on_mute_click = move |_| {
        if !connected.get() {
            return;
        }
        let new_muted = !muted.get();
        let _ = set_muted.try_set(new_muted); // Optimistic update — immediate UI feedback
        let _ = set_global_touched.try_set(true);
        ws_send(ws, &iem_core::ClientMsg::SetGlobalMute { muted: new_muted });
        // Post-release guard for mute
        let cb = Closure::once_into_js(move || {
            let _ = set_global_touched.try_set(false);
        });
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.unchecked_ref(),
                POST_RELEASE_GUARD_MS,
            );
        }
    };

    let level_signal = Signal::derive(move || level.get());

    // Derive meter levels from the output track's meter data
    let meter_l = Signal::derive(move || {
        output_track_idx
            .get()
            .and_then(|idx| meters.with(|m| m.get(&idx).map(|v| v[0])))
            .unwrap_or(0.0)
    });
    let meter_r = Signal::derive(move || {
        output_track_idx
            .get()
            .and_then(|idx| meters.with(|m| m.get(&idx).map(|v| v[1])))
            .unwrap_or(0.0)
    });

    view! {
        <div
            class=move || {
                let mut classes = vec!["channel", "global-volume"];
                if muted.get() { classes.push("muted"); }
                if !connected.get() { classes.push("disconnected"); }
                if is_fader_active.get() { classes.push("fader-active"); }
                classes.join(" ")
            }
            data-testid="global-volume-fader"
        >
            <div class="ch-label">
                <div class="ch-name">"IEM VOL"</div>
                <div class="ch-type">"master"</div>
            </div>

            <div style="grid-area: menu"></div>

            <Meter level_l=meter_l level_r=meter_r />

            <div class="fader-area">
                <Fader
                    value=level_signal
                    min=-60.0
                    max=12.0
                    on_change=on_level_change
                    on_activate=Callback::new(move |active| { let _ = set_is_fader_active.try_set(active); })
                    on_touch_state=on_touch_state
                />
            </div>

            <div class="pan-container"></div>

            <div class="channel-btns global-vol-btns">
                <div class="db-display" data-value=move || level.get()>{move || format_db(level.get())}</div>
                <button
                    class="eq-btn-small"
                    on:click=move |_| {
                        if let Some(idx) = output_track_idx.get() {
                            let _ = set_eq_bands.try_set(Vec::new());
                            let _ = set_eq_loading.try_set(true);
                            let _ = set_eq_open.try_set(Some((idx, "IEM VOL".to_string())));
                            ws_send(
                                ws,
                                &iem_core::ClientMsg::GetEqParams { track_index: idx },
                            );
                        }
                    }
                >
                    "EQ"
                </button>
                <button
                    class="limiter-btn-small"
                    on:click=move |_| {
                        if let Some(idx) = output_track_idx.get() {
                            let _ = set_limiter_loading.try_set(true);
                            let _ = set_limiter_open.try_set(Some((idx, "IEM VOL".to_string())));
                            ws_send(
                                ws,
                                &iem_core::ClientMsg::GetLimiterParams { track_index: idx },
                            );
                        }
                    }
                >
                    "LIM"
                </button>
                <button
                    class=move || if muted.get() { "mute-btn on" } else { "mute-btn off" }
                    on:click=on_mute_click
                >
                    "M"
                </button>
            </div>
        </div>
    }
}

/// Stems group bus volume fader rendered on Main and Stems tabs
#[component]
pub(super) fn StemsVolumeFader(
    level: ReadSignal<f32>,
    set_level: WriteSignal<f32>,
    muted: ReadSignal<bool>,
    set_muted: WriteSignal<bool>,
    set_stems_touched: WriteSignal<bool>,
    connected: ReadSignal<bool>,
    ws: ReadSignal<Option<web_sys::WebSocket>>,
    meters: ReadSignal<HashMap<usize, [f32; 2]>>,
    stems_bus_idx: ReadSignal<Option<usize>>,
    set_eq_open: WriteSignal<Option<(usize, String)>>,
    set_eq_bands: WriteSignal<Vec<EqBandState>>,
    set_eq_loading: WriteSignal<bool>,
) -> impl IntoView {
    let (is_fader_active, set_is_fader_active) = signal(false);

    // Guard timeout for post-release protection
    let (guard_id, set_guard_id) = signal(Option::<i32>::None);

    // Throttle state
    let (last_send_time, set_last_send_time) = signal(0.0_f64);
    let (pending_value, set_pending_value) = signal(Option::<f32>::None);
    let (pending_timeout, set_pending_timeout) = signal(Option::<i32>::None);

    let cancel_guard = move || {
        if let Some(id) = guard_id.get_untracked() {
            if let Some(w) = web_sys::window() {
                w.clear_timeout_with_handle(id);
            }
            let _ = set_guard_id.try_set(None);
        }
    };

    let set_guard = move || {
        cancel_guard();
        let cb = Closure::once_into_js(move || {
            let _ = set_guard_id.try_set(None);
            let _ = set_stems_touched.try_set(false);
        });
        if let Some(w) = web_sys::window() {
            if let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.unchecked_ref(),
                POST_RELEASE_GUARD_MS,
            ) {
                let _ = set_guard_id.try_set(Some(id));
            }
        }
    };

    let cancel_pending = move || {
        if let Some(id) = pending_timeout.get_untracked() {
            if let Some(w) = web_sys::window() {
                w.clear_timeout_with_handle(id);
            }
            let _ = set_pending_timeout.try_set(None);
        }
    };

    let on_level_change = Callback::new(move |new_level: f32| {
        let _ = set_level.try_set(new_level);
        if !connected.get() {
            return;
        }

        let now = js_sys::Date::now();
        let last = last_send_time.get_untracked();

        if now - last >= THROTTLE_INTERVAL_MS {
            let _ = set_last_send_time.try_set(now);
            let _ = set_pending_value.try_set(None);
            cancel_pending();
            ws_send(
                ws,
                &iem_core::ClientMsg::SetStemsLevel {
                    level_db: new_level,
                },
            );
        } else {
            let _ = set_pending_value.try_set(Some(new_level));
            cancel_pending();
            let cb = Closure::once_into_js(move || {
                let pending = pending_value.get_untracked();
                if let Some(val) = pending {
                    let _ = set_last_send_time.try_set(js_sys::Date::now());
                    let _ = set_pending_value.try_set(None);
                    let _ = set_pending_timeout.try_set(None);
                    ws_send(ws, &iem_core::ClientMsg::SetStemsLevel { level_db: val });
                }
            });
            if let Some(w) = web_sys::window() {
                if let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                    cb.unchecked_ref(),
                    THROTTLE_INTERVAL_MS as i32,
                ) {
                    let _ = set_pending_timeout.try_set(Some(id));
                }
            }
        }
    });

    let on_touch_state = Callback::new(move |touching: bool| {
        if touching {
            cancel_guard();
            let _ = set_stems_touched.try_set(true);
        } else {
            let pending = pending_value.get_untracked();
            if let Some(val) = pending {
                let _ = set_last_send_time.try_set(js_sys::Date::now());
                let _ = set_pending_value.try_set(None);
                cancel_pending();
                ws_send(ws, &iem_core::ClientMsg::SetStemsLevel { level_db: val });
            }
            set_guard();
        }
    });

    let on_mute_click = move |_| {
        if !connected.get() {
            return;
        }
        let new_muted = !muted.get();
        let _ = set_muted.try_set(new_muted);
        let _ = set_stems_touched.try_set(true);
        ws_send(ws, &iem_core::ClientMsg::SetStemsMute { muted: new_muted });
        let cb = Closure::once_into_js(move || {
            let _ = set_stems_touched.try_set(false);
        });
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.unchecked_ref(),
                POST_RELEASE_GUARD_MS,
            );
        }
    };

    let level_signal = Signal::derive(move || level.get());

    let meter_l = Signal::derive(move || {
        stems_bus_idx
            .get()
            .and_then(|idx| meters.with(|m| m.get(&idx).map(|v| v[0])))
            .unwrap_or(0.0)
    });
    let meter_r = Signal::derive(move || {
        stems_bus_idx
            .get()
            .and_then(|idx| meters.with(|m| m.get(&idx).map(|v| v[1])))
            .unwrap_or(0.0)
    });

    // Only render if stems bus exists
    let has_stems_bus = Signal::derive(move || stems_bus_idx.get().is_some());

    view! {
        <Show when=move || has_stems_bus.get() fallback=|| ()>
            <div
                class=move || {
                    let mut classes = vec!["channel", "stems-volume"];
                    if muted.get() { classes.push("muted"); }
                    if !connected.get() { classes.push("disconnected"); }
                    if is_fader_active.get() { classes.push("fader-active"); }
                    classes.join(" ")
                }
                data-testid="stems-volume-fader"
            >
                <div class="ch-label">
                    <div class="ch-name">"STEMS"</div>
                    <div class="ch-type">"group"</div>
                </div>

                <div style="grid-area: menu"></div>

                <Meter level_l=meter_l level_r=meter_r />

                <div class="fader-area">
                    <Fader
                        value=level_signal
                        min=-60.0
                        max=12.0
                        on_change=on_level_change
                        on_activate=Callback::new(move |active| { let _ = set_is_fader_active.try_set(active); })
                        on_touch_state=on_touch_state
                    />
                </div>

                <div class="db-display" data-value=move || level.get()>{move || format_db(level.get())}</div>

                <div class="pan-container"></div>

                <div class="channel-btns">
                    <button
                        class="eq-btn-small"
                        on:click=move |_| {
                            if let Some(idx) = stems_bus_idx.get() {
                                let _ = set_eq_bands.try_set(Vec::new());
                                let _ = set_eq_loading.try_set(true);
                                let _ = set_eq_open.try_set(Some((idx, "STEMS".to_string())));
                                ws_send(
                                    ws,
                                    &iem_core::ClientMsg::GetEqParams { track_index: idx },
                                );
                            }
                        }
                    >
                        "EQ"
                    </button>
                    <button
                        class=move || if muted.get() { "mute-btn on" } else { "mute-btn off" }
                        on:click=on_mute_click
                    >
                        "M"
                    </button>
                </div>
            </div>
        </Show>
    }
}

/// Channel list component to handle individual channel rendering
#[component]
pub(super) fn ChannelList(
    display_channels: Signal<Vec<DisplayChannel>>,
    meters: ReadSignal<HashMap<usize, [f32; 2]>>,
    channels: ReadSignal<Vec<Channel>>,
    set_channels: WriteSignal<Vec<Channel>>,
    set_fader_touched: WriteSignal<HashMap<usize, bool>>,
    soloed: ReadSignal<std::collections::HashSet<usize>>,
    set_soloed: WriteSignal<std::collections::HashSet<usize>>,
    pre_solo_mutes: ReadSignal<HashMap<usize, bool>>,
    set_pre_solo_mutes: WriteSignal<HashMap<usize, bool>>,
    connected: ReadSignal<bool>,
    ws: ReadSignal<Option<web_sys::WebSocket>>,
    double_tap_fader: ReadSignal<bool>,
    pinned_channels: ReadSignal<Vec<usize>>,
    set_pinned_channels: WriteSignal<Vec<usize>>,
    hidden_channels: ReadSignal<Vec<usize>>,
    set_hidden_channels: WriteSignal<Vec<usize>>,
    active_category: ReadSignal<Category>,
    set_eq_open: WriteSignal<Option<(usize, String)>>,
    set_eq_bands: WriteSignal<Vec<EqBandState>>,
    set_eq_loading: WriteSignal<bool>,
    /// Member ID for EQ access control (e.g., "oldmember1")
    #[prop(into)]
    member_id: String,
    /// Whether the current user is an engineer (engineers can access all EQ)
    #[prop(default = false)]
    is_engineer: bool,
) -> impl IntoView {
    // Guard timeout IDs as raw JS setTimeout handles (i32 = Copy + Send + Sync).
    // Key scheme: track_idx for fader, track_idx+10000 for pan, track_idx+20000 for mute.
    let (_guard_ids, set_guard_ids) = signal(HashMap::<usize, i32>::new());

    // Throttle state signals — all Copy + Send + Sync for use in Callback::new closures.
    let (last_send_times, set_last_send_times) = signal(HashMap::<usize, f64>::new());
    let (pending_values, set_pending_values) = signal(HashMap::<usize, f32>::new());
    let (_pending_timeouts, set_pending_timeouts) = signal(HashMap::<usize, i32>::new());

    // Shared signal: which channel's kebab menu is open (None = all closed)
    let (open_menu, set_open_menu) = signal(Option::<usize>::None);

    // EQ access control: store member_id as StoredValue for use in closures
    let eq_member_id = StoredValue::new(member_id.to_uppercase());
    let eq_is_engineer = is_engineer;

    // CRITICAL: Use <For> with stable key to preserve Fader component identity
    // across re-renders. Without this, optimistic updates cause all Faders to
    // remount, losing their is_activated state (the "glow disappears" bug).
    view! {
        <Show
            when=move || !display_channels.get().is_empty()
            fallback=|| view! { <div class="no-channels">"No channels in this category"</div> }
        >
            <For
                each=move || display_channels.get()
                key=|ch| (ch.display_name.clone(), ch.track_index)
                children=move |ch| {
                    let track_idx = ch.track_index;
                    let partner_idx = ch.partner_index;
                    let name = ch.display_name.clone();
                    let eq_name = StoredValue::new(name.clone()); // For EQ button closure (Copy)
                    // EQ access: engineer can EQ any track; members only their own
                    let show_eq = eq_is_engineer || {
                        let mid = eq_member_id.get_value();
                        let upper_name = name.to_uppercase();
                        upper_name.starts_with(&mid)
                    };
                    let is_my = ch.is_my_input;
                    let is_stereo = ch.is_stereo;
                    let ch_is_pinned =
                        move || pinned_channels.get().contains(&track_idx);

                    // Derived signals using .with() to avoid cloning entire collections
                    let level_signal = Signal::derive(move || {
                        channels.with(|chs| {
                            chs.iter()
                                .find(|c| c.track_index == track_idx)
                                .map(|c| c.level_db)
                                .unwrap_or(-60.0)
                        })
                    });

                    let muted_signal = Signal::derive(move || {
                        channels.with(|chs| {
                            chs.iter()
                                .find(|c| c.track_index == track_idx)
                                .map(|c| c.muted)
                                .unwrap_or(false)
                        })
                    });

                    let pan_signal = Signal::derive(move || {
                        channels.with(|chs| {
                            chs.iter()
                                .find(|c| c.track_index == track_idx)
                                .map(|c| c.pan)
                                .unwrap_or(0.5)
                        })
                    });

                    // Meters show raw input level — NOT scaled by send fader, pan, or mute.
                    // This matches REAPER's own meter display: the meter shows what's
                    // coming IN on the track, independent of where/how it's being sent.
                    let meter_l = Signal::derive(move || {
                        meters.with(|m| m.get(&track_idx).map(|v| v[0]).unwrap_or(0.0))
                    });
                    let meter_r = Signal::derive(move || {
                        meters.with(|m| m.get(&track_idx).map(|v| v[1]).unwrap_or(0.0))
                    });

                    // Fader activation state for channel glow
                    let (is_fader_active, set_is_fader_active) = signal(false);

                    // Helper: cancel a guard timeout by key.
                    // All captures are Copy + Send + Sync, so this closure is too.
                    let cancel_guard = move |key: usize| {
                        let _ = set_guard_ids.try_update(|ids| {
                            if let Some(id) = ids.remove(&key) {
                                if let Some(w) = web_sys::window() {
                                    w.clear_timeout_with_handle(id);
                                }
                            }
                        });
                    };

                    // Helper: set a post-release guard timeout that clears
                    // fader_touched after POST_RELEASE_GUARD_MS.
                    let set_guard = move |key: usize| {
                        cancel_guard(key);
                        let cb = Closure::once_into_js(move || {
                            let _ = set_guard_ids.try_update(|ids| {
                                ids.remove(&key);
                            });
                            let _ = set_fader_touched.try_update(|t| {
                                t.remove(&track_idx);
                                if let Some(p) = partner_idx {
                                    t.remove(&p);
                                }
                            });
                        });
                        if let Some(w) = web_sys::window() {
                            if let Ok(id) =
                                w.set_timeout_with_callback_and_timeout_and_arguments_0(
                                    cb.unchecked_ref(),
                                    POST_RELEASE_GUARD_MS,
                                )
                            {
                                let _ = set_guard_ids.try_update(|ids| {
                                    ids.insert(key, id);
                                });
                            }
                        }
                    };

                    // Helper: cancel a pending throttle timeout for a track
                    let cancel_pending_timeout = move |tidx: usize| {
                        let _ = set_pending_timeouts.try_update(|m| {
                            if let Some(id) = m.remove(&tidx) {
                                if let Some(w) = web_sys::window() {
                                    w.clear_timeout_with_handle(id);
                                }
                            }
                        });
                    };

                    // Level change handler with throttling.
                    // Optimistic UI updates happen at full rate; WebSocket sends are
                    // throttled to max ~20/sec per track to avoid server queue buildup.
                    let on_level_change = Callback::new(move |new_level: f32| {
                        if !connected.get() {
                            return;
                        }

                        // Optimistic update at full rate
                        let _ = set_channels.try_update(|chs| {
                            if let Some(ch) =
                                chs.iter_mut().find(|c| c.track_index == track_idx)
                            {
                                ch.level_db = new_level;
                            }
                            if let Some(partner) = partner_idx {
                                if let Some(ch) =
                                    chs.iter_mut().find(|c| c.track_index == partner)
                                {
                                    ch.level_db = new_level;
                                }
                            }
                        });

                        // Throttled WebSocket send
                        let now = js_sys::Date::now();
                        let last_time =
                            last_send_times.with(|m| m.get(&track_idx).copied().unwrap_or(0.0));

                        if now - last_time >= THROTTLE_INTERVAL_MS {
                            // Enough time has passed — send immediately
                            let _ = set_last_send_times.try_update(|m| {
                                m.insert(track_idx, now);
                            });
                            let _ = set_pending_values.try_update(|m| {
                                m.remove(&track_idx);
                            });
                            cancel_pending_timeout(track_idx);

                            ws_send(
                                ws,
                                &iem_core::ClientMsg::SetLevel {
                                    track_index: track_idx,
                                    level_db: new_level,
                                },
                            );
                            if let Some(partner) = partner_idx {
                                ws_send(
                                    ws,
                                    &iem_core::ClientMsg::SetLevel {
                                        track_index: partner,
                                        level_db: new_level,
                                    },
                                );
                            }
                        } else {
                            // Too soon — store as pending, schedule deferred send
                            let _ = set_pending_values.try_update(|m| {
                                m.insert(track_idx, new_level);
                            });
                            cancel_pending_timeout(track_idx);

                            let cb = Closure::once_into_js(move || {
                                let pending =
                                    pending_values.with(|m| m.get(&track_idx).copied());
                                if let Some(val) = pending {
                                    let _ = set_last_send_times.try_update(|m| {
                                        m.insert(track_idx, js_sys::Date::now());
                                    });
                                    let _ = set_pending_values.try_update(|m| {
                                        m.remove(&track_idx);
                                    });
                                    let _ = set_pending_timeouts.try_update(|m| {
                                        m.remove(&track_idx);
                                    });
                                    ws_send(
                                        ws,
                                        &iem_core::ClientMsg::SetLevel {
                                            track_index: track_idx,
                                            level_db: val,
                                        },
                                    );
                                    if let Some(partner) = partner_idx {
                                        ws_send(
                                            ws,
                                            &iem_core::ClientMsg::SetLevel {
                                                track_index: partner,
                                                level_db: val,
                                            },
                                        );
                                    }
                                }
                            });
                            if let Some(w) = web_sys::window() {
                                if let Ok(id) =
                                    w.set_timeout_with_callback_and_timeout_and_arguments_0(
                                        cb.unchecked_ref(),
                                        THROTTLE_INTERVAL_MS as i32,
                                    )
                                {
                                    let _ = set_pending_timeouts.try_update(|m| {
                                        m.insert(track_idx, id);
                                    });
                                }
                            }
                        }
                    });

                    // Pan change handler with throttling + cancellable guard
                    // Uses pan_key = track_idx + 10000 to avoid collision with level keys
                    let on_pan_change = Callback::new(move |new_pan: f32| {
                        if !connected.get() {
                            return;
                        }

                        let _ = set_fader_touched.try_update(|t| {
                            t.insert(track_idx, true);
                            if let Some(partner) = partner_idx {
                                t.insert(partner, true);
                            }
                        });

                        // Optimistic UI update at full rate
                        let _ = set_channels.try_update(|chs| {
                            if let Some(ch) =
                                chs.iter_mut().find(|c| c.track_index == track_idx)
                            {
                                ch.pan = new_pan;
                            }
                            if let Some(partner) = partner_idx {
                                if let Some(ch) =
                                    chs.iter_mut().find(|c| c.track_index == partner)
                                {
                                    ch.pan = 1.0 - new_pan;
                                }
                            }
                        });

                        // Throttled WebSocket send (same pattern as level)
                        let pan_key = track_idx + 10000;
                        let now = js_sys::Date::now();
                        let last_time =
                            last_send_times.with(|m| m.get(&pan_key).copied().unwrap_or(0.0));

                        if now - last_time >= THROTTLE_INTERVAL_MS {
                            let _ = set_last_send_times.try_update(|m| {
                                m.insert(pan_key, now);
                            });
                            let _ = set_pending_values.try_update(|m| {
                                m.remove(&pan_key);
                            });
                            cancel_pending_timeout(pan_key);

                            ws_send(
                                ws,
                                &iem_core::ClientMsg::SetPan {
                                    track_index: track_idx,
                                    pan: new_pan,
                                },
                            );
                            if let Some(partner) = partner_idx {
                                ws_send(
                                    ws,
                                    &iem_core::ClientMsg::SetPan {
                                        track_index: partner,
                                        pan: 1.0 - new_pan,
                                    },
                                );
                            }
                        } else {
                            let _ = set_pending_values.try_update(|m| {
                                m.insert(pan_key, new_pan);
                            });
                            cancel_pending_timeout(pan_key);

                            let cb = Closure::once_into_js(move || {
                                let pending =
                                    pending_values.with(|m| m.get(&pan_key).copied());
                                if let Some(val) = pending {
                                    let _ = set_last_send_times.try_update(|m| {
                                        m.insert(pan_key, js_sys::Date::now());
                                    });
                                    let _ = set_pending_values.try_update(|m| {
                                        m.remove(&pan_key);
                                    });
                                    let _ = set_pending_timeouts.try_update(|m| {
                                        m.remove(&pan_key);
                                    });
                                    ws_send(
                                        ws,
                                        &iem_core::ClientMsg::SetPan {
                                            track_index: track_idx,
                                            pan: val,
                                        },
                                    );
                                    if let Some(partner) = partner_idx {
                                        ws_send(
                                            ws,
                                            &iem_core::ClientMsg::SetPan {
                                                track_index: partner,
                                                pan: 1.0 - val,
                                            },
                                        );
                                    }
                                }
                            });
                            if let Some(w) = web_sys::window() {
                                if let Ok(id) =
                                    w.set_timeout_with_callback_and_timeout_and_arguments_0(
                                        cb.unchecked_ref(),
                                        THROTTLE_INTERVAL_MS as i32,
                                    )
                                {
                                    let _ = set_pending_timeouts.try_update(|m| {
                                        m.insert(pan_key, id);
                                    });
                                }
                            }
                        }

                        // Cancellable post-release guard
                        set_guard(pan_key);
                    });

                    // Mute toggle handler with cancellable guard
                    let on_mute_click = move |_| {
                        if !connected.get() {
                            return;
                        }

                        let current_muted = channels.with(|chs| {
                            chs.iter()
                                .find(|c| c.track_index == track_idx)
                                .map(|c| c.muted)
                                .unwrap_or(false)
                        });
                        let new_muted = !current_muted;

                        let _ = set_fader_touched.try_update(|t| {
                            t.insert(track_idx, true);
                            if let Some(partner) = partner_idx {
                                t.insert(partner, true);
                            }
                        });

                        let _ = set_channels.try_update(|chs| {
                            if let Some(ch) =
                                chs.iter_mut().find(|c| c.track_index == track_idx)
                            {
                                ch.muted = new_muted;
                            }
                            if let Some(partner) = partner_idx {
                                if let Some(ch) =
                                    chs.iter_mut().find(|c| c.track_index == partner)
                                {
                                    ch.muted = new_muted;
                                }
                            }
                        });

                        ws_send(
                            ws,
                            &iem_core::ClientMsg::SetMute {
                                track_index: track_idx,
                                muted: new_muted,
                            },
                        );
                        if let Some(partner) = partner_idx {
                            ws_send(
                                ws,
                                &iem_core::ClientMsg::SetMute {
                                    track_index: partner,
                                    muted: new_muted,
                                },
                            );
                        }

                        // Cancellable post-release guard (mute key = track_idx + 20000)
                        set_guard(track_idx + 20000);
                    };

                    // Solo toggle handler
                    let on_solo_click = move |_| {
                        if !connected.get() {
                            return;
                        }

                        let all_channels = channels.get();
                        let current_soloed = soloed.get();
                        let is_currently_soloed = current_soloed.contains(&track_idx);

                        if is_currently_soloed {
                            // UN-SOLO this track
                            let mut new_soloed = current_soloed.clone();
                            new_soloed.remove(&track_idx);
                            if let Some(partner) = partner_idx {
                                new_soloed.remove(&partner);
                            }

                            if new_soloed.is_empty() {
                                // Restore pre-solo mutes (optimistic UI)
                                let saved = pre_solo_mutes.get();
                                let _ = set_channels.try_update(|chs| {
                                    for c in chs.iter_mut() {
                                        let should_be_muted = saved.get(&c.track_index).copied().unwrap_or(false);
                                        c.muted = should_be_muted;
                                    }
                                });
                                let _ = set_pre_solo_mutes.try_set(HashMap::new());
                            } else {
                                // Partial unsolo — mute the desoloed track(s)
                                let _ = set_channels.try_update(|chs| {
                                    if let Some(ch) = chs.iter_mut().find(|c| c.track_index == track_idx) {
                                        ch.muted = true;
                                    }
                                    if let Some(partner) = partner_idx {
                                        if let Some(ch) = chs.iter_mut().find(|c| c.track_index == partner) {
                                            ch.muted = true;
                                        }
                                    }
                                });
                            }

                            let soloed_vec: Vec<usize> = new_soloed.iter().copied().collect();
                            let _ = set_soloed.try_set(new_soloed);
                            ws_send(ws, &iem_core::ClientMsg::SetSolo { soloed: soloed_vec });
                        } else {
                            // SOLO this track
                            let was_empty = current_soloed.is_empty();

                            if was_empty {
                                // Save pre-solo mutes for optimistic UI restore
                                let mut saved_mutes = HashMap::new();
                                for ch in &all_channels {
                                    saved_mutes.insert(ch.track_index, ch.muted);
                                }
                                let _ = set_pre_solo_mutes.try_set(saved_mutes);
                            }

                            // Optimistic UI: mute everything except solo target
                            let _ = set_channels.try_update(|chs| {
                                for c in chs.iter_mut() {
                                    c.muted = c.track_index != track_idx
                                        && partner_idx != Some(c.track_index);
                                }
                            });

                            // Build soloed set — exclusive (only new track + partner)
                            let mut new_soloed = std::collections::HashSet::new();
                            new_soloed.insert(track_idx);
                            if let Some(partner) = partner_idx {
                                new_soloed.insert(partner);
                            }
                            let soloed_vec: Vec<usize> = new_soloed.iter().copied().collect();
                            let _ = set_soloed.try_set(new_soloed);
                            ws_send(ws, &iem_core::ClientMsg::SetSolo { soloed: soloed_vec });
                        }
                    };

                    // Touch state handler: manages fader_touched guards and flushes
                    // pending throttled values on release.
                    let on_touch_state = Callback::new(move |touching: bool| {
                        if touching {
                            // Cancel any pending release guard
                            cancel_guard(track_idx);
                            let _ = set_fader_touched.try_update(|t| {
                                t.insert(track_idx, true);
                                if let Some(partner) = partner_idx {
                                    t.insert(partner, true);
                                }
                            });
                        } else {
                            // Flush any pending throttled value immediately on release
                            let pending =
                                pending_values.with(|m| m.get(&track_idx).copied());
                            if let Some(val) = pending {
                                let _ = set_last_send_times.try_update(|m| {
                                    m.insert(track_idx, js_sys::Date::now());
                                });
                                let _ = set_pending_values.try_update(|m| {
                                    m.remove(&track_idx);
                                });
                                cancel_pending_timeout(track_idx);
                                ws_send(
                                    ws,
                                    &iem_core::ClientMsg::SetLevel {
                                        track_index: track_idx,
                                        level_db: val,
                                    },
                                );
                                if let Some(partner) = partner_idx {
                                    ws_send(
                                        ws,
                                        &iem_core::ClientMsg::SetLevel {
                                            track_index: partner,
                                            level_db: val,
                                        },
                                    );
                                }
                            }

                            // Cancellable post-release guard
                            set_guard(track_idx);
                        }
                    });

                    let is_soloed = move || soloed.get().contains(&track_idx);
                    let is_connected = move || connected.get();
                    let is_hidden_tab = move || active_category.get() == Category::Hidden;

                    // Pin toggle: add/remove track from pinned list
                    let on_pin_click = move |_| {
                        let mut pinned = pinned_channels.get();
                        if pinned.contains(&track_idx) {
                            pinned.retain(|&x| x != track_idx);
                        } else {
                            pinned.push(track_idx);
                        }
                        let _ = set_pinned_channels.try_set(pinned.clone());
                        // Save to server via WS
                        let hidden = hidden_channels.get();
                        ws_send(ws, &iem_core::ClientMsg::UpdateCustomization {
                            pinned,
                            hidden,
                        });
                    };

                    // Hide/unhide toggle: add/remove track from hidden list
                    let on_hide_click = move |_| {
                        let mut hidden = hidden_channels.get();
                        if hidden.contains(&track_idx) {
                            hidden.retain(|&x| x != track_idx);
                        } else {
                            hidden.push(track_idx);
                        }
                        let _ = set_hidden_channels.try_set(hidden.clone());
                        // Save to server via WS
                        let pinned = pinned_channels.get();
                        ws_send(ws, &iem_core::ClientMsg::UpdateCustomization {
                            pinned,
                            hidden,
                        });
                    };

                    view! {
                        <div
                            class=move || {
                                let mut classes = vec!["channel"];
                                if muted_signal.get() { classes.push("muted"); }
                                if is_my { classes.push("more-me"); }
                                if is_stereo { classes.push("stereo-pair"); }
                                if !is_connected() { classes.push("disconnected"); }
                                if is_fader_active.get() { classes.push("fader-active"); }
                                if open_menu.get() == Some(track_idx) { classes.push("menu-open"); }
                                classes.join(" ")
                            }
                            on:click=move |_| { let _ = set_open_menu.try_set(None); }
                        >
                            <div class="ch-label">
                                <div class="ch-name">{parse_track_name(&name).0}</div>
                                <div class="ch-type">
                                    {parse_track_name(&name).1}
                                    {if is_stereo { " (st)" } else { "" }}
                                </div>
                            </div>

                            <Meter level_l=meter_l level_r=meter_r />

                            <div class="fader-area">
                                <Fader
                                    value=level_signal
                                    min=-60.0
                                    max=12.0
                                    on_change=on_level_change
                                    on_activate=Callback::new(move |active| { let _ = set_is_fader_active.try_set(active); })
                                    on_touch_state=on_touch_state
                                    double_tap_enabled=double_tap_fader.into()
                                />
                            </div>

                            <div class="db-display">{move || format_db(level_signal.get())}</div>

                            <PanKnob
                                value=pan_signal
                                on_change=on_pan_change
                            />

                            <div class="channel-btns">
                                <button
                                    class=move || if is_soloed() { "solo-btn on" } else { "solo-btn off" }
                                    on:click=on_solo_click
                                >
                                    "S"
                                </button>
                                <button
                                    class=move || if muted_signal.get() { "mute-btn on" } else { "mute-btn off" }
                                    on:click=on_mute_click
                                >
                                    "M"
                                </button>
                            </div>
                            // Kebab menu button (⋮)
                            <button
                                class=move || if open_menu.get() == Some(track_idx) { "ch-menu-btn open" } else { "ch-menu-btn" }
                                on:click=move |ev: web_sys::MouseEvent| {
                                    ev.stop_propagation();
                                    let _ = set_open_menu.try_update(|v| {
                                        *v = if *v == Some(track_idx) { None } else { Some(track_idx) };
                                    });
                                }
                            >
                                "\u{22EE}"
                            </button>

                            // Kebab menu popup (only when this channel's menu is open)
                            <Show when=move || open_menu.get() == Some(track_idx) fallback=|| ()>
                                <div class="ch-menu-popup" on:click=move |ev: web_sys::MouseEvent| ev.stop_propagation()>
                                    <button
                                        class=move || if ch_is_pinned() { "ch-menu-item pinned" } else { "ch-menu-item" }
                                        on:click=move |ev: web_sys::MouseEvent| { ev.stop_propagation(); on_pin_click(ev); { let _ = set_open_menu.try_set(None); }; }
                                    >
                                        <span class="menu-icon">{move || if ch_is_pinned() { "\u{2605}" } else { "\u{2606}" }}</span>
                                        {move || if ch_is_pinned() { "Unpin" } else { "Pin to Main" }}
                                    </button>
                                    <button
                                        class="ch-menu-item"
                                        on:click=move |ev: web_sys::MouseEvent| { ev.stop_propagation(); on_hide_click(ev); { let _ = set_open_menu.try_set(None); }; }
                                    >
                                        <span class="menu-icon">{move || if is_hidden_tab() { "\u{25C9}" } else { "\u{2715}" }}</span>
                                        {move || if is_hidden_tab() { "Unhide" } else { "Hide" }}
                                    </button>
                                    {if show_eq { Some(view! {
                                        <button
                                            class="ch-menu-item"
                                            on:click=move |ev: web_sys::MouseEvent| {
                                                ev.stop_propagation();
                                                let _ = set_open_menu.try_set(None);
                                                let _ = set_eq_bands.try_set(Vec::new());
                                                let _ = set_eq_loading.try_set(true);
                                                let _ = set_eq_open.try_set(Some((track_idx, eq_name.get_value())));
                                                // Request EQ params from REAPER
                                                ws_send(ws, &iem_core::ClientMsg::GetEqParams { track_index: track_idx });
                                            }
                                        >
                                            <span class="menu-icon">"\u{2261}"</span>
                                            "EQ"
                                        </button>
                                    }) } else { None }}
                                </div>
                            </Show>
                        </div>
                    }
                }
            />
            </Show>
            // Backdrop to close kebab menu on outside tap
            <Show when=move || open_menu.get().is_some() fallback=|| ()>
                <div class="ch-menu-backdrop" on:click=move |_| { let _ = set_open_menu.try_set(None); }></div>
            </Show>
    }
}
