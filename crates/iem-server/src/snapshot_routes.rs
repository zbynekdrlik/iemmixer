//! History (F14): list, manual snapshot (captured server-side), delete,
//! pin with a label, unpin, and restore as a 50 ms ramp. The daily
//! automatic snapshot is taken before the first change of a day
//! (`AppState::auto_snapshot`); the oldest unpinned entries beyond 50 are
//! pruned.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
};
use iem_core::ApiError;
use iem_core::band::{Snapshot, SnapshotInfo};
use serde::Deserialize;

use crate::AppState;
use crate::band_store::capture;
use crate::preset_routes::{apply_ramp, member_page, store_error};

type Reject = (StatusCode, Json<ApiError>);

#[derive(Deserialize)]
pub struct CreateSnapshotRequest {
    pub label: Option<String>,
}

#[derive(Deserialize)]
pub struct PinRequest {
    pub label: String,
}

pub fn snapshot_routes() -> Router<AppState> {
    Router::new()
        .route("/api/snapshots/{member}", get(list_snapshots))
        .route("/api/snapshots/{member}", post(create_snapshot))
        .route(
            "/api/snapshots/{member}/{timestamp}",
            delete(delete_snapshot),
        )
        .route(
            "/api/snapshots/{member}/{timestamp}/pin",
            post(pin_snapshot),
        )
        .route(
            "/api/snapshots/{member}/{timestamp}/unpin",
            post(unpin_snapshot),
        )
        .route(
            "/api/snapshots/{member}/{timestamp}/restore",
            post(restore_snapshot),
        )
}

async fn list_snapshots(
    State(state): State<AppState>,
    Path(member): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<SnapshotInfo>>, Reject> {
    let (_, m) = member_page(&state, &headers, &member).await?;
    let list = state
        .band
        .snapshots(&m)
        .map_err(|e| store_error(e, "reading the history"))?;
    Ok(Json(list.iter().map(SnapshotInfo::from).collect()))
}

async fn create_snapshot(
    State(state): State<AppState>,
    Path(member): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateSnapshotRequest>,
) -> Result<impl IntoResponse, Reject> {
    let (page, m) = member_page(&state, &headers, &member).await?;
    let site = state.site().ok_or((
        StatusCode::BAD_REQUEST,
        Json(ApiError::new(
            "NO_STATE",
            "No mixer state available to snapshot",
        )),
    ))?;
    let c = capture(&site, &state.engine.mirror(), &page);
    let timestamp = chrono::Utc::now().timestamp();
    let snapshot = Snapshot {
        timestamp,
        label: req.label.unwrap_or_else(|| "manual".to_string()),
        pinned: false,
        sends: c.sends,
        groups: c.groups,
        input_eq: c.input_eq,
        archived: false,
        legacy_member: None,
    };
    state
        .band
        .add_snapshot(&m, snapshot)
        .map_err(|e| store_error(e, "saving the snapshot"))?;
    tracing::info!(member = %m, timestamp, "manual snapshot saved");
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "timestamp": timestamp })),
    ))
}

async fn delete_snapshot(
    State(state): State<AppState>,
    Path((member, timestamp)): Path<(String, i64)>,
    headers: axum::http::HeaderMap,
) -> Result<StatusCode, Reject> {
    let (_, m) = member_page(&state, &headers, &member).await?;
    if state
        .band
        .delete_snapshot(&m, timestamp)
        .map_err(|e| store_error(e, "deleting the snapshot"))?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::NOT_FOUND, Json(ApiError::not_found("Snapshot"))))
    }
}

async fn pin(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    member: &str,
    timestamp: i64,
    pinned: bool,
    label: Option<String>,
) -> Result<StatusCode, Reject> {
    let (_, m) = member_page(state, headers, member).await?;
    if state
        .band
        .pin_snapshot(&m, timestamp, pinned, label)
        .map_err(|e| store_error(e, "pinning the snapshot"))?
    {
        Ok(StatusCode::OK)
    } else {
        Err((StatusCode::NOT_FOUND, Json(ApiError::not_found("Snapshot"))))
    }
}

async fn pin_snapshot(
    State(state): State<AppState>,
    Path((member, timestamp)): Path<(String, i64)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<PinRequest>,
) -> Result<StatusCode, Reject> {
    pin(&state, &headers, &member, timestamp, true, Some(req.label)).await
}

async fn unpin_snapshot(
    State(state): State<AppState>,
    Path((member, timestamp)): Path<(String, i64)>,
    headers: axum::http::HeaderMap,
) -> Result<StatusCode, Reject> {
    pin(&state, &headers, &member, timestamp, false, None).await
}

async fn restore_snapshot(
    State(state): State<AppState>,
    Path((member, timestamp)): Path<(String, i64)>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, Reject> {
    let (page, m) = member_page(&state, &headers, &member).await?;
    let snap = state
        .band
        .snapshot(&m, timestamp)
        .map_err(|e| store_error(e, "reading the history"))?
        .ok_or((StatusCode::NOT_FOUND, Json(ApiError::not_found("Snapshot"))))?;
    let (restored, skipped) = apply_ramp(&state, &page, &snap.sends, &snap.groups).await?;
    tracing::info!(member = %m, timestamp, restored, skipped, "snapshot restored");
    Ok(Json(
        serde_json::json!({ "restored": restored, "skipped": skipped }),
    ))
}
