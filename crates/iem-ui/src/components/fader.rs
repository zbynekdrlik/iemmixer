//! Custom div-based fader component with touch-safe activation delay
//!
//! Replaces native `<input type="range">` to eliminate browser jump-to-click
//! behavior and enable fill-bar visualization. Implements a 150ms activation
//! pattern for BOTH touch AND mouse — ALL movement is relative-only.
//! No absolute positioning jumps on any platform.
//!
//! Double-tap (touch) or double-click (mouse) triggers smooth animation to 0dB.

use leptos::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use web_sys::TouchEvent;

/// Activation delay in milliseconds — reduced from 300ms for snappier stage feel
const ACTIVATION_DELAY_MS: u32 = 150;

/// Time window to ignore synthesized mouse events after touch (ms)
const TOUCH_MOUSE_GUARD_MS: f64 = 500.0;

/// Maximum time between taps for double-tap detection (ms)
const DOUBLE_TAP_MS: f64 = 500.0;

/// Maximum distance between taps for double-tap detection (px)
const DOUBLE_TAP_DISTANCE_PX: f64 = 30.0;

/// Animation tick interval in ms (20 ticks/sec, matches WS throttle)
const ANIMATION_TICK_MS: u32 = 50;

/// dB step per animation tick (1.0 dB/tick × 20 ticks/sec = 20 dB/sec)
const ANIMATION_STEP_DB: f32 = 1.0;

/// Target dB for double-tap animation
const ANIMATION_TARGET_DB: f32 = 0.0;

/// Convert a dB value to a percentage position on the fader track
fn value_to_percent(value: f32, min: f32, max: f32) -> f32 {
    ((value - min) / (max - min) * 100.0).clamp(0.0, 100.0)
}

/// Quantize to 0.2 dB steps (displays only .0/.2/.4/.6/.8 — eliminates odd tenths)
fn quantize(value: f32) -> f32 {
    (value * 5.0).round() / 5.0
}

/// Detect if raw position crossed an integer dB boundary.
/// Returns Some(integer) if crossed, None otherwise.
fn crossed_integer(prev_raw: f32, new_raw: f32) -> Option<f32> {
    if prev_raw == new_raw {
        return None;
    }
    let (lo, hi) = if new_raw > prev_raw {
        (prev_raw, new_raw)
    } else {
        (new_raw, prev_raw)
    };
    // Check if any integer lies in (lo, hi] (moving up) or [lo, hi) (moving down)
    let lo_ceil = lo.ceil();
    if lo_ceil > lo && lo_ceil <= hi {
        // There's at least one integer boundary in the range
        // Return the first one crossed in the direction of movement
        if new_raw > prev_raw {
            Some(lo_ceil) // Moving up: first integer above prev
        } else {
            Some(hi.floor()) // Moving down: first integer below prev
        }
    } else {
        None
    }
}

/// Horizontal fader component with touch-safe activation
///
/// Both touch and mouse use the same interaction model:
/// - Press and release before 150ms: NO change (prevents accidental jumps)
/// - Press and hold 150ms+: Fader activates with visual feedback, relative movement
/// - All movement is relative — fader never jumps to click/tap position
/// - Double-tap (touch) or double-click (mouse): Smooth animation to 0dB
#[component]
pub fn Fader(
    /// Current value in dB (reactive signal)
    value: Signal<f32>,
    /// Minimum value (default -60)
    #[prop(default = -60.0)]
    min: f32,
    /// Maximum value (default 12)
    #[prop(default = 12.0)]
    max: f32,
    /// Called when value changes
    on_change: Callback<f32>,
    /// Fires true on activation (300ms hold), false on release
    #[prop(optional)]
    on_activate: Option<Callback<bool>>,
    /// Fires true on touchstart/mousedown, false on touchend/mouseup
    #[prop(optional)]
    on_touch_state: Option<Callback<bool>>,
    /// Whether double-tap to 0dB is enabled
    #[prop(default = true.into())]
    double_tap_enabled: Signal<bool>,
) -> impl IntoView {
    let (local_value, set_local_value) = signal(value.get_untracked());
    let (is_activated, set_is_activated) = signal(false);
    let (is_pending, set_is_pending) = signal(false);
    let (is_touch_interaction, set_is_touch_interaction) = signal(false);
    let (is_animating, set_is_animating) = signal(false);

    // Raw (unquantized) drag accumulator — prevents quantization error compounding
    // that causes integer dB values (e.g., -4.0) to be skipped during drag
    let raw_drag_value: Rc<Cell<f32>> = Rc::new(Cell::new(0.0));

    // Store the timeout handle so we can cancel it
    let timeout_handle: Rc<RefCell<Option<gloo_timers::callback::Timeout>>> =
        Rc::new(RefCell::new(None));

    // Original touch position for vertical-scroll detection
    let touch_start_x: Rc<RefCell<Option<f64>>> = Rc::new(RefCell::new(None));
    let touch_start_y: Rc<RefCell<Option<f64>>> = Rc::new(RefCell::new(None));

    // Latest pointer position — baseline for relative movement after activation.
    // Updated continuously during pre-activation to eliminate accumulated drift.
    let move_base_x: Rc<RefCell<Option<f64>>> = Rc::new(RefCell::new(None));

    // Timestamp of last touch event — guards against synthesized mouse events
    let last_touch_time: Rc<RefCell<f64>> = Rc::new(RefCell::new(0.0));

    // Double-tap detection state (same pattern as pan.rs)
    let last_tap_time: Rc<RefCell<f64>> = Rc::new(RefCell::new(0.0));
    let last_tap_x: Rc<RefCell<f64>> = Rc::new(RefCell::new(0.0));

    // Animation state
    let animation_handle: Rc<RefCell<Option<gloo_timers::callback::Interval>>> =
        Rc::new(RefCell::new(None));

    // Guard: blocks Effect from overwriting local_value for 300ms after animation completes
    let animation_guard_time: Rc<RefCell<f64>> = Rc::new(RefCell::new(0.0));

    // Node ref for measuring track dimensions
    let track_ref = NodeRef::<leptos::html::Div>::new();

    // --- Rc clones for animation ---
    let animation_handle_dbl = animation_handle.clone();
    let animation_guard_time_effect = animation_guard_time.clone();
    let animation_guard_time_anim = animation_guard_time.clone();
    let animation_guard_time_dbl = animation_guard_time.clone();

    // Update local value when external signal changes (but only if not actively touching
    // and not animating, and not within 300ms guard after animation)
    Effect::new(move |_| {
        let guard_active = js_sys::Date::now() - *animation_guard_time_effect.borrow() < 300.0;
        if !is_activated.get()
            && !is_pending.get()
            && !is_touch_interaction.get()
            && !is_animating.get()
            && !guard_active
        {
            let _ = set_local_value.try_set(value.get());
        }
    });

    // --- Helper: start animation toward 0dB ---
    let start_animation = {
        let animation_handle_start = animation_handle.clone();
        let animation_guard_start = animation_guard_time_anim;
        move || {
            // Cancel any existing animation
            animation_handle_start.borrow_mut().take();

            let _ = set_is_animating.try_set(true);
            if let Some(cb) = on_touch_state {
                cb.run(true); // prevents server echo
            }

            let animation_handle_inner = animation_handle_start.clone();
            let animation_guard_inner = animation_guard_start.clone();

            let interval = gloo_timers::callback::Interval::new(ANIMATION_TICK_MS, move || {
                let current = local_value.get_untracked();
                let diff = ANIMATION_TARGET_DB - current;

                if diff.abs() < ANIMATION_STEP_DB {
                    // Close enough — snap to target and stop
                    let _ = set_local_value.try_set(ANIMATION_TARGET_DB);
                    on_change.run(ANIMATION_TARGET_DB);
                    // Stop animation
                    animation_handle_inner.borrow_mut().take();
                    let _ = set_is_animating.try_set(false);
                    // Set guard time to block Effect overwrite
                    *animation_guard_inner.borrow_mut() = js_sys::Date::now();
                    if let Some(cb) = on_touch_state {
                        cb.run(false);
                    }
                } else {
                    // Step toward target
                    let step = if diff > 0.0 {
                        ANIMATION_STEP_DB
                    } else {
                        -ANIMATION_STEP_DB
                    };
                    let new_val = quantize(current + step);
                    let _ = set_local_value.try_set(new_val);
                    on_change.run(new_val);
                }
            });

            *animation_handle_start.borrow_mut() = Some(interval);
        }
    };

    let start_animation_ts = start_animation.clone();
    let start_animation_dbl = start_animation;

    // --- Helper: cancel animation (called on touch/mouse interrupt) ---
    let cancel_animation = {
        let animation_handle_cancel = animation_handle.clone();
        move || {
            if is_animating.get_untracked() {
                animation_handle_cancel.borrow_mut().take();
                let _ = set_is_animating.try_set(false);
                // Note: no on_touch_state(false) here — the touchstart/mousedown
                // handler immediately calls on_touch_state(true) itself
            }
        }
    };

    let cancel_animation_ts = cancel_animation.clone();
    let cancel_animation_md = cancel_animation;

    // --- Rc clones for each closure ---

    let timeout_handle_ts = timeout_handle.clone();
    let timeout_handle_tm = timeout_handle.clone();
    let timeout_handle_te = timeout_handle.clone();
    let timeout_handle_tc = timeout_handle.clone();
    let timeout_handle_md = timeout_handle; // mousedown (cloned inside for inner closures)

    let touch_start_x_ts = touch_start_x.clone();
    let touch_start_x_tm = touch_start_x.clone();
    // touch_start_x consumed by touchend

    let touch_start_y_ts = touch_start_y.clone();
    let touch_start_y_tm = touch_start_y.clone();
    // touch_start_y consumed by touchend

    let move_base_x_ts = move_base_x.clone();
    let move_base_x_tm = move_base_x.clone();
    let move_base_x_te = move_base_x.clone();
    let move_base_x_md = move_base_x; // mousedown (cloned inside for inner closures)

    let last_touch_time_ts = last_touch_time.clone();
    let last_touch_time_te = last_touch_time.clone();
    let last_touch_time_tc = last_touch_time.clone();
    let last_touch_time_md = last_touch_time; // mousedown (last user)

    let raw_drag_value_ts = raw_drag_value.clone();
    let raw_drag_value_tm = raw_drag_value.clone();
    let raw_drag_value_md = raw_drag_value; // mousedown (cloned inside for inner closures)

    let last_tap_time_ts = last_tap_time.clone();
    let last_tap_x_ts = last_tap_x.clone();

    // --- Touch handlers ---

    let handle_touchstart = move |ev: TouchEvent| {
        let now = js_sys::Date::now();
        *last_touch_time_ts.borrow_mut() = now;

        // Cancel any running animation first
        cancel_animation_ts();

        if let Some(touch) = ev.touches().get(0) {
            let x = touch.client_x() as f64;

            // Check for double-tap: two taps within time and distance thresholds
            let prev_time = *last_tap_time_ts.borrow();
            let prev_x = *last_tap_x_ts.borrow();
            let dt = now - prev_time;
            let dx = (x - prev_x).abs();

            if dt < DOUBLE_TAP_MS
                && dx < DOUBLE_TAP_DISTANCE_PX
                && prev_time > 0.0
                && double_tap_enabled.get_untracked()
            {
                // Double-tap detected → start animation to 0dB
                ev.prevent_default();
                // Reset tap tracking
                *last_tap_time_ts.borrow_mut() = 0.0;
                let _ = set_is_touch_interaction.try_set(true);
                start_animation_ts();
                return;
            }

            // Record this tap for potential double-tap
            *last_tap_time_ts.borrow_mut() = now;
            *last_tap_x_ts.borrow_mut() = x;

            *touch_start_x_ts.borrow_mut() = Some(x);
            *touch_start_y_ts.borrow_mut() = Some(touch.client_y() as f64);
            *move_base_x_ts.borrow_mut() = Some(x);
        }

        let _ = set_is_touch_interaction.try_set(true);
        raw_drag_value_ts.set(local_value.get_untracked());
        let _ = set_is_pending.try_set(true);

        if let Some(cb) = on_touch_state {
            cb.run(true);
        }

        let timeout = gloo_timers::callback::Timeout::new(ACTIVATION_DELAY_MS, move || {
            let _ = set_is_activated.try_set(true);
            let _ = set_is_pending.try_set(false);

            if let Some(cb) = on_activate {
                cb.run(true);
            }

            // Haptic feedback
            if let Some(window) = web_sys::window() {
                let navigator = window.navigator();
                let _ = navigator.vibrate_with_duration(50);
            }
        });

        *timeout_handle_ts.borrow_mut() = Some(timeout);
    };

    let handle_touchmove = move |ev: TouchEvent| {
        if let Some(touch) = ev.touches().get(0) {
            let current_x = touch.client_x() as f64;
            let current_y = touch.client_y() as f64;

            // Check if movement is mostly vertical (scrolling intent)
            if let (Some(start_x), Some(start_y)) =
                (*touch_start_x_tm.borrow(), *touch_start_y_tm.borrow())
            {
                let dx = (current_x - start_x).abs();
                let dy = (current_y - start_y).abs();

                if dy > dx + 10.0 && !is_activated.get() {
                    *timeout_handle_tm.borrow_mut() = None;
                    let _ = set_is_pending.try_set(false);
                    return;
                }
            }

            if !is_activated.get() {
                // Pre-activation: track latest position for clean baseline
                *move_base_x_tm.borrow_mut() = Some(current_x);
                return;
            }

            // Activated — prevent scroll and use relative positioning
            ev.prevent_default();

            if let Some(el) = track_ref.get() {
                let rect = el.get_bounding_client_rect();

                // Extract value BEFORE if let to release immutable borrow
                // (avoids RefCell panic when we borrow_mut inside the block)
                let base_x_opt = *move_base_x_tm.borrow();
                if let Some(base_x) = base_x_opt {
                    let delta_x = current_x - base_x;
                    let delta_ratio = delta_x / rect.width();
                    // Use raw (unquantized) accumulator to prevent skipping integer dB values
                    let raw = raw_drag_value_tm.get();
                    let new_raw = (raw + (delta_ratio as f32) * (max - min)).clamp(min, max);
                    raw_drag_value_tm.set(new_raw);
                    // If we crossed an integer boundary, snap to it; otherwise quantize
                    let new_value = match crossed_integer(raw, new_raw) {
                        Some(integer) => integer,
                        None => quantize(new_raw),
                    };
                    let _ = set_local_value.try_set(new_value);
                    on_change.run(new_value);
                    // Update base for next incremental move
                    *move_base_x_tm.borrow_mut() = Some(current_x);
                }
            }
        }
    };

    let handle_touchend = move |_ev: TouchEvent| {
        *last_touch_time_te.borrow_mut() = js_sys::Date::now();
        *timeout_handle_te.borrow_mut() = None;
        let _ = set_is_pending.try_set(false);

        if is_activated.get_untracked() {
            if let Some(cb) = on_activate {
                cb.run(false);
            }
        }

        let _ = set_is_activated.try_set(false);
        let _ = set_is_touch_interaction.try_set(false);
        *touch_start_x.borrow_mut() = None;
        *touch_start_y.borrow_mut() = None;
        *move_base_x_te.borrow_mut() = None;

        // Don't fire on_touch_state(false) if animation is running
        // (animation manages its own touch state lifecycle)
        if !is_animating.get_untracked() {
            if let Some(cb) = on_touch_state {
                cb.run(false);
            }
        }
    };

    let handle_touchcancel = move |_ev: TouchEvent| {
        *last_touch_time_tc.borrow_mut() = js_sys::Date::now();
        *timeout_handle_tc.borrow_mut() = None;
        let _ = set_is_pending.try_set(false);

        if is_activated.get_untracked() {
            if let Some(cb) = on_activate {
                cb.run(false);
            }
        }

        let _ = set_is_activated.try_set(false);
        let _ = set_is_touch_interaction.try_set(false);

        if !is_animating.get_untracked() {
            if let Some(cb) = on_touch_state {
                cb.run(false);
            }
        }
    };

    // --- Mouse handler (300ms activation, relative movement — same as touch) ---

    // Store document-level closures to remove them on mouseup (prevents listener leak)
    let mouse_move_closure: Rc<RefCell<Option<Closure<dyn FnMut(web_sys::MouseEvent)>>>> =
        Rc::new(RefCell::new(None));
    let mouse_up_closure: Rc<RefCell<Option<Closure<dyn FnMut(web_sys::MouseEvent)>>>> =
        Rc::new(RefCell::new(None));
    let mm_closure_md = mouse_move_closure.clone();
    let mu_closure_md = mouse_up_closure.clone();

    let handle_mousedown = move |ev: web_sys::MouseEvent| {
        // Only handle left button, skip if touch interaction is active
        if ev.button() != 0 || is_touch_interaction.get_untracked() {
            return;
        }

        // Guard against synthesized mouse events from touch
        if js_sys::Date::now() - *last_touch_time_md.borrow() < TOUCH_MOUSE_GUARD_MS {
            return;
        }

        // Cancel any running animation
        cancel_animation_md();

        ev.prevent_default();

        let document = web_sys::window().unwrap().document().unwrap();
        let doc_target: web_sys::EventTarget = document.clone().into();

        // Clean up any previous listeners (safety net)
        if let Some(old_mc) = mm_closure_md.borrow_mut().take() {
            let _ = doc_target
                .remove_event_listener_with_callback("mousemove", old_mc.as_ref().unchecked_ref());
        }
        if let Some(old_uc) = mu_closure_md.borrow_mut().take() {
            let _ = doc_target
                .remove_event_listener_with_callback("mouseup", old_uc.as_ref().unchecked_ref());
        }

        // Save current value and mouse position — NO JUMP, relative only
        raw_drag_value_md.set(local_value.get_untracked());
        *move_base_x_md.borrow_mut() = Some(ev.client_x() as f64);
        let _ = set_is_pending.try_set(true);

        if let Some(cb) = on_touch_state {
            cb.run(true);
        }

        // Start 300ms activation timer (same as touch)
        let timeout = gloo_timers::callback::Timeout::new(ACTIVATION_DELAY_MS, move || {
            let _ = set_is_activated.try_set(true);
            let _ = set_is_pending.try_set(false);

            if let Some(cb) = on_activate {
                cb.run(true);
            }
        });

        *timeout_handle_md.borrow_mut() = Some(timeout);

        // Clone Rcs for inner closures
        let move_base_x_mm = move_base_x_md.clone();
        let move_base_x_mu = move_base_x_md.clone();
        let raw_drag_value_mm = raw_drag_value_md.clone();
        let timeout_handle_mu = timeout_handle_md.clone();
        let mm_cleanup = mm_closure_md.clone();
        let mu_cleanup = mu_closure_md.clone();
        let doc_cleanup = doc_target.clone();

        // mousemove: RELATIVE movement from move_base_x (no absolute positioning)
        let track_ref_move = track_ref;
        let mc = Closure::wrap(Box::new(move |ev: web_sys::MouseEvent| {
            let current_x = ev.client_x() as f64;

            if !is_activated.get() {
                // Pre-activation: track position for clean baseline
                *move_base_x_mm.borrow_mut() = Some(current_x);
                return;
            }

            // Activated: relative delta from move_base_x
            if let Some(el) = track_ref_move.get() {
                let base_x_opt = *move_base_x_mm.borrow();
                if let Some(base_x) = base_x_opt {
                    let rect = el.get_bounding_client_rect();
                    let delta_x = current_x - base_x;
                    let delta_ratio = delta_x / rect.width();
                    // Use raw (unquantized) accumulator to prevent skipping integer dB values
                    let raw = raw_drag_value_mm.get();
                    let new_raw = (raw + (delta_ratio as f32) * (max - min)).clamp(min, max);
                    raw_drag_value_mm.set(new_raw);
                    // If we crossed an integer boundary, snap to it; otherwise quantize
                    let new_value = match crossed_integer(raw, new_raw) {
                        Some(integer) => integer,
                        None => quantize(new_raw),
                    };
                    let _ = set_local_value.try_set(new_value);
                    on_change.run(new_value);
                    *move_base_x_mm.borrow_mut() = Some(current_x);
                }
            }
        }) as Box<dyn FnMut(web_sys::MouseEvent)>);

        let _ =
            doc_target.add_event_listener_with_callback("mousemove", mc.as_ref().unchecked_ref());
        *mm_closure_md.borrow_mut() = Some(mc);

        // mouseup: cleanup listeners and state
        let uc = Closure::wrap(Box::new(move |_ev: web_sys::MouseEvent| {
            *timeout_handle_mu.borrow_mut() = None;
            let _ = set_is_pending.try_set(false);

            if is_activated.try_get_untracked().unwrap_or(false) {
                if let Some(cb) = on_activate {
                    cb.run(false);
                }
            }

            let _ = set_is_activated.try_set(false);
            *move_base_x_mu.borrow_mut() = None;

            if let Some(cb) = on_touch_state {
                cb.run(false);
            }

            // Remove mousemove listener
            if let Some(mc) = mm_cleanup.borrow_mut().take() {
                let _ = doc_cleanup
                    .remove_event_listener_with_callback("mousemove", mc.as_ref().unchecked_ref());
            }
            // Mark mouseup closure for cleanup on next mousedown
            mu_cleanup.borrow_mut().take();
        }) as Box<dyn FnMut(web_sys::MouseEvent)>);

        let _ = doc_target.add_event_listener_with_callback("mouseup", uc.as_ref().unchecked_ref());
        *mu_closure_md.borrow_mut() = Some(uc);
    };

    // Desktop double-click to animate to 0dB
    let handle_dblclick = move |_ev: web_sys::MouseEvent| {
        if !double_tap_enabled.get_untracked() {
            return;
        }
        // Cancel any existing animation
        animation_handle_dbl.borrow_mut().take();
        // Guard time for Effect
        *animation_guard_time_dbl.borrow_mut() = js_sys::Date::now();
        start_animation_dbl();
    };

    // Compute percentage for rendering
    let pct = move || value_to_percent(local_value.get(), min, max);

    view! {
        <div
            node_ref=track_ref
            class=move || {
                let mut classes = vec!["fader-track"];
                if is_pending.get() { classes.push("activating"); }
                if is_activated.get() { classes.push("active"); }
                if is_animating.get() { classes.push("animating"); }
                classes.join(" ")
            }
            on:touchstart=handle_touchstart
            on:touchmove=handle_touchmove
            on:touchend=handle_touchend
            on:touchcancel=handle_touchcancel
            on:mousedown=handle_mousedown
            on:dblclick=handle_dblclick
        >
            <div class="fader-fill" style=move || format!("width:{}%", pct()) />
            <div class="fader-handle" style=move || format!("left:{}%", pct()) />
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero_db_marker_position() {
        let pct = value_to_percent(0.0, -60.0, 12.0);
        assert!(
            (pct - 83.33).abs() < 0.1,
            "0 dB should be at ~83.33%, got {pct}"
        );
    }

    #[test]
    fn test_value_to_percent_boundaries() {
        assert!((value_to_percent(-60.0, -60.0, 12.0) - 0.0).abs() < 0.01);
        assert!((value_to_percent(12.0, -60.0, 12.0) - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_quantize_02_db_steps() {
        assert_eq!(quantize(0.0), 0.0);
        assert_eq!(quantize(-3.3), -3.4);
        assert_eq!(quantize(-4.09), -4.0);
        assert_eq!(quantize(6.7), 6.8);
        assert_eq!(quantize(-12.0), -12.0);
        // Only .0/.2/.4/.6/.8 values should be produced
        assert_eq!(quantize(-1.1), -1.2);
        assert_eq!(quantize(-1.3), -1.4);
        assert_eq!(quantize(-1.5), -1.4); // midpoint rounds to even (.4 is 7*0.2, .6 is 8*0.2 — round to -1.4)
        assert_eq!(quantize(-1.7), -1.6); // -.7 is 3.5 steps below -1.0, rounds to -1.6
        assert_eq!(quantize(-1.9), -2.0);
    }

    #[test]
    fn test_crossed_integer_up() {
        assert_eq!(crossed_integer(-4.3, -3.8), Some(-4.0));
        assert_eq!(crossed_integer(-5.1, -4.9), Some(-5.0));
    }

    #[test]
    fn test_crossed_integer_down() {
        assert_eq!(crossed_integer(-3.7, -4.2), Some(-4.0));
    }

    #[test]
    fn test_crossed_integer_none() {
        assert_eq!(crossed_integer(-4.5, -4.1), None);
        assert_eq!(crossed_integer(-4.0, -4.0), None);
    }

    #[test]
    fn test_integer_db_values_reachable_with_boundary_crossing() {
        // Simulate a drag across -5.0 to -3.0 with per-pixel step of 0.257 dB
        // (typical phone: 280px wide, 72 dB range)
        // Uses the full pipeline: raw accumulator + crossed_integer + quantize
        let step = 72.0 / 280.0; // ~0.2571 dB per pixel
        let mut raw = -5.0_f32;
        let mut seen_values: Vec<f32> = vec![quantize(raw)];

        // Drag 8 pixels (covers ~2 dB)
        for _ in 0..8 {
            let prev_raw = raw;
            raw += step;
            let value = match crossed_integer(prev_raw, raw) {
                Some(integer) => integer,
                None => quantize(raw),
            };
            seen_values.push(value);
        }

        // -4.0 MUST appear in the sequence (guaranteed by crossed_integer)
        assert!(
            seen_values.iter().any(|v| (*v - -4.0).abs() < f32::EPSILON),
            "Must reach -4.0 dB, but got: {:?}",
            seen_values
        );
    }

    #[test]
    fn test_quantize_only_produces_02_values() {
        // Sweep through a range and verify all outputs are on the 0.2 grid
        let step = 0.01_f32;
        let mut val = -60.0;
        while val <= 12.0 {
            let q = quantize(val);
            let remainder = (q * 5.0).round() - q * 5.0;
            assert!(
                remainder.abs() < 1e-4,
                "quantize({val}) = {q} is not on 0.2 grid"
            );
            val += step;
        }
    }

    #[test]
    fn test_all_integer_db_values_reachable_across_range() {
        // Verify that sweeping the full range with raw accumulator + crossed_integer
        // hits every integer dB value from -59 to 12
        let step = 72.0 / 280.0;
        let mut raw = -60.0_f32;
        let mut seen_values: Vec<f32> = Vec::new();

        while raw <= 12.0 {
            let prev_raw = raw;
            raw += step;
            let value = match crossed_integer(prev_raw, raw) {
                Some(integer) => integer,
                None => quantize(raw),
            };
            seen_values.push(value);
        }

        // Check every integer from -59 to 11
        for target in -59..=11 {
            let target_f = target as f32;
            assert!(
                seen_values
                    .iter()
                    .any(|v| (*v - target_f).abs() < f32::EPSILON),
                "Must reach {target_f} dB, but it was skipped",
            );
        }
    }

    #[test]
    fn test_animation_constants() {
        // Animation should reach 0dB from -60dB in ~3 seconds (60 steps at 50ms)
        let steps = (60.0 / ANIMATION_STEP_DB) as u32;
        let duration_ms = steps * ANIMATION_TICK_MS;
        assert_eq!(duration_ms, 3000);
        // Animation from +12dB to 0dB should take ~0.6s
        let steps_down = (12.0 / ANIMATION_STEP_DB) as u32;
        let duration_down = steps_down * ANIMATION_TICK_MS;
        assert_eq!(duration_down, 600);
    }
}
