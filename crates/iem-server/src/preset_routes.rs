//! Presets (F13): list, save (captured server-side from the page's mix),
//! overwrite, delete, and load as a 50 ms ramp; at most 20 per member;
//! archived entries (D8) are loadable but never changed.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
};
use iem_core::ApiError;
use iem_core::band::{MixSend, Preset, PresetInfo};
use iem_engine_proto::{Cmd, GroupId};
use serde::Deserialize;
use std::collections::BTreeMap;

use crate::AppState;
use crate::band_store::{RAMP_STEP_MS, StoreError, capture, ramp};
use crate::site_view::Page;

type Reject = (StatusCode, Json<ApiError>);

#[derive(Deserialize)]
pub struct SavePresetRequest {
    pub name: String,
}

pub fn preset_routes() -> Router<AppState> {
    Router::new()
        .route("/api/presets/{member}", get(list_presets))
        .route("/api/presets/{member}", post(save_preset))
        .route("/api/presets/{member}/{name}", get(get_preset))
        .route("/api/presets/{member}/{name}", put(update_preset))
        .route("/api/presets/{member}/{name}", delete(delete_preset))
        .route("/api/presets/{member}/{name}/restore", post(restore_preset))
}

/// A band-store error as an HTTP answer.
pub(crate) fn store_error(e: StoreError, what: &str) -> Reject {
    match e {
        StoreError::Full => (
            StatusCode::CONFLICT,
            Json(ApiError::new("LIMIT_REACHED", e.to_string())),
        ),
        StoreError::Archived => (
            StatusCode::FORBIDDEN,
            Json(ApiError::new("ARCHIVED", e.to_string())),
        ),
        StoreError::BadMember(_) => (StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))),
        StoreError::Io(_) | StoreError::Corrupt(..) => {
            tracing::error!(error = %e, "{what} failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("IO_ERROR", format!("{what} failed"))),
            )
        }
    }
}

/// The member page the request may use (presets and history are a member's).
pub(crate) async fn member_page(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    member: &str,
) -> Result<(Page, String), Reject> {
    let (page, _) = crate::routes::page_for(state, headers, member).await?;
    let m = page
        .member
        .clone()
        .ok_or((StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))))?;
    Ok((page, m))
}

fn engine_unavailable() -> Reject {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiError::new(
            "ENGINE_UNAVAILABLE",
            "The engine is not connected",
        )),
    )
}

/// Applies levels and group faders to the page's mix as the 50 ms ramp;
/// returns (values sent, entries skipped).
pub(crate) async fn apply_ramp(
    state: &AppState,
    page: &Page,
    sends: &[MixSend],
    groups: &BTreeMap<GroupId, f64>,
) -> Result<(usize, usize), Reject> {
    let site = state.site().ok_or_else(engine_unavailable)?;
    if !state.engine.connected() {
        return Err(engine_unavailable());
    }
    let (steps, skipped) = ramp(&site, &state.engine.mirror(), page, sends, groups);
    state.auto_snapshot(page);
    let changed = steps.first().map_or(0, Vec::len);
    for (i, ops) in steps.into_iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(RAMP_STEP_MS)).await;
        }
        state
            .engine
            .request_applied(Cmd::Batch { ops }, None)
            .await
            .map_err(|e| {
                tracing::error!(page = %page.id, error = %e, "ramp step failed");
                (
                    StatusCode::BAD_GATEWAY,
                    Json(ApiError::new("ENGINE_ERROR", e.to_string())),
                )
            })?;
    }
    Ok((changed, skipped))
}

async fn list_presets(
    State(state): State<AppState>,
    Path(member): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<PresetInfo>>, Reject> {
    let (_, m) = member_page(&state, &headers, &member).await?;
    let list = state
        .band
        .presets(&m)
        .map_err(|e| store_error(e, "reading presets"))?;
    Ok(Json(list.iter().map(PresetInfo::from).collect()))
}

async fn save(state: &AppState, page: &Page, member: &str, name: &str) -> Result<Preset, Reject> {
    let name = name.trim();
    if name.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError::new("INVALID_NAME", "Preset name cannot be empty")),
        ));
    }
    let site = state.site().ok_or_else(engine_unavailable)?;
    let content = capture(&site, &state.engine.mirror(), page);
    state
        .band
        .save_preset(member, name, content, chrono::Utc::now().timestamp())
        .map_err(|e| store_error(e, "saving the preset"))
}

async fn save_preset(
    State(state): State<AppState>,
    Path(member): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<SavePresetRequest>,
) -> Result<impl IntoResponse, Reject> {
    let (page, m) = member_page(&state, &headers, &member).await?;
    let p = save(&state, &page, &m, &req.name).await?;
    tracing::info!(member = %m, preset = %p.name, "preset saved");
    Ok((StatusCode::CREATED, Json(PresetInfo::from(&p))))
}

async fn get_preset(
    State(state): State<AppState>,
    Path((member, name)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Preset>, Reject> {
    let (_, m) = member_page(&state, &headers, &member).await?;
    state
        .band
        .preset(&m, &name)
        .map_err(|e| store_error(e, "reading presets"))?
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, Json(ApiError::not_found("Preset"))))
}

async fn update_preset(
    State(state): State<AppState>,
    Path((member, name)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Result<Json<PresetInfo>, Reject> {
    let (page, m) = member_page(&state, &headers, &member).await?;
    if state
        .band
        .preset(&m, &name)
        .map_err(|e| store_error(e, "reading presets"))?
        .is_none()
    {
        return Err((StatusCode::NOT_FOUND, Json(ApiError::not_found("Preset"))));
    }
    let p = save(&state, &page, &m, &name).await?;
    tracing::info!(member = %m, preset = %p.name, "preset overwritten");
    Ok(Json(PresetInfo::from(&p)))
}

async fn delete_preset(
    State(state): State<AppState>,
    Path((member, name)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Result<StatusCode, Reject> {
    let (_, m) = member_page(&state, &headers, &member).await?;
    if state
        .band
        .delete_preset(&m, &name)
        .map_err(|e| store_error(e, "deleting the preset"))?
    {
        tracing::info!(member = %m, preset = %name, "preset deleted");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::NOT_FOUND, Json(ApiError::not_found("Preset"))))
    }
}

async fn restore_preset(
    State(state): State<AppState>,
    Path((member, name)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, Reject> {
    let (page, m) = member_page(&state, &headers, &member).await?;
    let preset = state
        .band
        .preset(&m, &name)
        .map_err(|e| store_error(e, "reading presets"))?
        .ok_or((StatusCode::NOT_FOUND, Json(ApiError::not_found("Preset"))))?;
    let (restored, skipped) = apply_ramp(&state, &page, &preset.sends, &preset.groups).await?;
    tracing::info!(member = %m, preset = %name, restored, skipped, "preset loaded");
    Ok(Json(
        serde_json::json!({ "restored": restored, "skipped": skipped }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_errors_map_to_http_answers() {
        assert_eq!(store_error(StoreError::Full, "x").0, StatusCode::CONFLICT);
        assert_eq!(
            store_error(StoreError::Archived, "x").0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            store_error(StoreError::BadMember("m".into()), "x").0,
            StatusCode::NOT_FOUND
        );
        let (code, body) = store_error(StoreError::Io("disk".into()), "saving");
        assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.0.message, "saving failed");
        assert_eq!(
            store_error(StoreError::Corrupt("f".into(), "x".into()), "x").0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(store_error(StoreError::Full, "x").1.0.code, "LIMIT_REACHED");
    }
}
