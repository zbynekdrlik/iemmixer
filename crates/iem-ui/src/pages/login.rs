//! Login page with PIN numpad

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use std::rc::Rc;
use wasm_bindgen_futures::spawn_local;

use crate::api;
use crate::auth::save_auth;
use leptos::html;

/// Login page with PIN numpad
#[component]
pub fn LoginPage() -> impl IntoView {
    let query = use_query_map();
    let navigate = use_navigate();
    let navigate_back = navigate.clone();

    // Get member and next URL from query params
    let member = move || {
        query
            .get()
            .get("member")
            .map(|s| s.to_string())
            .unwrap_or_default()
    };
    let next = move || {
        query
            .get()
            .get("next")
            .map(|s| s.to_string())
            .unwrap_or_else(|| "/".to_string())
    };

    // PIN state
    let (pin, set_pin) = signal(String::new());
    let (error, set_error) = signal(Option::<String>::None);
    let (loading, set_loading) = signal(false);

    // Wrap the digit handler in Rc for cloning
    let handle_digit = Rc::new(move |digit: char| {
        let _ = set_error.try_set(None);
        let mut current_pin = pin.get();
        if current_pin.len() < 4 {
            current_pin.push(digit);
            let _ = set_pin.try_set(current_pin.clone());
        }

        // Auto-submit when 4 digits entered
        if current_pin.len() == 4 {
            let member_val = member();
            let next_val = next();
            let pin_val = current_pin;
            let nav = navigate.clone();

            let _ = set_loading.try_set(true);

            // try_update: the login page can unmount mid-await (e.g. the
            // user hits the back button while the request is in flight). reaperiem#153
            spawn_local(async move {
                match api::login(&member_val, &pin_val).await {
                    Ok(auth) => {
                        save_auth(&auth);
                        nav(&next_val, Default::default());
                    }
                    Err(e) => {
                        let _ = set_error.try_set(Some(e));
                        let _ = set_pin.try_set(String::new());
                        let _ = set_loading.try_set(false);
                    }
                }
            });
        }
    });

    // Keyboard support: local handler on login container (scoped to this page only)
    let login_ref = NodeRef::<html::Div>::new();
    let hd_key = handle_digit.clone();

    let on_keydown = move |e: web_sys::KeyboardEvent| {
        let key = e.key();
        match key.as_str() {
            "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" => {
                e.prevent_default();
                hd_key(key.chars().next().unwrap());
            }
            "Backspace" => {
                e.prevent_default();
                let _ = set_error.try_set(None);
                let _ = set_pin.try_update(|p| {
                    p.pop();
                });
            }
            "Escape" | "Delete" => {
                e.prevent_default();
                let _ = set_error.try_set(None);
                let _ = set_pin.try_set(String::new());
            }
            _ => {}
        }
    };

    // Auto-focus the login container so keyboard events are captured immediately
    Effect::new(move || {
        if let Some(el) = login_ref.get() {
            let _ = el.focus();
        }
    });

    // Create clones for each button
    let hd1 = handle_digit.clone();
    let hd2 = handle_digit.clone();
    let hd3 = handle_digit.clone();
    let hd4 = handle_digit.clone();
    let hd5 = handle_digit.clone();
    let hd6 = handle_digit.clone();
    let hd7 = handle_digit.clone();
    let hd8 = handle_digit.clone();
    let hd9 = handle_digit.clone();
    let hd0 = handle_digit.clone();

    view! {
        <div class="app" tabindex="-1" node_ref=login_ref on:keydown=on_keydown>
            <header class="mixer-header">
                <button class="back-btn" on:click=move |_| { navigate_back("/", Default::default()); }>
                    "\u{2190}"
                </button>
                <h1>"IEM Mixer"</h1>
                <div style="width: 40px"></div>
            </header>
            <main class="main">
                <div class="login-container">
                    <div class="login-box">
                        <h2>"Enter PIN"</h2>
                        <p class="subtitle">{move || {
                            let m = member();
                            if m.is_empty() {
                                "Enter your PIN".to_string()
                            } else {
                                format!("PIN for {}", m)
                            }
                        }}</p>

                        // PIN dots
                        <div class="pin-display">
                            {move || {
                                let len = pin.get().len();
                                let has_error = error.get().is_some();
                                (0..4).map(|i| {
                                    let class = if has_error {
                                        "pin-dot error"
                                    } else if i < len {
                                        "pin-dot filled"
                                    } else {
                                        "pin-dot"
                                    };
                                    view! { <div class=class></div> }
                                }).collect::<Vec<_>>()
                            }}
                        </div>

                        // Numpad
                        <div class="numpad">
                            <button class="numpad-btn" on:click=move |_| hd1('1') disabled=loading>"1"</button>
                            <button class="numpad-btn" on:click=move |_| hd2('2') disabled=loading>"2"</button>
                            <button class="numpad-btn" on:click=move |_| hd3('3') disabled=loading>"3"</button>
                            <button class="numpad-btn" on:click=move |_| hd4('4') disabled=loading>"4"</button>
                            <button class="numpad-btn" on:click=move |_| hd5('5') disabled=loading>"5"</button>
                            <button class="numpad-btn" on:click=move |_| hd6('6') disabled=loading>"6"</button>
                            <button class="numpad-btn" on:click=move |_| hd7('7') disabled=loading>"7"</button>
                            <button class="numpad-btn" on:click=move |_| hd8('8') disabled=loading>"8"</button>
                            <button class="numpad-btn" on:click=move |_| hd9('9') disabled=loading>"9"</button>
                            <button
                                class="numpad-btn clear"
                                on:click=move |_| {
                                    let _ = set_error.try_set(None);
                                    let _ = set_pin.try_set(String::new());
                                }
                                disabled=loading
                            >"CLR"</button>
                            <button class="numpad-btn" on:click=move |_| hd0('0') disabled=loading>"0"</button>
                            <button
                                class="numpad-btn backspace"
                                on:click=move |_| {
                                    let _ = set_error.try_set(None);
                                    let _ = set_pin.try_update(|p| { p.pop(); });
                                }
                                disabled=loading
                            >"\u{232B}"</button>
                        </div>

                        // Error message
                        {move || error.get().map(|e| view! {
                            <p class="login-error">{e}</p>
                        })}
                    </div>
                </div>
            </main>
        </div>
    }
}
