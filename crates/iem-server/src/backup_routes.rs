//! REST API routes for backup management (engineer-only)

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{StatusCode, header},
    routing::{get, post},
};
use iem_core::{
    ApiError,
    backup::{BackupInfo, MixerBackup, RestorePreview, RestoreResult},
};

use crate::AppState;

/// Verify that the request carries a valid engineer JWT.
///
/// Returns `Ok(())` on success, `Err(403)` if the token is missing,
/// invalid, or belongs to a non-engineer.
async fn verify_engineer(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Result<(), (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");

    let claims = crate::auth::extract_claims(token, &config.jwt_secret);
    drop(config);

    match claims {
        Some(c) if c.engineer => Ok(()),
        _ => Err((
            StatusCode::FORBIDDEN,
            Json(ApiError::new("FORBIDDEN", "engineer access required")),
        )),
    }
}

/// Backup routes — all endpoints require an engineer JWT.
pub fn backup_routes() -> Router<AppState> {
    Router::new()
        // List all backups (newest-first)
        .route("/api/backups", get(list_backups))
        // Get a specific backup by filename
        .route("/api/backups/{filename}", get(get_backup))
        // Preview what a restore would change
        .route("/api/backups/{filename}/preview", post(preview_backup))
        // Apply a backup (full restore)
        .route("/api/backups/{filename}/restore", post(restore_backup))
        // Trigger a manual backup capture
        .route("/api/backups/capture", post(trigger_capture))
}

/// `GET /api/backups` — return list of available backup files (newest first).
async fn list_backups(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<BackupInfo>>, (StatusCode, Json<ApiError>)> {
    verify_engineer(&state, &headers).await?;
    let list = state.backup_store.list();
    Ok(Json(list))
}

/// `GET /api/backups/:filename` — return full backup contents.
async fn get_backup(
    State(state): State<AppState>,
    Path(filename): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<MixerBackup>, (StatusCode, Json<ApiError>)> {
    verify_engineer(&state, &headers).await?;
    state.backup_store.load(&filename).map(Json).map_err(|e| {
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

/// `POST /api/backups/:filename/preview` — diff backup vs. live state.
async fn preview_backup(
    State(state): State<AppState>,
    Path(filename): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<RestorePreview>, (StatusCode, Json<ApiError>)> {
    verify_engineer(&state, &headers).await?;

    let backup = state
        .backup_store
        .load(&filename)
        .map_err(|e| (StatusCode::NOT_FOUND, Json(ApiError::new("NOT_FOUND", &e))))?;

    crate::backup_restore::preview_restore(&state, &backup)
        .await
        .map(Json)
        .map_err(|e| {
            tracing::error!(filename = %filename, error = %e, "backup preview failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("PREVIEW_FAILED", &e)),
            )
        })
}

/// `POST /api/backups/:filename/restore` — apply backup to live system.
async fn restore_backup(
    State(state): State<AppState>,
    Path(filename): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<RestoreResult>, (StatusCode, Json<ApiError>)> {
    verify_engineer(&state, &headers).await?;

    let backup = state
        .backup_store
        .load(&filename)
        .map_err(|e| (StatusCode::NOT_FOUND, Json(ApiError::new("NOT_FOUND", &e))))?;

    crate::backup_restore::apply_restore(&state, &backup)
        .await
        .map(Json)
        .map_err(|e| {
            tracing::error!(filename = %filename, error = %e, "backup restore failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("RESTORE_FAILED", &e)),
            )
        })
}

/// `POST /api/backups/capture` — capture current mixer state and save as backup.
///
/// Returns the `BackupInfo` plus a `CaptureAudit` for the newly created backup file.
async fn trigger_capture(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    verify_engineer(&state, &headers).await?;

    let (backup, audit) = crate::backup_capture::capture_mixer_state(&state)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "backup capture failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("CAPTURE_FAILED", &e)),
            )
        })?;

    let filename = state.backup_store.save(&backup).map_err(|e| {
        tracing::error!(error = %e, "backup save failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("SAVE_FAILED", e.to_string())),
        )
    })?;

    // Build BackupInfo from what we just saved
    let size_bytes = state
        .backup_store
        .list()
        .into_iter()
        .find(|info| info.filename == filename)
        .map(|info| info.size_bytes)
        .unwrap_or(0);

    let info = BackupInfo {
        filename,
        timestamp: backup.timestamp,
        size_bytes,
        send_count: backup.sends.len(),
        track_count: backup.track_layout.len(),
    };

    Ok(Json(serde_json::json!({
        "filename": info.filename,
        "timestamp": info.timestamp,
        "size_bytes": info.size_bytes,
        "send_count": info.send_count,
        "track_count": info.track_count,
        "audit": audit,
    })))
}
