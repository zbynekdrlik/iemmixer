//! The band-activity banner on engineer pages (program spec §4.2): while
//! iemmixer runs for development and the band starts playing, the engineer
//! sees it here (and gets one push) with the "Back to REAPER" switch — the
//! engineer PIN confirms it, the server runs the site's switch command.

use leptos::prelude::*;
use wasm_bindgen::JsCast;

/// The message for a failed switch request (`status` = HTTP status).
pub fn switch_error_message(status: u16) -> String {
    match status {
        401 => "Nesprávny PIN.".to_string(),
        403 => "Prepnúť môže len inžinier.".to_string(),
        404 => "Prepínanie nie je na tomto mieste nastavené.".to_string(),
        429 => "Priveľa pokusov. Skús to o chvíľu.".to_string(),
        other => format!("Prepnutie zlyhalo ({other})."),
    }
}

/// Whether `pin` is a PIN the switch can send (four digits).
pub fn pin_ready(pin: &str) -> bool {
    pin.len() == 4 && pin.bytes().all(|b| b.is_ascii_digit())
}

#[derive(Debug, Clone, PartialEq)]
enum SwitchState {
    Idle,
    Asking,
    Sending,
    Started,
    Failed(String),
}

#[component]
pub fn ActivityBanner(
    /// (the band is playing, the switch is configured)
    activity: ReadSignal<(bool, bool)>,
) -> impl IntoView {
    let (state, set_state) = signal(SwitchState::Idle);
    let (pin, set_pin) = signal(String::new());

    let submit = move || {
        let p = pin.get_untracked();
        if !pin_ready(&p) {
            return;
        }
        let _ = set_state.try_set(SwitchState::Sending);
        wasm_bindgen_futures::spawn_local(async move {
            let next = match crate::api::back_to_reaper(&p).await {
                Ok(()) => SwitchState::Started,
                Err(msg) => SwitchState::Failed(msg),
            };
            let _ = set_state.try_set(next);
            let _ = set_pin.try_set(String::new());
        });
    };

    // Closing unmounts the dialog: after the click has finished bubbling.
    let cancel = move || {
        crate::components::after_event(move || {
            let _ = set_pin.try_set(String::new());
            let _ = set_state.try_set(SwitchState::Idle);
        });
    };

    let active = move || activity.get().0;
    let can_switch = move || activity.get().1;
    let asking = move || {
        matches!(
            state.get(),
            SwitchState::Asking | SwitchState::Sending | SwitchState::Failed(_)
        )
    };

    view! {
        <Show when=active fallback=|| ()>
            <div class="band-activity-banner" data-testid="band-activity">
                "Kapela hrá — mixuje iemmixer (vývoj)"
                <Show when=can_switch fallback=|| ()>
                    <button
                        class="settings-action-btn back-to-reaper-btn"
                        on:click=move |_| { let _ = set_state.try_set(SwitchState::Asking); }
                    >
                        "Späť na REAPER"
                    </button>
                </Show>
                {move || (state.get() == SwitchState::Started).then(|| view! {
                    <div class="band-activity-status">"Prepína sa na REAPER…"</div>
                })}
            </div>
        </Show>
        <Show when=asking fallback=|| ()>
            <div class="pin-modal-overlay" on:click=move |_| cancel()>
                <div class="pin-modal" on:click=move |e| e.stop_propagation()>
                    <h2>"Späť na REAPER?"</h2>
                    <p>"iemmixer sa zastaví a spustí sa REAPER. Potvrď PIN-om inžiniera."</p>
                    <input
                        type="password"
                        inputmode="numeric"
                        maxlength="4"
                        class="pin-input"
                        data-testid="switch-pin"
                        prop:value=move || pin.get()
                        on:input=move |ev: web_sys::Event| {
                            if let Some(input) = ev
                                .target()
                                .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                            {
                                let _ = set_pin.try_set(input.value());
                            }
                        }
                    />
                    {move || match state.get() {
                        SwitchState::Failed(msg) => Some(view! { <div class="pin-error">{msg}</div> }),
                        _ => None,
                    }}
                    <div class="confirm-actions">
                        <button
                            class="settings-action-btn"
                            on:click=move |_| cancel()
                        >
                            "Zrušiť"
                        </button>
                        <button
                            class="settings-action-btn back-to-reaper-confirm"
                            disabled=move || !pin_ready(&pin.get()) || state.get() == SwitchState::Sending
                            on:click=move |_| submit()
                        >
                            "Prepnúť"
                        </button>
                    </div>
                </div>
            </div>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_errors_read_as_sentences() {
        assert_eq!(switch_error_message(401), "Nesprávny PIN.");
        assert!(switch_error_message(403).contains("inžinier"));
        assert!(switch_error_message(404).contains("nastavené"));
        assert!(switch_error_message(429).starts_with("Priveľa"));
        assert_eq!(switch_error_message(500), "Prepnutie zlyhalo (500).");
    }

    #[test]
    fn only_four_digits_are_sent() {
        assert!(pin_ready("0000"));
        assert!(pin_ready("4821"));
        assert!(!pin_ready("482"));
        assert!(!pin_ready("48210"));
        assert!(!pin_ready("48a1"));
        assert!(!pin_ready(""));
    }
}
