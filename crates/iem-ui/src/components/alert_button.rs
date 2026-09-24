//! Band member alert button — calls engineer for help (reaperiem#125)

use leptos::prelude::*;

/// Alert/SOS button for band members.
/// Toggles between "send alert" (idle) and "cancel alert" (active).
#[component]
pub fn AlertButton(
    ws: ReadSignal<Option<web_sys::WebSocket>>,
    /// Whether this member has an active alert
    active: ReadSignal<bool>,
) -> impl IntoView {
    let on_click = move |_| {
        if let Some(socket) = ws.get_untracked() {
            if socket.ready_state() == web_sys::WebSocket::OPEN {
                let cmd = if active.get_untracked() {
                    serde_json::to_string(&iem_core::ClientMsg::ClearAlert)
                } else {
                    serde_json::to_string(&iem_core::ClientMsg::CallEngineer)
                };
                if let Ok(json) = cmd {
                    let _ = socket.send_with_str(&json);
                }
            }
        }

        // Vibrate to confirm action
        if let Some(window) = web_sys::window() {
            let _ = window.navigator().vibrate_with_duration(100);
        }
    };

    view! {
        <button
            class="alert-btn"
            class:active=move || active.get()
            on:click=on_click
        >
            {move || {
                if active.get() {
                    "SOS Active".to_string()
                } else {
                    "SOS".to_string()
                }
            }}
        </button>
    }
}
