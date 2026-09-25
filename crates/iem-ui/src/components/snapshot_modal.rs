//! Snapshot history modal component
//!
//! Displays mix snapshot history with restore, pin, and delete functionality.

use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;

use crate::auth::get_token;
use crate::components::confirm_dialog::ConfirmDialog;

/// Snapshot info from the API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotInfo {
    pub timestamp: i64,
    pub label: String,
    pub pinned: bool,
    pub channel_count: usize,
}

/// Slovak day names
const SLOVAK_DAYS: [&str; 7] = [
    "Nedeľa", "Pondelok", "Utorok", "Streda", "Štvrtok", "Piatok", "Sobota",
];

/// Format timestamp with Slovak day name for primary display (e.g., "Nedeľa 5.3. 14:30")
fn format_timestamp_with_day(ts: i64) -> String {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64((ts * 1000) as f64));
    let day_of_week = date.get_day() as usize; // 0=Sunday
    let day = date.get_date();
    let month = date.get_month() + 1;
    let hours = date.get_hours();
    let mins = date.get_minutes();
    format!(
        "{} {}.{}. {:02}:{:02}",
        SLOVAK_DAYS[day_of_week], day, month, hours, mins
    )
}

/// Format relative time (e.g., "2 hours ago", "yesterday")
fn format_relative(ts: i64) -> String {
    let now = (js_sys::Date::now() / 1000.0) as i64;
    let diff = now - ts;

    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        let mins = diff / 60;
        if mins == 1 {
            "1 minute ago".to_string()
        } else {
            format!("{} minutes ago", mins)
        }
    } else if diff < 86400 {
        let hours = diff / 3600;
        if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{} hours ago", hours)
        }
    } else if diff < 172800 {
        "yesterday".to_string()
    } else {
        let days = diff / 86400;
        format!("{} days ago", days)
    }
}

/// Fetch snapshots from API
async fn fetch_snapshots(member_id: &str) -> Result<Vec<SnapshotInfo>, String> {
    let token = get_token().ok_or("Not authenticated")?;
    let url = format!("/api/snapshots/{}", member_id);

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

/// Create a manual snapshot
async fn create_snapshot(member_id: &str, label: Option<String>) -> Result<(), String> {
    let token = get_token().ok_or("Not authenticated")?;
    let url = format!("/api/snapshots/{}", member_id);

    #[derive(Serialize)]
    struct CreateReq {
        label: Option<String>,
    }

    let resp = gloo_net::http::Request::post(&url)
        .header("Authorization", &format!("Bearer {}", token))
        .json(&CreateReq { label })
        .map_err(|e| format!("Request error: {}", e))?
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if resp.ok() {
        Ok(())
    } else {
        Err(format!("Server error: {}", resp.status()))
    }
}

/// Delete a snapshot
async fn delete_snapshot(member_id: &str, timestamp: i64) -> Result<(), String> {
    let token = get_token().ok_or("Not authenticated")?;
    let url = format!("/api/snapshots/{}/{}", member_id, timestamp);

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

/// Restore a snapshot
async fn restore_snapshot(member_id: &str, timestamp: i64) -> Result<(), String> {
    let token = get_token().ok_or("Not authenticated")?;
    let url = format!("/api/snapshots/{}/{}/restore", member_id, timestamp);

    let resp = gloo_net::http::Request::post(&url)
        .header("Authorization", &format!("Bearer {}", token))
        .send()
        .await
        .map_err(|_| "Chyba siete — snapshot sa nedal obnoviť.".to_string())?;

    if resp.ok() {
        Ok(())
    } else {
        Err(format!("Chyba servera ({}).", resp.status()))
    }
}

/// Pin a snapshot (protects it from the 50-snapshot prune). (reaperiem#206)
async fn pin_snapshot(member_id: &str, timestamp: i64, label: String) -> Result<(), String> {
    let token = get_token().ok_or("Not authenticated")?;
    let url = format!("/api/snapshots/{}/{}/pin", member_id, timestamp);

    #[derive(Serialize)]
    struct PinReq {
        label: String,
    }

    let resp = gloo_net::http::Request::post(&url)
        .header("Authorization", &format!("Bearer {}", token))
        .json(&PinReq { label })
        .map_err(|e| format!("Request error: {}", e))?
        .send()
        .await
        .map_err(|_| "Chyba siete — snapshot sa nedal pripnúť.".to_string())?;

    if resp.ok() {
        Ok(())
    } else {
        Err(format!("Chyba servera ({}).", resp.status()))
    }
}

/// Unpin a snapshot. (reaperiem#206)
async fn unpin_snapshot(member_id: &str, timestamp: i64) -> Result<(), String> {
    let token = get_token().ok_or("Not authenticated")?;
    let url = format!("/api/snapshots/{}/{}/unpin", member_id, timestamp);

    let resp = gloo_net::http::Request::post(&url)
        .header("Authorization", &format!("Bearer {}", token))
        .send()
        .await
        .map_err(|_| "Chyba siete — snapshot sa nedal odopnúť.".to_string())?;

    if resp.ok() {
        Ok(())
    } else {
        Err(format!("Chyba servera ({}).", resp.status()))
    }
}

/// Snapshot history modal component
#[component]
pub fn SnapshotModal(
    /// Whether modal is visible
    visible: ReadSignal<bool>,
    /// Member ID for snapshot storage
    member_id: String,
    /// Called to close modal
    on_close: Callback<()>,
) -> impl IntoView {
    let (snapshots, set_snapshots) = signal(Vec::<SnapshotInfo>::new());
    let (loading, set_loading) = signal(false);
    let (error, set_error) = signal(Option::<String>::None);
    // reaperiem#206: confirmation dialog state for delete.
    let (confirm_visible, set_confirm_visible) = signal(false);
    let (confirm_title, set_confirm_title) = signal(String::new());
    let (confirm_body, set_confirm_body) = signal(String::new());
    let (pending_delete, set_pending_delete) = signal(Option::<i64>::None);
    let member_id_stored = StoredValue::new(member_id);

    // The actual delete, called from the confirm dialog. (reaperiem#206)
    let do_delete = Callback::new(move |timestamp: i64| {
        let member_id = member_id_stored.get_value();
        let _ = set_loading.try_set(true);
        let _ = set_error.try_set(None);
        wasm_bindgen_futures::spawn_local(async move {
            match delete_snapshot(&member_id, timestamp).await {
                Ok(()) => {
                    if let Ok(list) = fetch_snapshots(&member_id).await {
                        let _ = set_snapshots.try_set(list);
                    }
                }
                Err(e) => {
                    let _ = set_error.try_set(Some(e));
                }
            }
            let _ = set_loading.try_set(false);
        });
    });

    // Refresh snapshots when modal opens
    Effect::new(move |_| {
        if visible.get() {
            let member_id = member_id_stored.get_value();
            let _ = set_loading.try_set(true);
            let _ = set_error.try_set(None);

            // try_update: modal can close mid-await. reaperiem#153
            wasm_bindgen_futures::spawn_local(async move {
                match fetch_snapshots(&member_id).await {
                    Ok(list) => {
                        let _ = set_snapshots.try_set(list);
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

    let handle_save_now = move |_| {
        let member_id = member_id_stored.get_value();
        let _ = set_loading.try_set(true);

        wasm_bindgen_futures::spawn_local(async move {
            // try_update: modal can close mid-await. reaperiem#153
            match create_snapshot(&member_id, Some("manual".to_string())).await {
                Ok(()) => {
                    if let Ok(list) = fetch_snapshots(&member_id).await {
                        let _ = set_snapshots.try_set(list);
                    }
                }
                Err(e) => {
                    let _ = set_error.try_set(Some(e));
                }
            }
            let _ = set_loading.try_set(false);
        });
    };

    let handle_overlay_click = move |ev: web_sys::MouseEvent| {
        let target = ev.target().unwrap();
        if let Ok(elem) = target.dyn_into::<web_sys::HtmlElement>()
            && elem.class_list().contains("modal-overlay")
        {
            on_close.run(());
        }
    };

    // Named closure — the `>=` comparison must not sit inline in a view! `when=`
    // attribute, where the macro mis-tokenizes `>` as a tag boundary.
    let at_snapshot_limit = move || snapshots.get().len() >= iem_core::MAX_SNAPSHOTS;

    view! {
        <>
        <div
            class=move || if visible.get() { "modal-overlay visible" } else { "modal-overlay" }
            on:click=handle_overlay_click
        >
            <div class="modal snapshot-modal">
                <button class="modal-close" on:click=move |_| on_close.run(())>
                    "\u{00D7}"
                </button>
                <h2>"História mixu"</h2>

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

                <Show when=at_snapshot_limit fallback=|| ()>
                    <div class="snapshot-limit-notice">
                        {move || format!(
                            "História je plná ({} z {}). Staré nepripnuté snapshoty sa prepisujú — dôležité si pripni.",
                            snapshots.get().len(),
                            iem_core::MAX_SNAPSHOTS,
                        )}
                    </div>
                </Show>

                <div class="snapshot-list">
                    {move || {
                        let current = snapshots.get();
                        if current.is_empty() && !loading.get() {
                            view! {
                                <div class="no-presets">"Zatiaľ žiadne snapshoty. Zmeny sa ukladajú automaticky denne."</div>
                            }.into_any()
                        } else {
                            view! {
                                <>
                                    {current.into_iter().map(|snap| {
                                        let timestamp = snap.timestamp;
                                        let is_pinned = snap.pinned;
                                        let label = snap.label.clone();
                                        let label_for_pin = snap.label.clone();
                                        let ts_label = format_timestamp_with_day(timestamp);
                                        let channel_count = snap.channel_count;

                                        view! {
                                            <div class=move || if is_pinned { "snapshot-item pinned" } else { "snapshot-item" }>
                                                <div class="snapshot-info">
                                                    <div class="snapshot-label">
                                                        {format_timestamp_with_day(timestamp)}
                                                        <span class="snapshot-type">{label.clone()}</span>
                                                        <span class="snapshot-channels">{format!("({} ch)", channel_count)}</span>
                                                    </div>
                                                    <div class="snapshot-time">
                                                        <span class="relative">{format_relative(timestamp)}</span>
                                                    </div>
                                                </div>
                                                <div class="snapshot-actions">
                                                    <button
                                                        class="restore-btn"
                                                        on:click={
                                                            let member_id = member_id_stored.get_value();
                                                            move |_| {
                                                                let member_id = member_id.clone();
                                                                let _ = set_loading.try_set(true);
                                                                wasm_bindgen_futures::spawn_local(async move {
                                                                    // try_update: modal can close mid-await. reaperiem#153
                                                                    if let Err(e) = restore_snapshot(&member_id, timestamp).await {
                                                                        let _ = set_error.try_set(Some(e));
                                                                    }
                                                                    let _ = set_loading.try_set(false);
                                                                    on_close.run(());
                                                                });
                                                            }
                                                        }
                                                    >
                                                        "Obnoviť"
                                                    </button>
                                                    <button
                                                        class=move || if is_pinned { "snapshot-pin-btn pinned" } else { "snapshot-pin-btn" }
                                                        on:click={
                                                            let member_id = member_id_stored.get_value();
                                                            move |_| {
                                                                let member_id = member_id.clone();
                                                                let lbl = label_for_pin.clone();
                                                                let _ = set_loading.try_set(true);
                                                                let _ = set_error.try_set(None);
                                                                wasm_bindgen_futures::spawn_local(async move {
                                                                    let res = if is_pinned {
                                                                        unpin_snapshot(&member_id, timestamp).await
                                                                    } else {
                                                                        pin_snapshot(&member_id, timestamp, lbl).await
                                                                    };
                                                                    match res {
                                                                        Ok(()) => {
                                                                            if let Ok(list) = fetch_snapshots(&member_id).await {
                                                                                let _ = set_snapshots.try_set(list);
                                                                            }
                                                                        }
                                                                        Err(e) => {
                                                                            let _ = set_error.try_set(Some(e));
                                                                        }
                                                                    }
                                                                    let _ = set_loading.try_set(false);
                                                                });
                                                            }
                                                        }
                                                    >
                                                        {if is_pinned { "Odopnúť" } else { "Pripnúť" }}
                                                    </button>
                                                    <button
                                                        class="delete-btn"
                                                        on:click=move |_| {
                                                            let _ = set_confirm_title.try_set("Zmazať snapshot?".to_string());
                                                            let _ = set_confirm_body.try_set(format!(
                                                                "Snapshot z {} sa natrvalo zmaže.",
                                                                ts_label
                                                            ));
                                                            let _ = set_pending_delete.try_set(Some(timestamp));
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

                <button class="snapshot-save-btn" on:click=handle_save_now disabled=move || loading.get()>
                    "Uložiť teraz"
                </button>
            </div>
        </div>

        <ConfirmDialog
            visible=confirm_visible
            title=confirm_title
            body=confirm_body
            on_confirm=Callback::new(move |_: ()| {
                let _ = set_confirm_visible.try_set(false);
                if let Some(ts) = pending_delete.get_untracked() {
                    do_delete.run(ts);
                }
                let _ = set_pending_delete.try_set(None);
            })
            on_cancel=Callback::new(move |_: ()| {
                let _ = set_confirm_visible.try_set(false);
                let _ = set_pending_delete.try_set(None);
            })
        />
        </>
    }
}
