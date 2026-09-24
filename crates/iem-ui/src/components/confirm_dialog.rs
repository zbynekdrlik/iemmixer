//! Reusable confirmation dialog (reaperiem#206).
//!
//! The app's first confirmation primitive — used by the preset and snapshot
//! modals before any destructive action (overwrite / delete). Potvrdiť/Zrušiť,
//! Esc = cancel, overlay click = cancel. Deliberately a styled in-app dialog
//! (not `window.confirm`) so it can show the item's name + date and be asserted
//! in zero-console-error E2E.

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

#[component]
pub fn ConfirmDialog(
    /// Whether the dialog is shown.
    visible: ReadSignal<bool>,
    /// Dialog heading (e.g. "Prepísať preset?").
    #[prop(into)]
    title: Signal<String>,
    /// Body text (e.g. the item's name + date).
    #[prop(into)]
    body: Signal<String>,
    /// Confirm button label.
    #[prop(default = "Potvrdiť".to_string())]
    confirm_label: String,
    /// Cancel button label.
    #[prop(default = "Zrušiť".to_string())]
    cancel_label: String,
    /// Called when the user confirms.
    on_confirm: Callback<()>,
    /// Called on cancel (Zrušiť button, Esc key, or overlay click).
    on_cancel: Callback<()>,
) -> impl IntoView {
    // Esc-to-cancel: a document-level keydown listener (mirrors the proven
    // Closure pattern in connection.rs). Only acts while this dialog is visible;
    // leaked for the session (the modal lives the whole session).
    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        let closure = Closure::wrap(Box::new(move |ev: web_sys::KeyboardEvent| {
            // try_get_untracked: if this dialog's signal was ever disposed the
            // leaked listener must NOT panic (that would trip zero-console E2E).
            if visible.try_get_untracked() == Some(true) && ev.key() == "Escape" {
                on_cancel.run(());
            }
        }) as Box<dyn FnMut(web_sys::KeyboardEvent)>);
        let _ = doc.add_event_listener_with_callback("keydown", closure.as_ref().unchecked_ref());
        closure.forget();
    }

    let handle_overlay_click = move |ev: web_sys::MouseEvent| {
        let target = ev.target().unwrap();
        if let Ok(elem) = target.dyn_into::<web_sys::HtmlElement>()
            && elem.class_list().contains("confirm-overlay")
        {
            on_cancel.run(());
        }
    };

    view! {
        <div
            class=move || {
                if visible.get() { "confirm-overlay visible" } else { "confirm-overlay" }
            }
            on:click=handle_overlay_click
        >
            <div class="confirm-dialog">
                <h3 class="confirm-title">{move || title.get()}</h3>
                <p class="confirm-body">{move || body.get()}</p>
                <div class="confirm-actions">
                    <button
                        class="confirm-btn confirm-btn-cancel"
                        on:click=move |_| on_cancel.run(())
                    >
                        {cancel_label}
                    </button>
                    <button
                        class="confirm-btn confirm-btn-confirm"
                        on:click=move |_| on_confirm.run(())
                    >
                        {confirm_label}
                    </button>
                </div>
            </div>
        </div>
    }
}
