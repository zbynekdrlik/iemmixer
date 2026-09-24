//! Full-screen parametric EQ modal with SVG frequency response curve
//!
//! Loads EQ state on-demand from REAPER via GetEqParams, displays draggable
//! band points on a log-frequency curve, and sends SetEqBand on slider changes.
//!
//! v1.104.0: Fix snap-back bug — EqSlider maintains local reactive state so
//! parent re-renders don't destroy active drag gestures. Each band card owns
//! its own signals, and on_change only sends WebSocket (no set_bands.set()).
//!
//! v1.107.0: Display values from REAPER (fh/gd/bo), double-tap to default,
//! per-band on/off toggle and reset buttons.
//!
//! v1.108.0: Band ordering (HPF first), ±12dB gain range fix, HPF toggle via
//! frequency, professional biquad curve rendering (Audio EQ Cookbook).
//!
//! ### EQ value precision (set→reopen drift fix, reaperiem#194 + reaperiem#196)
//!
//! Three layers protect the round-trip set→display→store→reopen against drift:
//!
//! 1. **ReaScript Newton refinement** (`scripts/reascripts/set_eq_param.lua`) —
//!    after the initial 21-sample interpolation, the script reads REAPER's
//!    actual stored value, estimates slope via finite difference, nudges the
//!    norm by error/slope, repeats ≤5 iterations. Converges to within 0.05 dB
//!    / 0.5 Hz / 0.005 oct of the requested value.
//! 2. **UI snap to display granularity** (`snap_db` / `snap_hz` / `snap_oct`
//!    in this file) — slider on_change rounds the projected value to the same
//!    granularity the UI label uses (`{:.1} dB`, `format_freq`, `{:.2} oct`).
//!    Sent value matches displayed value exactly.
//! 3. **drag_end force-flush** — slider on_change is throttled to 50 ms,
//!    which can drop the last position before drag-end. on_drag_end now
//!    unconditionally sends the local sig value, guaranteeing the final
//!    position reaches REAPER. The local sig already holds the snapped value
//!    from on_change.
//!
//! Together these make sent = displayed = stored = reopened. Removing any
//! layer reintroduces a drift surface.

use leptos::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::prelude::*;

/// Activation delay in milliseconds (matches fader.rs pattern)
const ACTIVATION_DELAY_MS: u32 = 150;

/// Maximum time between taps for double-tap detection (ms)
const DOUBLE_TAP_MS: f64 = 300.0;

/// Sample rate for biquad filter calculations (Dante network rate)
const SAMPLE_RATE: f32 = 96000.0;

/// UI-fixed visual range for the freq slider (Hz). REAPER's actual ReaEQ range is
/// 20 Hz – ~24 kHz; we mirror that here for the slider's log-scale projection.
const UI_FREQ_MIN_HZ: f32 = 20.0;
const UI_FREQ_MAX_HZ: f32 = 24000.0;

/// UI-fixed visual range for the bw slider (octaves). REAPER's typical ReaEQ bw is
/// 0.01 – 4.00 oct (varies per band type, but UI displays this nominal range).
const UI_BW_MIN_OCT: f32 = 0.01;
const UI_BW_MAX_OCT: f32 = 4.00;

/// UI-fixed visual range for the gain slider (dB). REAPER's actual gd range is
/// -150 dB to +12 dB, but musical gains live in ±12 — UI clamps display to this
/// range so the slider isn't squashed into the far-right 10%.
const UI_GAIN_MIN_DB: f32 = -12.0;
const UI_GAIN_MAX_DB: f32 = 12.0;

/// EQ band data (mirrors iem_core::EqBand for frontend use)
#[derive(Debug, Clone, PartialEq)]
pub struct EqBandState {
    pub band_type: String,
    pub freq_hz: f32,
    pub gain_db: f32,
    pub bw: f32,
    pub freq_norm: f32,
    pub gain_norm: f32,
    pub bw_norm: f32,
    /// Whether this band is enabled (disabled bands should not affect the curve)
    pub enabled: bool,
}

/// Band type colors for visual distinction on the curve
fn band_color(band_type: &str) -> &'static str {
    match band_type {
        "lowshelf" => "#f4d35e",
        "highshelf" => "#ff6b6b",
        "highpass" => "#4ecdc4",
        "lowpass" => "#a06cd5",
        "notch" => "#ff9f43",
        _ => "#4ecdc4", // "band" = default accent
    }
}

/// Convert frequency in Hz to SVG x position (20Hz-20kHz log scale)
fn freq_to_x(freq_hz: f32, width: f32) -> f32 {
    let log_min = 20.0_f32.ln();
    let log_max = 20000.0_f32.ln();
    let log_freq = freq_hz.clamp(20.0, 20000.0).ln();
    ((log_freq - log_min) / (log_max - log_min)) * width
}

/// Convert gain in dB to SVG y position (±12 dB range)
fn gain_to_y(gain_db: f32, height: f32) -> f32 {
    let clamped = gain_db.clamp(-12.0, 12.0);
    // +12 at top (y=0), -12 at bottom (y=height)
    ((12.0 - clamped) / 24.0) * height
}

/// Display order for band types (professional EQ convention: filters first)
fn display_order(band_type: &str) -> u8 {
    match band_type {
        "highpass" => 0,
        "lowshelf" => 1,
        "highshelf" => 4,
        "lowpass" => 5,
        _ => 2, // "band", "notch", "bandpass" in the middle
    }
}

// ─── Biquad filter coefficient functions (Audio EQ Cookbook) ───

/// Convert bandwidth in octaves to Q factor for biquad filters
fn bw_to_q(bw_oct: f32, w0: f32) -> f32 {
    let sinh_val = (2.0_f32.ln() / 2.0 * bw_oct * w0 / w0.sin()).sinh();
    if sinh_val > 0.0 {
        1.0 / (2.0 * sinh_val)
    } else {
        0.707 // fallback to Butterworth Q
    }
}

/// Biquad coefficients: (b0, b1, b2, a0, a1, a2)
type BiquadCoeffs = (f32, f32, f32, f32, f32, f32);

fn biquad_peaking(w0: f32, gain_db: f32, bw_oct: f32) -> BiquadCoeffs {
    let a = 10.0_f32.powf(gain_db / 40.0);
    let q = bw_to_q(bw_oct, w0);
    let alpha = w0.sin() / (2.0 * q);
    let cos_w0 = w0.cos();

    let b0 = 1.0 + alpha * a;
    let b1 = -2.0 * cos_w0;
    let b2 = 1.0 - alpha * a;
    let a0 = 1.0 + alpha / a;
    let a1 = -2.0 * cos_w0;
    let a2 = 1.0 - alpha / a;
    (b0, b1, b2, a0, a1, a2)
}

fn biquad_low_shelf(w0: f32, gain_db: f32, bw_oct: f32) -> BiquadCoeffs {
    let a = 10.0_f32.powf(gain_db / 40.0);
    // Audio EQ Cookbook shelf-slope formula (NOT the peaking-Q formula).
    // S is the shelf slope parameter. REAPER exposes a "bandwidth in
    // octaves" for shelves; we map it to S = 1 / bw_oct and CLAMP to
    // [0.01, 1.0] where 1.0 = Butterworth shelf (maximum S without
    // overshoot). Above S = 1 the cookbook formula re-introduces
    // resonance near the corner — the exact bug reaperiem#167 is trying to
    // eliminate — so we forbid it. Narrow-bandwidth shelves render as
    // Butterworth; wide-bandwidth shelves render as gentler slopes.
    // This matches REAPER's ReaEQ display, which is always visually
    // smooth regardless of user-set bandwidth.
    let s = (1.0 / bw_oct.max(0.01)).clamp(0.01, 1.0);
    let alpha = w0.sin() / 2.0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).max(0.0).sqrt();
    let cos_w0 = w0.cos();
    let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

    let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
    let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
    let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
    let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
    let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
    let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha;
    (b0, b1, b2, a0, a1, a2)
}

fn biquad_high_shelf(w0: f32, gain_db: f32, bw_oct: f32) -> BiquadCoeffs {
    let a = 10.0_f32.powf(gain_db / 40.0);
    // Audio EQ Cookbook shelf-slope formula. See biquad_low_shelf comment.
    let s = (1.0 / bw_oct.max(0.01)).clamp(0.01, 1.0);
    let alpha = w0.sin() / 2.0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).max(0.0).sqrt();
    let cos_w0 = w0.cos();
    let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

    let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
    let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
    let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
    let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
    let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
    let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha;
    (b0, b1, b2, a0, a1, a2)
}

fn biquad_hpf(w0: f32, bw_oct: f32) -> BiquadCoeffs {
    let q = bw_to_q(bw_oct, w0);
    let alpha = w0.sin() / (2.0 * q);
    let cos_w0 = w0.cos();

    let b0 = (1.0 + cos_w0) / 2.0;
    let b1 = -(1.0 + cos_w0);
    let b2 = (1.0 + cos_w0) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_w0;
    let a2 = 1.0 - alpha;
    (b0, b1, b2, a0, a1, a2)
}

fn biquad_lpf(w0: f32, bw_oct: f32) -> BiquadCoeffs {
    let q = bw_to_q(bw_oct, w0);
    let alpha = w0.sin() / (2.0 * q);
    let cos_w0 = w0.cos();

    let b0 = (1.0 - cos_w0) / 2.0;
    let b1 = 1.0 - cos_w0;
    let b2 = (1.0 - cos_w0) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_w0;
    let a2 = 1.0 - alpha;
    (b0, b1, b2, a0, a1, a2)
}

fn biquad_notch(w0: f32, bw_oct: f32) -> BiquadCoeffs {
    let q = bw_to_q(bw_oct, w0);
    let alpha = w0.sin() / (2.0 * q);
    let cos_w0 = w0.cos();

    let b0 = 1.0;
    let b1 = -2.0 * cos_w0;
    let b2 = 1.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_w0;
    let a2 = 1.0 - alpha;
    (b0, b1, b2, a0, a1, a2)
}

/// Evaluate biquad frequency response magnitude in dB at a given frequency.
/// Uses H(e^jω) = (b0 + b1·e^-jω + b2·e^-2jω) / (a0 + a1·e^-jω + a2·e^-2jω)
fn eval_biquad_db(freq: f32, coeffs: BiquadCoeffs) -> f32 {
    let (b0, b1, b2, a0, a1, a2) = coeffs;
    let w = 2.0 * std::f32::consts::PI * freq / SAMPLE_RATE;
    let cos_w = w.cos();
    let cos_2w = (2.0 * w).cos();
    let sin_w = w.sin();
    let sin_2w = (2.0 * w).sin();

    let num_re = b0 + b1 * cos_w + b2 * cos_2w;
    let num_im = -(b1 * sin_w + b2 * sin_2w);
    let den_re = a0 + a1 * cos_w + a2 * cos_2w;
    let den_im = -(a1 * sin_w + a2 * sin_2w);

    let num_mag_sq = num_re * num_re + num_im * num_im;
    let den_mag_sq = den_re * den_re + den_im * den_im;

    if den_mag_sq < 1e-20 {
        return 0.0;
    }
    10.0 * (num_mag_sq / den_mag_sq).max(1e-10).log10()
}

/// Compute the gain contribution of a single band at a given frequency
/// using proper biquad transfer function evaluation.
fn compute_band_gain(freq: f32, band: &EqBandState) -> f32 {
    let w0 = 2.0 * std::f32::consts::PI * band.freq_hz.max(20.0) / SAMPLE_RATE;
    let bw = band.bw.max(0.01);

    let coeffs = match band.band_type.as_str() {
        "band" => {
            if band.gain_db.abs() < 0.01 {
                return 0.0;
            }
            biquad_peaking(w0, band.gain_db, bw)
        }
        "lowshelf" => {
            if band.gain_db.abs() < 0.01 {
                return 0.0;
            }
            biquad_low_shelf(w0, band.gain_db, bw)
        }
        "highshelf" => {
            if band.gain_db.abs() < 0.01 {
                return 0.0;
            }
            biquad_high_shelf(w0, band.gain_db, bw)
        }
        "highpass" => biquad_hpf(w0, bw),
        "lowpass" => biquad_lpf(w0, bw),
        "notch" => biquad_notch(w0, bw),
        _ => return 0.0,
    };

    eval_biquad_db(freq, coeffs)
}

/// Generate the frequency response curve path as SVG "d" attribute.
/// Sums biquad transfer function responses from all active bands.
fn generate_curve_path(bands: &[EqBandState], width: f32, height: f32) -> String {
    let num_points = 200;
    let log_min = 20.0_f32.ln();
    let log_max = 20000.0_f32.ln();
    let mut path = String::with_capacity(num_points * 20);

    for i in 0..=num_points {
        let x = (i as f32 / num_points as f32) * width;
        let log_freq = log_min + (i as f32 / num_points as f32) * (log_max - log_min);
        let freq = log_freq.exp();

        // Sum contributions from enabled bands only
        let mut total_gain = 0.0_f32;
        for band in bands {
            if !band.enabled {
                continue;
            }
            total_gain += compute_band_gain(freq, band);
        }

        let y = gain_to_y(total_gain, height);

        if i == 0 {
            path.push_str(&format!("M{:.1},{:.1}", x, y));
        } else {
            path.push_str(&format!(" L{:.1},{:.1}", x, y));
        }
    }
    path
}

/// Format frequency for display
fn format_freq(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{:.1}k", hz / 1000.0)
    } else {
        format!("{:.0}", hz)
    }
}

/// Snap a dB value to UI display granularity (0.1 dB).
/// Ensures slider on_change sends exactly the value shown in the label,
/// preventing reopen-drift caused by float-precision mismatch between
/// displayed text (`{:.1} dB`) and the underlying float sent to ReaScript.
fn snap_db(db: f32) -> f32 {
    (db * 10.0).round() / 10.0
}

/// Snap a Hz value to UI display granularity. Matches `format_freq`:
/// integer Hz below 1 kHz, 100 Hz above.
fn snap_hz(hz: f32) -> f32 {
    if hz >= 1000.0 {
        (hz / 100.0).round() * 100.0
    } else {
        hz.round()
    }
}

/// Snap an oct value to UI display granularity (0.01 oct).
fn snap_oct(oct: f32) -> f32 {
    (oct * 100.0).round() / 100.0
}

/// Per-band local state signals that survive parent re-renders.
/// These are created once per band when the modal opens and persist until close.
#[derive(Clone)]
struct BandLocalState {
    /// Original REAPER band index (0-4) — used for API calls
    reaper_band_idx: u8,
    band_type: String,
    freq_norm: RwSignal<f32>,
    gain_norm: RwSignal<f32>,
    bw_norm: RwSignal<f32>,
    /// REAPER-formatted display values (accurate, loaded from server)
    freq_hz: RwSignal<f32>,
    gain_db: RwSignal<f32>,
    bw_oct: RwSignal<f32>,
    /// Whether this band is enabled
    enabled: RwSignal<bool>,
}

/// Full-screen EQ modal component
#[component]
pub fn EQModal(
    /// Track index to show EQ for
    track_index: usize,
    /// Track name for the header
    track_name: String,
    /// EQ bands data from server (synced to local signals when not dragging)
    bands: ReadSignal<Vec<EqBandState>>,
    /// Whether EQ data is loading
    loading: ReadSignal<bool>,
    /// Callback when a band parameter changes (band_index, param_name, normalized_value)
    on_param_change: Callback<(u8, String, f32)>,
    /// Callback to close the modal
    on_close: Callback<()>,
) -> impl IntoView {
    let _ = track_index;
    let track_name = StoredValue::new(track_name);

    // SVG dimensions
    let svg_width = 800.0_f32;
    let svg_height = 300.0_f32;

    // Generate the 0dB reference line y position
    let zero_db_y = gain_to_y(0.0, svg_height);

    // Grid lines for frequency axis (log scale)
    let freq_grid_lines: Vec<(f32, String)> = [
        20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0,
    ]
    .iter()
    .map(|&f| (freq_to_x(f, svg_width), format_freq(f)))
    .collect();

    // Grid lines for gain axis (±12 dB range)
    let gain_grid_lines: Vec<(f32, String)> = [-12.0, -6.0, 0.0, 6.0, 12.0]
        .iter()
        .map(|&g| (gain_to_y(g, svg_height), format!("{:+.0}", g)))
        .collect();

    // Per-band local signals stored ONCE — never replaced, so DOM stays stable.
    // StoredValue is NOT reactive: reading it does not subscribe, so the band card
    // DOM created from it is rendered exactly once and never torn down.
    let stored_locals: StoredValue<Vec<BandLocalState>> = StoredValue::new(Vec::new());

    // Gate signal: flips to true ONCE when bands data first arrives.
    // The <Show> component renders band cards when this becomes true, but
    // since stored_locals is non-reactive, the cards are created once and persist.
    let local_state_created = RwSignal::new(false);

    // Track whether any slider is currently being dragged (guards against server echo)
    let any_dragging = RwSignal::new(false);

    // Explicit trigger for curve + display updates. Incremented AFTER signal writes complete.
    // The Memo and display closures subscribe to THIS (not to individual band signals),
    // preventing recursive closure invocation that causes WASM panics.
    let curve_trigger = RwSignal::new(0u32);

    // Sync from parent bands signal into local signals.
    // First arrival: populate stored_locals and flip the gate.
    // Subsequent: update existing RwSignals (no DOM destruction).
    Effect::new(move |_| {
        let parent = bands.get();
        if parent.is_empty() {
            return;
        }

        // On disposal, treat as "already created" so the init path
        // (which further writes signals) is skipped.
        if !local_state_created.try_get_untracked().unwrap_or(true) {
            // First time: create local signals, sorted by display order
            // Build (reaper_index, band) pairs then sort for display
            let mut indexed: Vec<(usize, &EqBandState)> = parent.iter().enumerate().collect();
            indexed.sort_by(|a, b| {
                let ord_a = display_order(&a.1.band_type);
                let ord_b = display_order(&b.1.band_type);
                ord_a.cmp(&ord_b).then_with(|| {
                    a.1.freq_hz
                        .partial_cmp(&b.1.freq_hz)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            });

            let locals: Vec<BandLocalState> = indexed
                .iter()
                .map(|(reaper_idx, b)| BandLocalState {
                    reaper_band_idx: *reaper_idx as u8,
                    band_type: b.band_type.clone(),
                    freq_norm: RwSignal::new(b.freq_norm),
                    gain_norm: RwSignal::new(b.gain_norm),
                    bw_norm: RwSignal::new(b.bw_norm),
                    freq_hz: RwSignal::new(b.freq_hz),
                    gain_db: RwSignal::new(b.gain_db),
                    bw_oct: RwSignal::new(b.bw),
                    enabled: RwSignal::new(b.enabled),
                })
                .collect();
            stored_locals.set_value(locals);
            // Defer gate flip to next microtask — Effect must complete before
            // <Show> renders its body (which reads the RwSignals we just created).
            // Without deferral, Leptos detects recursive closure invocation → WASM panic.
            wasm_bindgen_futures::spawn_local(async move {
                let _ = local_state_created.try_set(true);
            });
        } else if !any_dragging.try_get_untracked().unwrap_or(true) {
            // Subsequent: sync values then trigger display update
            let locals = stored_locals.get_value();
            for local in locals.iter() {
                let ri = local.reaper_band_idx as usize;
                if let Some(parent_band) = parent.get(ri) {
                    let _ = local.freq_norm.try_set(parent_band.freq_norm);
                    let _ = local.gain_norm.try_set(parent_band.gain_norm);
                    let _ = local.bw_norm.try_set(parent_band.bw_norm);
                    let _ = local.freq_hz.try_set(parent_band.freq_hz);
                    let _ = local.gain_db.try_set(parent_band.gain_db);
                    let _ = local.bw_oct.try_set(parent_band.bw);
                    let _ = local.enabled.try_set(parent_band.enabled);
                }
            }
            let _ = curve_trigger.try_update(|n| *n += 1);
        }
    });

    view! {
        <div class="eq-overlay" on:click=move |_| on_close.run(())>
            <div class="eq-modal" on:click=move |e: web_sys::MouseEvent| e.stop_propagation()>
                // Header
                <div class="eq-header">
                    <span class="eq-title">"EQ: " {move || track_name.get_value()}</span>
                    <button class="eq-close-btn" on:click=move |_| on_close.run(())>
                        "\u{2715}"
                    </button>
                </div>

                // Loading indicator
                <Show when=move || loading.get() fallback=|| ()>
                    <div class="eq-loading">"Loading EQ..."</div>
                </Show>

                // No EQ message
                <Show when=move || !loading.get() && !local_state_created.get() fallback=|| ()>
                    <div class="eq-no-eq">"No ReaEQ found on this track"</div>
                </Show>

                // SVG Curve display + band controls
                // This <Show> flips ONCE when bands data arrives. The content inside
                // is rendered once and never torn down (stored_locals is non-reactive).
                <Show when=move || local_state_created.get() fallback=|| ()>
                    <div class="eq-curve-container">
                        <svg
                            viewBox=format!("0 0 {} {}", svg_width, svg_height)
                            class="eq-curve-svg"
                            preserveAspectRatio="none"
                        >
                            // Background grid - frequency lines
                            {freq_grid_lines.iter().map(|(x, label)| {
                                let x = *x;
                                let label = label.clone();
                                view! {
                                    <line
                                        x1=x x2=x y1=0 y2=svg_height
                                        stroke="rgba(255,255,255,0.08)" stroke-width="1"
                                    />
                                    <text x=x y=svg_height - 4.0 fill="rgba(255,255,255,0.3)"
                                        font-size="10" text-anchor="middle">
                                        {label}
                                    </text>
                                }
                            }).collect::<Vec<_>>()}

                            // Background grid - gain lines
                            {gain_grid_lines.iter().map(|(y, label)| {
                                let y = *y;
                                let label = label.clone();
                                let opacity = if label == "+0" { "0.25" } else { "0.08" };
                                let width = if label == "+0" { "1.5" } else { "1" };
                                view! {
                                    <line
                                        x1=0 x2=svg_width y1=y y2=y
                                        stroke=format!("rgba(255,255,255,{})", opacity)
                                        stroke-width=width
                                    />
                                    <text x=4 y=y - 2.0 fill="rgba(255,255,255,0.3)"
                                        font-size="10">
                                        {label}
                                    </text>
                                }
                            }).collect::<Vec<_>>()}

                            // 0dB reference line (brighter)
                            <line
                                x1=0 x2=svg_width y1=zero_db_y y2=zero_db_y
                                stroke="rgba(255,255,255,0.25)" stroke-width="1.5"
                            />

                            // Frequency response curve — triggered ONLY by curve_trigger (not band signals)
                            // Uses get_untracked() to read band values without subscribing,
                            // preventing recursive closure invocation that kills the Memo.
                            {
                                let curve_memo = Memo::new(move |_| {
                                    curve_trigger.get(); // ONLY subscription
                                    let locals = stored_locals.get_value();
                                    let states: Vec<EqBandState> = locals.iter().map(|l| {
                                        let fn_ = l.freq_norm.get_untracked();
                                        let gn_ = l.gain_norm.get_untracked();
                                        let bn_ = l.bw_norm.get_untracked();
                                        EqBandState {
                                            band_type: l.band_type.clone(),
                                            freq_hz: l.freq_hz.get_untracked(),
                                            gain_db: l.gain_db.get_untracked(),
                                            bw: l.bw_oct.get_untracked(),
                                            freq_norm: fn_,
                                            gain_norm: gn_,
                                            bw_norm: bn_,
                                            enabled: l.enabled.get_untracked(),
                                        }
                                    }).collect();
                                    generate_curve_path(&states, svg_width, svg_height)
                                });
                                view! {
                                    <path
                                        d=move || curve_memo.get()
                                        fill="none"
                                        stroke="var(--accent)"
                                        stroke-width="2.5"
                                    />
                                    <path
                                        d=move || {
                                            let curve = curve_memo.get();
                                            format!("{} L{:.1},{:.1} L0,{:.1} Z", curve, svg_width, zero_db_y, zero_db_y)
                                        }
                                        fill="rgba(78, 205, 196, 0.08)"
                                    />
                                }
                            }

                            // Band points — stable SVG elements with reactive attributes.
                            // NOT wrapped in {move || ...} to avoid re-creating DOM.
                            {
                                let locals = stored_locals.get_value();
                                locals.iter().enumerate().map(|(i, local)| {
                                    let color = band_color(&local.band_type).to_string();
                                    let freq_hz_sig = local.freq_hz;
                                    let gain_db_sig = local.gain_db;
                                    view! {
                                        <circle
                                            cx=move || { curve_trigger.get(); freq_to_x(freq_hz_sig.get_untracked(), svg_width) }
                                            cy=move || { curve_trigger.get(); gain_to_y(gain_db_sig.get_untracked(), svg_height) }
                                            r="8"
                                            fill=color.clone()
                                            stroke="white" stroke-width="2"
                                            opacity="0.9"
                                            // data-band-dot enables stable targeting by E2E tests
                                            // (see eq.spec.ts reaperiem#167 curve-shape test). Avoids relying
                                            // on r>=6 heuristics that would break if decorative
                                            // circles were added to the SVG.
                                            data-band-dot="true"
                                        />
                                        <text
                                            x=move || { curve_trigger.get(); freq_to_x(freq_hz_sig.get_untracked(), svg_width) }
                                            y=move || { curve_trigger.get(); gain_to_y(gain_db_sig.get_untracked(), svg_height) - 12.0 }
                                            fill="white" font-size="11" text-anchor="middle"
                                            font-weight="bold"
                                        >
                                            {format!("{}", i + 1)}
                                        </text>
                                    }
                                }).collect::<Vec<_>>()
                            }
                        </svg>
                    </div>

                    // Band controls — rendered ONCE from stored_locals (non-reactive).
                    <div class="eq-band-controls">
                        {
                            let locals = stored_locals.get_value();
                            locals.iter().enumerate().map(|(i, local)| {
                                let band_idx = local.reaper_band_idx;
                                // Store band_idx in Leptos StoredValue for robust access from
                                // slider callbacks (where DOM data-attribute approach isn't possible).
                                let band_idx_sv = StoredValue::new(band_idx);
                                let band_type = local.band_type.clone();
                                let band_type_reset = band_type.clone();
                                let color = band_color(&band_type).to_string();

                                // Get the stable local signals for this band
                                let freq_hz_sig = local.freq_hz;
                                let gain_db_sig = local.gain_db;
                                let bw_oct_sig = local.bw_oct;
                                let enabled_sig = local.enabled;
                                // Throttle WebSocket sends to 50ms intervals per band.
                                let last_send_freq = RwSignal::new(0.0_f64);
                                let last_send_gain = RwSignal::new(0.0_f64);
                                let last_send_bw = RwSignal::new(0.0_f64);

                                view! {
                                    <div class="eq-band-card" style=format!("border-color: {}", color)>
                                        <div class="eq-band-header">
                                            <span class="eq-band-num" style=format!("background: {}", color)>
                                                {i + 1}
                                            </span>
                                            <span class="eq-band-type">{band_type.clone()}</span>
                                            // Toggle band enabled/disabled
                                            <button
                                                class=move || {
                                                    if enabled_sig.get() { "eq-band-toggle on" } else { "eq-band-toggle off" }
                                                }
                                                on:click=move |_| {
                                                    let idx = band_idx_sv.get_value();
                                                    // Toggle band enabled/disabled via BANDENABLEDM
                                                    // (no colon — per-band control)
                                                    if enabled_sig.get_untracked() {
                                                        let _ = enabled_sig.try_set(false);
                                                        on_param_change.run((idx, "enabled".to_string(), 0.0));
                                                    } else {
                                                        let _ = enabled_sig.try_set(true);
                                                        on_param_change.run((idx, "enabled".to_string(), 1.0));
                                                    }
                                                    let _ = curve_trigger.try_update(|n| *n += 1);
                                                }
                                            />
                                            // Reset button
                                            <button
                                                class="eq-band-reset"
                                                title="Reset band"
                                                on:click=move |_| {
                                                    let idx = band_idx_sv.get_value();
                                                    // Reset gain to 0dB via new gain_db protocol
                                                    let _ = gain_db_sig.try_set(0.0);
                                                    on_param_change.run((idx, "gain_db".to_string(), 0.0));
                                                    // Reset freq to per-band default Hz (reaperiem#196)
                                                    let default_freq_hz: f32 = match band_type_reset.as_str() {
                                                        "highpass" => 80.0,
                                                        "lowshelf" => 200.0,
                                                        "highshelf" => 8000.0,
                                                        "lowpass" => 12000.0,
                                                        _ => {
                                                            // Parametric bands: use REAPER index
                                                            if idx == 3 { 3000.0 } else { 800.0 }
                                                        }
                                                    };
                                                    // Reset bw to per-band default oct (reaperiem#196)
                                                    let default_bw_oct: f32 = match band_type_reset.as_str() {
                                                        "highpass" | "lowshelf" | "highshelf" | "lowpass" => 2.00,
                                                        _ => 1.00,
                                                    };
                                                    let _ = freq_hz_sig.try_set(default_freq_hz);
                                                    on_param_change.run((idx, "freq_hz".to_string(), default_freq_hz));
                                                    let _ = bw_oct_sig.try_set(default_bw_oct);
                                                    on_param_change.run((idx, "bw_oct".to_string(), default_bw_oct));
                                                    // Enable/disable state NOT changed — reset only affects parameters
                                                    let _ = curve_trigger.try_update(|n| *n += 1);
                                                }
                                            >
                                                "\u{21BA}"
                                            </button>
                                        </div>

                                        // Frequency slider: derives position from REAPER's actual freq_hz
                                        // mapped onto a fixed UI log scale (20 Hz – 24 kHz). Single source
                                        // of truth = REAPER (reaperiem#196).
                                        <div class="eq-param-row">
                                            <label class="eq-param-label">"Freq"</label>
                                            <EqSlider
                                                value=Signal::derive(move || {
                                                    let hz = freq_hz_sig.get().clamp(UI_FREQ_MIN_HZ, UI_FREQ_MAX_HZ);
                                                    let log_min = UI_FREQ_MIN_HZ.ln();
                                                    let log_max = UI_FREQ_MAX_HZ.ln();
                                                    (hz.ln() - log_min) / (log_max - log_min)
                                                })
                                                on_change=Callback::new(move |v: f32| {
                                                    let log_min = UI_FREQ_MIN_HZ.ln();
                                                    let log_max = UI_FREQ_MAX_HZ.ln();
                                                    // Snap to UI display granularity — same lock-step
                                                    // sent/displayed/stored argument as gain_db (reaperiem#196).
                                                    let hz = snap_hz((log_min + v * (log_max - log_min)).exp());
                                                    let now = js_sys::Date::now();
                                                    if now - last_send_freq.get_untracked() > 50.0 {
                                                        let _ = last_send_freq.try_set(now);
                                                        on_param_change.run((band_idx_sv.get_value(), "freq_hz".to_string(), hz));
                                                    }
                                                    let _ = freq_hz_sig.try_set(hz);
                                                    let _ = curve_trigger.try_update(|n| *n += 1);
                                                })
                                                on_drag_start=Callback::new(move |_: ()| {
                                                    let _ = any_dragging.try_set(true);
                                                })
                                                on_drag_end=Callback::new(move |_: ()| {
                                                    let _ = any_dragging.try_set(false);
                                                    // Force-flush final value: 50 ms throttle in on_change
                                                    // can drop the last position. Without this, REAPER
                                                    // stores a value from up to 50 ms before drag-end
                                                    // and reopen reads that → drift (reaperiem#196).
                                                    // Always send (no last_send_* check) — extra send is
                                                    // cheaper than missing the final position. ReaScript
                                                    // Newton refinement is idempotent on duplicate writes.
                                                    let final_hz = freq_hz_sig.get_untracked();
                                                    on_param_change.run((band_idx_sv.get_value(), "freq_hz".to_string(), final_hz));
                                                })
                                                css_class="eq-slider-freq"
                                            />
                                            <span class="eq-param-value">
                                                {move || {
                                                    curve_trigger.get();
                                                    let hz = freq_hz_sig.get_untracked().clamp(UI_FREQ_MIN_HZ, UI_FREQ_MAX_HZ);
                                                    format_freq(hz)
                                                }}
                                            </span>
                                        </div>

                                        // Gain slider: derives position from REAPER's actual gain_db
                                        // clamped to a fixed ±12 dB UI range for sensible UX.
                                        // (REAPER's actual range is -150..+12 dB which would squash
                                        // all musical gains into the far-right 10% of slider travel.)
                                        <div class="eq-param-row">
                                            <label class="eq-param-label">"Gain"</label>
                                            <EqSlider
                                                value=Signal::derive(move || {
                                                    // Single source of truth — REAPER's formatted dB.
                                                    // Slider VISUAL range fixed at ±12 dB for UX
                                                    // (REAPER's actual range is -150..+12 dB which would
                                                    // squash all musical gains into the far-right 10%
                                                    // of slider travel). Out-of-range values clamp.
                                                    let db = gain_db_sig.get().clamp(UI_GAIN_MIN_DB, UI_GAIN_MAX_DB);
                                                    (db - UI_GAIN_MIN_DB) / (UI_GAIN_MAX_DB - UI_GAIN_MIN_DB)
                                                })
                                                on_change=Callback::new(move |v: f32| {
                                                    // Project slider position 0-1 to dB, then snap to UI
                                                    // display granularity (0.1 dB). Without snapping, the
                                                    // displayed `{:.1}` rounds e.g. 2.04 → "+2.0 dB" while
                                                    // ReaScript receives 2.04, REAPER stores ~2.04, and on
                                                    // reopen the display may round differently → drift.
                                                    let db = snap_db(UI_GAIN_MIN_DB + v * (UI_GAIN_MAX_DB - UI_GAIN_MIN_DB));
                                                    let now = js_sys::Date::now();
                                                    if now - last_send_gain.get_untracked() > 50.0 {
                                                        let _ = last_send_gain.try_set(now);
                                                        on_param_change.run((band_idx_sv.get_value(), "gain_db".to_string(), db));
                                                    }
                                                    let _ = gain_db_sig.try_set(db);
                                                    let _ = curve_trigger.try_update(|n| *n += 1);
                                                })
                                                on_drag_start=Callback::new(move |_: ()| {
                                                    let _ = any_dragging.try_set(true);
                                                })
                                                on_drag_end=Callback::new(move |_: ()| {
                                                    let _ = any_dragging.try_set(false);
                                                    // Force-flush final value past the 50 ms throttle (reaperiem#196).
                                                    // Always send (no last_send_* check) — extra send is
                                                    // cheaper than missing the final position. ReaScript
                                                    // Newton refinement is idempotent on duplicate writes.
                                                    let final_db = gain_db_sig.get_untracked();
                                                    on_param_change.run((band_idx_sv.get_value(), "gain_db".to_string(), final_db));
                                                })
                                                css_class="eq-slider-gain"
                                                default_value=0.5
                                            />
                                            <span class="eq-param-value">
                                                {move || {
                                                    curve_trigger.get();
                                                    // Clamp display to ±12 dB to match slider visual range —
                                                    // single source of truth for both thumb position and text.
                                                    let db = gain_db_sig.get_untracked().clamp(UI_GAIN_MIN_DB, UI_GAIN_MAX_DB);
                                                    if db >= 0.0 { format!("+{:.1} dB", db) } else { format!("{:.1} dB", db) }
                                                }}
                                            </span>
                                        </div>

                                        // Bandwidth/Q slider: derives position from REAPER's actual bw_oct
                                        // mapped onto a fixed UI linear scale (0.01 – 4.00 oct). Single
                                        // source of truth = REAPER (reaperiem#196).
                                        <div class="eq-param-row">
                                            <label class="eq-param-label">"BW"</label>
                                            <EqSlider
                                                value=Signal::derive(move || {
                                                    let oct = bw_oct_sig.get().clamp(UI_BW_MIN_OCT, UI_BW_MAX_OCT);
                                                    (oct - UI_BW_MIN_OCT) / (UI_BW_MAX_OCT - UI_BW_MIN_OCT)
                                                })
                                                on_change=Callback::new(move |v: f32| {
                                                    // Snap to UI display granularity (0.01 oct).
                                                    let oct = snap_oct(UI_BW_MIN_OCT + v * (UI_BW_MAX_OCT - UI_BW_MIN_OCT));
                                                    let now = js_sys::Date::now();
                                                    if now - last_send_bw.get_untracked() > 50.0 {
                                                        let _ = last_send_bw.try_set(now);
                                                        on_param_change.run((band_idx_sv.get_value(), "bw_oct".to_string(), oct));
                                                    }
                                                    let _ = bw_oct_sig.try_set(oct);
                                                    let _ = curve_trigger.try_update(|n| *n += 1);
                                                })
                                                on_drag_start=Callback::new(move |_: ()| {
                                                    let _ = any_dragging.try_set(true);
                                                })
                                                on_drag_end=Callback::new(move |_: ()| {
                                                    let _ = any_dragging.try_set(false);
                                                    // Force-flush final value past the 50 ms throttle (reaperiem#196).
                                                    // Always send (no last_send_* check) — extra send is
                                                    // cheaper than missing the final position. ReaScript
                                                    // Newton refinement is idempotent on duplicate writes.
                                                    let final_oct = bw_oct_sig.get_untracked();
                                                    on_param_change.run((band_idx_sv.get_value(), "bw_oct".to_string(), final_oct));
                                                })
                                                css_class=""
                                                default_value=0.5
                                            />
                                            <span class="eq-param-value">
                                                {move || {
                                                    curve_trigger.get();
                                                    let oct = bw_oct_sig.get_untracked().clamp(UI_BW_MIN_OCT, UI_BW_MAX_OCT);
                                                    format!("{:.2} oct", oct)
                                                }}
                                            </span>
                                        </div>
                                    </div>
                                }
                            }).collect::<Vec<_>>()
                        }
                    </div>
                </Show>
            </div>
        </div>
    }
}

/// Touch-safe horizontal slider for EQ parameters.
///
/// Follows the same 150ms activation pattern as the Fader component:
/// - Press and hold 150ms: activates with visual feedback
/// - All movement is relative (never jumps to tap position)
/// - Short taps are ignored (prevents accidental changes while scrolling)
/// - Double-tap resets to default_value (if set, within 300ms)
///
/// v1.104.0: Uses internal `RwSignal<f32>` for display so parent re-renders
/// don't destroy the drag gesture. The `value` prop is a `ReadSignal<f32>`
/// that syncs to the internal signal only when not dragging.
///
/// v1.107.0: Double-tap to default value support.
#[component]
fn EqSlider(
    /// Current normalized value (0-1) from parent signal
    value: Signal<f32>,
    /// Called when value changes during drag
    on_change: Callback<f32>,
    /// Called when drag gesture starts (activation delay passed)
    on_drag_start: Callback<()>,
    /// Called when drag gesture ends (touch/mouse up)
    on_drag_end: Callback<()>,
    /// Additional CSS class for styling variants (e.g., "eq-slider-gain")
    #[prop(default = "")]
    css_class: &'static str,
    /// Default value for double-tap reset (None = no double-tap)
    #[prop(optional)]
    default_value: Option<f32>,
) -> impl IntoView {
    let (is_activated, set_is_activated) = signal(false);
    let (is_pending, set_is_pending) = signal(false);

    // Internal local value signal — source of truth for display during drag
    let local_value = RwSignal::new(value.get_untracked());

    // Parent sync happens via Effect below (after Rc clones) — only when not dragging.
    // During drag, local_value is the source of truth to avoid reactive_graph recursion.

    let timeout_handle: Rc<RefCell<Option<gloo_timers::callback::Timeout>>> =
        Rc::new(RefCell::new(None));
    let move_base_x: Rc<RefCell<Option<f64>>> = Rc::new(RefCell::new(None));
    let touch_start_x: Rc<RefCell<Option<f64>>> = Rc::new(RefCell::new(None));
    let touch_start_y: Rc<RefCell<Option<f64>>> = Rc::new(RefCell::new(None));
    let drag_value: Rc<Cell<f32>> = Rc::new(Cell::new(value.get_untracked()));
    let last_touch_time: Rc<RefCell<f64>> = Rc::new(RefCell::new(0.0));

    // Double-tap detection state
    let last_tap_time: Rc<Cell<f64>> = Rc::new(Cell::new(0.0));

    let track_ref = NodeRef::<leptos::html::Div>::new();

    // Store document-level closures for mouse (same pattern as fader.rs)
    let mouse_move_closure: crate::components::MouseCallbackSlot = Rc::new(RefCell::new(None));
    let mouse_up_closure: crate::components::MouseCallbackSlot = Rc::new(RefCell::new(None));

    // --- Rc clones for closures ---
    let timeout_ts = timeout_handle.clone();
    let timeout_tm = timeout_handle.clone();
    let timeout_te = timeout_handle.clone();
    let timeout_md = timeout_handle;

    let base_x_ts = move_base_x.clone();
    let base_x_tm = move_base_x.clone();
    let base_x_te = move_base_x.clone();
    let base_x_md = move_base_x;

    let start_x_ts = touch_start_x.clone();
    let start_x_tm = touch_start_x;
    let start_y_ts = touch_start_y.clone();
    let start_y_tm = touch_start_y;

    let drag_ts = drag_value.clone();
    let drag_tm = drag_value.clone();
    let drag_sync = drag_value.clone();
    let drag_md = drag_value;

    let last_touch_ts = last_touch_time.clone();
    let last_touch_te = last_touch_time.clone();
    let last_touch_md = last_touch_time;

    let last_tap_ts = last_tap_time.clone();

    let mm_closure_md = mouse_move_closure.clone();
    let mu_closure_md = mouse_up_closure.clone();

    // Sync from parent when not dragging (e.g., reset button)
    Effect::new(move || {
        let parent_val = value.get();
        if !is_activated.try_get_untracked().unwrap_or(true) {
            let _ = local_value.try_set(parent_val);
            drag_sync.set(parent_val);
        }
    });

    // --- Touch handlers ---
    let handle_touchstart = move |ev: web_sys::TouchEvent| {
        let now = js_sys::Date::now();
        *last_touch_ts.borrow_mut() = now;

        // Double-tap detection: if within DOUBLE_TAP_MS and default_value is set
        if let Some(def) = default_value {
            let prev_time = last_tap_ts.get();
            if now - prev_time < DOUBLE_TAP_MS && prev_time > 0.0 && !is_activated.get_untracked() {
                // Double-tap detected — reset to default
                ev.prevent_default();
                last_tap_ts.set(0.0);
                let _ = local_value.try_set(def);
                on_change.run(def);
                return;
            }
            last_tap_ts.set(now);
        }

        if let Some(touch) = ev.touches().get(0) {
            let x = touch.client_x() as f64;
            *start_x_ts.borrow_mut() = Some(x);
            *start_y_ts.borrow_mut() = Some(touch.client_y() as f64);
            *base_x_ts.borrow_mut() = Some(x);
        }

        drag_ts.set(local_value.get_untracked());
        let _ = set_is_pending.try_set(true);

        let timeout = gloo_timers::callback::Timeout::new(ACTIVATION_DELAY_MS, move || {
            let _ = set_is_activated.try_set(true);
            let _ = set_is_pending.try_set(false);
            on_drag_start.run(());
            // Haptic feedback
            if let Some(window) = web_sys::window() {
                let navigator = window.navigator();
                let _ = navigator.vibrate_with_duration(30);
            }
        });
        *timeout_ts.borrow_mut() = Some(timeout);
    };

    let handle_touchmove = move |ev: web_sys::TouchEvent| {
        if let Some(touch) = ev.touches().get(0) {
            let current_x = touch.client_x() as f64;
            let current_y = touch.client_y() as f64;

            // Check if movement is mostly vertical (scrolling intent)
            if let (Some(sx), Some(sy)) = (*start_x_tm.borrow(), *start_y_tm.borrow()) {
                let dx = (current_x - sx).abs();
                let dy = (current_y - sy).abs();
                if dy > dx + 10.0 && !is_activated.get() {
                    *timeout_tm.borrow_mut() = None;
                    let _ = set_is_pending.try_set(false);
                    return;
                }
            }

            if !is_activated.get() {
                *base_x_tm.borrow_mut() = Some(current_x);
                return;
            }

            ev.prevent_default();

            if let Some(el) = track_ref.get() {
                let base_opt = *base_x_tm.borrow();
                if let Some(base_x) = base_opt {
                    let rect = el.get_bounding_client_rect();
                    let delta_x = current_x - base_x;
                    let delta_ratio = delta_x / rect.width();
                    let raw = drag_tm.get();
                    let new_val = (raw + delta_ratio as f32).clamp(0.0, 1.0);
                    drag_tm.set(new_val);
                    let quantized = (new_val * 200.0).round() / 200.0; // 0.005 steps
                    let _ = local_value.try_set(quantized);
                    on_change.run(quantized);
                    *base_x_tm.borrow_mut() = Some(current_x);
                }
            }
        }
    };

    let handle_touchend = move |_ev: web_sys::TouchEvent| {
        *last_touch_te.borrow_mut() = js_sys::Date::now();
        *timeout_te.borrow_mut() = None;
        let was_active = is_activated.get_untracked();
        let _ = set_is_pending.try_set(false);
        let _ = set_is_activated.try_set(false);
        *base_x_te.borrow_mut() = None;
        if was_active {
            on_drag_end.run(());
        }
    };

    let handle_touchcancel = move |_ev: web_sys::TouchEvent| {
        let was_active = is_activated.get_untracked();
        let _ = set_is_pending.try_set(false);
        let _ = set_is_activated.try_set(false);
        if was_active {
            on_drag_end.run(());
        }
    };

    // --- Mouse handler ---
    let handle_mousedown = move |ev: web_sys::MouseEvent| {
        if ev.button() != 0 {
            return;
        }
        // Guard against synthesized mouse events from touch
        if js_sys::Date::now() - *last_touch_md.borrow() < 500.0 {
            return;
        }

        ev.prevent_default();
        ev.stop_propagation();

        let document = web_sys::window().unwrap().document().unwrap();
        let doc_target: web_sys::EventTarget = document.clone().into();

        // Clean up previous listeners
        if let Some(old_mc) = mm_closure_md.borrow_mut().take() {
            let _ = doc_target
                .remove_event_listener_with_callback("mousemove", old_mc.as_ref().unchecked_ref());
        }
        if let Some(old_uc) = mu_closure_md.borrow_mut().take() {
            let _ = doc_target
                .remove_event_listener_with_callback("mouseup", old_uc.as_ref().unchecked_ref());
        }

        drag_md.set(local_value.get_untracked());
        *base_x_md.borrow_mut() = Some(ev.client_x() as f64);
        let _ = set_is_pending.try_set(true);

        let timeout = gloo_timers::callback::Timeout::new(ACTIVATION_DELAY_MS, move || {
            let _ = set_is_activated.try_set(true);
            let _ = set_is_pending.try_set(false);
            on_drag_start.run(());
        });
        *timeout_md.borrow_mut() = Some(timeout);

        let base_x_mm = base_x_md.clone();
        let base_x_mu = base_x_md.clone();
        let drag_mm = drag_md.clone();
        let mm_cleanup = mm_closure_md.clone();
        let mu_cleanup = mu_closure_md.clone();
        let doc_cleanup = doc_target.clone();
        let timeout_mu = timeout_md.clone();

        let track_ref_move = track_ref;
        let mc = Closure::wrap(Box::new(move |ev: web_sys::MouseEvent| {
            let current_x = ev.client_x() as f64;

            if !is_activated.get() {
                *base_x_mm.borrow_mut() = Some(current_x);
                return;
            }

            if let Some(el) = track_ref_move.get() {
                let base_opt = *base_x_mm.borrow();
                if let Some(base_x) = base_opt {
                    let rect = el.get_bounding_client_rect();
                    let delta_x = current_x - base_x;
                    let delta_ratio = delta_x / rect.width();
                    let raw = drag_mm.get();
                    let new_val = (raw + delta_ratio as f32).clamp(0.0, 1.0);
                    drag_mm.set(new_val);
                    let quantized = (new_val * 200.0).round() / 200.0;
                    let _ = local_value.try_set(quantized);
                    on_change.run(quantized);
                    *base_x_mm.borrow_mut() = Some(current_x);
                }
            }
        }) as Box<dyn FnMut(web_sys::MouseEvent)>);

        let _ =
            doc_target.add_event_listener_with_callback("mousemove", mc.as_ref().unchecked_ref());
        *mm_closure_md.borrow_mut() = Some(mc);

        let uc = Closure::wrap(Box::new(move |_ev: web_sys::MouseEvent| {
            *timeout_mu.borrow_mut() = None;
            let was_active = is_activated.get();
            let _ = set_is_pending.try_set(false);
            let _ = set_is_activated.try_set(false);
            *base_x_mu.borrow_mut() = None;

            if let Some(mc) = mm_cleanup.borrow_mut().take() {
                let _ = doc_cleanup
                    .remove_event_listener_with_callback("mousemove", mc.as_ref().unchecked_ref());
            }
            mu_cleanup.borrow_mut().take();

            if was_active {
                on_drag_end.run(());
            }
        }) as Box<dyn FnMut(web_sys::MouseEvent)>);

        let _ = doc_target.add_event_listener_with_callback("mouseup", uc.as_ref().unchecked_ref());
        *mu_closure_md.borrow_mut() = Some(uc);
    };

    // Desktop double-click to reset to default
    let handle_dblclick = move |_ev: web_sys::MouseEvent| {
        if let Some(def) = default_value {
            let _ = local_value.try_set(def);
            on_change.run(def);
        }
    };

    let pct = move || (local_value.get() * 100.0).clamp(0.0, 100.0);

    view! {
        <div
            node_ref=track_ref
            class=move || {
                let mut classes = vec!["eq-slider-track"];
                if !css_class.is_empty() { classes.push(css_class); }
                if is_pending.get() { classes.push("activating"); }
                if is_activated.get() { classes.push("active"); }
                classes.join(" ")
            }
            on:touchstart=handle_touchstart
            on:touchmove=handle_touchmove
            on:touchend=handle_touchend
            on:touchcancel=handle_touchcancel
            on:mousedown=handle_mousedown
            on:dblclick=handle_dblclick
        >
            <div class="eq-slider-fill" style=move || format!("width:{}%", pct()) />
            <div class="eq-slider-thumb" style=move || format!("left:{}%", pct()) />
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_order() {
        assert_eq!(display_order("highpass"), 0);
        assert_eq!(display_order("lowshelf"), 1);
        assert_eq!(display_order("band"), 2);
        assert_eq!(display_order("highshelf"), 4);
        assert_eq!(display_order("lowpass"), 5);
    }

    #[test]
    fn test_gain_to_y_center() {
        let height = 300.0;
        let y = gain_to_y(0.0, height);
        assert!((y - height / 2.0).abs() < 0.01);
    }

    #[test]
    fn test_gain_to_y_extremes() {
        let height = 300.0;
        assert!((gain_to_y(12.0, height) - 0.0).abs() < 0.01);
        assert!((gain_to_y(-12.0, height) - height).abs() < 0.01);
    }

    #[test]
    fn test_biquad_peaking_at_center() {
        let band = EqBandState {
            band_type: "band".to_string(),
            freq_hz: 1000.0,
            gain_db: 6.0,
            bw: 1.0,
            freq_norm: 0.5,
            gain_norm: 0.3,
            bw_norm: 0.25,
            enabled: true,
        };
        let gain_at_center = compute_band_gain(1000.0, &band);
        // At center frequency, gain should approximately equal band gain_db
        assert!(
            (gain_at_center - 6.0).abs() < 0.5,
            "Expected ~6dB at center, got {}",
            gain_at_center
        );
    }

    #[test]
    fn test_biquad_peaking_far_from_center() {
        let band = EqBandState {
            band_type: "band".to_string(),
            freq_hz: 1000.0,
            gain_db: 12.0,
            bw: 1.0,
            freq_norm: 0.5,
            gain_norm: 0.3,
            bw_norm: 0.25,
            enabled: true,
        };
        // At 10x the center frequency, gain should be near 0 dB
        let gain_far = compute_band_gain(10000.0, &band);
        assert!(
            gain_far.abs() < 1.0,
            "Expected ~0dB far from center, got {}",
            gain_far
        );
    }

    #[test]
    fn test_biquad_hpf_rolloff() {
        let band = EqBandState {
            band_type: "highpass".to_string(),
            freq_hz: 100.0,
            gain_db: 0.0,
            bw: 2.0,
            freq_norm: 0.14,
            gain_norm: 0.25,
            bw_norm: 0.5,
            enabled: true,
        };
        // Well above cutoff: should be ~0 dB
        let gain_above = compute_band_gain(1000.0, &band);
        assert!(
            gain_above.abs() < 0.5,
            "Expected ~0dB above HPF cutoff, got {}",
            gain_above
        );
        // Well below cutoff: should be significantly negative
        let gain_below = compute_band_gain(10.0, &band);
        assert!(
            gain_below < -6.0,
            "Expected strong rolloff below HPF cutoff, got {}",
            gain_below
        );
    }

    #[test]
    fn test_biquad_low_shelf() {
        let band = EqBandState {
            band_type: "lowshelf".to_string(),
            freq_hz: 200.0,
            gain_db: 6.0,
            bw: 0.8,
            freq_norm: 0.2,
            gain_norm: 0.3,
            bw_norm: 0.2,
            enabled: true,
        };
        // Well below shelf: should be near shelf gain
        let gain_low = compute_band_gain(20.0, &band);
        assert!(
            (gain_low - 6.0).abs() < 1.5,
            "Expected ~6dB below low shelf, got {}",
            gain_low
        );
        // Well above shelf: should be near 0 dB
        let gain_high = compute_band_gain(5000.0, &band);
        assert!(
            gain_high.abs() < 0.5,
            "Expected ~0dB above low shelf, got {}",
            gain_high
        );
    }

    #[test]
    fn test_disabled_band_does_not_affect_curve() {
        let disabled_hpf = EqBandState {
            band_type: "highpass".to_string(),
            freq_hz: 100.0,
            gain_db: 0.0,
            bw: 2.0,
            freq_norm: 0.14,
            gain_norm: 0.25,
            bw_norm: 0.5,
            enabled: false,
        };
        let bands = vec![disabled_hpf];
        let path = generate_curve_path(&bands, 400.0, 300.0);

        // A flat curve at 0dB should be a horizontal line at height/2
        let flat_path = generate_curve_path(&[], 400.0, 300.0);
        assert_eq!(
            path, flat_path,
            "Disabled HPF should produce flat curve identical to no bands"
        );
    }

    #[test]
    fn test_enabled_band_affects_curve() {
        let enabled_hpf = EqBandState {
            band_type: "highpass".to_string(),
            freq_hz: 100.0,
            gain_db: 0.0,
            bw: 2.0,
            freq_norm: 0.14,
            gain_norm: 0.25,
            bw_norm: 0.5,
            enabled: true,
        };
        let bands = vec![enabled_hpf];
        let path = generate_curve_path(&bands, 400.0, 300.0);
        let flat_path = generate_curve_path(&[], 400.0, 300.0);
        assert_ne!(
            path, flat_path,
            "Enabled HPF should produce a different curve than flat"
        );
    }

    /// Helper: build an EqBandState with sensible defaults.
    fn band(ty: &str, freq_hz: f32, gain_db: f32, bw: f32) -> EqBandState {
        EqBandState {
            band_type: ty.to_string(),
            freq_hz,
            gain_db,
            bw,
            freq_norm: 0.0,
            gain_norm: 0.0,
            bw_norm: 0.0,
            enabled: true,
        }
    }

    /// Regression guard: peaking filter's magnitude at its center frequency
    /// must equal the stated gain. This already passes on v1.146.0 — we
    /// commit it so future changes can't break it.
    #[test]
    fn test_peaking_exact_at_center_frequency() {
        for &gain in &[-12.0_f32, -6.0, -3.0, 0.0, 3.0, 6.0, 12.0] {
            for &bw in &[0.5_f32, 1.0, 2.0] {
                let b = band("band", 1000.0, gain, bw);
                let g = compute_band_gain(1000.0, &b);
                assert!(
                    (g - gain).abs() < 0.05,
                    "peaking {gain} dB bw={bw}: got {g} at center freq"
                );
            }
        }
    }

    /// Lowshelf passband (well below corner) must equal stated gain.
    #[test]
    fn test_lowshelf_passband_equals_gain() {
        for &gain in &[-6.0_f32, -3.0, 3.0, 6.0] {
            for &bw in &[0.5_f32, 1.0] {
                let b = band("lowshelf", 500.0, gain, bw);
                // Evaluate far below corner — should be full shelf gain.
                let g = compute_band_gain(20.0, &b);
                assert!(
                    (g - gain).abs() < 0.3,
                    "lowshelf 500 Hz {gain} dB bw={bw}: passband at 20 Hz = {g}"
                );
            }
        }
    }

    /// Highshelf passband (well above corner) must equal stated gain.
    #[test]
    fn test_highshelf_passband_equals_gain() {
        for &gain in &[-6.0_f32, -3.0, 3.0, 6.0] {
            for &bw in &[0.5_f32, 1.0] {
                let b = band("highshelf", 5000.0, gain, bw);
                // Evaluate far above corner — should be full shelf gain.
                let g = compute_band_gain(20000.0, &b);
                assert!(
                    (g - gain).abs() < 0.3,
                    "highshelf 5 kHz {gain} dB bw={bw}: passband at 20 kHz = {g}"
                );
            }
        }
    }

    /// Shelf response must not overshoot (positive gain) or undershoot
    /// (negative gain) its passband — the curve must stay within the band's
    /// [gain, 0] envelope across the entire 20 Hz .. 20 kHz range.
    /// Covers BOTH positive and negative shelf gains (symmetric regression).
    #[test]
    fn test_shelf_no_overshoot_or_undershoot() {
        // (band_type, corner_hz, gain_db)
        let cases = &[
            ("lowshelf", 500.0_f32, 6.0_f32),
            ("lowshelf", 500.0, -6.0),
            ("highshelf", 5000.0, 6.0),
            ("highshelf", 5000.0, -6.0),
        ];
        for &(ty, corner, gain) in cases {
            let b = band(ty, corner, gain, 0.5);
            let mut max_gain = f32::NEG_INFINITY;
            let mut min_gain = f32::INFINITY;
            // Log-sweep 20 Hz .. 20 kHz in 400 steps.
            for i in 0..=400 {
                let t = i as f32 / 400.0;
                let freq = 20.0 * (1000.0_f32).powf(t);
                let g = compute_band_gain(freq, &b);
                if g > max_gain {
                    max_gain = g;
                }
                if g < min_gain {
                    min_gain = g;
                }
            }
            // 0.3 dB of slop covers the smooth transition region.
            let (lo, hi) = if gain >= 0.0 {
                (-0.3, gain + 0.3)
            } else {
                (gain - 0.3, 0.3)
            };
            assert!(
                max_gain <= hi,
                "{ty} {gain} dB bw=0.5: max={max_gain} > {hi}"
            );
            assert!(
                min_gain >= lo,
                "{ty} {gain} dB bw=0.5: min={min_gain} < {lo}"
            );
        }
    }

    /// reaperiem#167 regression: shelf immediately adjacent to peaking band does not
    /// ring upward into the peaking band's region. The constants below are
    /// a snapshot of MEMBER6 mic's EQ as of 2026-04-12 — if the engineer
    /// changes MEMBER6's EQ in REAPER, these values drift but the test still
    /// upholds the invariant (upper bound only). For a synthetic standalone
    /// test of the same invariant, see `test_shelf_no_overshoot_or_undershoot`.
    ///
    /// With the pre-fix peaking-Q shelf math, summing these four bands
    /// produced a peak of +5.73 dB at 640 Hz (+1.43 dB over b2's stated
    /// +4.3 dB). With the fix the peak is ≤ +4.6 dB.
    #[test]
    fn test_shelf_adjacent_to_peaking_does_not_ring_167() {
        let bands = vec![
            // b0 highpass disabled — skip
            band("lowshelf", 510.8, -2.1, 0.56),
            band("band", 640.6, 4.3, 1.14),
            band("band", 1473.3, -1.5, 0.92),
            band("highshelf", 4448.1, 3.6, 0.80),
        ];
        // Sum responses at 640 Hz — must not overshoot b2's stated +4.3 dB.
        // Real sum is lower than 4.3 because adjacent bands bleed negative
        // contributions (lowshelf past corner ~-0.31 dB, peaking at 1473 Hz
        // bleeding down). The reaperiem#167 bug made this sum ~+5.7 dB (shelf ringing
        // adding instead of settling). Fix invariant: sum must stay ≤ +4.6 dB
        // at the peaking band's center frequency.
        let mut total = 0.0_f32;
        for b in &bands {
            total += compute_band_gain(640.6, b);
        }
        assert!(
            total <= 4.6,
            "fixture sum at 640 Hz = {total} dB, expected ≤ 4.6 (no overshoot)"
        );
        // And the whole curve max (scanned log-sweep) must not exceed +4.6 dB.
        let mut curve_max = f32::NEG_INFINITY;
        for i in 0..=400 {
            let t = i as f32 / 400.0;
            let freq = 20.0 * (1000.0_f32).powf(t);
            let mut sum = 0.0;
            for b in &bands {
                sum += compute_band_gain(freq, b);
            }
            if sum > curve_max {
                curve_max = sum;
            }
        }
        assert!(
            curve_max <= 4.6,
            "fixture curve max = {curve_max} dB, expected ≤ 4.6 (no shelf ringing)"
        );
    }

    #[test]
    fn test_snap_db_rounds_to_tenth() {
        assert!((snap_db(2.04) - 2.0).abs() < f32::EPSILON);
        // 2.05 isn't exactly representable in f32 (≈2.0499998...), so the snap
        // result lands ≈2.1 within ~1e-7 but not within f32::EPSILON. Use 1e-3
        // tolerance for any half-up boundary case.
        assert!((snap_db(2.05) - 2.1).abs() < 0.001);
        assert!((snap_db(-3.46) - -3.5).abs() < 0.001);
        assert!((snap_db(0.0) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_snap_hz_below_1k_rounds_to_integer() {
        assert!((snap_hz(322.4) - 322.0).abs() < f32::EPSILON);
        assert!((snap_hz(322.6) - 323.0).abs() < f32::EPSILON);
        assert!((snap_hz(20.0) - 20.0).abs() < f32::EPSILON);
        assert!((snap_hz(999.4) - 999.0).abs() < f32::EPSILON);
        // Boundary at 1 kHz: half-up still uses integer-Hz branch when input < 1000.
        assert!((snap_hz(999.6) - 1000.0).abs() < 0.001);
        // Exactly 1000 falls into the >=1000 branch, rounds to 1000 in 100-Hz space.
        assert!((snap_hz(1000.0) - 1000.0).abs() < 0.001);
    }

    #[test]
    fn test_snap_hz_above_1k_rounds_to_hundred() {
        assert!((snap_hz(1234.0) - 1200.0).abs() < f32::EPSILON);
        assert!((snap_hz(1250.0) - 1300.0).abs() < f32::EPSILON); // half-up
        assert!((snap_hz(10049.0) - 10000.0).abs() < f32::EPSILON);
        assert!((snap_hz(10050.0) - 10100.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_snap_oct_rounds_to_hundredth() {
        assert!((snap_oct(1.184) - 1.18).abs() < 0.001);
        // 1.125 is exact in f32, so it is a true midpoint: half away from zero.
        assert!((snap_oct(1.125) - 1.13).abs() < 0.001);
        // 1.185 is not: 1.185_f32 ≈ 1.18499994, below the midpoint.
        assert!((snap_oct(1.185) - 1.18).abs() < 0.001);
        assert!((snap_oct(2.005) - 2.01).abs() < 0.001);
        assert!((snap_oct(0.01) - 0.01).abs() < f32::EPSILON);
    }
}
