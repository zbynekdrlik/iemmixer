//! Stereo VU Meter component — two thin gradient bars with ballistics and peak hold
//!
//! All meter animations run in a single shared `requestAnimationFrame` loop
//! instead of per-component `setInterval` timers. This reduces ~1500 timer
//! callbacks/sec (50 meters × 30fps) down to 1 RAF callback at display refresh rate.

use leptos::prelude::*;
use std::cell::{Cell, RefCell};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

/// PPM-style decay rate: 20 dB/s (smooth analog VU feel)
const DECAY_DB_PER_SEC: f32 = 20.0;
/// Peak hold duration before decay starts (ms)
const PEAK_HOLD_MS: f64 = 1500.0;
/// Peak indicator decay rate: 20 dB/s
const PEAK_DECAY_DB_PER_SEC: f32 = 20.0;
/// Display floor — below this we show 0%
const DISPLAY_FLOOR: f32 = 0.0001;

/// Convert linear amplitude to percentage (0..100) using logarithmic scale.
/// Maps -60 dB..+6 dB → 0%..100%.
fn linear_to_pct(level: f32) -> f32 {
    if level <= DISPLAY_FLOOR {
        return 0.0;
    }
    let db = 20.0 * level.log10();
    ((db + 60.0) / 66.0 * 100.0).clamp(0.0, 100.0)
}

/// Decay factor for a given time step (in seconds).
fn decay_factor_for(dt_s: f32) -> f32 {
    10.0_f32.powf(-DECAY_DB_PER_SEC * dt_s / 20.0)
}

/// Peak decay factor for a given time step (in seconds).
fn peak_decay_factor_for(dt_s: f32) -> f32 {
    10.0_f32.powf(-PEAK_DECAY_DB_PER_SEC * dt_s / 20.0)
}

/// Apply ballistic smoothing for one tick.
/// Returns new display level given current display and target.
fn ballistic_tick(display: f32, target: f32, decay: f32) -> f32 {
    if target > display {
        // Instant attack
        target
    } else {
        // Smooth decay
        let decayed = display * decay;
        if decayed < DISPLAY_FLOOR {
            0.0
        } else {
            decayed
        }
    }
}

/// Apply peak hold logic for one tick.
/// Returns (new_peak_level, new_hold_remaining_ms).
fn peak_hold_tick(
    peak: f32,
    hold_remaining: f64,
    target: f32,
    peak_decay: f32,
    tick_ms: f64,
) -> (f32, f64) {
    if target > peak {
        // New peak — reset hold timer
        (target, PEAK_HOLD_MS)
    } else if hold_remaining > 0.0 {
        // Holding — keep peak level, count down timer
        (peak, (hold_remaining - tick_ms).max(0.0))
    } else {
        // Hold expired — decay peak
        let decayed = peak * peak_decay;
        let new_peak = if decayed < DISPLAY_FLOOR {
            0.0
        } else {
            decayed
        };
        (new_peak, 0.0)
    }
}

// ── Shared animation loop (single RAF for all meters) ───────────────────

/// State for one registered meter channel (L or R).
struct MeterChannel {
    display: f32,
    peak: f32,
    hold_remaining: f64,
}

/// One registered meter (L+R pair).
struct MeterEntry {
    /// Read input levels from these signals (get_untracked)
    level_l: Signal<f32>,
    level_r: Signal<f32>,
    /// Write smoothed display values back to these signals
    set_display_l: WriteSignal<f32>,
    set_display_r: WriteSignal<f32>,
    set_peak_l: WriteSignal<f32>,
    set_peak_r: WriteSignal<f32>,
    /// Internal ballistic state (not reactive — mutated in RAF callback)
    left: MeterChannel,
    right: MeterChannel,
    /// Unique ID for unregistration
    id: usize,
}

thread_local! {
    static REGISTRY: RefCell<Vec<MeterEntry>> = RefCell::new(Vec::new());
    static NEXT_ID: Cell<usize> = Cell::new(0);
    static RAF_HANDLE: Cell<i32> = Cell::new(0);
    static RAF_RUNNING: Cell<bool> = Cell::new(false);
    static LAST_TICK_TIME: Cell<f64> = Cell::new(0.0);
}

/// Register a meter and ensure the shared RAF loop is running.
/// Returns an ID for later unregistration.
fn register_meter(
    level_l: Signal<f32>,
    level_r: Signal<f32>,
    set_display_l: WriteSignal<f32>,
    set_display_r: WriteSignal<f32>,
    set_peak_l: WriteSignal<f32>,
    set_peak_r: WriteSignal<f32>,
) -> usize {
    let id = NEXT_ID.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    });

    REGISTRY.with(|reg| {
        reg.borrow_mut().push(MeterEntry {
            level_l,
            level_r,
            set_display_l,
            set_display_r,
            set_peak_l,
            set_peak_r,
            left: MeterChannel {
                display: 0.0,
                peak: 0.0,
                hold_remaining: 0.0,
            },
            right: MeterChannel {
                display: 0.0,
                peak: 0.0,
                hold_remaining: 0.0,
            },
            id,
        });
    });

    ensure_raf_running();
    id
}

/// Unregister a meter. Stops the RAF loop if no meters remain.
fn unregister_meter(id: usize) {
    REGISTRY.with(|reg| {
        reg.borrow_mut().retain(|e| e.id != id);
    });

    let count = REGISTRY.with(|reg| reg.borrow().len());
    if count == 0 {
        stop_raf();
    }
}

fn ensure_raf_running() {
    RAF_RUNNING.with(|running| {
        if !running.get() {
            running.set(true);
            LAST_TICK_TIME.with(|t| t.set(js_sys::Date::now()));
            schedule_raf();
        }
    });
}

fn stop_raf() {
    RAF_RUNNING.with(|running| {
        if running.get() {
            running.set(false);
            let handle = RAF_HANDLE.with(|h| h.get());
            if handle != 0 {
                if let Some(w) = web_sys::window() {
                    let _ = w.cancel_animation_frame(handle);
                }
                RAF_HANDLE.with(|h| h.set(0));
            }
        }
    });
}

fn schedule_raf() {
    let closure = Closure::once_into_js(move || {
        tick_all_meters();
    });
    if let Some(w) = web_sys::window() {
        let handle = w
            .request_animation_frame(closure.as_ref().unchecked_ref())
            .unwrap_or(0);
        RAF_HANDLE.with(|h| h.set(handle));
    }
}

/// Single RAF callback that processes ALL registered meters.
fn tick_all_meters() {
    let still_running = RAF_RUNNING.with(|r| r.get());
    if !still_running {
        return;
    }

    let now = js_sys::Date::now();
    let dt_ms = LAST_TICK_TIME.with(|t| {
        let prev = t.get();
        t.set(now);
        (now - prev).clamp(1.0, 100.0) // clamp to avoid huge jumps
    });
    let dt_s = dt_ms as f32 / 1000.0;

    let decay = decay_factor_for(dt_s);
    let p_decay = peak_decay_factor_for(dt_s);

    REGISTRY.with(|reg| {
        for entry in reg.borrow_mut().iter_mut() {
            let target_l = entry.level_l.try_get_untracked().unwrap_or(0.0);
            let target_r = entry.level_r.try_get_untracked().unwrap_or(0.0);

            // Ballistic smoothing for bar levels
            entry.left.display = ballistic_tick(entry.left.display, target_l, decay);
            entry.right.display = ballistic_tick(entry.right.display, target_r, decay);

            // Peak hold logic
            let (new_peak_l, new_hold_l) = peak_hold_tick(
                entry.left.peak,
                entry.left.hold_remaining,
                target_l,
                p_decay,
                dt_ms,
            );
            entry.left.peak = new_peak_l;
            entry.left.hold_remaining = new_hold_l;

            let (new_peak_r, new_hold_r) = peak_hold_tick(
                entry.right.peak,
                entry.right.hold_remaining,
                target_r,
                p_decay,
                dt_ms,
            );
            entry.right.peak = new_peak_r;
            entry.right.hold_remaining = new_hold_r;

            // Write display values to signals (batched — all in one frame)
            entry.set_display_l.try_set(entry.left.display);
            entry.set_display_r.try_set(entry.right.display);
            entry.set_peak_l.try_set(entry.left.peak);
            entry.set_peak_r.try_set(entry.right.peak);
        }
    });

    // Schedule next frame
    schedule_raf();
}

/// Stereo VU meter with ballistics and peak hold
#[component]
pub fn Meter(
    /// Left channel peak level (0.0 to 1.0+)
    level_l: Signal<f32>,
    /// Right channel peak level (0.0 to 1.0+)
    level_r: Signal<f32>,
) -> impl IntoView {
    // Smoothed display values for L and R bars
    let (display_l, set_display_l) = signal(0.0_f32);
    let (display_r, set_display_r) = signal(0.0_f32);
    // Peak hold values
    let (peak_l, set_peak_l) = signal(0.0_f32);
    let (peak_r, set_peak_r) = signal(0.0_f32);

    // Register with the shared animation loop
    let meter_id = register_meter(
        level_l,
        level_r,
        set_display_l,
        set_display_r,
        set_peak_l,
        set_peak_r,
    );

    on_cleanup(move || {
        unregister_meter(meter_id);
    });

    // Derived percentage signals for rendering
    let bar_l_pct = move || linear_to_pct(display_l.get());
    let bar_r_pct = move || linear_to_pct(display_r.get());
    let peak_l_pct = move || linear_to_pct(peak_l.get());
    let peak_r_pct = move || linear_to_pct(peak_r.get());

    view! {
        <div class="meter-stereo">
            <div class="meter-bar">
                <div
                    class="meter-fill"
                    style=move || format!("width:{}%", bar_l_pct())
                />
                <div
                    class="meter-peak"
                    style=move || {
                        let pct = peak_l_pct();
                        if pct < 0.5 {
                            "display:none".to_string()
                        } else {
                            format!("left:{}%", pct)
                        }
                    }
                />
            </div>
            <div class="meter-bar">
                <div
                    class="meter-fill"
                    style=move || format!("width:{}%", bar_r_pct())
                />
                <div
                    class="meter-peak"
                    style=move || {
                        let pct = peak_r_pct();
                        if pct < 0.5 {
                            "display:none".to_string()
                        } else {
                            format!("left:{}%", pct)
                        }
                    }
                />
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Target tick interval for ballistic math (~30fps)
    const TICK_MS: f64 = 33.0;

    /// Decay factor per 33ms tick.
    fn decay_factor() -> f32 {
        decay_factor_for(TICK_MS as f32 / 1000.0)
    }

    /// Peak decay factor per 33ms tick.
    fn peak_decay_factor() -> f32 {
        peak_decay_factor_for(TICK_MS as f32 / 1000.0)
    }

    #[test]
    fn test_instant_attack() {
        // New peak should jump immediately to target
        let result = ballistic_tick(0.0, 0.8, decay_factor());
        assert!(
            (result - 0.8).abs() < 0.001,
            "Attack should be instant: got {}",
            result
        );
    }

    #[test]
    fn test_smooth_decay() {
        // When target drops, display should decay gradually, not jump
        let display = 0.8;
        let target = 0.0;
        let result = ballistic_tick(display, target, decay_factor());
        assert!(
            result < display,
            "Display should decrease: {} should be < {}",
            result,
            display
        );
        assert!(
            result > 0.0,
            "Display should not drop to 0 in one tick: got {}",
            result
        );
        // Verify it's close to display * decay_factor
        let expected = display * decay_factor();
        assert!(
            (result - expected).abs() < 0.001,
            "Expected ~{}, got {}",
            expected,
            result
        );
    }

    #[test]
    fn test_peak_hold() {
        // Initial peak capture
        let (peak, hold) = peak_hold_tick(0.0, 0.0, 0.5, peak_decay_factor(), TICK_MS);
        assert!((peak - 0.5).abs() < 0.001, "Peak should capture 0.5");
        assert!(
            (hold - PEAK_HOLD_MS).abs() < 0.1,
            "Hold timer should reset to {}",
            PEAK_HOLD_MS
        );

        // During hold period — peak should stay, timer counts down
        let (peak2, hold2) =
            peak_hold_tick(0.5, PEAK_HOLD_MS - 100.0, 0.0, peak_decay_factor(), TICK_MS);
        assert!(
            (peak2 - 0.5).abs() < 0.001,
            "Peak should hold during hold period"
        );
        assert!(hold2 < PEAK_HOLD_MS - 100.0, "Timer should count down");

        // After hold expires — peak should decay
        let (peak3, _) = peak_hold_tick(0.5, 0.0, 0.0, peak_decay_factor(), TICK_MS);
        assert!(peak3 < 0.5, "Peak should decay after hold expires");
        assert!(peak3 > 0.0, "Peak should not drop to 0 in one tick");
    }

    #[test]
    fn test_silence_floor() {
        // Very small values should display as 0%
        assert_eq!(linear_to_pct(0.0), 0.0);
        assert_eq!(linear_to_pct(0.00001), 0.0);
        assert_eq!(linear_to_pct(DISPLAY_FLOOR), 0.0);
    }

    #[test]
    fn test_full_scale() {
        // 1.0 linear (0 dB) should be near 90.9% (60/66 * 100)
        let pct = linear_to_pct(1.0);
        assert!(
            (pct - 90.9).abs() < 1.0,
            "1.0 linear (0 dB) should be ~90.9%, got {}",
            pct
        );
    }

    #[test]
    fn test_clipping_caps_at_100() {
        // Values above +6 dB should cap at 100%
        let pct = linear_to_pct(2.0); // +6 dB
        assert_eq!(pct, 100.0, "+6 dB should be 100%");
    }

    #[test]
    fn test_decay_converges_to_zero() {
        // After many ticks of silence, display should reach 0
        let mut display = 1.0;
        let decay = decay_factor();
        for _ in 0..200 {
            display = ballistic_tick(display, 0.0, decay);
        }
        assert_eq!(display, 0.0, "Should converge to 0 after ~6 seconds");
    }

    #[test]
    fn test_decay_factor_scales_with_dt() {
        // Larger dt should produce more decay (smaller factor)
        let short = decay_factor_for(0.016); // ~60fps
        let long = decay_factor_for(0.033); // ~30fps
        assert!(
            short > long,
            "Shorter dt should decay less: {} vs {}",
            short,
            long
        );
        // Both should be between 0 and 1
        assert!(short > 0.0 && short < 1.0);
        assert!(long > 0.0 && long < 1.0);
    }
}
