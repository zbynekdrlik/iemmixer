//! Lifecycle helpers: panic hook, WebSocket watchdog staleness check, reconnect backoff.
//!
//! Pure helpers (`backoff_delay_ms`, `is_stale`, `build_overlay_html`, and the
//! small string helpers) are unit-tested at module root. DOM- and network-
//! touching functions (`render_panic_overlay`, `post_panic_report`, the WS
//! watchdog closure in `mixer.rs`) are thin wrappers over web-sys APIs and
//! are verified end-to-end by the post-deploy Playwright test.

/// Interval (ms) between WebSocket watchdog ticks.
///
/// The watchdog fires this often and checks whether the socket has received
/// any frame in the last `WS_STALENESS_THRESHOLD_MS` milliseconds. Shared
/// from here so `mixer.rs` does not carry a magic number.
pub const WS_WATCHDOG_INTERVAL_MS: u32 = 5_000;

/// A socket is considered stale — and force-closed — if no frame has arrived
/// within this many milliseconds.
///
/// Chosen conservatively: the server poller broadcasts meter updates every
/// 150 ms, so missing 200 consecutive broadcasts (30 s) is unambiguously a
/// zombie socket rather than a brief network blip.
pub const WS_STALENESS_THRESHOLD_MS: f64 = 30_000.0;

/// Maximum number of characters from a panic message to show in the reload
/// overlay. Longer messages are truncated with an ellipsis to keep the
/// overlay legible on phone-sized viewports.
pub const PANIC_MESSAGE_MAX_DISPLAY_CHARS: usize = 200;

/// Return the reconnect delay (in ms) for the given attempt number.
///
/// Schedule: 2s, 4s, 8s, 15s, then 30s forever. Matches the design in
/// docs/superpowers/specs/2026-04-11-pwa-self-healing-design.md.
pub fn backoff_delay_ms(attempt: u32) -> u32 {
    match attempt {
        0 => 2_000,
        1 => 4_000,
        2 => 8_000,
        3 => 15_000,
        _ => 30_000,
    }
}

/// Return true if the time since `last_frame_ms` exceeds `threshold_ms`.
///
/// `now_ms` and `last_frame_ms` are millisecond timestamps (e.g. from
/// `js_sys::Date::now()`). Used by the WS watchdog to detect zombie sockets.
pub fn is_stale(last_frame_ms: f64, now_ms: f64, threshold_ms: f64) -> bool {
    now_ms - last_frame_ms > threshold_ms
}

// ---------------------------------------------------------------------------
// Panic hook — thin DOM/network wrappers over the pure helpers above.
// ---------------------------------------------------------------------------

use wasm_bindgen::JsCast;

/// Install a custom panic hook that:
/// 1. Logs the panic to the browser console (via `console_error_panic_hook`).
/// 2. Renders a hardcoded red overlay into `document.body` with a "Reload" button.
/// 3. POSTs a `ClientErrorReport` to `/api/client-error` (fire-and-forget).
///
/// Replaces `console_error_panic_hook::set_once()`. Must be called BEFORE
/// `leptos::mount::mount_to_body` so any panic during mount is captured.
pub fn install_panic_hook() {
    // Keep the nice console-side formatting from console_error_panic_hook.
    console_error_panic_hook::set_once();

    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Delegate to the previous hook first so the console still gets a
        // structured backtrace. If that panics, we swallow it.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            prev(info);
        }));

        // Best-effort: render the overlay and POST diagnostics. Both are
        // wrapped in catch_unwind so a failure in one doesn't block the other
        // and nothing can re-panic inside the hook.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            render_panic_overlay(info);
        }));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            post_panic_report(info);
        }));
    }));
}

/// Build the red reload overlay HTML for a given panic summary and version.
///
/// Pure function — no DOM access, no globals. Extracted from
/// `render_panic_overlay` so it can be unit-tested. The `summary` and
/// `version` strings are HTML-escaped before interpolation so that a panic
/// payload like `<script>alert(1)</script>` becomes visible text in the
/// overlay instead of injected script.
fn build_overlay_html(summary: &str, version: &str) -> String {
    format!(
        r#"<div id="iem-panic-overlay" style="
            position:fixed;inset:0;z-index:2147483647;
            background:rgba(20,0,0,0.95);color:#fff;
            font-family:system-ui,-apple-system,sans-serif;
            display:flex;flex-direction:column;
            align-items:center;justify-content:center;
            padding:24px;text-align:center;">
            <h1 style="color:#ff4444;margin:0 0 16px 0;font-size:22px;">
                IEM Mixer encountered an error
            </h1>
            <p style="opacity:0.85;max-width:480px;margin:0 0 24px 0;
                font-family:monospace;font-size:13px;word-break:break-word;">
                {}
            </p>
            <button id="iem-panic-reload" style="
                padding:14px 32px;font-size:16px;
                background:#ff4444;color:#fff;border:0;border-radius:8px;
                cursor:pointer;">
                Reload
            </button>
            <p style="opacity:0.5;font-size:11px;margin-top:16px;">{}</p>
        </div>"#,
        html_escape(summary),
        html_escape(version),
    )
}

fn render_panic_overlay(info: &std::panic::PanicHookInfo<'_>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let Some(body) = document.body() else { return };

    let message = format_panic_message(info);
    let summary = truncate_for_display(&message, PANIC_MESSAGE_MAX_DISPLAY_CHARS);
    let version = iem_core::version_label();
    let overlay_html = build_overlay_html(&summary, &version);

    body.set_inner_html(&overlay_html);

    // Wire the reload button.
    if let Some(btn) = document.get_element_by_id("iem-panic-reload") {
        let closure = wasm_bindgen::closure::Closure::wrap(Box::new(move || {
            if let Some(w) = web_sys::window() {
                let _ = w.location().reload();
            }
        }) as Box<dyn FnMut()>);
        if let Some(el) = btn.dyn_ref::<web_sys::HtmlElement>() {
            el.set_onclick(Some(closure.as_ref().unchecked_ref()));
        }
        // Leak the closure — the overlay is a terminal state, the page will reload.
        closure.forget();
    }
}

fn post_panic_report(info: &std::panic::PanicHookInfo<'_>) {
    let Some(window) = web_sys::window() else {
        return;
    };

    let url = window
        .location()
        .pathname()
        .unwrap_or_else(|_| String::from("/"));
    let user_agent = window.navigator().user_agent().unwrap_or_default();
    let message = format_panic_message(info);
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_default();

    // Note: the server-side ClientErrorReport struct also has a `backtrace`
    // field, but we deliberately do not populate it here. Rust WASM builds
    // strip symbol information by default, so any backtrace captured inside
    // the browser would be just wasm instruction offsets — worse than useless
    // for debugging. The native backtrace remains available via the browser
    // console (through console_error_panic_hook). If we later add symbol
    // shipping (.wasm.map) we can revisit.
    let body = serde_json::json!({
        "panic_message": message,
        "version": iem_core::VERSION,
        "git_hash": iem_core::git_hash(),
        "url": url,
        "user_agent": user_agent,
        "location": location,
    });

    let body_string = match serde_json::to_string(&body) {
        Ok(s) => s,
        Err(_) => return,
    };

    // Fire-and-forget fetch. We don't await; the page is about to reload anyway.
    let opts = web_sys::RequestInit::new();
    opts.set_method("POST");
    opts.set_body(&wasm_bindgen::JsValue::from_str(&body_string));

    let Ok(headers) = web_sys::Headers::new() else {
        return;
    };
    let _ = headers.set("content-type", "application/json");
    opts.set_headers(&headers);

    if let Ok(request) = web_sys::Request::new_with_str_and_init("/api/client-error", &opts) {
        let _ = window.fetch_with_request(&request);
        // Intentionally drop the returned Promise without awaiting.
    }
}

/// Extract a human-readable string from a panic payload.
///
/// `std::panic::PanicHookInfo` cannot be constructed by user code, so this
/// helper takes the `&dyn Any` payload directly (the same thing
/// `PanicHookInfo::payload()` returns). That makes it unit-testable with
/// synthetic `&str`, `String`, and arbitrary types.
fn format_panic_payload(payload: &dyn std::any::Any) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        String::from("(unknown panic payload)")
    }
}

fn format_panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    format_panic_payload(info.payload())
}

fn truncate_for_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ---------------------------------------------------------------------------
// Reconnect-banner debounce — issue reaperiem#186.
// ---------------------------------------------------------------------------

use gloo_timers::callback::Timeout;
use leptos::prelude::*;

/// Returns `true` when the banner-show transition should be scheduled — i.e.
/// we're disconnected, the banner is not yet shown, AND there is no pending
/// timer already counting down. This is the single source of truth for the
/// scheduling decision, used both by `debounced_disconnect`'s reactive
/// Effect and by the unit tests that exercise the branch logic without a
/// Leptos runtime.
///
/// `timer_pending` is critical: while disconnected, every failed reconnect
/// attempt fires the WS `onclose` handler which calls `set_connected(false)`
/// again, re-running our Effect even though the signal value didn't change.
/// Without checking `timer_pending`, we'd cancel and restart the 3 s timer
/// on every reconnect attempt (every 2 s, 4 s, 8 s … per backoff schedule)
/// and the banner would never appear during a sustained outage.
///
/// Pure function — no reactive context, no side effects.
fn should_schedule_disconnect_timer(
    is_connected: bool,
    banner_shown: bool,
    timer_pending: bool,
) -> bool {
    !is_connected && !banner_shown && !timer_pending
}

/// Returns a derived signal that becomes `true` only after `connected == false`
/// for `delay_ms` continuously. Flips back to `false` instantly when `connected`
/// becomes `true` again.
///
/// Used to debounce dramatic "Reconnecting" UI elements without delaying
/// instant-feedback UI like the status dot. The underlying `connected` signal
/// stays untouched.
///
/// Implementation: a Leptos `Effect` that watches `connected` and uses a
/// `gloo_timers::callback::Timeout` stored in `Rc<RefCell<Option<Timeout>>>`
/// for cancellation. Dropping a `Timeout` cancels the underlying JS timer, so
/// replacing the stored value with `None` (or with a new `Timeout`) cancels
/// any prior pending transition. `Rc<RefCell<...>>` rather than Leptos
/// `StoredValue` because `gloo_timers::callback::Timeout` is not `Send +
/// Sync` and `StoredValue` requires `Send + Sync` even on `wasm32` targets.
/// This matches the pattern used elsewhere in `iem-ui` for closure-shared
/// non-reactive state (see `pages/mixer/helpers.rs:34`).
///
/// All signal writes inside the `Effect` use `try_set` / `try_get_untracked`
/// per the project's disposal-safety policy (reaperiem#153) — the danger-zone scanner
/// flags any `.set()` / `.get_untracked()` inside `Effect::new`.
pub fn debounced_disconnect(connected: ReadSignal<bool>, delay_ms: u32) -> Signal<bool> {
    let (show, set_show) = signal(false);
    let timeout: std::rc::Rc<std::cell::RefCell<Option<Timeout>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));

    Effect::new(move |_| {
        let is_connected = connected.get();

        if is_connected {
            // Reconnected — cancel any pending timer and hide the banner.
            // (Cancellation only happens here: while disconnected, the
            // existing timer must be allowed to count down regardless of
            // how many times the Effect re-fires.)
            *timeout.borrow_mut() = None;
            let _ = set_show.try_set(false);
        } else {
            // `try_get_untracked` returns None only if the signal has been
            // disposed; treat disposed-but-disconnected as "banner not
            // shown" so the timer still gets scheduled (the Effect itself
            // will be torn down before it can fire on a disposed signal).
            let banner_shown = show.try_get_untracked().unwrap_or(false);
            let timer_pending = timeout.borrow().is_some();
            if should_schedule_disconnect_timer(is_connected, banner_shown, timer_pending) {
                // Disconnected, banner not yet shown, and no timer counting
                // down yet — schedule the transition. Subsequent re-fires
                // of `connected = false` (e.g. from failed reconnect
                // attempts) leave the in-flight timer alone, so the banner
                // appears `delay_ms` after the FIRST disconnect tick, not
                // `delay_ms` after the LAST one.
                let new_timeout = Timeout::new(delay_ms, move || {
                    let _ = set_show.try_set(true);
                });
                *timeout.borrow_mut() = Some(new_timeout);
            }
        }
    });

    show.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_delay_ms_follows_schedule() {
        assert_eq!(backoff_delay_ms(0), 2_000);
        assert_eq!(backoff_delay_ms(1), 4_000);
        assert_eq!(backoff_delay_ms(2), 8_000);
        assert_eq!(backoff_delay_ms(3), 15_000);
        assert_eq!(backoff_delay_ms(4), 30_000);
        assert_eq!(backoff_delay_ms(5), 30_000);
        assert_eq!(backoff_delay_ms(100), 30_000);
    }

    #[test]
    fn is_stale_below_threshold_returns_false() {
        // 29_999 ms since last frame, threshold 30_000 ms → NOT stale
        assert!(!is_stale(100.0, 30_099.0, 30_000.0));
    }

    #[test]
    fn is_stale_above_threshold_returns_true() {
        // 30_001 ms since last frame, threshold 30_000 ms → stale
        assert!(is_stale(0.0, 30_001.0, 30_000.0));
    }

    #[test]
    fn truncate_for_display_below_limit_is_unchanged() {
        assert_eq!(truncate_for_display("short", 200), "short");
    }

    #[test]
    fn truncate_for_display_above_limit_is_truncated_with_ellipsis() {
        let long = "a".repeat(250);
        let result = truncate_for_display(&long, 200);
        assert_eq!(result.chars().count(), 201); // 200 + ellipsis
        assert!(result.ends_with('…'));
    }

    #[test]
    fn html_escape_replaces_all_special_chars() {
        assert_eq!(
            html_escape(r#"<script>alert("&")</script>"#),
            "&lt;script&gt;alert(&quot;&amp;&quot;)&lt;/script&gt;"
        );
    }

    #[test]
    fn build_overlay_html_contains_title_and_reload_button() {
        let html = build_overlay_html("some panic", "v1.142.0");
        assert!(
            html.contains("IEM Mixer encountered an error"),
            "overlay missing title: {html}"
        );
        assert!(
            html.contains(r#"id="iem-panic-reload""#),
            "overlay missing reload button: {html}"
        );
        assert!(
            html.contains(r#"id="iem-panic-overlay""#),
            "overlay missing container id: {html}"
        );
        assert!(html.contains("Reload"), "overlay missing reload label");
    }

    #[test]
    fn build_overlay_html_interpolates_summary_and_version() {
        let html = build_overlay_html("my custom panic msg", "v9.9.9");
        assert!(html.contains("my custom panic msg"));
        assert!(html.contains("v9.9.9"));
    }

    #[test]
    fn build_overlay_html_escapes_hostile_summary() {
        // Hostile panic messages must not inject script tags into the overlay.
        let hostile = r#"<script>alert("xss")</script>"#;
        let html = build_overlay_html(hostile, "v1.0.0");
        assert!(
            !html.contains("<script>"),
            "raw <script> tag leaked into overlay: {html}"
        );
        assert!(
            html.contains("&lt;script&gt;"),
            "hostile input was not escaped: {html}"
        );
    }

    #[test]
    fn build_overlay_html_uses_high_zindex_and_fixed_position() {
        // Regression guard: the overlay must cover the entire viewport on top
        // of whatever Leptos rendered before the panic. If the position/
        // z-index rules are removed the overlay would be invisible behind the
        // (probably still-mounted) app shell.
        let html = build_overlay_html("x", "v1.0");
        assert!(html.contains("position:fixed"));
        assert!(html.contains("inset:0"));
        assert!(html.contains("z-index:2147483647"));
    }

    #[test]
    fn format_panic_payload_extracts_str_slice() {
        // &'static str is the most common panic payload type.
        let payload: &dyn std::any::Any = &"boom";
        assert_eq!(format_panic_payload(payload), "boom");
    }

    #[test]
    fn format_panic_payload_extracts_owned_string() {
        // String payloads happen when formatting is used (panic!("{}", x)).
        let payload: String = String::from("owned boom");
        assert_eq!(format_panic_payload(&payload), "owned boom");
    }

    #[test]
    fn format_panic_payload_handles_unknown_type_gracefully() {
        // Arbitrary types fall through to a stable sentinel string so the
        // overlay and POST body are always populated.
        let payload: u32 = 42;
        assert_eq!(format_panic_payload(&payload), "(unknown panic payload)");
    }

    #[test]
    fn watchdog_threshold_is_larger_than_interval() {
        // Sanity: the staleness threshold must be strictly larger than the
        // interval, otherwise the watchdog would false-positive on its own
        // tick latency.
        const { assert!((WS_WATCHDOG_INTERVAL_MS as f64) < WS_STALENESS_THRESHOLD_MS) }
    }

    #[test]
    fn panic_message_display_cap_matches_truncate_behavior() {
        // Guard against someone silently dropping the truncation by making
        // the cap ridiculously large. 200 chars is small enough to fit on
        // a phone viewport.
        const { assert!(PANIC_MESSAGE_MAX_DISPLAY_CHARS <= 500) }
        const { assert!(PANIC_MESSAGE_MAX_DISPLAY_CHARS > 0) }
    }

    // -----------------------------------------------------------------------
    // debounced_disconnect — runtime-dependent behavior is covered by the
    // Playwright test (e2e/tests/reconnect-debounce.spec.ts in T5); here we
    // test the small pure-logic branch decision directly without a Leptos
    // runtime. The test calls the SAME `should_schedule_disconnect_timer`
    // function that the production Effect calls, so the two cannot drift.
    // -----------------------------------------------------------------------

    #[test]
    fn debounced_disconnect_helper_branch_decisions() {
        // (is_connected, banner_shown, timer_pending) → should_schedule?

        // Connected — never schedule, regardless of other state.
        assert!(!should_schedule_disconnect_timer(true, false, false));
        assert!(!should_schedule_disconnect_timer(true, true, false));
        assert!(!should_schedule_disconnect_timer(true, false, true));
        assert!(!should_schedule_disconnect_timer(true, true, true));

        // Disconnected, banner hidden, no timer pending → SCHEDULE.
        // This is the only path that should actually start the timer.
        assert!(should_schedule_disconnect_timer(false, false, false));

        // Disconnected, banner already shown → never re-schedule.
        // Continued disconnect should keep the banner visible without
        // restarting any timer.
        assert!(!should_schedule_disconnect_timer(false, true, false));
        assert!(!should_schedule_disconnect_timer(false, true, true));

        // Disconnected, banner hidden, but a timer is ALREADY counting
        // down → never restart it. This is the regression guard for the
        // sticky-timer fix: failed reconnect attempts re-fire the
        // `connected = false` signal but must NOT restart the debounce
        // window from zero each time.
        assert!(!should_schedule_disconnect_timer(false, false, true));
    }
}
