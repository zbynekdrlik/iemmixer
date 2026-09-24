//! Listen button component for engineer audio monitoring
//!
//! Opens a separate WebSocket to /ws/audio for binary Opus frame streaming.
//! Uses JS interop (audio_player.js) for WebCodecs decoding and Web Audio playback.

use leptos::prelude::*;
use wasm_bindgen::prelude::*;

use crate::components::settings_modal::UserSettings;

// JS interop functions from audio_player.js
#[wasm_bindgen(module = "/audio_player.js")]
extern "C" {
    #[wasm_bindgen(js_name = "initAudioPlayer")]
    fn init_audio_player();

    #[wasm_bindgen(js_name = "feedOpusFrame")]
    fn feed_opus_frame(data: &[u8]);

    #[wasm_bindgen(js_name = "stopAudioPlayer")]
    fn stop_audio_player();

    #[wasm_bindgen(js_name = "isAudioSupported")]
    fn is_audio_supported() -> bool;

    #[wasm_bindgen(js_name = "setListenGain")]
    pub fn set_listen_gain(db: f32);

    #[wasm_bindgen(js_name = "getStreamStats")]
    fn get_stream_stats() -> JsValue;
}

/// Audio listening states
#[derive(Debug, Clone, Copy, PartialEq)]
enum ListenState {
    Idle,
    Listening,
    Reconnecting,
    NoSource,
    Unsupported,
}

/// Stream quality stats from JS audio player
#[derive(Debug, Clone, Default)]
struct StreamStats {
    dropouts: u32,
    buffer_ms: u32,
    quality: String,
}

fn poll_stream_stats() -> StreamStats {
    let val = get_stream_stats();
    let dropouts = js_sys::Reflect::get(&val, &"dropouts".into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u32;
    let buffer_ms = js_sys::Reflect::get(&val, &"bufferMs".into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u32;
    let quality = js_sys::Reflect::get(&val, &"quality".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| "good".to_string());
    StreamStats {
        dropouts,
        buffer_ms,
        quality,
    }
}

/// Listen button for engineer audio monitoring
///
/// Opens a separate WebSocket connection to /ws/audio when activated.
/// Receives binary Opus frames and plays them via Web Audio API.
/// `member_id` specifies whose mix to listen to (e.g., "oldmember1", "engineer").
#[component]
pub fn ListenButton(
    /// Which member's mix to listen to
    member_id: String,
) -> impl IntoView {
    let member_id = member_id.clone();
    let (state, set_state) = signal(ListenState::Idle);
    let (ws, set_ws) = signal(Option::<web_sys::WebSocket>::None);
    let (listen_target, set_listen_target) = signal(String::new());
    let (stream_stats, set_stream_stats) = signal(StreamStats::default());
    let (reconnect_interval, set_reconnect_interval) = signal(Option::<i32>::None);
    // Flag to distinguish user-initiated stop from unexpected disconnect.
    // When true, on_close should NOT trigger auto-reconnect.
    let (intentional_stop, set_intentional_stop) = signal(false);

    // Check browser support on mount
    Effect::new(move || {
        if !is_audio_supported() {
            let _ = set_state.try_set(ListenState::Unsupported);
        }
    });

    // Poll stream stats every 500ms while listening.
    // Uses raw JS setInterval to get an i32 handle (Send+Sync) for on_cleanup,
    // since gloo_timers::Interval contains non-Send closures.
    let (stats_interval, set_stats_interval) = signal(Option::<i32>::None);
    let stats_closure_effect: crate::components::CallbackSlot =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    Effect::new(move || {
        let current_state = state.get();

        // Clear previous interval if any. Double-Option: outer guards the
        // signal disposal race (scope may be gone when effect re-runs
        // during teardown); inner is the signal's own `Option<i32>`.
        if let Some(Some(id)) = stats_interval.try_get_untracked() {
            if let Some(w) = web_sys::window() {
                w.clear_interval_with_handle(id);
            }
            let _ = set_stats_interval.try_set(None);
        }
        // Drop old closure (prevents leak)
        stats_closure_effect.borrow_mut().take();

        if current_state == ListenState::Listening {
            // try_update defends against the disposal race where the 500ms
            // tick fires between on_cleanup running and the interval actually
            // being cancelled by the browser. reaperiem#153
            let closure = Closure::wrap(Box::new(move || {
                let _ = set_stream_stats.try_set(poll_stream_stats());
            }) as Box<dyn FnMut()>);
            let id = web_sys::window()
                .unwrap()
                .set_interval_with_callback_and_timeout_and_arguments_0(
                    closure.as_ref().unchecked_ref(),
                    500,
                )
                .unwrap();
            // Store closure to keep it alive (and allow cleanup on re-run)
            *stats_closure_effect.borrow_mut() = Some(closure);
            let _ = set_stats_interval.try_set(Some(id));
        }
    });

    // Auto-reconnect: when state becomes Reconnecting, start exponential backoff
    let member_id_reconnect = member_id.clone();
    let reconnect_closure_effect: crate::components::CallbackSlot =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    Effect::new(move || {
        let current_state = state.get();

        // Clear existing reconnect timer if state changed away from Reconnecting
        if current_state != ListenState::Reconnecting {
            // Double-Option: outer = disposal guard, inner = signal value.
            if let Some(Some(id)) = reconnect_interval.try_get_untracked() {
                if let Some(w) = web_sys::window() {
                    w.clear_interval_with_handle(id);
                }
                let _ = set_reconnect_interval.try_set(None);
            }
            return;
        }

        // Already have a reconnect timer running. On disposal we treat
        // the signal as "no timer", so `flatten()` collapses the outer
        // None into an inner None and the is_some() check stays correct.
        if reconnect_interval.try_get_untracked().flatten().is_some() {
            return;
        }

        // Start reconnection with exponential backoff (1s, 2s, 4s, 8s cap)
        let backoff = std::rc::Rc::new(std::cell::Cell::new(1000u32));
        let member_id_inner = member_id_reconnect.clone();
        let closure = Closure::wrap(Box::new(move || {
            let current_backoff = backoff.get();
            web_sys::console::log_1(
                &format!("[audio] Reconnect attempt (backoff {}ms)", current_backoff).into(),
            );

            // Try to reconnect
            start_listening(
                set_state,
                set_ws,
                set_listen_target,
                member_id_inner.clone(),
                intentional_stop,
                set_intentional_stop,
            );

            // Increase backoff for next attempt (cap at 8s)
            let next = (current_backoff * 2).min(8000);
            backoff.set(next);
        }) as Box<dyn FnMut()>);

        // Use the initial backoff for the first attempt
        let id = web_sys::window()
            .unwrap()
            .set_interval_with_callback_and_timeout_and_arguments_0(
                closure.as_ref().unchecked_ref(),
                2000, // Check every 2s, backoff controls actual attempt timing
            )
            .unwrap();
        // Store closure to keep it alive (and allow cleanup on re-run)
        *reconnect_closure_effect.borrow_mut() = Some(closure);
        let _ = set_reconnect_interval.try_set(Some(id));
    });

    // Cleanup on unmount
    // Note: stats_closure_ref and reconnect_closure_ref are Rc<RefCell<>> (not Send),
    // so they can't be captured in on_cleanup. They are dropped naturally when the
    // component scope drops. The Effects already clean them up on re-run.
    on_cleanup(move || {
        if let Some(id) = stats_interval.get_untracked()
            && let Some(w) = web_sys::window()
        {
            w.clear_interval_with_handle(id);
        }
        if let Some(id) = reconnect_interval.get_untracked()
            && let Some(w) = web_sys::window()
        {
            w.clear_interval_with_handle(id);
        }
        let _ = set_intentional_stop.try_set(true);
        if let Some(ws) = ws.get_untracked() {
            let _ = ws.close();
        }
        stop_audio_player();
    });

    let member_id_toggle = member_id.clone();
    let toggle = move |_: web_sys::MouseEvent| {
        if state.get() == ListenState::Unsupported {
            return;
        }

        if state.get() == ListenState::Idle || state.get() == ListenState::NoSource {
            // Start listening
            start_listening(
                set_state,
                set_ws,
                set_listen_target,
                member_id_toggle.clone(),
                intentional_stop,
                set_intentional_stop,
            );
        } else {
            // Stop listening (also cancels reconnection)
            if let Some(id) = reconnect_interval.get_untracked() {
                if let Some(w) = web_sys::window() {
                    w.clear_interval_with_handle(id);
                }
                let _ = set_reconnect_interval.try_set(None);
            }
            stop_listening(ws, set_state, set_ws, set_intentional_stop);
            let _ = set_listen_target.try_set(String::new());
            let _ = set_stream_stats.try_set(StreamStats::default());
        }
    };

    let btn_class = move || match state.get() {
        ListenState::Idle => "toolbar-btn toolbar-btn-listen",
        ListenState::Listening => "toolbar-btn toolbar-btn-listen listening",
        ListenState::Reconnecting => "toolbar-btn toolbar-btn-listen reconnecting",
        ListenState::NoSource => "toolbar-btn toolbar-btn-listen no-source",
        ListenState::Unsupported => "toolbar-btn toolbar-btn-listen unsupported",
    };

    let btn_text = move || match state.get() {
        ListenState::Idle => "\u{1F50A} Listen".to_string(),
        ListenState::Listening => {
            let target = listen_target.get();
            if target.is_empty() || target == "engineer" {
                "\u{1F50A}".to_string()
            } else {
                let mut chars = target.chars();
                let cap: String = match chars.next() {
                    None => String::new(),
                    Some(c) => c.to_uppercase().chain(chars).collect(),
                };
                format!("\u{1F50A} {}", cap)
            }
        }
        ListenState::Reconnecting => "\u{1F50A} Reconnecting...".to_string(),
        ListenState::NoSource => "\u{1F50A} No Source".to_string(),
        ListenState::Unsupported => "\u{1F507} Unsupported".to_string(),
    };

    let stats_class = move || {
        let stats = stream_stats.get();
        format!("stream-stats {}", stats.quality)
    };

    let stats_text = move || {
        let stats = stream_stats.get();
        format!("{} drops | buf {}ms", stats.dropouts, stats.buffer_ms)
    };

    view! {
        <button
            class=btn_class
            on:click=toggle
            disabled=move || state.get() == ListenState::Unsupported
        >
            {btn_text}
            <Show when=move || state.get() == ListenState::Listening>
                <span class=stats_class data-testid="stream-stats">
                    {stats_text}
                </span>
            </Show>
        </button>
    }
}

/// Opus frames logged to the console: the first three, then every 200th.
fn logs_frame(count: u32) -> bool {
    count < 3 || count.is_multiple_of(200)
}

/// Listen state for an `AudioStatus` from the server (`None`: unknown status,
/// the state stays as it is).
fn listen_state_for(status: &str) -> Option<ListenState> {
    match status {
        "listening" => Some(ListenState::Listening),
        "no_source" => Some(ListenState::NoSource),
        "stopped" => Some(ListenState::Idle),
        _ => None,
    }
}

fn start_listening(
    set_state: WriteSignal<ListenState>,
    set_ws: WriteSignal<Option<web_sys::WebSocket>>,
    set_listen_target: WriteSignal<String>,
    member_id: String,
    intentional_stop: ReadSignal<bool>,
    set_intentional_stop: WriteSignal<bool>,
) {
    // Build WebSocket URL
    let auth = match crate::auth::get_auth() {
        Some(a) => a,
        None => return,
    };

    let location = web_sys::window().unwrap().location();
    let protocol = location.protocol().unwrap_or_default();
    let host = location.host().unwrap_or_default();
    let ws_protocol = if protocol == "https:" { "wss:" } else { "ws:" };
    let url = format!("{}//{}/ws/audio?token={}", ws_protocol, host, auth.token);

    // Init audio player NOW — in the click handler's call stack.
    // Mobile browsers require AudioContext to be created from a user gesture.
    // If we wait for the async on_open callback, the context will be suspended
    // and resume() won't work (it's also not a user gesture).
    init_audio_player();

    // Apply saved listen boost from engineer settings
    let settings = UserSettings::load("engineer");
    set_listen_gain(settings.listen_boost_db);

    let socket = match web_sys::WebSocket::new(&url) {
        Ok(s) => s,
        Err(e) => {
            web_sys::console::error_1(&format!("Audio WS connect failed: {:?}", e).into());
            return;
        }
    };
    socket.set_binary_type(web_sys::BinaryType::Arraybuffer);

    // reaperiem#186: Debounce ListenState::Reconnecting — 3s after WS close. If the
    // new socket opens before the timer fires, on_open drops the Timeout and
    // the user never sees the "Reconnecting" text for transient blips.
    // Mirrors the 3s debounce on the mixer banner (lifecycle.rs::debounced_disconnect).
    let _reconnecting_timeout: std::rc::Rc<
        std::cell::RefCell<Option<gloo_timers::callback::Timeout>>,
    > = std::rc::Rc::new(std::cell::RefCell::new(None));
    let reconnecting_timeout_open = _reconnecting_timeout.clone();
    let reconnecting_timeout_close = _reconnecting_timeout.clone();

    // On open: send ListenStart with member_id (audio player already initialized above)
    let member_id_open = member_id.clone();
    let on_open = Closure::wrap(Box::new(move |_: web_sys::Event| {
        // reaperiem#186: socket reopened — cancel any pending "Reconnecting" timer.
        *reconnecting_timeout_open.borrow_mut() = None;

        // Send ListenStart command with target member
        let cmd = serde_json::to_string(&iem_core::ClientMsg::ListenStart {
            member_id: member_id_open.clone(),
        })
        .unwrap_or_default();
        if let Some(w) = web_sys::window()
            && let Ok(ws_val) = js_sys::Reflect::get(&w, &"__iem_audio_ws".into())
            && let Some(ws) = ws_val.dyn_ref::<web_sys::WebSocket>()
        {
            let _ = ws.send_with_str(&cmd);
        }
    }) as Box<dyn FnMut(_)>);

    // On message: handle text (status) and binary (Opus frames)
    let set_state_msg = set_state;
    let frame_counter = std::cell::Cell::new(0u32);
    // Track whether we've already set Listening state to avoid re-triggering
    // the stats polling Effect on every binary frame (~50/sec). Leptos signals
    // always notify subscribers even when the value hasn't changed, which would
    // destroy and recreate the 500ms polling interval before it ever fires.
    let is_listening = std::cell::Cell::new(false);
    // All signal writes in this WS callback use try_update — the WebSocket
    // (and therefore this closure) can outlive the component if the socket is
    // still open when ListenButton unmounts, and a late frame would panic on
    // a disposed signal. See reaperiem#153 follow-up.
    let on_message = Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
        // Binary message = Opus frame
        if let Ok(buf) = event.data().dyn_into::<js_sys::ArrayBuffer>() {
            let array = js_sys::Uint8Array::new(&buf);
            let raw_len = array.length();
            let data = array.to_vec();
            let count = frame_counter.get();
            if logs_frame(count) {
                web_sys::console::log_1(
                    &format!(
                        "[audio-ws] #{} arraybuf={}B vec={}B",
                        count,
                        raw_len,
                        data.len()
                    )
                    .into(),
                );
            }
            frame_counter.set(count + 1);
            feed_opus_frame(&data);
            if !is_listening.get() {
                let _ = set_state_msg.try_set(ListenState::Listening);
                is_listening.set(true);
            }
            return;
        }

        // Text message = status update
        if let Some(text) = event.data().as_string()
            && let Ok(msg) = serde_json::from_str::<iem_core::ServerMsg>(&text)
            && let iem_core::ServerMsg::AudioStatus { status, target } = msg
        {
            if let Some(t) = target {
                let _ = set_listen_target.try_set(t);
            }
            if let Some(next) = listen_state_for(&status) {
                let _ = set_state_msg.try_set(next);
                is_listening.set(next == ListenState::Listening);
            }
        }
    }) as Box<dyn FnMut(_)>);

    // On close: reconnect only if this was NOT a user-initiated stop.
    // When the user clicks stop, stop_listening() sets intentional_stop=true
    // BEFORE calling socket.close(), so on_close sees the flag and stays Idle.
    let set_state_close = set_state;
    let set_ws_close = set_ws;
    // All signal writes use try_update — same disposal-race reason as on_message. reaperiem#153
    let on_close = Closure::wrap(Box::new(move |_: web_sys::Event| {
        let _ = set_ws_close.try_set(None);
        let intentional = intentional_stop.try_get_untracked().unwrap_or(false);
        if intentional {
            // User clicked stop — stay in Idle, don't reconnect
            web_sys::console::log_1(&"[audio] WebSocket closed (user stopped)".into());
            let _ = set_intentional_stop.try_set(false);
            let _ = set_state_close.try_set(ListenState::Idle);
            // Cancel any pending Reconnecting transition (defensive — should
            // already have been cancelled by on_open of the prior socket).
            *reconnecting_timeout_close.borrow_mut() = None;
        } else {
            // Unexpected disconnect — schedule "Reconnecting" UI after 3s.
            // If a new socket opens before then, on_open drops this Timeout
            // and the user never sees the flash. (reaperiem#186)
            web_sys::console::log_1(&"[audio] WebSocket closed — will reconnect".into());
            // Don't stop audio player — keep AudioContext alive for seamless resume.
            let set_state_for_timer = set_state_close;
            let new_timeout = gloo_timers::callback::Timeout::new(3000, move || {
                let _ = set_state_for_timer.try_set(ListenState::Reconnecting);
            });
            *reconnecting_timeout_close.borrow_mut() = Some(new_timeout);
        }
    }) as Box<dyn FnMut(_)>);

    // On error: log only — let on_close handle reconnection
    let on_error = Closure::wrap(Box::new(move |_: web_sys::Event| {
        web_sys::console::error_1(&"[audio] WebSocket error".into());
    }) as Box<dyn FnMut(_)>);

    // Store WS ref globally BEFORE registering on_open callback.
    // on_open reads window.__iem_audio_ws to send ListenStart.
    // If set_onopen fires synchronously before this assignment, ListenStart is never sent.
    if let Some(w) = web_sys::window() {
        let _ = js_sys::Reflect::set(&w, &"__iem_audio_ws".into(), &socket);
    }

    socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    on_open.forget();
    on_message.forget();
    on_close.forget();
    on_error.forget();

    // try_update because start_listening is called from the reconnect
    // interval closure which can outlive the component. If ListenButton is
    // unmounted between reconnect attempts, set_ws is disposed here. reaperiem#153
    let _ = set_ws.try_set(Some(socket.clone()));
    // Do NOT set ListenState::Listening here — wait for the first binary frame
    // to arrive in on_message (line 149). Setting it prematurely makes the UI
    // show "Listening..." even when the WS fails to connect or ListenStart
    // is never sent. The on_message handler already sets Listening on first frame.
}

fn stop_listening(
    ws: ReadSignal<Option<web_sys::WebSocket>>,
    set_state: WriteSignal<ListenState>,
    set_ws: WriteSignal<Option<web_sys::WebSocket>>,
    set_intentional_stop: WriteSignal<bool>,
) {
    // Set the flag BEFORE closing — on_close checks this to avoid auto-reconnect
    let _ = set_intentional_stop.try_set(true);
    if let Some(socket) = ws.get_untracked() {
        // Send ListenStop command before closing
        let cmd = serde_json::to_string(&iem_core::ClientMsg::ListenStop).unwrap_or_default();
        let _ = socket.send_with_str(&cmd);
        let _ = socket.close();
    }
    stop_audio_player();
    let _ = set_ws.try_set(None);
    let _ = set_state.try_set(ListenState::Idle);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_three_frames_and_every_200th_are_logged() {
        let logged: Vec<u32> = (0..=400).filter(|&n| logs_frame(n)).collect();
        assert_eq!(logged, [0, 1, 2, 200, 400]);
    }

    #[test]
    fn audio_status_maps_to_the_listen_state() {
        assert_eq!(listen_state_for("listening"), Some(ListenState::Listening));
        assert_eq!(listen_state_for("no_source"), Some(ListenState::NoSource));
        assert_eq!(listen_state_for("stopped"), Some(ListenState::Idle));
        assert_eq!(listen_state_for("buffering"), None);
    }
}
