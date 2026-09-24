//! Limiter modal — single "Max Level" control for IEM output bus (reaperiem#72)

use leptos::prelude::*;
use wasm_bindgen::prelude::*;

/// Activation delay in ms — matches fader.rs pattern
const ACTIVATION_DELAY_MS: u32 = 150;

/// Double-tap detection window (ms)
const DOUBLE_TAP_MS: f64 = 500.0;

/// Default limit in normalized units: -6 dB → norm = 0.0
const DEFAULT_NORM: f32 = 0.0;

/// Convert norm (0-1) to dB (-6 to 0), handling negative zero
fn norm_to_db(norm: f32) -> f32 {
    let db = norm * 6.0 - 6.0;
    if db == 0.0 { 0.0 } else { db } // eliminate -0.0
}

/// Format active seconds for the limiter-activity row (reaperiem#145).
/// The result is a self-explanatory sentence — no separate "Active:" prefix
/// needed — so a user who has never seen this feature understands the number.
/// Examples:
///   0.0  -> "not limited yet"
///   0.4  -> "0.4 sec limited"
///   21.3 -> "21.3 sec limited"
///   59.9 -> "59.9 sec limited"
///   60.0 -> "1 min 0 sec limited"
///   83.5 -> "1 min 23 sec limited"
///   3661.0 -> "61 min 1 sec limited"
fn format_active(secs: f64) -> String {
    if secs <= 0.0 {
        return "not limited yet".to_string();
    }
    if secs < 60.0 {
        return format!("{:.1} sec limited", secs);
    }
    let total_secs = secs.floor() as u64;
    let m = total_secs / 60;
    let s = total_secs % 60;
    format!("{} min {} sec limited", m, s)
}

#[cfg(test)]
mod tests {
    use super::format_active;

    #[test]
    fn format_active_zero_is_not_limited_yet() {
        assert_eq!(format_active(0.0), "not limited yet");
    }

    #[test]
    fn format_active_negative_is_not_limited_yet() {
        assert_eq!(format_active(-1.0), "not limited yet");
    }

    #[test]
    fn format_active_under_one_minute_shows_decimal_seconds() {
        assert_eq!(format_active(0.4), "0.4 sec limited");
        assert_eq!(format_active(1.0), "1.0 sec limited");
        assert_eq!(format_active(21.3), "21.3 sec limited");
        assert_eq!(format_active(59.9), "59.9 sec limited");
    }

    #[test]
    fn format_active_minutes_no_decimal() {
        assert_eq!(format_active(60.0), "1 min 0 sec limited");
        assert_eq!(format_active(83.5), "1 min 23 sec limited");
        assert_eq!(format_active(125.0), "2 min 5 sec limited");
        assert_eq!(format_active(3661.0), "61 min 1 sec limited");
    }
}

/// Limiter modal for controlling max output level on a member's output bus
#[component]
pub fn LimiterModal(
    /// Track name for the header
    track_name: String,
    /// Max level in dB (-6 to 0)
    limit_db: ReadSignal<f32>,
    /// Normalized slider position (0-1)
    limit_norm: ReadSignal<f32>,
    /// Whether limiter FX is enabled (not bypassed)
    enabled: ReadSignal<bool>,
    /// Whether data is loading
    loading: ReadSignal<bool>,
    /// Cumulative seconds the limiter has actively reduced gain (reaperiem#145).
    active_seconds: ReadSignal<f64>,
    /// Callback when a parameter changes: (param_name, normalized_value)
    on_param_change: Callback<(String, f32)>,
    /// Callback when the user clicks the Reset activity button (reaperiem#145).
    on_reset: Callback<()>,
    /// Callback when enabled state changes
    on_enabled_change: Callback<bool>,
    /// Callback to close the modal
    on_close: Callback<()>,
) -> impl IntoView {
    let on_close_overlay = on_close;
    let on_close_btn = on_close;

    view! {
        <div class="limiter-overlay" on:click=move |_| on_close_overlay.run(())>
            <div class="limiter-modal" on:click=move |ev| ev.stop_propagation()>
                <div class="limiter-header">
                    <span class="limiter-title">{track_name} " — Limiter"</span>
                    <button class="limiter-close-btn" on:click=move |_| on_close_btn.run(())>
                        "\u{00d7}"
                    </button>
                </div>
                {move || {
                    if loading.get() {
                        view! { <div class="limiter-loading">"Loading..."</div> }.into_any()
                    } else {
                        view! {
                            <div class="limiter-params">
                                <LimiterSlider
                                    label="MAX LEVEL"
                                    value_db=limit_db
                                    value_norm=limit_norm
                                    on_change=on_param_change
                                />
                                <div class="limiter-toggle-row">
                                    <span>"Limiter"</span>
                                    <button
                                        class=move || {
                                            if enabled.get() {
                                                "limiter-toggle-btn on"
                                            } else {
                                                "limiter-toggle-btn off"
                                            }
                                        }
                                        on:click=move |_| {
                                            on_enabled_change.run(!enabled.get_untracked())
                                        }
                                    >
                                        {move || if enabled.get() { "ON" } else { "OFF" }}
                                    </button>
                                    {move || {
                                        if !enabled.get() {
                                            view! {
                                                <span class="limiter-warning">
                                                    "HEARING PROTECTION OFF"
                                                </span>
                                            }
                                                .into_any()
                                        } else {
                                            view! {}.into_any()
                                        }
                                    }}
                                </div>
                                <div class="limiter-activity-row">
                                    <span class="limiter-activity-label">
                                        {move || format_active(active_seconds.get())}
                                    </span>
                                    <button
                                        class="limiter-reset-btn"
                                        on:click=move |_| on_reset.run(())
                                    >
                                        "Reset"
                                    </button>
                                </div>
                            </div>
                        }
                            .into_any()
                    }
                }}
            </div>
        </div>
    }
}

/// Limiter slider with relative movement and double-tap to reset.
/// Matches the fader.rs interaction pattern:
/// - Press and hold 150ms → activates, relative drag only
/// - Double-tap → resets to -6 dB default
/// - Never jumps to click position
#[component]
fn LimiterSlider(
    label: &'static str,
    value_db: ReadSignal<f32>,
    value_norm: ReadSignal<f32>,
    on_change: Callback<(String, f32)>,
) -> impl IntoView {
    let (local_norm, set_local_norm) = signal(value_norm.get_untracked());
    let (is_active, set_is_active) = signal(false);
    let (is_activating, set_is_activating) = signal(false);
    let track_ref = NodeRef::<leptos::html::Div>::new();

    // Shared state for activation delay and relative drag
    let start_x = std::rc::Rc::new(std::cell::Cell::new(0.0_f64));
    let start_norm = std::rc::Rc::new(std::cell::Cell::new(0.0_f32));
    let activation_timer = std::rc::Rc::new(std::cell::Cell::new(None::<i32>));
    let last_tap_time = std::rc::Rc::new(std::cell::Cell::new(0.0_f64));

    // Sync from server when not dragging
    Effect::new(move |_| {
        let v = value_norm.get();
        if !is_active.try_get_untracked().unwrap_or(true) {
            let _ = set_local_norm.try_set(v);
        }
    });

    let start_x_down = start_x.clone();
    let start_norm_down = start_norm.clone();
    let activation_timer_down = activation_timer.clone();
    let last_tap_clone = last_tap_time.clone();

    let handle_pointerdown = move |ev: web_sys::PointerEvent| {
        ev.prevent_default();

        // Capture pointer
        if let Some(el) = track_ref.get() {
            let target: &web_sys::Element = &el;
            let _ = target.set_pointer_capture(ev.pointer_id());
        }

        // Store start position for relative movement
        start_x_down.set(ev.client_x() as f64);
        start_norm_down.set(local_norm.get_untracked());

        // Double-tap detection
        let now = web_sys::js_sys::Date::now();
        let prev = last_tap_clone.get();
        last_tap_clone.set(now);
        if now - prev < DOUBLE_TAP_MS {
            // Double-tap → reset to default (-6 dB = norm 0.0)
            let _ = set_local_norm.try_set(DEFAULT_NORM);
            on_change.run(("limit".to_string(), DEFAULT_NORM));
            return;
        }

        // Start activation delay (150ms hold before fader responds)
        let _ = set_is_activating.try_set(true);
        let cb = Closure::once_into_js(move || {
            let _ = set_is_activating.try_set(false);
            let _ = set_is_active.try_set(true);
        });
        if let Some(w) = web_sys::window()
            && let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                ACTIVATION_DELAY_MS as i32,
            )
        {
            activation_timer_down.set(Some(id));
        }
    };

    let start_x_move = start_x.clone();
    let start_norm_move = start_norm.clone();

    let handle_pointermove = move |ev: web_sys::PointerEvent| {
        if !is_active.get_untracked() {
            return;
        }
        ev.prevent_default();

        if let Some(el) = track_ref.get() {
            let rect = el.get_bounding_client_rect();
            let width = rect.width();
            if width <= 0.0 {
                return;
            }

            // Relative movement: delta from start position
            let delta_px = ev.client_x() as f64 - start_x_move.get();
            let delta_norm = (delta_px / width) as f32;
            let new_norm = (start_norm_move.get() + delta_norm).clamp(0.0, 1.0);

            let _ = set_local_norm.try_set(new_norm);
            on_change.run(("limit".to_string(), new_norm));
        }
    };

    let activation_timer_up = activation_timer.clone();

    let handle_pointerup = move |ev: web_sys::PointerEvent| {
        // Cancel activation timer if still pending
        if let Some(id) = activation_timer_up.get() {
            if let Some(w) = web_sys::window() {
                w.clear_timeout_with_handle(id);
            }
            activation_timer_up.set(None);
        }

        let _ = set_is_activating.try_set(false);
        let _ = set_is_active.try_set(false);

        if let Some(el) = track_ref.get() {
            let target: &web_sys::Element = &el;
            let _ = target.release_pointer_capture(ev.pointer_id());
        }
    };

    let pct = move || local_norm.get() * 100.0;

    view! {
        <div class="limiter-param-row">
            <span class="limiter-param-label">{label}</span>
            <div
                node_ref=track_ref
                class=move || {
                    if is_active.get() {
                        "limiter-slider-track active"
                    } else if is_activating.get() {
                        "limiter-slider-track activating"
                    } else {
                        "limiter-slider-track"
                    }
                }
                on:pointerdown=handle_pointerdown
                on:pointermove=handle_pointermove
                on:pointerup=handle_pointerup
            >
                <div class="limiter-slider-fill" style=move || format!("width:{}%", pct()) />
                <div class="limiter-slider-thumb" style=move || format!("left:{}%", pct()) />
            </div>
            <span class="limiter-param-value">
                {move || {
                    let db = if is_active.get() {
                        norm_to_db(local_norm.get())
                    } else {
                        let v = value_db.get();
                        if v == 0.0 { 0.0 } else { v } // eliminate -0.0 from server
                    };
                    format!("{:.1} dB", db)
                }}
            </span>
        </div>
    }
}
