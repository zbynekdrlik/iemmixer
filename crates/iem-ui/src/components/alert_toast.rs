//! Engineer alert toast — persistent until cleared (reaperiem#125)

use leptos::prelude::*;
use wasm_bindgen::prelude::*;

/// Persistent alert toast for engineer.
/// Pattern vibration (45s cycle, 30s refresh), plays subtle chime every 10s, shows system notification.
/// Stays until engineer clicks dismiss (sends ClearAlert via WS).
#[component]
pub fn AlertToast(
    alert: ReadSignal<Option<(String, String)>>,
    ws: ReadSignal<Option<web_sys::WebSocket>>,
) -> impl IntoView {
    // Start/stop vibration loop and sound loop when alert changes
    let vib_effect: crate::components::CallbackSlot =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let snd_effect: crate::components::CallbackSlot =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let vis_effect: crate::components::CallbackSlot =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    Effect::new(move || {
        let current = alert.get();
        if let Some((_, ref name)) = current {
            // Clean up previous alert state if Effect re-fires (Some→Some transition)
            if let Some(ref cb) = *vis_effect.borrow()
                && let Some(doc) = web_sys::window().and_then(|w| w.document())
            {
                let _ = doc.remove_event_listener_with_callback(
                    "visibilitychange",
                    cb.as_ref().unchecked_ref(),
                );
            }
            vib_effect.borrow_mut().take();
            snd_effect.borrow_mut().take();
            vis_effect.borrow_mut().take();
            stop_loops();

            // System notification (ask permission if needed)
            let name_clone = name.clone();
            wasm_bindgen_futures::spawn_local(async move {
                request_and_notify(&name_clone).await;
            });

            // Build vibration pattern: [500, 1000] × 30 = 45s of pulsing
            // Browser handles timing natively — immune to JS timer throttling
            let pattern = js_sys::Array::new();
            for _ in 0..30 {
                pattern.push(&JsValue::from(500)); // vibrate 500ms
                pattern.push(&JsValue::from(1000)); // pause 1000ms
            }

            // Fire pattern immediately
            if let Some(window) = web_sys::window() {
                let _ = window.navigator().vibrate_with_pattern(&pattern);
            }

            // 30s safety-net interval: re-fire pattern for very long alerts
            let pattern_clone = pattern.clone();
            let vib_cb = Closure::wrap(Box::new(move || {
                if let Some(window) = web_sys::window() {
                    let _ = window.navigator().vibrate_with_pattern(&pattern_clone);
                }
            }) as Box<dyn FnMut()>);
            if let Some(window) = web_sys::window() {
                let id = window
                    .set_interval_with_callback_and_timeout_and_arguments_0(
                        vib_cb.as_ref().unchecked_ref(),
                        30_000,
                    )
                    .unwrap_or(0);
                let _ = js_sys::Reflect::set(
                    &window,
                    &JsValue::from_str("__iem_alert_vib"),
                    &JsValue::from(id),
                );
            }
            *vib_effect.borrow_mut() = Some(vib_cb);

            // visibilitychange listener: re-fire pattern when engineer returns to app
            let pattern_vis = pattern.clone();
            let vis_cb = Closure::wrap(Box::new(move || {
                if let Some(window) = web_sys::window()
                    && let Some(doc) = window.document()
                    && !doc.hidden()
                {
                    let _ = window.navigator().vibrate_with_pattern(&pattern_vis);
                }
            }) as Box<dyn FnMut()>);
            if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                let _ = doc.add_event_listener_with_callback(
                    "visibilitychange",
                    vis_cb.as_ref().unchecked_ref(),
                );
            }
            *vis_effect.borrow_mut() = Some(vis_cb);

            // Start sound loop (play chime, repeat every 10s)
            play_chime();
            let sound_cb = Closure::wrap(Box::new(move || {
                play_chime();
            }) as Box<dyn FnMut()>);
            if let Some(window) = web_sys::window() {
                let id = window
                    .set_interval_with_callback_and_timeout_and_arguments_0(
                        sound_cb.as_ref().unchecked_ref(),
                        10_000,
                    )
                    .unwrap_or(0);
                let _ = js_sys::Reflect::set(
                    &window,
                    &JsValue::from_str("__iem_alert_snd"),
                    &JsValue::from(id),
                );
            }
            *snd_effect.borrow_mut() = Some(sound_cb);

            // Red page pulse overlay for SOS alert (reaperiem#123)
            if let Some(window) = web_sys::window()
                && let Some(doc) = window.document()
                && let Some(body) = doc.body()
            {
                let _ = body.class_list().add_1("talk-live-overlay");
            }
        } else {
            // Remove visibilitychange listener before dropping the closure
            if let Some(ref cb) = *vis_effect.borrow()
                && let Some(doc) = web_sys::window().and_then(|w| w.document())
            {
                let _ = doc.remove_event_listener_with_callback(
                    "visibilitychange",
                    cb.as_ref().unchecked_ref(),
                );
            }
            // Drop closures
            vib_effect.borrow_mut().take();
            snd_effect.borrow_mut().take();
            vis_effect.borrow_mut().take();
            stop_loops();
            // Remove red page pulse overlay
            if let Some(window) = web_sys::window()
                && let Some(doc) = window.document()
                && let Some(body) = doc.body()
            {
                let _ = body.class_list().remove_1("talk-live-overlay");
            }
        }
    });

    let on_dismiss = move |_| {
        if let Some(socket) = ws.get_untracked()
            && socket.ready_state() == web_sys::WebSocket::OPEN
        {
            let cmd = serde_json::to_string(&iem_core::ClientMsg::ClearAlert).unwrap_or_default();
            let _ = socket.send_with_str(&cmd);
        }
    };

    view! {
        <Show when=move || alert.get().is_some()>
            <div class="alert-toast">
                <div class="alert-toast-content">
                    <span class="alert-toast-icon">"!"</span>
                    <span class="alert-toast-text">
                        {move || {
                            alert.get()
                                .map(|(_, name)| format!("{} needs help!", name))
                                .unwrap_or_default()
                        }}
                    </span>
                    <button class="alert-toast-dismiss" on:click=on_dismiss>"OK"</button>
                </div>
            </div>
        </Show>
    }
}

fn play_chime() {
    let audio = web_sys::HtmlAudioElement::new_with_src("/alert.mp3").ok();
    if let Some(audio) = audio {
        audio.set_volume(1.0);
        let _ = audio.play();
    }
}

fn stop_loops() {
    if let Some(window) = web_sys::window() {
        let mut had_loops = false;
        if let Ok(val) = js_sys::Reflect::get(&window, &JsValue::from_str("__iem_alert_vib"))
            && let Some(id) = val.as_f64()
        {
            window.clear_interval_with_handle(id as i32);
            let _ =
                js_sys::Reflect::delete_property(&window, &JsValue::from_str("__iem_alert_vib"));
            had_loops = true;
        }
        if let Ok(val) = js_sys::Reflect::get(&window, &JsValue::from_str("__iem_alert_snd"))
            && let Some(id) = val.as_f64()
        {
            window.clear_interval_with_handle(id as i32);
            let _ =
                js_sys::Reflect::delete_property(&window, &JsValue::from_str("__iem_alert_snd"));
            had_loops = true;
        }
        // Only cancel vibration if we actually had loops running
        if had_loops {
            let _ = window.navigator().vibrate_with_duration(0);
        }
    }
}

/// Send notification via service worker (works in background + when app minimized)
async fn request_and_notify(name: &str) {
    let window = match web_sys::window() {
        Some(w) => w,
        None => return,
    };
    let navigator = window.navigator();

    // Try to send via service worker (most reliable, works in background)
    if let Ok(sw) = js_sys::Reflect::get(&navigator, &JsValue::from_str("serviceWorker"))
        && let Ok(ready) = js_sys::Reflect::get(&sw, &JsValue::from_str("ready"))
        && let Ok(promise) = ready.dyn_into::<js_sys::Promise>()
        && let Ok(reg) = wasm_bindgen_futures::JsFuture::from(promise).await
    {
        // Post message to SW to show notification
        let msg = js_sys::Object::new();
        let _ = js_sys::Reflect::set(
            &msg,
            &JsValue::from_str("type"),
            &JsValue::from_str("ALERT"),
        );
        let _ = js_sys::Reflect::set(&msg, &JsValue::from_str("name"), &JsValue::from_str(name));
        if let Ok(active) = js_sys::Reflect::get(&reg, &JsValue::from_str("active"))
            && let Ok(post_fn) = js_sys::Reflect::get(&active, &JsValue::from_str("postMessage"))
            && let Some(func) = post_fn.dyn_ref::<js_sys::Function>()
        {
            let _ = func.call1(&active, &msg);
        }
    }

    // Fallback: try direct Notification API
    let opts = web_sys::NotificationOptions::new();
    opts.set_body(&format!("{} needs help!", name));
    opts.set_require_interaction(true);
    let _ = web_sys::Notification::new_with_options(&format!("IEM Alert: {}", name), &opts);
}
