//! Settings modal for user preferences (persisted in localStorage)

use gloo_storage::{LocalStorage, Storage};
use leptos::prelude::*;
use leptos_router::hooks::use_navigate;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

fn default_true() -> bool {
    true
}

/// User-configurable settings, persisted per member in localStorage
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UserSettings {
    #[serde(default = "default_true")]
    pub double_tap_fader: bool,
    /// Listen boost in dB (0-24), engineer-only. Applied immediately via GainNode.
    #[serde(default)]
    pub listen_boost_db: f32,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            double_tap_fader: true,
            listen_boost_db: 0.0,
        }
    }
}

impl UserSettings {
    fn storage_key(member_id: &str) -> String {
        format!("iem_settings_{}", member_id)
    }

    pub fn load(member_id: &str) -> Self {
        LocalStorage::get(Self::storage_key(member_id)).unwrap_or_default()
    }

    pub fn save(&self, member_id: &str) {
        let _ = LocalStorage::set(Self::storage_key(member_id), self);
    }
}

/// Settings modal with toggle switches for preferences
#[component]
pub fn SettingsModal(
    /// Whether the modal is visible
    visible: ReadSignal<bool>,
    /// Callback when the modal should close
    on_close: Callback<()>,
    /// Callback to open PIN change modal
    on_open_pin_change: Callback<()>,
    /// Signal for fader double-tap setting
    double_tap_fader: ReadSignal<bool>,
    set_double_tap_fader: WriteSignal<bool>,
    /// Member ID for localStorage persistence
    member_id: String,
    /// Whether the current user is an engineer (shows Audio section)
    #[prop(default = false)]
    is_engineer: bool,
    /// Whether the member has a profile photo (reaperiem#16)
    #[prop(default = false.into())]
    has_photo: Signal<bool>,
    /// Callback to update has_photo state after upload/delete (reaperiem#16)
    #[prop(optional)]
    set_has_photo: Option<WriteSignal<bool>>,
) -> impl IntoView {
    // StoredValue is Copy + Send + Sync — closures inside view! can use it freely
    let member_id = StoredValue::new(member_id);
    let file_input_ref = NodeRef::<leptos::html::Input>::new();

    // Listen boost signal (engineer-only, loaded from settings)
    let initial_boost = if is_engineer {
        UserSettings::load(&member_id.get_value()).listen_boost_db
    } else {
        0.0
    };
    let (listen_boost, set_listen_boost) = signal(initial_boost);

    // Get navigate function at component level (Leptos hooks rule)
    let navigate = use_navigate();

    view! {
        <Show when=move || visible.get() fallback=|| ()>
            <div class="pin-modal-overlay" on:click=move |_| on_close.run(())>
                <div class="pin-modal settings-modal" on:click=move |e| e.stop_propagation()>
                    <button class="modal-close" on:click=move |_| on_close.run(())>
                        "\u{00D7}"
                    </button>
                    <h2>"Settings"</h2>

                    // Profile Photo section (reaperiem#16)
                    <div class="settings-section">
                        <div class="settings-section-title">"Profile Photo"</div>
                        <div class="photo-upload-row">
                            <div class="photo-preview">
                                {move || if has_photo.get() {
                                    let url = format!("/api/members/{}/photo?t={}", member_id.get_value(), js_sys::Date::now() as u64);
                                    view! { <img class="photo-preview-img" src=url /> }.into_any()
                                } else {
                                    let ch = member_id.get_value().chars().next().unwrap_or('?').to_uppercase().to_string();
                                    view! { <span class="photo-preview-initial">{ch}</span> }.into_any()
                                }}
                            </div>
                            <div class="photo-actions">
                                <button class="settings-action-btn" on:click=move |_| {
                                    if let Some(input) = file_input_ref.get() {
                                        let _ = input.click();
                                    }
                                }>
                                    "Change Photo"
                                </button>
                                <Show when=move || has_photo.get() fallback=|| ()>
                                    <button class="settings-action-btn" on:click=move |_| {
                                        let mid = member_id.get_value();
                                        let set_hp = set_has_photo;
                                        wasm_bindgen_futures::spawn_local(async move {
                                            if crate::api::delete_photo(&mid).await.is_ok() && let Some(set) = set_hp {
                                                    // disposal-race-safe: try_set on destructured
                                                    // WriteSignal after .await in spawn_local.
                                                    let _ = set.try_set(false);
                                                }
                                        });
                                    }>
                                        "Remove Photo"
                                    </button>
                                </Show>
                            </div>
                        </div>
                        <input
                            type="file"
                            accept="image/*"
                            style="display: none"
                            node_ref=file_input_ref
                            on:change=move |_| {
                                let Some(input) = file_input_ref.get() else { return };
                                let Some(files) = input.files() else { return };
                                if files.length() == 0 { return; }
                                let Some(file) = files.get(0) else { return };

                                let reader = web_sys::FileReader::new().unwrap();
                                let reader_clone = reader.clone();
                                let mid = member_id.get_value();
                                let set_hp = set_has_photo;

                                let onload = Closure::once(Box::new(move || {
                                    let result = reader_clone.result().unwrap();
                                    let data_url: String = result.as_string().unwrap();

                                    let img = web_sys::HtmlImageElement::new().unwrap();
                                    let img_clone = img.clone();
                                    let onload_img = Closure::once(Box::new(move || {
                                        let w = img_clone.natural_width() as f64;
                                        let h = img_clone.natural_height() as f64;

                                        let doc = web_sys::window().unwrap().document().unwrap();
                                        let canvas: web_sys::HtmlCanvasElement = doc
                                            .create_element("canvas").unwrap()
                                            .unchecked_into();
                                        canvas.set_width(128);
                                        canvas.set_height(128);
                                        let ctx: web_sys::CanvasRenderingContext2d = canvas
                                            .get_context("2d").unwrap().unwrap()
                                            .unchecked_into();

                                        // Center-crop: use shorter dimension as crop size
                                        let crop = w.min(h);
                                        let sx = (w - crop) / 2.0;
                                        let sy = (h - crop) / 2.0;

                                        ctx.draw_image_with_html_image_element_and_sw_and_sh_and_dx_and_dy_and_dw_and_dh(
                                            &img_clone, sx, sy, crop, crop, 0.0, 0.0, 128.0, 128.0,
                                        ).unwrap();

                                        let jpeg_url = canvas
                                            .to_data_url_with_type_and_encoder_options("image/jpeg", &wasm_bindgen::JsValue::from(0.85))
                                            .unwrap();

                                        // Strip "data:image/jpeg;base64," prefix
                                        let base64 = jpeg_url.split(',').nth(1).unwrap_or("").to_string();

                                        wasm_bindgen_futures::spawn_local(async move {
                                            if crate::api::upload_photo(&mid, &base64).await.is_ok() && let Some(set) = set_hp {
                                                    // disposal-race-safe: try_set on destructured
                                                    // WriteSignal after .await in spawn_local.
                                                    let _ = set.try_set(true);
                                                }
                                        });
                                    }) as Box<dyn FnOnce()>);

                                    img.set_onload(Some(onload_img.as_ref().unchecked_ref()));
                                    onload_img.forget();
                                    img.set_src(&data_url);
                                }) as Box<dyn FnOnce()>);

                                reader.set_onload(Some(onload.as_ref().unchecked_ref()));
                                onload.forget();
                                reader.read_as_data_url(&file).unwrap();

                                // Reset input so same file can be re-selected
                                input.set_value("");
                            }
                        />
                    </div>

                    <div class="settings-section">
                        <div class="settings-section-title">"Preferences"</div>

                        <div class="settings-row" on:click=move |_| {
                            let new_val = !double_tap_fader.get_untracked();
                            let _ = set_double_tap_fader.try_set(new_val);
                            let mid = member_id.get_value();
                            let mut settings = UserSettings::load(&mid);
                            settings.double_tap_fader = new_val;
                            settings.save(&mid);
                        }>
                            <div class="settings-label">
                                <div class="settings-name">"Fader double-tap"</div>
                                <div class="settings-desc">"Double-tap fader to animate to 0 dB"</div>
                            </div>
                            <div class=move || if double_tap_fader.get() { "toggle-switch on" } else { "toggle-switch" }>
                                <div class="toggle-knob"></div>
                            </div>
                        </div>
                    </div>

                    {if is_engineer {
                        Some(view! {
                            <div class="settings-section" data-testid="listen-boost-section">
                                <div class="settings-section-title">"Audio"</div>

                                <div class="settings-row listen-boost-row">
                                    <div class="settings-label">
                                        <div class="settings-name">"Listen boost"</div>
                                        <div class="settings-desc">"Applied immediately"</div>
                                    </div>
                                    <div class="listen-boost-stepper">
                                        <button
                                            class="boost-btn"
                                            data-testid="boost-minus"
                                            on:click=move |_| {
                                                let new_val = (listen_boost.get_untracked() - 3.0).max(0.0);
                                                let _ = set_listen_boost.try_set(new_val);
                                                let mid = member_id.get_value();
                                                let mut settings = UserSettings::load(&mid);
                                                settings.listen_boost_db = new_val;
                                                settings.save(&mid);
                                                crate::components::audio_player::set_listen_gain(new_val);
                                            }
                                        >
                                            "\u{2212}"
                                        </button>
                                        <span class="boost-value" data-testid="boost-value">
                                            {move || {
                                                let db = listen_boost.get() as i32;
                                                if db == 0 { "0 dB".to_string() } else { format!("+{} dB", db) }
                                            }}
                                        </span>
                                        <button
                                            class="boost-btn"
                                            data-testid="boost-plus"
                                            on:click=move |_| {
                                                let new_val = (listen_boost.get_untracked() + 3.0).min(24.0);
                                                let _ = set_listen_boost.try_set(new_val);
                                                let mid = member_id.get_value();
                                                let mut settings = UserSettings::load(&mid);
                                                settings.listen_boost_db = new_val;
                                                settings.save(&mid);
                                                crate::components::audio_player::set_listen_gain(new_val);
                                            }
                                        >
                                            "+"
                                        </button>
                                    </div>
                                </div>
                            </div>

                            // Backup & Restore (engineer-only)
                            <crate::components::backup_section::BackupSection />
                        })
                    } else {
                        None
                    }}

                    <div class="settings-section">
                        <div class="settings-section-title">"Security"</div>

                        <button class="settings-action-btn" on:click=move |_| {
                            on_close.run(());
                            on_open_pin_change.run(());
                        }>
                            "Change PIN"
                        </button>
                    </div>

                    <div class="settings-section">
                        <div class="settings-section-title">"Session"</div>

                        <button class="settings-action-btn logout-btn" on:click={
                            let navigate = navigate.clone();
                            move |_| {
                                // Revoke push subscription FIRST so the helper can read the
                                // auth token before clear_auth() wipes it. The helper itself
                                // does this synchronously then spawn_local's an async block —
                                // logout never blocks on it. (reaperiem#188)
                                crate::pages::mixer::push::unsubscribe_from_push();
                                // Clear auth state
                                crate::auth::clear_auth();
                                // Navigate to landing page
                                navigate("/", Default::default());
                            }
                        }>
                            "Logout"
                        </button>
                    </div>
                </div>
            </div>
        </Show>
    }
}
