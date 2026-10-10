//! Listen, talkback and their diagnostics (engineer only); without the
//! `audio` feature each answers that it is not available.

#[cfg(feature = "audio")]
use axum::extract::State;
#[cfg(not(feature = "audio"))]
use axum::http::StatusCode;
use axum::{Json, response::IntoResponse};

#[cfg(feature = "audio")]
use super::{Reject, require_engineer};
#[cfg(feature = "audio")]
use crate::AppState;

/// Listen WebSocket (engineer)
#[cfg(feature = "audio")]
pub(super) async fn ws_audio_handler(
    ws: axum::extract::ws::WebSocketUpgrade,
    state: State<AppState>,
    query: axum::extract::Query<crate::mixer_ws::WsQuery>,
) -> Result<impl IntoResponse, Reject> {
    crate::listen_ws::ws_audio(ws, state, query).await
}

#[cfg(not(feature = "audio"))]
pub(super) async fn ws_audio_handler() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        "Audio streaming not available (compiled without audio feature)",
    )
}

/// Talkback WebSocket (engineer, bound to the talk id)
#[cfg(feature = "audio")]
pub(super) async fn ws_talkback_handler(
    ws: axum::extract::ws::WebSocketUpgrade,
    state: State<AppState>,
    query: axum::extract::Query<crate::mixer_ws::WsQuery>,
) -> Result<impl IntoResponse, Reject> {
    crate::talkback_ws::ws_talkback(ws, state, query).await
}

#[cfg(not(feature = "audio"))]
pub(super) async fn ws_talkback_handler() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "Talkback not available")
}

#[cfg(feature = "audio")]
pub(super) async fn talkback_diagnostics_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, Reject> {
    require_engineer(&state, &headers).await?;
    Ok(Json(crate::talkback_ws::diagnostics(&state)))
}

#[cfg(not(feature = "audio"))]
pub(super) async fn talkback_diagnostics_handler() -> impl IntoResponse {
    Json(serde_json::json!({"error": "not available"}))
}

#[cfg(feature = "audio")]
pub(super) async fn audio_diagnostics_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<crate::engine::media::AudioDiagnostics>, Reject> {
    require_engineer(&state, &headers).await?;
    Ok(Json(state.media.diagnostics()))
}

#[cfg(not(feature = "audio"))]
pub(super) async fn audio_diagnostics_handler() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        "Audio diagnostics not available (compiled without audio feature)",
    )
}
