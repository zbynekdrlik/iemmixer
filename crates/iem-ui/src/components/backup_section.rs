//! Backup & Restore section for the Settings modal (engineer-only)

use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

/// Backup restore section shown in engineer Settings modal.
/// Lists available backups, shows preview on click, allows restore.
#[component]
pub fn BackupSection() -> impl IntoView {
    let (backups, set_backups) = signal(Vec::<iem_core::BackupInfo>::new());
    let (selected, set_selected) = signal(Option::<String>::None);
    let (preview, set_preview) = signal(Option::<iem_core::RestorePreview>::None);
    let (restoring, set_restoring) = signal(false);
    let (elapsed, set_elapsed) = signal(0u32);
    let (result, set_result) = signal(Option::<iem_core::RestoreResult>::None);
    let (error, set_error) = signal(Option::<String>::None);
    let (loading, set_loading) = signal(false);

    // Load backups on mount.
    // Use try_update to guard signal writes against the disposal race — if the
    // Settings modal closes while list_backups is awaiting, the resumed task
    // would panic on a disposed signal. See reaperiem#153 follow-up.
    {
        spawn_local(async move {
            let token = match crate::auth::get_auth() {
                Some(a) => a.token,
                None => return,
            };
            match crate::api::list_backups(&token).await {
                Ok(list) => {
                    let _ = set_backups.try_set(list);
                }
                Err(e) => {
                    let _ = set_error.try_set(Some(e));
                }
            }
        });
    }

    view! {
        <div class="settings-section" data-testid="backup-section">
            <div class="settings-section-title">"Backups"</div>

            // Error display
            {move || error.get().map(|e| view! {
                <div class="backup-error" style="color: #ff6b6b; padding: 8px; font-size: 0.85em;">{e}</div>
            })}

            // Result display
            {move || result.get().map(|r| view! {
                <div class="backup-result" style="color: #51cf66; padding: 8px; font-size: 0.85em;">
                    {format!("Restored {} values", r.restored_count)}
                    {if !r.skipped.is_empty() {
                        format!(", {} skipped", r.skipped.len())
                    } else {
                        String::new()
                    }}
                    {if r.project_saved { " — project saved" } else { " — project NOT saved!" }}
                </div>
            })}

            // Backup list
            <div class="backup-list" style="max-height: 200px; overflow-y: auto; margin: 8px 0;">
                {move || {
                    let list = backups.get();
                    if list.is_empty() {
                        return view! { <div style="color: #888; font-size: 0.85em; padding: 8px;">"No backups available"</div> }.into_any();
                    }
                    list.into_iter().map(|b| {
                        let filename = b.filename.clone();
                        let filename_for_style = filename.clone();
                        let filename_for_click = filename.clone();
                        let display_time = b.timestamp.get(..16).unwrap_or(&b.timestamp).to_string();
                        let meta = format!("{} sends", b.send_count);
                        view! {
                            <div
                                class="settings-row"
                                style=move || format!(
                                    "cursor: pointer; padding: 6px 8px; border-radius: 4px; {}",
                                    if selected.get().as_deref() == Some(&filename_for_style) { "background: rgba(255,255,255,0.1);" } else { "" }
                                )
                                on:click={
                                    let filename = filename_for_click.clone();
                                    move |_| {
                                        let fname = filename.clone();
                                        let _ = set_selected.try_set(Some(fname.clone()));
                                        let _ = set_preview.try_set(None);
                                        let _ = set_result.try_set(None);
                                        let _ = set_error.try_set(None);
                                        let _ = set_loading.try_set(true);
                                        spawn_local(async move {
                                            let token = match crate::auth::get_auth() {
                                                Some(a) => a.token,
                                                None => return,
                                            };
                                            // try_update guards against disposal race
                                            // if the modal closes during preview_restore. reaperiem#153
                                            match crate::api::preview_restore(&token, &fname).await {
                                                Ok(p) => {
                                                    let _ = set_preview
                                                        .try_set(Some(p));
                                                }
                                                Err(e) => {
                                                    let _ = set_error
                                                        .try_set(Some(e));
                                                }
                                            }
                                            let _ = set_loading.try_set(false);
                                        });
                                    }
                                }
                            >
                                <div class="settings-label">
                                    <div class="settings-name" style="font-size: 0.9em;">{display_time}</div>
                                    <div class="settings-desc">{meta}</div>
                                </div>
                            </div>
                        }
                    }).collect_view().into_any()
                }}
            </div>

            // Loading indicator
            {move || loading.get().then(|| view! {
                <div style="padding: 8px; color: #888; font-size: 0.85em;">"Loading preview..."</div>
            })}

            // Preview + Restore button
            {move || preview.get().map(|p| {
                let change_count = p.changes.len();
                let skipped_count = p.skipped.len();
                view! {
                    <div class="backup-preview" style="padding: 8px; border-top: 1px solid rgba(255,255,255,0.1); margin-top: 8px;">
                        <div style="font-size: 0.85em; margin-bottom: 8px;">
                            <div style="color: #51cf66;">
                                {format!("{} values to restore", change_count)}
                            </div>
                            <div style="color: #888;">
                                {format!("{} unchanged", p.unchanged_count)}
                            </div>
                            {(skipped_count > 0).then(|| view! {
                                <div style="color: #fcc419;">
                                    {format!("{} skipped (not found)", skipped_count)}
                                </div>
                            })}
                            {(p.estimated_seconds > 0 && change_count > 0).then(|| {
                                let est = p.estimated_seconds;
                                view! {
                                    <div style="color: #aaa; margin-top: 4px;">
                                        {format!("Estimated time: ~{}s", est)}
                                    </div>
                                }
                            })}
                            {(!p.tracks_in_reaper_not_in_backup.is_empty()).then(|| {
                                let names = p.tracks_in_reaper_not_in_backup.clone();
                                view! {
                                    <div class="preview-panel preview-warning">
                                        <div style="color: #f0ad4e; font-weight: bold; margin-top: 6px; margin-bottom: 2px;">
                                            "⚠ Will NOT restore (tracks not in this backup)"
                                        </div>
                                        <ul style="margin: 0; padding-left: 1.4em; color: #c8a050; font-size: 0.85em;">
                                            {names.into_iter().map(|name| view! {
                                                <li>{name}" — its current state will be unchanged"</li>
                                            }).collect_view()}
                                        </ul>
                                    </div>
                                }
                            })}
                            {(!p.tracks_in_backup_not_in_reaper.is_empty()).then(|| {
                                let names = p.tracks_in_backup_not_in_reaper.clone();
                                view! {
                                    <div class="preview-panel preview-warning">
                                        <div style="color: #f0ad4e; font-weight: bold; margin-top: 6px; margin-bottom: 2px;">
                                            "⚠ Will skip (tracks in backup but not in REAPER)"
                                        </div>
                                        <ul style="margin: 0; padding-left: 1.4em; color: #c8a050; font-size: 0.85em;">
                                            {names.into_iter().map(|name| view! {
                                                <li>{name}</li>
                                            }).collect_view()}
                                        </ul>
                                    </div>
                                }
                            })}
                        </div>
                        <button
                            class="settings-action-btn"
                            disabled=move || restoring.get() || change_count == 0
                            on:click=move |_| {
                                if let Some(fname) = selected.get_untracked() {
                                    let _ = set_restoring.try_set(true);
                                    let _ = set_elapsed.try_set(0);
                                    let _ = set_error.try_set(None);
                                    // Start elapsed timer (1s ticks).
                                    // Both the `restoring` read and the `set_elapsed` write
                                    // use try_* so the loop terminates cleanly if the modal
                                    // closes mid-restore instead of panicking. reaperiem#153
                                    spawn_local(async move {
                                        loop {
                                            let promise = js_sys::Promise::new(&mut |resolve, _| {
                                                web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 1000).unwrap();
                                            });
                                            wasm_bindgen_futures::JsFuture::from(promise).await.ok();
                                            // If the signal is disposed, treat as "not restoring"
                                            // and break out of the loop.
                                            let still_restoring = restoring
                                                .try_get_untracked()
                                                .unwrap_or(false);
                                            if !still_restoring {
                                                break;
                                            }
                                            if set_elapsed
                                                .try_update(|e| *e += 1)
                                                .is_none()
                                            {
                                                break;
                                            }
                                        }
                                    });
                                    // Start restore
                                    spawn_local(async move {
                                        let token = match crate::auth::get_auth() {
                                            Some(a) => a.token,
                                            None => return,
                                        };
                                        match crate::api::apply_restore(&token, &fname).await {
                                            Ok(r) => {
                                                let _ = set_result
                                                    .try_set(Some(r));
                                                let _ = set_preview.try_set(None);
                                            }
                                            Err(e) => {
                                                let _ = set_error
                                                    .try_set(Some(e));
                                            }
                                        }
                                        let _ = set_restoring.try_set(false);
                                    });
                                }
                            }
                        >
                            {move || {
                                if restoring.get() {
                                    format!("Restoring... ({}s)", elapsed.get())
                                } else {
                                    "Restore".to_string()
                                }
                            }}
                        </button>
                    </div>
                }
            })}
        </div>
    }
}
