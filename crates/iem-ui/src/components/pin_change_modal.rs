//! PIN change modal component
//!
//! Allows band members to change their personal PIN via a numpad UI.

use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

/// PIN change modal with numpad input
#[component]
pub fn PinChangeModal(
    /// Whether the modal is visible
    visible: ReadSignal<bool>,
    /// Callback when the modal should close
    on_close: Callback<()>,
    /// Target member ID (whose PIN is being changed)
    member_id: String,
) -> impl IntoView {
    let is_engineer = crate::auth::get_auth().map(|a| a.engineer).unwrap_or(false);
    // StoredValue is Copy, so closures capturing it remain Fn (not FnOnce)
    let member_stored = StoredValue::new(member_id);

    let (current_pin, set_current_pin) = signal(String::new());
    let (new_pin, set_new_pin) = signal(String::new());
    let (confirm_pin, set_confirm_pin) = signal(String::new());
    let (error_msg, set_error_msg) = signal(String::new());
    let (success, set_success) = signal(false);
    let (loading, set_loading) = signal(false);
    // 0 = current PIN, 1 = new PIN, 2 = confirm PIN
    // Engineers skip field 0 (they don't know the member's current PIN)
    let initial_field: u8 = if is_engineer { 1 } else { 0 };
    let (active_field, set_active_field) = signal(initial_field);

    // Reset state when modal opens
    Effect::new(move |_| {
        if visible.get() {
            let _ = set_current_pin.try_set(String::new());
            let _ = set_new_pin.try_set(String::new());
            let _ = set_confirm_pin.try_set(String::new());
            let _ = set_error_msg.try_set(String::new());
            let _ = set_success.try_set(false);
            let _ = set_loading.try_set(false);
            let _ = set_active_field.try_set(initial_field);
        }
    });

    let on_numpad = move |digit: char| {
        let field = active_field.get();
        match field {
            0 => {
                let _ = set_current_pin.try_update(|p| {
                    if p.len() < 4 {
                        p.push(digit);
                        if p.len() == 4 {
                            let _ = set_active_field.try_set(1);
                        }
                    }
                });
            }
            1 => {
                let _ = set_new_pin.try_update(|p| {
                    if p.len() < 4 {
                        p.push(digit);
                        if p.len() == 4 {
                            let _ = set_active_field.try_set(2);
                        }
                    }
                });
            }
            _ => {
                let _ = set_confirm_pin.try_update(|p| {
                    if p.len() < 4 {
                        p.push(digit);
                    }
                });
            }
        }
        let _ = set_error_msg.try_set(String::new());
    };

    let on_backspace = move |_| {
        let field = active_field.get();
        match field {
            0 => {
                let _ = set_current_pin.try_update(|p| {
                    p.pop();
                });
            }
            1 => {
                let _ = set_new_pin.try_update(|p| {
                    if p.is_empty() {
                        // Engineers have no field 0, so stay on field 1
                        if !is_engineer {
                            let _ = set_active_field.try_set(0);
                        }
                    } else {
                        p.pop();
                    }
                });
            }
            _ => {
                let _ = set_confirm_pin.try_update(|p| {
                    if p.is_empty() {
                        let _ = set_active_field.try_set(1);
                    } else {
                        p.pop();
                    }
                });
            }
        }
    };

    let on_save = move |_| {
        let cur = current_pin.get();
        let new = new_pin.get();
        let confirm = confirm_pin.get();

        // Engineers skip current PIN validation
        if !is_engineer && cur.len() != 4 {
            let _ = set_error_msg.try_set("Enter current PIN".to_string());
            let _ = set_active_field.try_set(0);
            return;
        }
        if new.len() != 4 {
            let _ = set_error_msg.try_set("Enter new PIN".to_string());
            let _ = set_active_field.try_set(1);
            return;
        }
        if new != confirm {
            let _ = set_error_msg.try_set("PINs don't match".to_string());
            let _ = set_confirm_pin.try_set(String::new());
            let _ = set_active_field.try_set(2);
            return;
        }

        let _ = set_loading.try_set(true);
        let _ = set_error_msg.try_set(String::new());

        let member = member_stored.get_value();
        let old_pin = if is_engineer {
            String::new()
        } else {
            cur.clone()
        };
        // try_update on every signal write — this task may outlive the
        // modal if the user closes it while change_pin is awaiting. reaperiem#153
        spawn_local(async move {
            match crate::api::change_pin(&old_pin, &new, &member).await {
                Ok(()) => {
                    let _ = set_success.try_set(true);
                    let _ = set_loading.try_set(false);
                }
                Err(e) => {
                    let _ = set_error_msg.try_set(e);
                    let _ = set_loading.try_set(false);
                    if !is_engineer {
                        let _ = set_current_pin.try_set(String::new());
                        let _ = set_active_field.try_set(0);
                    } else {
                        let _ = set_new_pin.try_set(String::new());
                        let _ = set_confirm_pin.try_set(String::new());
                        let _ = set_active_field.try_set(1);
                    }
                }
            }
        });
    };

    let pin_dots = move |pin: ReadSignal<String>, field_id: u8| {
        let active = active_field.get();
        let len = pin.get().len();
        let class = if active == field_id {
            "pin-field active"
        } else {
            "pin-field"
        };
        view! {
            <div class=class on:click=move |_| { let _ = set_active_field.try_set(field_id); }>
                {(0..4)
                    .map(|i| {
                        let filled = i < len;
                        view! {
                            <div class=if filled { "pin-dot filled" } else { "pin-dot" }></div>
                        }
                    })
                    .collect::<Vec<_>>()}
            </div>
        }
    };

    view! {
        <Show when=move || visible.get() fallback=|| ()>
            <div class="pin-modal-overlay" on:click=move |_| on_close.run(())>
                <div class="pin-modal" on:click=move |e| e.stop_propagation()>
                    <Show
                        when=move || success.get()
                        fallback=move || {
                            view! {
                                <h2>"Change PIN"</h2>

                                <div class="pin-form">
                                    <Show when=move || !is_engineer fallback=|| ()>
                                        <label>"Current PIN"</label>
                                        {pin_dots(current_pin, 0)}
                                    </Show>

                                    <label>"New PIN"</label>
                                    {pin_dots(new_pin, 1)}

                                    <label>"Confirm PIN"</label>
                                    {pin_dots(confirm_pin, 2)}
                                </div>

                                <Show when=move || !error_msg.get().is_empty() fallback=|| ()>
                                    <div class="pin-error">{move || error_msg.get()}</div>
                                </Show>

                                <div class="pin-numpad">
                                    {(1..=9)
                                        .map(|d| {
                                            let digit = std::char::from_digit(d, 10).unwrap();
                                            view! {
                                                <button
                                                    class="numpad-btn"
                                                    on:click=move |_| on_numpad(digit)
                                                    disabled=move || loading.get()
                                                >
                                                    {d.to_string()}
                                                </button>
                                            }
                                        })
                                        .collect::<Vec<_>>()}
                                    <button class="numpad-btn" on:click=on_backspace disabled=move || loading.get()>
                                        "\u{232B}"
                                    </button>
                                    <button
                                        class="numpad-btn"
                                        on:click=move |_| on_numpad('0')
                                        disabled=move || loading.get()
                                    >
                                        "0"
                                    </button>
                                    <button
                                        class="numpad-btn save"
                                        on:click=on_save
                                        disabled=move || loading.get() || confirm_pin.get().len() != 4
                                    >
                                        "\u{2713}"
                                    </button>
                                </div>

                                <div class="pin-actions">
                                    <button class="pin-cancel" on:click=move |_| on_close.run(())>
                                        "Cancel"
                                    </button>
                                </div>
                            }
                        }
                    >
                        <div class="pin-success">
                            <div class="pin-success-icon">"\u{2713}"</div>
                            <h2>"PIN Changed"</h2>
                            <p>"Your PIN has been updated."</p>
                            <button class="pin-done" on:click=move |_| on_close.run(())>
                                "Done"
                            </button>
                        </div>
                    </Show>
                </div>
            </div>
        </Show>
    }
}
