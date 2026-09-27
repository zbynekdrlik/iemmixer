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
    DisplayChannel, MuteClick, POST_RELEASE_GUARD_MS, THROTTLE_INTERVAL_MS, format_db, intern,
    mute_click, parse_track_name, ws_send,
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
    meters: ReadSignal<HashMap<String, [f32; 2]>>,
    /// The page's mix id (meter, EQ and limiter of IEM VOL)
    page_mix: ReadSignal<Option<String>>,
    set_eq_open: WriteSignal<Option<(String, String)>>,
    set_eq_bands: WriteSignal<Vec<EqBandState>>,
    set_eq_loading: WriteSignal<bool>,
    set_limiter_open: WriteSignal<Option<String>>,
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
        if let Some(w) = web_sys::window()
            && let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.unchecked_ref(),
                POST_RELEASE_GUARD_MS,
            )
        {
            let _ = set_guard_id.try_set(Some(id));
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
            if let Some(w) = web_sys::window()
                && let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                    cb.unchecked_ref(),
                    THROTTLE_INTERVAL_MS as i32,
                )
            {
                let _ = set_pending_timeout.try_set(Some(id));
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

    // The meter of the page's mix (post volume and mute)
    let meter_l = Signal::derive(move || {
        page_mix
            .with(|mix| {
                mix.as_ref()
                    .and_then(|id| meters.with(|m| m.get(id).map(|v| v[0])))
            })
            .unwrap_or(0.0)
    });
    let meter_r = Signal::derive(move || {
        page_mix
            .with(|mix| {
                mix.as_ref()
                    .and_then(|id| meters.with(|m| m.get(id).map(|v| v[1])))
            })
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
                        if let Some(mix) = page_mix.get() {
                            let _ = set_eq_bands.try_set(Vec::new());
                            let _ = set_eq_loading.try_set(true);
                            let _ = set_eq_open.try_set(Some((mix.clone(), "IEM VOL".to_string())));
                            ws_send(ws, &iem_core::ClientMsg::GetEqParams { target: mix });
                        }
                    }
                >
                    "EQ"
                </button>
                <button
                    class="limiter-btn-small"
                    on:click=move |_| {
                        if page_mix.get().is_some() {
                            let _ = set_limiter_loading.try_set(true);
                            let _ = set_limiter_open.try_set(Some("IEM VOL".to_string()));
                            ws_send(ws, &iem_core::ClientMsg::GetLimiterParams);
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
    meters: ReadSignal<HashMap<String, [f32; 2]>>,
    /// The stems strip's group id (its meter and EQ); no strip without one
    stems_group: ReadSignal<Option<String>>,
    set_eq_open: WriteSignal<Option<(String, String)>>,
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
        if let Some(w) = web_sys::window()
            && let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.unchecked_ref(),
                POST_RELEASE_GUARD_MS,
            )
        {
            let _ = set_guard_id.try_set(Some(id));
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
            if let Some(w) = web_sys::window()
                && let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                    cb.unchecked_ref(),
                    THROTTLE_INTERVAL_MS as i32,
                )
            {
                let _ = set_pending_timeout.try_set(Some(id));
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
        stems_group
            .with(|g| {
                g.as_ref()
                    .and_then(|id| meters.with(|m| m.get(id).map(|v| v[0])))
            })
            .unwrap_or(0.0)
    });
    let meter_r = Signal::derive(move || {
        stems_group
            .with(|g| {
                g.as_ref()
                    .and_then(|id| meters.with(|m| m.get(id).map(|v| v[1])))
            })
            .unwrap_or(0.0)
    });

    // Only render if the page's mix has the stems group
    let has_stems_bus = Signal::derive(move || stems_group.with(Option::is_some));

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
                            if let Some(group) = stems_group.get() {
                                let _ = set_eq_bands.try_set(Vec::new());
                                let _ = set_eq_loading.try_set(true);
                                let _ = set_eq_open.try_set(Some((group.clone(), "STEMS".to_string())));
                                ws_send(ws, &iem_core::ClientMsg::GetEqParams { target: group });
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

/// Which control of a strip a guard or throttle entry belongs to.
type StripKey = (&'static str, u8);
const LEVEL: u8 = 0;
const PAN: u8 = 1;
const MUTE: u8 = 2;

/// Channel list component to handle individual channel rendering
#[component]
pub(super) fn ChannelList(
    display_channels: Signal<Vec<DisplayChannel>>,
    meters: ReadSignal<HashMap<String, [f32; 2]>>,
    channels: ReadSignal<Vec<Channel>>,
    set_channels: WriteSignal<Vec<Channel>>,
    set_fader_touched: WriteSignal<HashMap<String, bool>>,
    soloed: ReadSignal<std::collections::HashSet<String>>,
    set_soloed: WriteSignal<std::collections::HashSet<String>>,
    pre_solo_mutes: ReadSignal<HashMap<String, bool>>,
    set_pre_solo_mutes: WriteSignal<HashMap<String, bool>>,
    connected: ReadSignal<bool>,
    ws: ReadSignal<Option<web_sys::WebSocket>>,
    double_tap_fader: ReadSignal<bool>,
    pinned_channels: ReadSignal<Vec<String>>,
    set_pinned_channels: WriteSignal<Vec<String>>,
    hidden_channels: ReadSignal<Vec<String>>,
    set_hidden_channels: WriteSignal<Vec<String>>,
    active_category: ReadSignal<Category>,
    set_eq_open: WriteSignal<Option<(String, String)>>,
    set_eq_bands: WriteSignal<Vec<EqBandState>>,
    set_eq_loading: WriteSignal<bool>,
) -> impl IntoView {
    // Guard timeout IDs as raw JS setTimeout handles (i32 = Copy + Send + Sync),
    // keyed by (channel id, control).
    let (_guard_ids, set_guard_ids) = signal(HashMap::<StripKey, i32>::new());

    // Throttle state signals — all Copy + Send + Sync for use in Callback::new closures.
    let (last_send_times, set_last_send_times) = signal(HashMap::<StripKey, f64>::new());
    let (pending_values, set_pending_values) = signal(HashMap::<StripKey, f32>::new());
    let (_pending_timeouts, set_pending_timeouts) = signal(HashMap::<StripKey, i32>::new());

    // Shared signal: which channel's kebab menu is open (None = all closed)
    let (open_menu, set_open_menu) = signal(Option::<&'static str>::None);

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
                key=|ch| (ch.display_name.clone(), ch.id.clone())
                children=move |ch| {
                    let id: &'static str = intern(&ch.id);
                    let name = ch.display_name.clone();
                    let eq_name = StoredValue::new(name.clone()); // For EQ button closure (Copy)
                    // EQ access (X7): the server says whether this viewer may open it
                    let show_eq = ch.eq;
                    let is_my = ch.is_my_input;
                    let ch_is_pinned = move || pinned_channels.with(|p| p.iter().any(|x| x == id));

                    // Derived signals using .with() to avoid cloning entire collections
                    let level_signal = Signal::derive(move || {
                        channels.with(|chs| {
                            chs.iter()
                                .find(|c| c.id == id)
                                .map(|c| c.level_db)
                                .unwrap_or(-60.0)
                        })
                    });

                    let muted_signal = Signal::derive(move || {
                        channels.with(|chs| {
                            chs.iter()
                                .find(|c| c.id == id)
                                .map(|c| c.muted)
                                .unwrap_or(false)
                        })
                    });

                    let pan_signal = Signal::derive(move || {
                        channels.with(|chs| {
                            chs.iter()
                                .find(|c| c.id == id)
                                .map(|c| c.pan)
                                .unwrap_or(0.5)
                        })
                    });

                    // Meters show the input's own level (post its mute) — NOT
                    // scaled by this mix's fader, pan or mute; a heard mix shows
                    // its output.
                    let meter_l = Signal::derive(move || {
                        meters.with(|m| m.get(id).map(|v| v[0]).unwrap_or(0.0))
                    });
                    let meter_r = Signal::derive(move || {
                        meters.with(|m| m.get(id).map(|v| v[1]).unwrap_or(0.0))
                    });

                    // Fader activation state for channel glow
                    let (is_fader_active, set_is_fader_active) = signal(false);

                    // Helper: cancel a guard timeout by key.
                    // All captures are Copy + Send + Sync, so this closure is too.
                    let cancel_guard = move |key: StripKey| {
                        let _ = set_guard_ids.try_update(|ids| {
                            if let Some(t) = ids.remove(&key) && let Some(w) = web_sys::window() {
                                    w.clear_timeout_with_handle(t);
                                }
                        });
                    };

                    // Helper: set a post-release guard timeout that clears
                    // fader_touched after POST_RELEASE_GUARD_MS.
                    let set_guard = move |key: StripKey| {
                        cancel_guard(key);
                        let cb = Closure::once_into_js(move || {
                            let _ = set_guard_ids.try_update(|ids| {
                                ids.remove(&key);
                            });
                            let _ = set_fader_touched.try_update(|t| {
                                t.remove(id);
                            });
                        });
                        if let Some(w) = web_sys::window() && let Ok(t) =
                                w.set_timeout_with_callback_and_timeout_and_arguments_0(
                                    cb.unchecked_ref(),
                                    POST_RELEASE_GUARD_MS,
                                ) {
                                let _ = set_guard_ids.try_update(|ids| {
                                    ids.insert(key, t);
                                });
                            }
                    };

                    // Helper: cancel a pending throttle timeout
                    let cancel_pending_timeout = move |key: StripKey| {
                        let _ = set_pending_timeouts.try_update(|m| {
                            if let Some(t) = m.remove(&key) && let Some(w) = web_sys::window() {
                                    w.clear_timeout_with_handle(t);
                                }
                        });
                    };

                    // The command a throttled value becomes.
                    let command = move |key: StripKey, value: f32| {
                        if key.1 == PAN {
                            iem_core::ClientMsg::SetPan { id: id.to_string(), pan: value }
                        } else {
                            iem_core::ClientMsg::SetLevel { id: id.to_string(), level_db: value }
                        }
                    };

                    // Throttled send: at most one command per THROTTLE_INTERVAL_MS per
                    // control; the last value of a burst is sent when the interval ends.
                    let throttled_send = move |key: StripKey, value: f32| {
                        let now = js_sys::Date::now();
                        let last_time =
                            last_send_times.with(|m| m.get(&key).copied().unwrap_or(0.0));
                        if now - last_time >= THROTTLE_INTERVAL_MS {
                            // Enough time has passed — send immediately
                            let _ = set_last_send_times.try_update(|m| {
                                m.insert(key, now);
                            });
                            let _ = set_pending_values.try_update(|m| {
                                m.remove(&key);
                            });
                            cancel_pending_timeout(key);
                            ws_send(ws, &command(key, value));
                        } else {
                            // Too soon — store as pending, schedule deferred send
                            let _ = set_pending_values.try_update(|m| {
                                m.insert(key, value);
                            });
                            cancel_pending_timeout(key);
                            let cb = Closure::once_into_js(move || {
                                let pending = pending_values
                                    .try_with(|m| m.get(&key).copied())
                                    .flatten();
                                if let Some(val) = pending {
                                    let _ = set_last_send_times.try_update(|m| {
                                        m.insert(key, js_sys::Date::now());
                                    });
                                    let _ = set_pending_values.try_update(|m| {
                                        m.remove(&key);
                                    });
                                    let _ = set_pending_timeouts.try_update(|m| {
                                        m.remove(&key);
                                    });
                                    ws_send(ws, &command(key, val));
                                }
                            });
                            if let Some(w) = web_sys::window() && let Ok(t) =
                                    w.set_timeout_with_callback_and_timeout_and_arguments_0(
                                        cb.unchecked_ref(),
                                        THROTTLE_INTERVAL_MS as i32,
                                    ) {
                                    let _ = set_pending_timeouts.try_update(|m| {
                                        m.insert(key, t);
                                    });
                                }
                        }
                    };

                    // Level change handler with throttling.
                    // Optimistic UI updates happen at full rate; WebSocket sends are
                    // throttled to max ~20/sec per channel to avoid server queue buildup.
                    let on_level_change = Callback::new(move |new_level: f32| {
                        if !connected.get() {
                            return;
                        }
                        let _ = set_channels.try_update(|chs| {
                            if let Some(ch) = chs.iter_mut().find(|c| c.id == id) {
                                ch.level_db = new_level;
                            }
                        });
                        throttled_send((id, LEVEL), new_level);
                    });

                    // Pan change handler with throttling + cancellable guard
                    let on_pan_change = Callback::new(move |new_pan: f32| {
                        if !connected.get() {
                            return;
                        }
                        let _ = set_fader_touched.try_update(|t| {
                            t.insert(id.to_string(), true);
                        });
                        let _ = set_channels.try_update(|chs| {
                            if let Some(ch) = chs.iter_mut().find(|c| c.id == id) {
                                ch.pan = new_pan;
                            }
                        });
                        throttled_send((id, PAN), new_pan);
                        // Cancellable post-release guard
                        set_guard((id, PAN));
                    });

                    // Mute toggle handler with cancellable guard
                    let on_mute_click = move |_| {
                        if !connected.get() {
                            return;
                        }
                        let shown = muted_signal.get_untracked();
                        let click = soloed.with_untracked(|s| {
                            pre_solo_mutes.with_untracked(|pre| mute_click(id, shown, s, pre))
                        });
                        let new_muted = match click {
                            MuteClick::Toggle(m) => {
                                let _ = set_fader_touched.try_update(|t| {
                                    t.insert(id.to_string(), true);
                                });
                                let _ = set_channels.try_update(|chs| {
                                    if let Some(ch) = chs.iter_mut().find(|c| c.id == id) {
                                        ch.muted = m;
                                    }
                                });
                                m
                            }
                            MuteClick::Masked(m) => {
                                // Silent until the solo ends; restored then.
                                let _ = set_pre_solo_mutes.try_update(|pre| {
                                    pre.insert(id.to_string(), m);
                                });
                                m
                            }
                        };
                        ws_send(
                            ws,
                            &iem_core::ClientMsg::SetMute {
                                id: id.to_string(),
                                muted: new_muted,
                            },
                        );
                        // Cancellable post-release guard
                        set_guard((id, MUTE));
                    };

                    // Solo toggle handler (exclusive solo, F6)
                    let on_solo_click = move |_| {
                        if !connected.get() {
                            return;
                        }

                        let current_soloed = soloed.get();
                        if current_soloed.contains(id) {
                            // UN-SOLO: the only soloed channel, so every mute
                            // returns to what it was before the solo.
                            let saved = pre_solo_mutes.get();
                            let _ = set_channels.try_update(|chs| {
                                for c in chs.iter_mut() {
                                    c.muted = saved.get(&c.id).copied().unwrap_or(false);
                                }
                            });
                            let _ = set_pre_solo_mutes.try_set(HashMap::new());
                            let _ = set_soloed.try_set(std::collections::HashSet::new());
                            ws_send(ws, &iem_core::ClientMsg::SetSolo { soloed: vec![] });
                        } else {
                            if current_soloed.is_empty() {
                                // Save pre-solo mutes for optimistic UI restore
                                let saved: HashMap<String, bool> = channels.with(|chs| {
                                    chs.iter().map(|c| (c.id.clone(), c.muted)).collect()
                                });
                                let _ = set_pre_solo_mutes.try_set(saved);
                            }
                            // Optimistic UI: mute everything except solo target
                            let _ = set_channels.try_update(|chs| {
                                for c in chs.iter_mut() {
                                    c.muted = c.id != id;
                                }
                            });
                            let _ = set_soloed.try_set(std::collections::HashSet::from([id.to_string()]));
                            ws_send(ws, &iem_core::ClientMsg::SetSolo { soloed: vec![id.to_string()] });
                        }
                    };

                    // Touch state handler: manages fader_touched guards and flushes
                    // pending throttled values on release.
                    let on_touch_state = Callback::new(move |touching: bool| {
                        if touching {
                            // Cancel any pending release guard
                            cancel_guard((id, LEVEL));
                            let _ = set_fader_touched.try_update(|t| {
                                t.insert(id.to_string(), true);
                            });
                        } else {
                            // Flush any pending throttled value immediately on release
                            let key = (id, LEVEL);
                            let pending = pending_values.with(|m| m.get(&key).copied());
                            if let Some(val) = pending {
                                let _ = set_last_send_times.try_update(|m| {
                                    m.insert(key, js_sys::Date::now());
                                });
                                let _ = set_pending_values.try_update(|m| {
                                    m.remove(&key);
                                });
                                cancel_pending_timeout(key);
                                ws_send(ws, &command(key, val));
                            }

                            // Cancellable post-release guard
                            set_guard(key);
                        }
                    });

                    let is_soloed = move || soloed.with(|s| s.contains(id));
                    let is_connected = move || connected.get();
                    let is_hidden_tab = move || active_category.get() == Category::Hidden;

                    // Pin toggle: add/remove the channel from the pinned list
                    let on_pin_click = move |_| {
                        let mut pinned = pinned_channels.get();
                        if pinned.iter().any(|x| x == id) {
                            pinned.retain(|x| x != id);
                        } else {
                            pinned.push(id.to_string());
                        }
                        let _ = set_pinned_channels.try_set(pinned.clone());
                        // Save to server via WS
                        let hidden = hidden_channels.get();
                        ws_send(ws, &iem_core::ClientMsg::UpdateCustomization {
                            pinned,
                            hidden,
                        });
                    };

                    // Hide/unhide toggle: add/remove the channel from the hidden list
                    let on_hide_click = move |_| {
                        let mut hidden = hidden_channels.get();
                        if hidden.iter().any(|x| x == id) {
                            hidden.retain(|x| x != id);
                        } else {
                            hidden.push(id.to_string());
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
                                if !is_connected() { classes.push("disconnected"); }
                                if is_fader_active.get() { classes.push("fader-active"); }
                                if open_menu.get() == Some(id) { classes.push("menu-open"); }
                                classes.join(" ")
                            }
                            data-channel=id
                            on:click=move |_| { let _ = set_open_menu.try_set(None); }
                        >
                            <div class="ch-label">
                                <div class="ch-name">{parse_track_name(&name).0}</div>
                                <div class="ch-type">
                                    {parse_track_name(&name).1}
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
                                class=move || if open_menu.get() == Some(id) { "ch-menu-btn open" } else { "ch-menu-btn" }
                                on:click=move |ev: web_sys::MouseEvent| {
                                    ev.stop_propagation();
                                    let _ = set_open_menu.try_update(|v| {
                                        *v = if *v == Some(id) { None } else { Some(id) };
                                    });
                                }
                            >
                                "\u{22EE}"
                            </button>

                            // Kebab menu popup (only when this channel's menu is open)
                            <Show when=move || open_menu.get() == Some(id) fallback=|| ()>
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
                                                let _ = set_eq_open.try_set(Some((id.to_string(), eq_name.get_value())));
                                                ws_send(ws, &iem_core::ClientMsg::GetEqParams { target: id.to_string() });
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
