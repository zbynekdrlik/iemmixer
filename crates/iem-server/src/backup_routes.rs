//! Backups (F19) and restore with preview (F31), engineer only.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use iem_core::{
    ApiError,
    backup::{BackupInfo, MixerBackup, RestorePreview, RestoreResult},
};
use iem_engine_proto::{Cmd, Source};

use crate::AppState;

type Reject = (StatusCode, Json<ApiError>);

pub fn backup_routes() -> Router<AppState> {
    Router::new()
        .route("/api/backups", get(list_backups))
        .route("/api/backups/{filename}", get(get_backup))
        .route("/api/backups/{filename}/preview", post(preview_backup))
        .route("/api/backups/{filename}/restore", post(restore_backup))
        .route("/api/backups/capture", post(trigger_capture))
}

async fn engineer(state: &AppState, headers: &axum::http::HeaderMap) -> Result<(), Reject> {
    crate::routes::require_engineer(state, headers)
        .await
        .map(|_| ())
        .map_err(|(code, body)| {
            if code == StatusCode::UNAUTHORIZED {
                (
                    StatusCode::FORBIDDEN,
                    Json(ApiError::new("FORBIDDEN", "engineer access required")),
                )
            } else {
                (code, body)
            }
        })
}

fn load(state: &AppState, filename: &str) -> Result<MixerBackup, Reject> {
    state.backup_store.load(filename).map_err(|e| {
        if e.starts_with("invalid filename") {
            (
                StatusCode::BAD_REQUEST,
                Json(ApiError::new("INVALID_FILENAME", &e)),
            )
        } else {
            (StatusCode::NOT_FOUND, Json(ApiError::new("NOT_FOUND", &e)))
        }
    })
}

fn unavailable() -> Reject {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiError::new(
            "ENGINE_UNAVAILABLE",
            "The engine is not connected",
        )),
    )
}

/// A backup of the running state, saved; `None` when the engine never spoke.
pub fn capture_now(state: &AppState) -> Result<(String, MixerBackup), String> {
    let site = state
        .site()
        .ok_or("the engine has not announced its topology")?;
    if !state.engine.mirror().synced {
        return Err("the engine state is not synced".into());
    }
    let b = crate::backup::capture(
        &site,
        &state.engine.mirror(),
        &state.band,
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    );
    let name = state.backup_store.save(&b).map_err(|e| e.to_string())?;
    Ok((name, b))
}

async fn list_backups(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<BackupInfo>>, Reject> {
    engineer(&state, &headers).await?;
    Ok(Json(state.backup_store.list()))
}

async fn get_backup(
    State(state): State<AppState>,
    Path(filename): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<MixerBackup>, Reject> {
    engineer(&state, &headers).await?;
    load(&state, &filename).map(Json)
}

fn preview_of(state: &AppState, b: &MixerBackup) -> Result<RestorePreview, Reject> {
    let site = state.site().ok_or_else(unavailable)?;
    let current = state.engine.mirror().state.clone();
    let band = std::sync::Arc::clone(&state.band);
    Ok(crate::backup::preview(
        &site,
        &current,
        &move |m: &str| band.customization(m).ok(),
        b,
    ))
}

async fn preview_backup(
    State(state): State<AppState>,
    Path(filename): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<RestorePreview>, Reject> {
    engineer(&state, &headers).await?;
    let b = load(&state, &filename)?;
    preview_of(&state, &b).map(Json)
}

async fn restore_backup(
    State(state): State<AppState>,
    Path(filename): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<RestoreResult>, Reject> {
    engineer(&state, &headers).await?;
    let b = load(&state, &filename)?;
    let preview = preview_of(&state, &b)?;
    state
        .engine
        .request_applied(
            Cmd::ImportState {
                state: b.state.clone(),
                baseline: false,
            },
            None,
        )
        .await
        .map_err(|e| {
            tracing::error!(%filename, error = %e, "restore failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(ApiError::new("RESTORE_FAILED", e.to_string())),
            )
        })?;
    let site = state.site().ok_or_else(unavailable)?;
    for (member, c) in &b.customizations {
        if site.member(member).is_none() {
            continue;
        }
        let keep = |list: &[Source]| -> Vec<Source> {
            list.iter()
                .filter(|s| match s {
                    Source::Input(i) => site.input(&i.0).is_some(),
                    Source::Mix(m) => site.mix(m).is_some(),
                })
                .cloned()
                .collect()
        };
        if let Err(e) = state
            .band
            .save_customization(member, keep(&c.pinned), keep(&c.hidden))
        {
            tracing::error!(%member, error = %e, "restoring pins and hides failed");
        }
    }
    tracing::warn!(%filename, changes = preview.changes.len(), "backup restored");
    Ok(Json(RestoreResult {
        restored_count: preview.changes.len(),
        skipped: preview.skipped,
    }))
}

async fn trigger_capture(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<BackupInfo>, Reject> {
    engineer(&state, &headers).await?;
    let (filename, b) = capture_now(&state).map_err(|e| {
        tracing::error!(error = %e, "backup capture failed");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError::new("CAPTURE_FAILED", e)),
        )
    })?;
    let size_bytes = state
        .backup_store
        .list()
        .into_iter()
        .find(|i| i.filename == filename)
        .map_or(0, |i| i.size_bytes);
    Ok(Json(BackupInfo {
        filename,
        timestamp: b.timestamp.clone(),
        size_bytes,
        send_count: b.level_count(),
        track_count: b.state.mixes.len(),
    }))
}

#[cfg(test)]
mod tests {
    use crate::routes::api_tests::{app, call, token};
    use axum::http::{Method, StatusCode};

    #[tokio::test]
    async fn backups_are_the_engineers_and_need_the_engine() {
        let dir = tempfile::tempdir().unwrap();
        let (state, app) = app(dir.path());
        let eng = token("engineer", true);
        // No token or a member's: forbidden (never a login prompt).
        let (status, json) = call(&app, Method::GET, "/api/backups", None, None).await;
        assert_eq!(
            (status, json["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("FORBIDDEN"))
        );
        let member = token("member1", false);
        let (status, _) = call(&app, Method::GET, "/api/backups", Some(&member), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, json) = call(&app, Method::GET, "/api/backups", Some(&eng), None).await;
        assert_eq!((status, json), (StatusCode::OK, serde_json::json!([])));
        // A missing file, and a name that leaves the folder.
        let missing = "/api/backups/20260101_000000.json";
        let (status, json) = call(&app, Method::GET, missing, Some(&eng), None).await;
        assert_eq!(
            (status, json["code"].as_str()),
            (StatusCode::NOT_FOUND, Some("NOT_FOUND"))
        );
        let (status, json) =
            call(&app, Method::GET, "/api/backups/a..json", Some(&eng), None).await;
        assert_eq!(
            (status, json["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("INVALID_FILENAME"))
        );
        // Without the engine there is nothing to capture or compare with.
        let capture = "/api/backups/capture";
        let (status, json) = call(&app, Method::POST, capture, Some(&eng), None).await;
        assert_eq!(
            (status, json["code"].as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, Some("CAPTURE_FAILED"))
        );
        let b = iem_core::backup::MixerBackup::new(
            "2026-09-27T13:00:00Z".into(),
            1,
            Default::default(),
        );
        let name = state.backup_store.save(&b).unwrap();
        let preview = format!("/api/backups/{name}/preview");
        let (status, json) = call(&app, Method::POST, &preview, Some(&eng), None).await;
        assert_eq!(
            (status, json["code"].as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, Some("ENGINE_UNAVAILABLE"))
        );
    }
}
