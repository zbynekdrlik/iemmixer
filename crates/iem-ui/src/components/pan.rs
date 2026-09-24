//! Pan knob component with touch-safe activation delay
//!
//! Implements the same 150ms touch-and-hold activation pattern as the Fader
//! to prevent accidental pan changes when scrolling. Includes double-tap
//! to center for mobile (since dblclick doesn't fire on touch devices).
//! Uses RELATIVE positioning — pan moves proportionally to finger delta,
//! never jumps to finger position.
//!
//! Double-tap triggers smooth animation to center (matching fader animation pattern).

use leptos::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use web_sys::{HtmlInputElement, TouchEvent};

/// Activation delay in milliseconds — reduced from 300ms for snappier stage feel
const ACTIVATION_DELAY_MS: u32 = 150;

/// Maximum time between taps for double-tap detection (ms)
const DOUBLE_TAP_MS: f64 = 500.0;

/// Maximum distance between taps for double-tap detection (px)
const DOUBLE_TAP_DISTANCE_PX: f64 = 30.0;

/// Time window to ignore synthesized mouse events after touch (ms)
const TOUCH_MOUSE_GUARD_MS: f64 = 500.0;

/// Animation tick interval in ms (same as fader: 20 ticks/sec)
const ANIMATION_TICK_MS: u32 = 50;

/// Pan step per animation tick (0.02/tick — full sweep 0→1 in 2.5s, half in 1.25s)
const ANIMATION_STEP: f32 = 0.02;

/// Target pan for double-tap animation (center)
const ANIMATION_TARGET: f32 = 0.5;

/// Horizontal pan slider component with touch-safe activation
///
/// Touch behavior:
/// - Touch and release before 150ms: NO change (user was scrolling)
/// - Touch and hold 150ms+: Pan activates with haptic feedback
/// - Activated drag: RELATIVE movement (never jumps to finger position)
/// - Double-tap: Smooth animation toward center (0.5)
/// - Mouse/desktop: Immediate response (no delay needed)
/// - Desktop double-click: Smooth animation toward center (0.5)
#[component]
pub fn PanKnob(
    /// Current pan value (0.0 = left, 0.5 = center, 1.0 = right) - reactive signal
    value: Signal<f32>,
    /// Called when pan changes
    on_change: Callback<f32>,
) -> impl IntoView {
    let (local_value, set_local_value) = signal(value.get_untracked());
    let (is_activated, set_is_activated) = signal(false);
    let (is_pending, set_is_pending) = signal(false);
    let (is_touch_interaction, set_is_touch_interaction) = signal(false);
    let (saved_value, set_saved_value) = signal(0.0f32);
    let (is_animating, set_is_animating) = signal(false);

    // Store the timeout handle so we can cancel it
    let timeout_handle: Rc<RefCell<Option<gloo_timers::callback::Timeout>>> =
        Rc::new(RefCell::new(None));

    // Store initial touch position for relative movement
    let touch_start_x: Rc<RefCell<Option<f64>>> = Rc::new(RefCell::new(None));
    let touch_start_y: Rc<RefCell<Option<f64>>> = Rc::new(RefCell::new(None));

    // Latest finger position — baseline for relative movement after activation
    let move_base_x: Rc<RefCell<Option<f64>>> = Rc::new(RefCell::new(None));

    // Timestamp of last touch event — guards against synthesized mouse events
    let last_touch_time: Rc<RefCell<f64>> = Rc::new(RefCell::new(0.0));

    // Double-tap detection state
    let last_tap_time: Rc<RefCell<f64>> = Rc::new(RefCell::new(0.0));
    let last_tap_x: Rc<RefCell<f64>> = Rc::new(RefCell::new(0.0));

    // Animation state
    let animation_handle: Rc<RefCell<Option<gloo_timers::callback::Interval>>> =
        Rc::new(RefCell::new(None));

    // Guard: blocks Effect from overwriting local_value for 300ms after animation completes
    let animation_guard_time: Rc<RefCell<f64>> = Rc::new(RefCell::new(0.0));

    let animation_guard_time_effect = animation_guard_time.clone();
    let animation_guard_time_anim = animation_guard_time.clone();
    let animation_guard_time_dbl = animation_guard_time.clone();

    // Update local value when signal changes (but only if not actively touching
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

    // --- Helper: start animation toward center ---
    let start_animation = {
        let animation_handle_start = animation_handle.clone();
        let animation_guard_start = animation_guard_time_anim;
        move || {
            // Cancel any existing animation
            animation_handle_start.borrow_mut().take();

            let _ = set_is_animating.try_set(true);

            let animation_handle_inner = animation_handle_start.clone();
            let animation_guard_inner = animation_guard_start.clone();

            let interval = gloo_timers::callback::Interval::new(ANIMATION_TICK_MS, move || {
                let current = local_value.get_untracked();
                let diff = ANIMATION_TARGET - current;

                if diff.abs() < ANIMATION_STEP {
                    // Close enough — snap to target and stop
                    let _ = set_local_value.try_set(ANIMATION_TARGET);
                    on_change.run(ANIMATION_TARGET);
                    // Stop animation
                    animation_handle_inner.borrow_mut().take();
                    let _ = set_is_animating.try_set(false);
                    // Set guard time to block Effect overwrite
                    *animation_guard_inner.borrow_mut() = js_sys::Date::now();
                } else {
                    // Step toward target
                    let step = if diff > 0.0 {
                        ANIMATION_STEP
                    } else {
                        -ANIMATION_STEP
                    };
                    let new_val = current + step;
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
            }
        }
    };

    let cancel_animation_ts = cancel_animation.clone();
    let cancel_animation_dbl = cancel_animation;

    let on_change_touch = on_change;
    let on_change_input = on_change;

    // --- Rc clones for each closure ---

    let timeout_handle_ts = timeout_handle.clone();
    let timeout_handle_tm = timeout_handle.clone();
    let timeout_handle_te = timeout_handle;

    let touch_start_x_ts = touch_start_x.clone();
    let touch_start_x_tm = touch_start_x.clone();
    // touch_start_x consumed by touchend

    let touch_start_y_ts = touch_start_y.clone();
    let touch_start_y_tm = touch_start_y.clone();
    // touch_start_y consumed by touchend

    let move_base_x_ts = move_base_x.clone();
    let move_base_x_tm = move_base_x.clone();
    let move_base_x_te = move_base_x; // consumed by touchend (last user)

    let last_touch_time_ts = last_touch_time.clone();
    let last_touch_time_te = last_touch_time.clone();
    let last_touch_time_input = last_touch_time; // consumed by handle_input (last user)

    let last_tap_time_start = last_tap_time.clone();
    let last_tap_x_start = last_tap_x.clone();

    // Touch start: check for double-tap, then begin activation countdown
    let handle_touchstart = move |ev: TouchEvent| {
        let now = js_sys::Date::now();
        *last_touch_time_ts.borrow_mut() = now;

        // Cancel any running animation first
        cancel_animation_ts();

        if let Some(touch) = ev.touches().get(0) {
            let x = touch.client_x() as f64;
            let y = touch.client_y() as f64;

            // Check for double-tap: two taps within time and distance thresholds
            let prev_time = *last_tap_time_start.borrow();
            let prev_x = *last_tap_x_start.borrow();
            let dt = now - prev_time;
            let dx = (x - prev_x).abs();

            if dt < DOUBLE_TAP_MS && dx < DOUBLE_TAP_DISTANCE_PX && prev_time > 0.0 {
                // Double-tap detected → start animation to center
                ev.prevent_default();
                // Reset tap tracking
                *last_tap_time_start.borrow_mut() = 0.0;
                let _ = set_is_touch_interaction.try_set(true);
                start_animation_ts();
                return;
            }

            // Record this tap for potential double-tap
            *last_tap_time_start.borrow_mut() = now;
            *last_tap_x_start.borrow_mut() = x;

            // Store initial touch position for relative movement
            *touch_start_x_ts.borrow_mut() = Some(x);
            *touch_start_y_ts.borrow_mut() = Some(y);
            *move_base_x_ts.borrow_mut() = Some(x);
        }

        // Mark as touch interaction and save current value
        let _ = set_is_touch_interaction.try_set(true);
        let _ = set_saved_value.try_set(local_value.get_untracked());

        let _ = set_is_pending.try_set(true);

        // Start activation timeout
        let timeout = gloo_timers::callback::Timeout::new(ACTIVATION_DELAY_MS, move || {
            let _ = set_is_activated.try_set(true);
            let _ = set_is_pending.try_set(false);

            // Haptic feedback on activation
            if let Some(window) = web_sys::window() {
                let navigator = window.navigator();
                let _ = navigator.vibrate_with_duration(30);
            }
        });

        *timeout_handle_ts.borrow_mut() = Some(timeout);
    };

    // Touch move: relative positioning after activation
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

            // Pan is activated - prevent scroll and use RELATIVE positioning
            ev.prevent_default();

            if let Some(target) = ev.target() {
                if let Ok(input) = target.dyn_into::<HtmlInputElement>() {
                    let rect = input.get_bounding_client_rect();

                    if let Some(base_x) = *move_base_x_tm.borrow() {
                        let delta_x = current_x - base_x;
                        // Pan range is 0.0-1.0, slider width maps to full range
                        let delta_ratio = delta_x / rect.width();
                        let base = saved_value.get_untracked();
                        let new_value = (base + delta_ratio as f32).clamp(0.0, 1.0);
                        let _ = set_local_value.try_set(new_value);
                        input.set_value(&format!("{}", (new_value * 100.0) as i32));
                        on_change_touch.run(new_value);
                    }
                }
            }
        }
    };

    // Input handler: blocks ALL value changes during touch interaction.
    // Touch movement is handled exclusively by touchmove for relative positioning.
    let handle_input = move |ev: web_sys::Event| {
        let target = ev.target().unwrap();
        let input = target.dyn_into::<HtmlInputElement>().unwrap();

        // Guard against synthesized events from touch (mobile browsers fire mouse
        // events ~300ms after touchend, which change the native input value)
        if js_sys::Date::now() - *last_touch_time_input.borrow() < TOUCH_MOUSE_GUARD_MS {
            let restore = saved_value.get_untracked();
            input.set_value(&format!("{}", (restore * 100.0) as i32));
            return;
        }

        // During ANY touch interaction, block native input — touchmove handles positioning
        if is_touch_interaction.get_untracked() {
            let restore = saved_value.get_untracked();
            input.set_value(&format!("{}", (restore * 100.0) as i32));
            let _ = set_local_value.try_set(restore);
            return;
        }

        // Desktop/mouse: immediate response
        let new_value: f32 = input.value().parse().unwrap_or(50.0) / 100.0;
        let _ = set_local_value.try_set(new_value);
        on_change_input.run(new_value);
    };

    // Touch end: reset state
    let handle_touchend = move |_ev: TouchEvent| {
        *last_touch_time_te.borrow_mut() = js_sys::Date::now();
        *timeout_handle_te.borrow_mut() = None;
        let _ = set_is_pending.try_set(false);
        let _ = set_is_activated.try_set(false);
        let _ = set_is_touch_interaction.try_set(false);
        *touch_start_x.borrow_mut() = None;
        *touch_start_y.borrow_mut() = None;
        *move_base_x_te.borrow_mut() = None;
    };

    // Desktop double-click to animate to center
    let handle_dblclick = move |_ev: web_sys::MouseEvent| {
        // Cancel any existing animation
        cancel_animation_dbl();
        // Guard time for Effect
        *animation_guard_time_dbl.borrow_mut() = js_sys::Date::now();
        start_animation_dbl();
    };

    view! {
        <div class="pan-container">
            <input
                type="range"
                class=move || {
                    let mut classes = vec!["pan-slider"];
                    if is_pending.get() { classes.push("activating"); }
                    if is_activated.get() { classes.push("active"); }
                    if is_animating.get() { classes.push("animating"); }
                    if (local_value.get() - 0.5).abs() < 0.005 { classes.push("centered"); }
                    classes.join(" ")
                }
                min="0"
                max="100"
                prop:value=move || (local_value.get() * 100.0) as i32
                on:input=handle_input
                on:touchstart=handle_touchstart
                on:touchmove=handle_touchmove
                on:touchend=handle_touchend
                on:touchcancel=move |_| {
                    let _ = set_is_pending.try_set(false);
                    let _ = set_is_activated.try_set(false);
                    let _ = set_is_touch_interaction.try_set(false);
                }
                on:dblclick=handle_dblclick
                title="Pan (double-tap to center)"
            />
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_animation_constants() {
        // Animation from extreme (0.0) to center (0.5) should take ~1.25s
        let steps = (0.5 / ANIMATION_STEP) as u32;
        let duration_ms = steps * ANIMATION_TICK_MS;
        assert_eq!(duration_ms, 1250);
        // Full sweep (0.0 to 1.0) would take 2.5s
        let full_steps = (1.0 / ANIMATION_STEP) as u32;
        let full_duration = full_steps * ANIMATION_TICK_MS;
        assert_eq!(full_duration, 2500);
    }
}
