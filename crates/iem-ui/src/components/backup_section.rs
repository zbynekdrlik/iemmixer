//! Backup & Restore section for the Settings modal (engineer-only): the
//! backups, a preview of what a restore changes (F31: before → after, in the
//! UI's names), and the restore.

use iem_core::{RestoreCategory, RestorePreview, RestoreResult};
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

/// The heading of a restore category.
fn category_label(c: RestoreCategory) -> &'static str {
    match c {
        RestoreCategory::Level => "Levels",
        RestoreCategory::Group => "Stems",
        RestoreCategory::Output => "Outputs",
        RestoreCategory::Eq => "EQ",
        RestoreCategory::Limiter => "Limiters",
        RestoreCategory::Input => "Inputs",
        RestoreCategory::Customization => "Pins and hides",
    }
}

/// The preview's changes grouped by category (in category order), each as
/// "what: before → after".
fn preview_groups(p: &RestorePreview) -> Vec<(&'static str, Vec<String>)> {
    let mut changes: Vec<_> = p.changes.iter().collect();
    changes.sort_by_key(|c| c.category);
    let mut out: Vec<(&'static str, Vec<String>)> = Vec::new();
    for c in changes {
        let label = category_label(c.category);
        let line = format!(
            "{}: {} \u{2192} {}",
            c.description, c.current_value, c.backup_value
        );
        match out.last_mut() {
            Some((l, lines)) if *l == label => lines.push(line),
            _ => out.push((label, vec![line])),
        }
    }
    out
}

/// The line shown after a restore.
fn result_line(r: &RestoreResult) -> String {
    if r.skipped.is_empty() {
        format!("Restored {} values", r.restored_count)
    } else {
        format!(
            "Restored {} values, {} skipped",
            r.restored_count,
            r.skipped.len()
        )
    }
}

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
                    {result_line(&r)}
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
                        let meta = format!("{} levels, {} mixes", b.send_count, b.track_count);
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
                            // What the restore changes, before → after (F31)
                            <div class="backup-diff" data-testid="backup-diff" style="max-height: 220px; overflow-y: auto; margin-top: 6px;">
                                {preview_groups(&p).into_iter().map(|(label, lines)| view! {
                                    <div style="color: #ddd; font-weight: bold; margin-top: 6px;">{label}</div>
                                    <ul style="margin: 0; padding-left: 1.4em; color: #bbb; font-size: 0.85em;">
                                        {lines.into_iter().map(|l| view! { <li>{l}</li> }).collect_view()}
                                    </ul>
                                }).collect_view()}
                            </div>
                            {(!p.skipped.is_empty()).then(|| {
                                let names: Vec<String> = p.skipped.iter().map(|s| s.description.clone()).collect();
                                view! {
                                    <div class="preview-panel preview-warning">
                                        <div style="color: #f0ad4e; font-weight: bold; margin-top: 6px; margin-bottom: 2px;">
                                            "\u{26A0} Will skip (not in the current setup)"
                                        </div>
                                        <ul style="margin: 0; padding-left: 1.4em; color: #c8a050; font-size: 0.85em;">
                                            {names.into_iter().map(|name| view! {
                                                <li>{name}</li>
                                            }).collect_view()}
                                        </ul>
                                    </div>
                                }
                            })}
                            // Added after the backup was taken: the restore leaves it as it is (FG-1).
                            {(!p.not_in_backup.is_empty()).then(|| {
                                let names: Vec<String> = p.not_in_backup.iter().map(|s| s.description.clone()).collect();
                                view! {
                                    <div class="preview-panel preview-kept" data-testid="backup-kept">
                                        <div style="color: #8ab4f8; font-weight: bold; margin-top: 6px; margin-bottom: 2px;">
                                            "Not in this backup (stays as it is)"
                                        </div>
                                        <ul style="margin: 0; padding-left: 1.4em; color: #9fb8d8; font-size: 0.85em;">
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

#[cfg(test)]
mod tests {
    use super::*;
    use iem_core::{RestoreChange, SkippedEntry};

    fn change(category: RestoreCategory, what: &str, from: &str, to: &str) -> RestoreChange {
        RestoreChange {
            category,
            description: what.into(),
            current_value: from.into(),
            backup_value: to.into(),
        }
    }

    #[test]
    fn a_preview_groups_its_changes_by_category() {
        let p = RestorePreview {
            changes: vec![
                change(
                    RestoreCategory::Limiter,
                    "Member1: limiter",
                    "-1.0 dB",
                    "-3.0 dB",
                ),
                change(
                    RestoreCategory::Level,
                    "MEMBER3 mic \u{2192} Member1",
                    "-6.0 dB",
                    "0.0 dB",
                ),
                change(
                    RestoreCategory::Level,
                    "KEYS \u{2192} Member1",
                    "off",
                    "-12.0 dB",
                ),
                change(RestoreCategory::Input, "KEYS: trim", "0.0 dB", "3.0 dB"),
            ],
            unchanged_count: 7,
            skipped: vec![],
            not_in_backup: vec![],
        };
        let groups = preview_groups(&p);
        let labels: Vec<&str> = groups.iter().map(|(l, _)| *l).collect();
        assert_eq!(labels, ["Levels", "Limiters", "Inputs"]);
        assert_eq!(
            groups[0].1,
            [
                "MEMBER3 mic \u{2192} Member1: -6.0 dB \u{2192} 0.0 dB",
                "KEYS \u{2192} Member1: off \u{2192} -12.0 dB"
            ]
        );
        assert_eq!(groups[2].1, ["KEYS: trim: 0.0 dB \u{2192} 3.0 dB"]);
        assert!(preview_groups(&RestorePreview::default()).is_empty());
    }

    #[test]
    fn every_category_has_a_heading() {
        for c in [
            RestoreCategory::Level,
            RestoreCategory::Group,
            RestoreCategory::Output,
            RestoreCategory::Eq,
            RestoreCategory::Limiter,
            RestoreCategory::Input,
            RestoreCategory::Customization,
        ] {
            assert!(!category_label(c).is_empty());
        }
        assert_eq!(category_label(RestoreCategory::Group), "Stems");
        assert_eq!(
            category_label(RestoreCategory::Customization),
            "Pins and hides"
        );
    }

    #[test]
    fn the_result_line_counts_restored_and_skipped() {
        let mut r = RestoreResult {
            restored_count: 12,
            skipped: vec![],
        };
        assert_eq!(result_line(&r), "Restored 12 values");
        r.skipped.push(SkippedEntry {
            category: RestoreCategory::Level,
            description: "gone".into(),
            reason: "not in the topology".into(),
        });
        assert_eq!(result_line(&r), "Restored 12 values, 1 skipped");
    }
}
