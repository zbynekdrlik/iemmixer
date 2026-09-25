//! Engineer push-to-talk button for talkback to band members (reaperiem#123)

use leptos::prelude::*;
use wasm_bindgen::prelude::*;

// JS interop functions from talkback.js
#[wasm_bindgen(module = "/talkback.js")]
extern "C" {
    #[wasm_bindgen(js_name = "isTalkbackSupported")]
    fn is_talkback_supported() -> bool;

    #[wasm_bindgen(catch, js_name = "startTalkback")]
    async fn start_talkback(ws_url: &str) -> Result<(), JsValue>;

    #[wasm_bindgen(js_name = "stopTalkback")]
    fn stop_talkback();
}

/// Talkback state machine
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TalkState {
    /// Ready to talk
    Idle,
    /// Actively sending audio
    Live,
    /// Another engineer holds the lock
    InUse,
    /// Microphone permission denied
    MicBlocked,
    /// Browser doesn't support WebCodecs/getUserMedia
    Unsupported,
}

/// Push-to-talk button for engineer talkback.
///
/// Hold to talk, release to stop. Uses pointer events for touch+mouse.
/// Sends TalkStart/TalkStop via the mixer WebSocket; server responds with
/// TalkAcquired/TalkBusy/TalkReleased to coordinate the lock.
#[component]
pub fn TalkButton(
    /// Mixer WebSocket connection
    ws: ReadSignal<Option<web_sys::WebSocket>>,
    /// Current talkback state
    state: ReadSignal<TalkState>,
    /// State setter (updated by mixer WS handler)
    set_state: WriteSignal<TalkState>,
) -> impl IntoView {
    // Check browser support on mount
    Effect::new(move || {
        if !is_talkback_supported() {
            let _ = set_state.try_set(TalkState::Unsupported);
        }
    });

    // Vibration loop + page red overlay when Live
    let vib_closure_effect: crate::components::CallbackSlot =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    Effect::new(move || {
        let current = state.get();
        if let Some(window) = web_sys::window() {
            if current == TalkState::Live {
                // Start vibration loop (200ms pulse every 1s)
                let vib_cb = wasm_bindgen::closure::Closure::wrap(Box::new(move || {
                    if let Some(w) = web_sys::window() {
                        let _ = w.navigator().vibrate_with_duration(200);
                    }
                })
                    as Box<dyn FnMut()>);
                let id = window
                    .set_interval_with_callback_and_timeout_and_arguments_0(
                        vib_cb.as_ref().unchecked_ref(),
                        1000,
                    )
                    .unwrap_or(0);
                let _ = js_sys::Reflect::set(
                    &window,
                    &wasm_bindgen::JsValue::from_str("__iem_talk_vib"),
                    &wasm_bindgen::JsValue::from(id),
                );
                // Store closure (dropped when state changes or component unmounts)
                *vib_closure_effect.borrow_mut() = Some(vib_cb);
                // Initial vibrate
                let _ = window.navigator().vibrate_with_duration(200);
                // Add red overlay class to body
                if let Some(doc) = window.document()
                    && let Some(body) = doc.body()
                {
                    let _ = body.class_list().add_1("talk-live-overlay");
                }
            } else {
                // Drop old closure
                vib_closure_effect.borrow_mut().take();
                // Stop vibration loop
                if let Ok(val) = js_sys::Reflect::get(
                    &window,
                    &wasm_bindgen::JsValue::from_str("__iem_talk_vib"),
                ) && let Some(id) = val.as_f64()
                {
                    window.clear_interval_with_handle(id as i32);
                    let _ = js_sys::Reflect::delete_property(
                        &window,
                        &wasm_bindgen::JsValue::from_str("__iem_talk_vib"),
                    );
                }
                // Remove red overlay class from body
                if let Some(doc) = window.document()
                    && let Some(body) = doc.body()
                {
                    let _ = body.class_list().remove_1("talk-live-overlay");
                }
            }
        }
    });

    // Build the talkback WS URL (separate from mixer WS)
    let build_talkback_ws_url = move || -> Option<String> {
        let location = web_sys::window()?.location();
        let protocol = location.protocol().ok()?;
        let host = location.host().ok()?;
        let ws_protocol = if protocol == "https:" { "wss:" } else { "ws:" };
        let token = crate::auth::get_token()?;
        Some(format!(
            "{}//{}/ws/talkback?token={}",
            ws_protocol, host, token
        ))
    };

    // Pointer down: request talk lock via mixer WS
    let ws_url_builder = build_talkback_ws_url;
    let on_pointer_down = move |e: web_sys::PointerEvent| {
        e.prevent_default();
        // Capture pointer — prevents pointerleave from firing on slight finger movement
        if let Some(target) = e.target()
            && let Ok(el) = target.dyn_into::<web_sys::Element>()
        {
            let _ = el.set_pointer_capture(e.pointer_id());
        }
        let current = state.get_untracked();
        if current == TalkState::Unsupported
            || current == TalkState::MicBlocked
            || current == TalkState::InUse
        {
            return;
        }

        // Send TalkStart via mixer WS to acquire lock
        if let Some(socket) = ws.get_untracked()
            && socket.ready_state() == web_sys::WebSocket::OPEN
            && let Ok(json) = serde_json::to_string(&iem_core::ClientMsg::TalkStart)
        {
            let _ = socket.send_with_str(&json);
        }

        // Start mic capture (the WS handler will set state to Live on TalkAcquired,
        // but we start capturing proactively to reduce latency).
        //
        // Use try_update on set_state to guard against disposal race — if the
        // component unmounts while start_talkback is awaiting, writing to a
        // disposed signal would panic. See reaperiem#153 follow-up.
        if let Some(url) = ws_url_builder() {
            let set_state = set_state;
            wasm_bindgen_futures::spawn_local(async move {
                match start_talkback(&url).await {
                    Ok(()) => {
                        // Mic is capturing; state will be set to Live by TalkAcquired handler
                    }
                    Err(e) => {
                        let msg = format!("{:?}", e);
                        if msg.contains("NotAllowedError") || msg.contains("Permission") {
                            let _ = set_state.try_set(TalkState::MicBlocked);
                        } else {
                            web_sys::console::error_1(
                                &format!("[talk] start failed: {}", msg).into(),
                            );
                        }
                    }
                }
            });
        }
    };

    // Release talk lock helper (shared by pointerup and pointerleave)
    let release_talk = move || {
        let current = state.get_untracked();
        if current != TalkState::Live && current != TalkState::Idle {
            return;
        }

        // Stop mic capture
        stop_talkback();

        // Send TalkStop via mixer WS
        if let Some(socket) = ws.get_untracked()
            && socket.ready_state() == web_sys::WebSocket::OPEN
            && let Ok(json) = serde_json::to_string(&iem_core::ClientMsg::TalkStop)
        {
            let _ = socket.send_with_str(&json);
        }

        let _ = set_state.try_set(TalkState::Idle);
    };

    let on_pointer_up = move |e: web_sys::PointerEvent| {
        // Release pointer capture
        if let Some(target) = e.target()
            && let Ok(el) = target.dyn_into::<web_sys::Element>()
        {
            let _ = el.release_pointer_capture(e.pointer_id());
        }
        release_talk();
    };

    let btn_class = move || match state.get() {
        TalkState::Idle => "toolbar-btn-talk",
        TalkState::Live => "toolbar-btn-talk live",
        TalkState::InUse => "toolbar-btn-talk in-use",
        TalkState::MicBlocked => "toolbar-btn-talk mic-blocked",
        TalkState::Unsupported => "toolbar-btn-talk unsupported",
    };

    let btn_text = move || match state.get() {
        TalkState::Idle => "\u{1F3A4} Talk".to_string(),
        TalkState::Live => "\u{1F3A4} LIVE".to_string(),
        TalkState::InUse => "\u{1F3A4} In Use".to_string(),
        TalkState::MicBlocked => "\u{1F3A4} Mic Blocked".to_string(),
        TalkState::Unsupported => "\u{1F3A4} N/A".to_string(),
    };

    let is_disabled = move || {
        matches!(
            state.get(),
            TalkState::InUse | TalkState::MicBlocked | TalkState::Unsupported
        )
    };

    // Cleanup on unmount
    on_cleanup(move || {
        stop_talkback();
    });

    view! {
        <button
            class=btn_class
            on:pointerdown=on_pointer_down
            on:pointerup=on_pointer_up
            disabled=is_disabled
        >
            {btn_text}
        </button>
    }
}
