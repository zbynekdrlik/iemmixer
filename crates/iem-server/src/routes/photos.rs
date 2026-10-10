//! Member photos (F22): read by anyone, changed by the member or the engineer.

use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::Response,
};
use serde::Deserialize;

use crate::AppState;

#[derive(Deserialize)]
pub(super) struct PhotoUpload {
    photo: String, // base64-encoded JPEG
}

/// Get a member's profile photo (no auth — landing page needs it)
pub(super) async fn get_photo(
    State(state): State<AppState>,
    Path(member_id): Path<String>,
) -> Result<Response, StatusCode> {
    match state.photo_store.load(&member_id) {
        Some(data) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/jpeg")
            .header(header::CACHE_CONTROL, "public, max-age=3600")
            .body(Body::from(data))
            .unwrap()),
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// Upload a member's profile photo (auth: own or engineer)
pub(super) async fn post_photo(
    State(state): State<AppState>,
    Path(member_id): Path<String>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<PhotoUpload>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<iem_core::ApiError>)> {
    let config = state.config.read().await;
    crate::auth::verify_member_access(&headers, &member_id, &config.jwt_secret)?;
    drop(config);

    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD
        .decode(&payload.photo)
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(iem_core::ApiError::new("INVALID_DATA", "Invalid base64")),
            )
        })?;

    if data.len() > 256 * 1024 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(iem_core::ApiError::new("TOO_LARGE", "Photo exceeds 256 KB")),
        ));
    }

    state.photo_store.save(&member_id, &data).map_err(|e| {
        tracing::error!("Failed to save photo: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(iem_core::ApiError::new("IO_ERROR", "Failed to save photo")),
        )
    })?;

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Delete a member's profile photo (auth: own or engineer)
pub(super) async fn delete_photo(
    State(state): State<AppState>,
    Path(member_id): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<iem_core::ApiError>)> {
    let config = state.config.read().await;
    crate::auth::verify_member_access(&headers, &member_id, &config.jwt_secret)?;
    drop(config);

    state.photo_store.delete(&member_id).map_err(|e| {
        tracing::error!("Failed to delete photo: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(iem_core::ApiError::new(
                "IO_ERROR",
                "Failed to delete photo",
            )),
        )
    })?;

    Ok(Json(serde_json::json!({ "ok": true })))
}
