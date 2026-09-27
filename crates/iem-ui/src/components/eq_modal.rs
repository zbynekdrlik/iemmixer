//! Full-screen parametric EQ modal with SVG frequency response curve
//!
//! Loads the EQ of a target on demand (`GetEqParams{target}`: a channel id,
//! the page's mix or its stems group), displays draggable band points on a
//! log-frequency curve, and sends `SetEqBand{target, band, param, value}` in
//! the engine's units (freq_hz, gain_db, bw_oct, enabled) on slider changes.
//! The curve is the engine's own response (`iem_dsp::eq::response_db`).
//!
//! EqSlider keeps local reactive state so parent re-renders don't destroy
//! active drag gestures; each band card owns its own signals and on_change
//! only sends on the WebSocket. Slider values snap to the label's display
//! granularity (`snap_db` / `snap_hz` / `snap_oct`), and drag-end always
//! sends the final value past the 50 ms throttle, so sent = displayed =
//! stored = reopened.

use iem_dsp::eq::{Band, BandKind, EqParams, response_db};
use leptos::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::prelude::*;

/// Activation delay in milliseconds (matches fader.rs pattern)
const ACTIVATION_DELAY_MS: u32 = 150;

/// Maximum time between taps for double-tap detection (ms)
const DOUBLE_TAP_MS: f64 = 300.0;

/// The engine's sample rate (the curve is its response at this rate)
const SAMPLE_RATE: f64 = 96_000.0;

/// UI-fixed visual range for the freq slider (Hz), log scale.
const UI_FREQ_MIN_HZ: f32 = 20.0;
const UI_FREQ_MAX_HZ: f32 = 24000.0;

/// UI-fixed visual range for the bw slider (octaves).
const UI_BW_MIN_OCT: f32 = 0.01;
const UI_BW_MAX_OCT: f32 = 4.00;

/// UI-fixed visual range for the gain slider (dB). The engine's range goes down
/// to its off value (a notch), but musical gains live in ±12 — UI clamps
/// display to this range so the slider isn't squashed into the far-right 10%.
const UI_GAIN_MIN_DB: f32 = -12.0;
const UI_GAIN_MAX_DB: f32 = 12.0;

/// EQ band data (mirrors iem_core::EqBand for frontend use)
#[derive(Debug, Clone, PartialEq)]
pub struct EqBandState {
    pub band_type: String,
    pub freq_hz: f32,
    pub gain_db: f32,
    /// Bandwidth in octaves
    pub bw: f32,
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

/// The engine's kind of a band the server names `band_type`.
fn kind_of(band_type: &str) -> BandKind {
    match band_type {
        "highpass" => BandKind::HighPass,
        "lowshelf" => BandKind::LowShelf,
        "highshelf" => BandKind::HighShelf,
        _ => BandKind::Peak,
    }
}

/// Whether a gain change switches the band on, as the server does (FG-2,
/// `view::apply_band`): every band with a gain; the high-pass has none.
fn gain_switches_on(band_type: &str) -> bool {
    kind_of(band_type) != BandKind::HighPass
}

/// The engine's parameters of the displayed bands (at most five, in the
/// server's order; missing bands are off). A gain at or below the engine's off
/// value is a notch (linear gain 0).
fn engine_params(bands: &[EqBandState]) -> EqParams {
    let mut params = EqParams::standard_flat();
    for (slot, b) in params.bands.iter_mut().zip(bands) {
        *slot = Band {
            kind: kind_of(&b.band_type),
            enabled: b.enabled,
            freq_hz: f64::from(b.freq_hz),
            gain_lin: if b.gain_db <= -150.0 {
                0.0
            } else {
                10f64.powf(f64::from(b.gain_db) / 20.0)
            },
            bw_oct: f64::from(b.bw),
        };
    }
    params
}

/// The response of `bands` at `freq` in dB, as the engine applies it.
#[cfg(test)]
fn curve_db(bands: &[EqBandState], freq: f32) -> f32 {
    response_db(&engine_params(bands), SAMPLE_RATE, f64::from(freq)) as f32
}

/// Generate the frequency response curve path as SVG "d" attribute.
fn generate_curve_path(bands: &[EqBandState], width: f32, height: f32) -> String {
    let num_points = 200;
    let log_min = 20.0_f32.ln();
    let log_max = 20000.0_f32.ln();
    let params = engine_params(bands);
    let mut path = String::with_capacity(num_points * 20);

    for i in 0..=num_points {
        let x = (i as f32 / num_points as f32) * width;
        let log_freq = log_min + (i as f32 / num_points as f32) * (log_max - log_min);
        let freq = log_freq.exp();
        let total_gain = response_db(&params, SAMPLE_RATE, f64::from(freq)) as f32;
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
/// displayed text (`{:.1} dB`) and the underlying float sent to the engine.
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
    /// The engine's band index (0-4) — used for API calls
    band_idx: u8,
    band_type: String,
    /// The band's values (loaded from the server)
    freq_hz: RwSignal<f32>,
    gain_db: RwSignal<f32>,
    bw_oct: RwSignal<f32>,
    /// Whether this band is enabled
    enabled: RwSignal<bool>,
}

/// Full-screen EQ modal component
#[component]
pub fn EQModal(
    /// The EQ target (a channel id, the page's mix or its stems group)
    target: String,
    /// Track name for the header
    track_name: String,
    /// EQ bands data from server (synced to local signals when not dragging)
    bands: ReadSignal<Vec<EqBandState>>,
    /// Whether EQ data is loading
    loading: ReadSignal<bool>,
    /// Callback when a band parameter changes (band_index, param_name, value)
    on_param_change: Callback<(u8, String, f32)>,
    /// Callback to close the modal
    on_close: Callback<()>,
) -> impl IntoView {
    let _ = target;
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
            // Build (engine band index, band) pairs then sort for display
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
                .map(|(idx, b)| BandLocalState {
                    band_idx: *idx as u8,
                    band_type: b.band_type.clone(),
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
                let ri = local.band_idx as usize;
                if let Some(parent_band) = parent.get(ri) {
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
                    <div class="eq-no-eq">"No EQ on this channel"</div>
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
                                    // In the engine's band order (the display order differs).
                                    let mut by_idx: Vec<(u8, EqBandState)> = locals.iter().map(|l| {
                                        (l.band_idx, EqBandState {
                                            band_type: l.band_type.clone(),
                                            freq_hz: l.freq_hz.get_untracked(),
                                            gain_db: l.gain_db.get_untracked(),
                                            bw: l.bw_oct.get_untracked(),
                                            enabled: l.enabled.get_untracked(),
                                        })
                                    }).collect();
                                    by_idx.sort_by_key(|(i, _)| *i);
                                    let states: Vec<EqBandState> = by_idx.into_iter().map(|(_, b)| b).collect();
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
                                let band_idx = local.band_idx;
                                // Store band_idx in Leptos StoredValue for robust access from
                                // slider callbacks (where DOM data-attribute approach isn't possible).
                                let band_idx_sv = StoredValue::new(band_idx);
                                let band_type = local.band_type.clone();
                                let band_type_reset = band_type.clone();
                                let color = band_color(&band_type).to_string();
                                let gain_enables = gain_switches_on(&band_type);

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
                                                    // Reset gain to 0dB via new gain_db protocol; a gain
                                                    // change switches a band with a gain on in the server
                                                    // (ReaEQ's behaviour under the predecessor, FG-2), so
                                                    // show it on. The high-pass has no gain: its switch
                                                    // stays as it is.
                                                    let _ = gain_db_sig.try_set(0.0);
                                                    if gain_enables {
                                                        let _ = enabled_sig.try_set(true);
                                                    }
                                                    on_param_change.run((idx, "gain_db".to_string(), 0.0));
                                                    // Reset freq to per-band default Hz (reaperiem#196)
                                                    let default_freq_hz: f32 = match band_type_reset.as_str() {
                                                        "highpass" => 80.0,
                                                        "lowshelf" => 200.0,
                                                        "highshelf" => 8000.0,
                                                        "lowpass" => 12000.0,
                                                        _ => {
                                                            // Parametric bands: by band index
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
                                                    let _ = curve_trigger.try_update(|n| *n += 1);
                                                }
                                            >
                                                "\u{21BA}"
                                            </button>
                                        </div>

                                        // Frequency slider: derives position from the engine's freq_hz
                                        // mapped onto a fixed UI log scale (20 Hz – 24 kHz). Single source
                                        // of truth = the engine (reaperiem#196).
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
                                                    // can drop the last position. Without this, the engine
                                                    // stores a value from up to 50 ms before drag-end
                                                    // and reopen reads that → drift (reaperiem#196).
                                                    // Always send (no last_send_* check) — extra send is
                                                    // cheaper than missing the final position; a duplicate
                                                    // write of the same value changes nothing.
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

                                        // Gain slider: derives position from the engine's gain_db
                                        // clamped to a fixed ±12 dB UI range for sensible UX.
                                        // (the engine's range reaches down to its off value, which would squash
                                        // all musical gains into the far-right 10% of slider travel.)
                                        <div class="eq-param-row">
                                            <label class="eq-param-label">"Gain"</label>
                                            <EqSlider
                                                value=Signal::derive(move || {
                                                    // Single source of truth — the engine's dB.
                                                    // Slider VISUAL range fixed at ±12 dB for UX
                                                    // (the engine's range reaches down to its off value, which would
                                                    // squash all musical gains into the far-right 10%
                                                    // of slider travel). Out-of-range values clamp.
                                                    let db = gain_db_sig.get().clamp(UI_GAIN_MIN_DB, UI_GAIN_MAX_DB);
                                                    (db - UI_GAIN_MIN_DB) / (UI_GAIN_MAX_DB - UI_GAIN_MIN_DB)
                                                })
                                                on_change=Callback::new(move |v: f32| {
                                                    // Project slider position 0-1 to dB, then snap to UI
                                                    // display granularity (0.1 dB). Without snapping, the
                                                    // displayed `{:.1}` rounds e.g. 2.04 → "+2.0 dB" while
                                                    // the engine receives 2.04, stores 2.04, and on
                                                    // reopen the display may round differently → drift.
                                                    let db = snap_db(UI_GAIN_MIN_DB + v * (UI_GAIN_MAX_DB - UI_GAIN_MIN_DB));
                                                    let now = js_sys::Date::now();
                                                    if now - last_send_gain.get_untracked() > 50.0 {
                                                        let _ = last_send_gain.try_set(now);
                                                        on_param_change.run((band_idx_sv.get_value(), "gain_db".to_string(), db));
                                                    }
                                                    let _ = gain_db_sig.try_set(db);
                                                    // The server switches a band with a gain on with
                                                    // any gain change (FG-2); the toggle shows it at once.
                                                    if gain_enables {
                                                        let _ = enabled_sig.try_set(true);
                                                    }
                                                    let _ = curve_trigger.try_update(|n| *n += 1);
                                                })
                                                on_drag_start=Callback::new(move |_: ()| {
                                                    let _ = any_dragging.try_set(true);
                                                })
                                                on_drag_end=Callback::new(move |_: ()| {
                                                    let _ = any_dragging.try_set(false);
                                                    // Force-flush final value past the 50 ms throttle (reaperiem#196).
                                                    // Always send (no last_send_* check) — extra send is
                                                    // cheaper than missing the final position; a duplicate
                                                    // write of the same value changes nothing.
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

                                        // Bandwidth/Q slider: derives position from the engine's bw_oct
                                        // mapped onto a fixed UI linear scale (0.01 – 4.00 oct). Single
                                        // source of truth = the engine (reaperiem#196).
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
                                                    // cheaper than missing the final position; a duplicate
                                                    // write of the same value changes nothing.
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
            // Off the document before it is dropped (see fader.rs).
            if let Some(uc) = mu_cleanup.borrow_mut().take() {
                let _ = doc_cleanup
                    .remove_event_listener_with_callback("mouseup", uc.as_ref().unchecked_ref());
            }

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
    fn a_gain_change_switches_on_every_band_but_the_high_pass() {
        // The server's rule (view::apply_band, FG-2), shown by the toggle.
        assert!(!gain_switches_on("highpass"));
        for ty in ["lowshelf", "band", "highshelf"] {
            assert!(gain_switches_on(ty), "{ty}");
        }
    }

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

    /// Helper: build an enabled EqBandState.
    fn band(ty: &str, freq_hz: f32, gain_db: f32, bw: f32) -> EqBandState {
        EqBandState {
            band_type: ty.to_string(),
            freq_hz,
            gain_db,
            bw,
            enabled: true,
        }
    }

    /// The drawn curve in dB: the points of `generate_curve_path`'s SVG path
    /// read back through the inverse of `gain_to_y`. At 2400 px high, the
    /// path's 0.1 px is 0.001 dB.
    fn drawn_db(bands: &[EqBandState]) -> Vec<f32> {
        const HEIGHT: f32 = 2400.0;
        let path = generate_curve_path(bands, 400.0, HEIGHT);
        let db: Vec<f32> = path
            .split(['M', 'L'])
            .filter(|p| !p.trim().is_empty())
            .map(|p| {
                let (_, y) = p.trim().split_once(',').unwrap();
                12.0 - y.parse::<f32>().unwrap() / HEIGHT * 24.0
            })
            .collect();
        assert_eq!(db.len(), 201, "{path}");
        db
    }

    /// The response over reaperiem's sweep: 401 log steps from 20 Hz to 20 kHz.
    fn swept_db(bands: &[EqBandState]) -> Vec<f32> {
        (0..=400)
            .map(|i| {
                let t = i as f32 / 400.0;
                curve_db(bands, 20.0 * 1000.0_f32.powf(t))
            })
            .collect()
    }

    fn max(v: &[f32]) -> f32 {
        v.iter().copied().fold(f32::NEG_INFINITY, f32::max)
    }

    fn min(v: &[f32]) -> f32 {
        v.iter().copied().fold(f32::INFINITY, f32::min)
    }

    /// The curve is the engine's response: a peak reads its gain at its centre
    /// within reaperiem's 0.05 dB (`test_peaking_exact_at_center_frequency`;
    /// the engine's design is exact at the centre).
    #[test]
    fn a_peak_reads_its_gain_at_its_centre() {
        for &gain in &[-12.0_f32, -6.0, -3.0, 0.0, 3.0, 6.0, 12.0] {
            for &bw in &[0.5_f32, 1.0, 2.0] {
                let g = curve_db(&[band("band", 1000.0, gain, bw)], 1000.0);
                assert!(
                    (g - gain).abs() < 0.05,
                    "peaking {gain} dB bw={bw}: got {g} at the centre"
                );
            }
        }
        // Far from the centre the peak fades out.
        let far = curve_db(&[band("band", 1000.0, 12.0, 1.0)], 10_000.0);
        assert!(far.abs() < 1.0, "{far}");
    }

    #[test]
    fn a_disabled_band_is_flat() {
        let mut hpf = band("highpass", 100.0, 0.0, 2.0);
        hpf.enabled = false;
        let mut peak = band("band", 1000.0, 6.0, 1.0);
        peak.enabled = false;
        assert_eq!(curve_db(&[hpf.clone(), peak.clone()], 1000.0), 0.0);
        assert_eq!(curve_db(&[hpf.clone(), peak], 30.0), 0.0);
        assert_eq!(
            generate_curve_path(&[hpf], 400.0, 300.0),
            generate_curve_path(&[], 400.0, 300.0),
            "a disabled HPF draws the flat curve"
        );
        let enabled = band("highpass", 100.0, 0.0, 2.0);
        assert_ne!(
            generate_curve_path(&[enabled], 400.0, 300.0),
            generate_curve_path(&[], 400.0, 300.0)
        );
    }

    #[test]
    fn a_high_pass_rolls_off_below_its_corner() {
        let hpf = [band("highpass", 100.0, 0.0, 2.0)];
        assert!(
            curve_db(&hpf, 1000.0).abs() < 0.5,
            "{}",
            curve_db(&hpf, 1000.0)
        );
        assert!(curve_db(&hpf, 10.0) < -6.0, "{}", curve_db(&hpf, 10.0));
    }

    /// reaperiem's grid (`test_lowshelf_passband_equals_gain`,
    /// `test_highshelf_passband_equals_gain`): ±3 and ±6 dB at bw 0.5 and 1.0
    /// reach their gain in the passband within 0.3 dB (worst offline: 0.037 dB,
    /// the 5 kHz high shelf at bw 0.5) and stay flat on the far side.
    #[test]
    fn shelves_reach_their_gain_in_the_passband() {
        for &gain in &[-6.0_f32, -3.0, 3.0, 6.0] {
            for &bw in &[0.5_f32, 1.0] {
                let low = [band("lowshelf", 500.0, gain, bw)];
                let g = curve_db(&low, 20.0);
                assert!(
                    (g - gain).abs() < 0.3,
                    "lowshelf 500 Hz {gain} dB bw={bw}: passband at 20 Hz = {g}"
                );
                let far = curve_db(&low, 15_000.0);
                assert!(
                    far.abs() < 0.3,
                    "lowshelf {gain} dB bw={bw}: {far} at 15 kHz"
                );
                let high = [band("highshelf", 5000.0, gain, bw)];
                let g = curve_db(&high, 20_000.0);
                assert!(
                    (g - gain).abs() < 0.3,
                    "highshelf 5 kHz {gain} dB bw={bw}: passband at 20 kHz = {g}"
                );
                let far = curve_db(&high, 30.0);
                assert!(
                    far.abs() < 0.3,
                    "highshelf {gain} dB bw={bw}: {far} at 30 Hz"
                );
            }
            // A 2 kHz high shelf at bw 1 as well.
            let high = [band("highshelf", 2000.0, gain, 1.0)];
            assert!((curve_db(&high, 20_000.0) - gain).abs() < 0.3);
            assert!(curve_db(&high, 30.0).abs() < 0.3);
        }
        // reaperiem `test_biquad_low_shelf`: 200 Hz, +6 dB, bw 0.8.
        let low = [band("lowshelf", 200.0, 6.0, 0.8)];
        let g = curve_db(&low, 20.0);
        assert!((g - 6.0).abs() < 1.5, "~6 dB below the low shelf, got {g}");
        let g = curve_db(&low, 5000.0);
        assert!(g.abs() < 0.5, "~0 dB above the low shelf, got {g}");
    }

    /// A shelf stays inside its [gain, 0] envelope from 20 Hz to 20 kHz with
    /// reaperiem's 0.3 dB of slop for the transition
    /// (`test_shelf_no_overshoot_or_undershoot`; the engine's bw 0.5 shelf,
    /// slope capped at 1.2, overshoots 0.046 dB), in the swept response and in
    /// the drawn curve, and reaches its gain.
    #[test]
    fn a_shelf_neither_overshoots_nor_undershoots() {
        for &(ty, corner, gain) in &[
            ("lowshelf", 500.0_f32, 6.0_f32),
            ("lowshelf", 500.0, -6.0),
            ("highshelf", 5000.0, 6.0),
            ("highshelf", 5000.0, -6.0),
        ] {
            let b = [band(ty, corner, gain, 0.5)];
            let (lo, hi) = if gain >= 0.0 {
                (-0.3, gain + 0.3)
            } else {
                (gain - 0.3, 0.3)
            };
            for (what, db) in [("swept", swept_db(&b)), ("drawn", drawn_db(&b))] {
                let (top, bottom) = (max(&db), min(&db));
                assert!(top <= hi, "{ty} {gain} dB bw=0.5 {what}: max={top} > {hi}");
                assert!(
                    bottom >= lo,
                    "{ty} {gain} dB bw=0.5 {what}: min={bottom} < {lo}"
                );
                let reached = if gain >= 0.0 { top } else { bottom };
                assert!(
                    (reached - gain).abs() < 0.3,
                    "{ty} {gain} dB bw=0.5 {what}: reaches {reached}"
                );
            }
        }
    }

    /// reaperiem#167: a shelf next to a peak must not ring upward into the
    /// peak's region. The fixture is the predecessor's regression EQ (a
    /// disabled high-pass, a low shelf, two peaks, a high shelf); its first
    /// curve maths, a peaking Q on the shelves, summed it to +5.73 dB at
    /// 640 Hz, over the +4.3 dB peak. The bound stays reaperiem's +4.6 dB at
    /// the peak's centre, over the swept response and over the drawn curve
    /// (offline: 3.63 dB).
    #[test]
    fn a_shelf_next_to_a_peak_does_not_ring_167() {
        let mut hpf = band("highpass", 80.0, 0.0, 2.0);
        hpf.enabled = false;
        let bands = [
            hpf,
            band("lowshelf", 510.8, -2.1, 0.56),
            band("band", 640.6, 4.3, 1.14),
            band("band", 1473.3, -1.5, 0.92),
            band("highshelf", 4448.1, 3.6, 0.80),
        ];
        let at_peak = curve_db(&bands, 640.6);
        assert!(
            at_peak <= 4.6,
            "fixture at 640 Hz = {at_peak} dB, expected ≤ 4.6 (no overshoot)"
        );
        for (what, db) in [("swept", swept_db(&bands)), ("drawn", drawn_db(&bands))] {
            let top = max(&db);
            assert!(
                top <= 4.6,
                "fixture {what} max = {top} dB, expected ≤ 4.6 (no shelf ringing)"
            );
            // The bands are heard: the peak lifts the curve well above flat.
            assert!(top > 3.0, "fixture {what} max = {top} dB");
        }
    }

    #[test]
    fn the_engine_off_gain_is_a_notch() {
        let params = engine_params(&[band("band", 1000.0, -150.0, 1.0)]);
        assert_eq!(params.bands[0].gain_lin, 0.0);
        assert!(curve_db(&[band("band", 1000.0, -150.0, 1.0)], 1000.0) < -60.0);
        // Bands map onto the engine's five in order; the rest stay off.
        let two = engine_params(&[
            band("highpass", 80.0, 0.0, 2.0),
            band("lowshelf", 200.0, 3.0, 2.0),
        ]);
        assert_eq!(two.bands[0].kind, BandKind::HighPass);
        assert_eq!(two.bands[1].kind, BandKind::LowShelf);
        assert!((two.bands[1].gain_lin - 10f64.powf(3.0 / 20.0)).abs() < 1e-12);
        assert!(!two.bands[2].enabled && !two.bands[4].enabled);
        let hs = engine_params(&[band("highshelf", 8000.0, 0.0, 2.0)]);
        assert_eq!(hs.bands[0].kind, BandKind::HighShelf);
        assert_eq!(hs.bands[0].freq_hz, 8000.0);
        assert_eq!(hs.bands[0].bw_oct, 2.0);
        assert!(hs.bands[0].enabled);
    }

    /// The engine has four band kinds and the server names only those
    /// ("highpass", "lowshelf", "band", "highshelf"). Any other type, such as
    /// the predecessor's "lowpass" and "notch", is drawn as a peak with its
    /// values, never as a low-pass or a notch (the importer refuses both, so
    /// none reaches the page; iemmixer#25 §6).
    #[test]
    fn other_band_types_draw_as_peaks() {
        let peak = engine_params(&[band("band", 5000.0, 6.0, 0.5)]).bands[0];
        assert_eq!(peak.kind, BandKind::Peak);
        for ty in ["lowpass", "notch", "bandpass", ""] {
            let p = engine_params(&[band(ty, 5000.0, 6.0, 0.5)]).bands[0];
            assert_eq!(p, peak, "{ty:?}");
            let b = [band(ty, 5000.0, 6.0, 0.5)];
            // A low-pass would cut 20 kHz, a notch would cut its centre.
            assert!((curve_db(&b, 5000.0) - 6.0).abs() < 0.05, "{ty:?}");
            assert!(curve_db(&b, 20_000.0).abs() < 1.0, "{ty:?}");
        }
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
