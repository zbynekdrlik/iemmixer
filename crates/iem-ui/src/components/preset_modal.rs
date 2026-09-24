//! Preset modal component
//!
//! Presets are stored server-side and synced across devices.

use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;

use crate::auth::get_token;
use crate::components::confirm_dialog::ConfirmDialog;

/// Preset data used by the mixer to apply channel states
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetData {
    /// Track index -> channel state
    pub channels: std::collections::HashMap<usize, ChannelState>,
    /// Timestamp when preset was created (seconds since epoch)
    #[serde(default)]
    pub created_at: Option<i64>,
    /// Timestamp when preset was last updated (seconds since epoch)
    #[serde(default)]
    pub updated_at: Option<i64>,
    /// Stems bus volume in dB (None if no stems bus)
    #[serde(default)]
    pub stems_level_db: Option<f32>,
    /// EQ band data per track (None if no EQ was loaded during this session)
    #[serde(default)]
    pub eq_bands: Option<std::collections::HashMap<usize, Vec<EqBandPreset>>>,
}

/// Channel state in a preset
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelState {
    pub vol: f32,
    pub mute: bool,
    pub pan: f32,
}

/// Preset info from the server API
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PresetInfo {
    name: String,
    channel_count: usize,
    created_at: i64,
    updated_at: i64,
}

/// Full preset entry from the server API
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PresetEntry {
    name: String,
    channels: std::collections::HashMap<usize, ChannelState>,
    created_at: i64,
    updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stems_level_db: Option<f32>,
    /// EQ band data per track (reaperiem#205 — previously dropped on load).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    eq_bands: Option<std::collections::HashMap<usize, Vec<EqBandPreset>>>,
}

fn default_true() -> bool {
    true
}

/// EQ band data for preset save (mirrors iem_core::EqBand)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EqBandPreset {
    pub band_type: String,
    pub freq_hz: f32,
    pub gain_db: f32,
    pub bw: f32,
    pub freq_norm: f32,
    pub gain_norm: f32,
    pub bw_norm: f32,
    /// Whether the band is enabled (reaperiem#205 — round-trips the disabled state so a
    /// disabled band re-disables on load, matching the server restore path).
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Request to save a preset
#[derive(Serialize)]
struct SavePresetRequest {
    name: String,
    channels: std::collections::HashMap<usize, ChannelState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stems_level_db: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    eq_bands: Option<std::collections::HashMap<usize, Vec<EqBandPreset>>>,
}

/// Format timestamp for display in Slovak format (DD.MM. HH:MM)
fn format_timestamp(ts: i64) -> String {
    // Server timestamps are in seconds, JS Date expects milliseconds
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64((ts * 1000) as f64));
    let month = date.get_month() + 1;
    let day = date.get_date();
    let hours = date.get_hours();
    let mins = date.get_minutes();
    format!("{}.{}. {:02}:{:02}", day, month, hours, mins)
}

/// Fetch all presets from server
async fn fetch_presets(member_id: &str) -> Result<Vec<PresetInfo>, String> {
    let token = get_token().ok_or("Not authenticated")?;
    let url = format!("/api/presets/{}", member_id);

    let resp = gloo_net::http::Request::get(&url)
        .header("Authorization", &format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if resp.ok() {
        resp.json().await.map_err(|e| format!("Parse error: {}", e))
    } else {
        Err(format!("Server error: {}", resp.status()))
    }
}

/// Get a specific preset from server
async fn fetch_preset(member_id: &str, name: &str) -> Result<PresetEntry, String> {
    let token = get_token().ok_or("Not authenticated")?;
    let url = format!("/api/presets/{}/{}", member_id, encode_name(name));

    let resp = gloo_net::http::Request::get(&url)
        .header("Authorization", &format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if resp.ok() {
        resp.json().await.map_err(|e| format!("Parse error: {}", e))
    } else {
        Err(format!("Server error: {}", resp.status()))
    }
}

/// Save a preset to server
async fn save_preset_api(
    member_id: &str,
    name: &str,
    channels: std::collections::HashMap<usize, ChannelState>,
    stems_level_db: Option<f32>,
    eq_bands: Option<std::collections::HashMap<usize, Vec<EqBandPreset>>>,
) -> Result<(), String> {
    let token = get_token().ok_or("Not authenticated")?;
    let url = format!("/api/presets/{}", member_id);

    let req = SavePresetRequest {
        name: name.to_string(),
        channels,
        stems_level_db,
        eq_bands,
    };

    let resp = gloo_net::http::Request::post(&url)
        .header("Authorization", &format!("Bearer {}", token))
        .json(&req)
        .map_err(|e| format!("Request error: {}", e))?
        .send()
        .await
        .map_err(|_| "Chyba siete — preset sa neuložil.".to_string())?;

    if resp.ok() {
        Ok(())
    } else {
        Err(preset_error_message(resp).await)
    }
}

/// Update a preset on server
async fn update_preset_api(
    member_id: &str,
    name: &str,
    channels: std::collections::HashMap<usize, ChannelState>,
    stems_level_db: Option<f32>,
    eq_bands: Option<std::collections::HashMap<usize, Vec<EqBandPreset>>>,
) -> Result<(), String> {
    let token = get_token().ok_or("Not authenticated")?;
    let url = format!("/api/presets/{}/{}", member_id, encode_name(name));

    let req = SavePresetRequest {
        name: name.to_string(),
        channels,
        stems_level_db,
        eq_bands,
    };

    let resp = gloo_net::http::Request::put(&url)
        .header("Authorization", &format!("Bearer {}", token))
        .json(&req)
        .map_err(|e| format!("Request error: {}", e))?
        .send()
        .await
        .map_err(|_| "Chyba siete — preset sa neuložil.".to_string())?;

    if resp.ok() {
        Ok(())
    } else {
        Err(preset_error_message(resp).await)
    }
}

/// Delete a preset from server
async fn delete_preset_api(member_id: &str, name: &str) -> Result<(), String> {
    let token = get_token().ok_or("Not authenticated")?;
    let url = format!("/api/presets/{}/{}", member_id, encode_name(name));

    let resp = gloo_net::http::Request::delete(&url)
        .header("Authorization", &format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if resp.ok() {
        Ok(())
    } else {
        Err(format!("Server error: {}", resp.status()))
    }
}

/// Turn a failed preset request into a human Slovak message (reaperiem#205 — the UI used
/// to render the bare HTTP status, e.g. "Server error: 409" for the 20-preset
/// limit). Reads the server's structured `ApiError` body for context.
async fn preset_error_message(resp: gloo_net::http::Response) -> String {
    #[derive(Deserialize)]
    struct ApiErr {
        #[serde(default)]
        message: String,
    }
    let status = resp.status();
    let server_msg = resp.json::<ApiErr>().await.ok().map(|e| e.message);
    match status {
        409 => "Dosiahli ste maximum 20 presetov. Najprv niektorý zmažte.".to_string(),
        400 => server_msg
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "Neplatná požiadavka.".to_string()),
        401 | 403 => "Prihlásenie vypršalo. Obnovte stránku a skúste znova.".to_string(),
        _ => format!("Chyba servera ({}). Skúste to znova.", status),
    }
}

/// URL-encode preset name for use in path segments
fn encode_name(name: &str) -> String {
    js_sys::encode_uri_component(name)
        .as_string()
        .unwrap_or_else(|| name.to_string())
}

/// A destructive action awaiting user confirmation (reaperiem#206).
#[derive(Clone)]
enum PendingAction {
    /// Overwrite an existing preset with the current mix.
    Overwrite(String),
    /// Delete a preset.
    Delete(String),
}

/// Preset modal component
#[component]
pub fn PresetModal(
    /// Whether modal is visible
    visible: ReadSignal<bool>,
    /// Member ID for preset storage
    member_id: String,
    /// Whether the WebSocket to REAPER is connected (load is blocked otherwise, reaperiem#205)
    connected: ReadSignal<bool>,
    /// Called to close modal
    on_close: Callback<()>,
    /// Called when a preset is loaded
    on_load: Callback<PresetData>,
    /// Called to get current channel states for saving
    get_current_state: Callback<(), PresetData>,
) -> impl IntoView {
    let (presets, set_presets) = signal(Vec::<PresetInfo>::new());
    let (new_name, set_new_name) = signal(String::new());
    let (loading, set_loading) = signal(false);
    let (error, set_error) = signal(Option::<String>::None);
    // reaperiem#206: confirmation dialog state (overwrite / delete).
    let (confirm_visible, set_confirm_visible) = signal(false);
    let (confirm_title, set_confirm_title) = signal(String::new());
    let (confirm_body, set_confirm_body) = signal(String::new());
    let (pending, set_pending) = signal(Option::<PendingAction>::None);
    let member_id_stored = StoredValue::new(member_id);

    // The actual overwrite/delete operations, callable from the confirm dialog.
    let do_overwrite = Callback::new(move |name: String| {
        let state = get_current_state.run(());
        let member_id = member_id_stored.get_value();
        let _ = set_loading.try_set(true);
        let _ = set_error.try_set(None);
        wasm_bindgen_futures::spawn_local(async move {
            match update_preset_api(
                &member_id,
                &name,
                state.channels,
                state.stems_level_db,
                state.eq_bands,
            )
            .await
            {
                Ok(()) => {
                    if let Ok(list) = fetch_presets(&member_id).await {
                        let _ = set_presets.try_set(list);
                    }
                    let _ = set_new_name.try_set(String::new());
                }
                Err(e) => {
                    let _ = set_error.try_set(Some(e));
                }
            }
            let _ = set_loading.try_set(false);
        });
    });
    let do_delete = Callback::new(move |name: String| {
        let member_id = member_id_stored.get_value();
        let _ = set_loading.try_set(true);
        let _ = set_error.try_set(None);
        wasm_bindgen_futures::spawn_local(async move {
            match delete_preset_api(&member_id, &name).await {
                Ok(()) => {
                    if let Ok(list) = fetch_presets(&member_id).await {
                        let _ = set_presets.try_set(list);
                    }
                }
                Err(e) => {
                    let _ = set_error.try_set(Some(e));
                }
            }
            let _ = set_loading.try_set(false);
        });
    });

    // Refresh presets when modal opens
    Effect::new(move |_| {
        if visible.get() {
            let member_id = member_id_stored.get_value();
            let _ = set_loading.try_set(true);
            let _ = set_error.try_set(None);

            // try_update on every signal write — modal can close during
            // the fetch and dispose the surrounding scope. reaperiem#153
            wasm_bindgen_futures::spawn_local(async move {
                match fetch_presets(&member_id).await {
                    Ok(list) => {
                        let _ = set_presets.try_set(list);
                        let _ = set_loading.try_set(false);
                    }
                    Err(e) => {
                        let _ = set_error.try_set(Some(e));
                        let _ = set_loading.try_set(false);
                    }
                }
            });
        }
    });

    let handle_save = move |_| {
        // reaperiem#205: clear any stale error on every attempt.
        let _ = set_error.try_set(None);
        let name = new_name.get().trim().to_string();
        if name.is_empty() {
            let _ = set_error.try_set(Some("Zadajte názov presetu.".to_string()));
            return;
        }

        // reaperiem#206: saving under an existing name would silently overwrite it —
        // confirm first.
        if presets.get_untracked().iter().any(|p| p.name == name) {
            let _ = set_confirm_title.try_set("Prepísať preset?".to_string());
            let _ = set_confirm_body.try_set(format!(
                "Preset {} už existuje. Prepísať ho aktuálnym mixom?",
                name
            ));
            let _ = set_pending.try_set(Some(PendingAction::Overwrite(name)));
            let _ = set_confirm_visible.try_set(true);
            return;
        }

        let state = get_current_state.run(());
        let member_id = member_id_stored.get_value();
        let _ = set_loading.try_set(true);

        wasm_bindgen_futures::spawn_local(async move {
            match save_preset_api(
                &member_id,
                &name,
                state.channels,
                state.stems_level_db,
                state.eq_bands,
            )
            .await
            {
                Ok(()) => {
                    // Refresh list
                    if let Ok(list) = fetch_presets(&member_id).await {
                        let _ = set_presets.try_set(list);
                    }
                    let _ = set_new_name.try_set(String::new());
                }
                Err(e) => {
                    let _ = set_error.try_set(Some(e));
                }
            }
            let _ = set_loading.try_set(false);
        });
    };

    let handle_input = move |ev: web_sys::Event| {
        let target = ev.target().unwrap();
        let input = target.dyn_into::<web_sys::HtmlInputElement>().unwrap();
        let _ = set_new_name.try_set(input.value());
    };

    let handle_overlay_click = move |ev: web_sys::MouseEvent| {
        let target = ev.target().unwrap();
        if let Ok(elem) = target.dyn_into::<web_sys::HtmlElement>()
            && elem.class_list().contains("modal-overlay")
        {
            on_close.run(());
        }
    };

    view! {
        <>
        <div
            class=move || if visible.get() { "modal-overlay visible" } else { "modal-overlay" }
            on:click=handle_overlay_click
        >
            <div class="modal">
                <button class="modal-close" on:click=move |_| on_close.run(())>
                    "\u{00D7}"
                </button>
                <h2>"Presety"</h2>

                <Show when=move || loading.get() fallback=|| ()>
                    <div class="snapshot-loading">
                        <div class="spinner"></div>
                    </div>
                </Show>

                <Show when=move || error.get().is_some() fallback=|| ()>
                    <div class="snapshot-error">
                        {move || error.get().unwrap_or_default()}
                    </div>
                </Show>

                <div class="preset-list">
                    {move || {
                        let current_presets = presets.get();
                        if current_presets.is_empty() && !loading.get() {
                            view! {
                                <div class="no-presets">"Zatiaľ žiadne uložené presety"</div>
                            }.into_any()
                        } else {
                            view! {
                                <>
                                    {current_presets.into_iter().map(|info| {
                                        let name_load = info.name.clone();
                                        let name_overwrite = info.name.clone();
                                        let name_delete = info.name.clone();
                                        let updated_at = info.updated_at;

                                        view! {
                                            <div class="preset-item">
                                                <div class="preset-info">
                                                    <span class="name">{info.name.clone()}</span>
                                                    <span class="preset-timestamp">{format_timestamp(updated_at)}</span>
                                                </div>
                                                <div class="preset-actions">
                                                    <button
                                                        class="load-preset"
                                                        on:click=move |_| {
                                                            // reaperiem#205: block load when disconnected, with a Slovak message.
                                                            if !connected.get() {
                                                                let _ = set_error.try_set(Some(
                                                                    "Nie ste pripojení k REAPERu — preset sa nedá načítať.".to_string(),
                                                                ));
                                                                return;
                                                            }
                                                            let _ = set_error.try_set(None);
                                                            let member_id = member_id_stored.get_value();
                                                            let name = name_load.clone();
                                                            wasm_bindgen_futures::spawn_local(async move {
                                                                match fetch_preset(&member_id, &name).await {
                                                                    Ok(entry) => {
                                                                        let data = PresetData {
                                                                            channels: entry.channels.into_iter().map(|(k, v)| {
                                                                                (k, ChannelState { vol: v.vol, mute: v.mute, pan: v.pan })
                                                                            }).collect(),
                                                                            created_at: Some(entry.created_at),
                                                                            updated_at: Some(entry.updated_at),
                                                                            stems_level_db: entry.stems_level_db,
                                                                            eq_bands: entry.eq_bands,
                                                                        };
                                                                        on_load.run(data);
                                                                        on_close.run(());
                                                                    }
                                                                    Err(e) => {
                                                                        let _ = set_error.try_set(Some(
                                                                            format!("Preset sa nepodarilo načítať: {}", e),
                                                                        ));
                                                                    }
                                                                }
                                                            });
                                                        }
                                                    >
                                                        "Načítať"
                                                    </button>
                                                    <button
                                                        class="update-preset"
                                                        on:click=move |_| {
                                                            let name = name_overwrite.clone();
                                                            let _ = set_confirm_title.try_set("Prepísať preset?".to_string());
                                                            let _ = set_confirm_body.try_set(format!(
                                                                "Preset {} sa prepíše aktuálnym mixom.",
                                                                name
                                                            ));
                                                            let _ = set_pending.try_set(Some(PendingAction::Overwrite(name)));
                                                            let _ = set_confirm_visible.try_set(true);
                                                        }
                                                    >
                                                        "Prepísať"
                                                    </button>
                                                    <button
                                                        class="delete-preset"
                                                        on:click=move |_| {
                                                            let name = name_delete.clone();
                                                            let _ = set_confirm_title.try_set("Zmazať preset?".to_string());
                                                            let _ = set_confirm_body.try_set(format!(
                                                                "Preset {} sa natrvalo zmaže.",
                                                                name
                                                            ));
                                                            let _ = set_pending.try_set(Some(PendingAction::Delete(name)));
                                                            let _ = set_confirm_visible.try_set(true);
                                                        }
                                                    >
                                                        "Zmazať"
                                                    </button>
                                                </div>
                                            </div>
                                        }
                                    }).collect::<Vec<_>>()}
                                </>
                            }.into_any()
                        }
                    }}
                </div>

                <div class="preset-input-row">
                    <input
                        type="text"
                        class="preset-input"
                        placeholder="Názov presetu…"
                        maxlength="30"
                        prop:value=move || new_name.get()
                        on:input=handle_input
                    />
                    <button class="preset-save-btn" on:click=handle_save disabled=move || loading.get()>
                        "Uložiť ako nový"
                    </button>
                </div>
            </div>
        </div>

        <ConfirmDialog
            visible=confirm_visible
            title=confirm_title
            body=confirm_body
            on_confirm=Callback::new(move |_: ()| {
                let _ = set_confirm_visible.try_set(false);
                if let Some(action) = pending.get_untracked() {
                    match action {
                        PendingAction::Overwrite(n) => do_overwrite.run(n),
                        PendingAction::Delete(n) => do_delete.run(n),
                    }
                }
                let _ = set_pending.try_set(None);
            })
            on_cancel=Callback::new(move |_: ()| {
                let _ = set_confirm_visible.try_set(false);
                let _ = set_pending.try_set(None);
            })
        />
        </>
    }
}
