//! `/api/lineup` (#58 design §7, Part C's page reads it): which band plays,
//! every band, and what the bearer may do; `POST /api/lineup/switch` makes
//! another band play (`lineup_switch`). Any valid token; 404
//! `NOT_CONFIGURED` in legacy mode.

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    routing::{get, post},
};
use iem_core::roster::Lineup;
use iem_core::{ApiError, AuthClaims, LineupInfo, LineupRef, SwitchRequest};

use crate::AppState;
use crate::lineup_switch::{SwitchError, may_switch, switch_lineup};
use crate::roster_state::RosterState;

type Reject = (StatusCode, Json<ApiError>);

pub fn lineup_routes() -> Router<AppState> {
    Router::new()
        .route("/api/lineup", get(get_lineup))
        .route("/api/lineup/switch", post(post_switch))
}

/// The bearer's claims: any valid token of somebody (a person who left the
/// roster is nobody, `auth::subject_known`).
async fn bearer(state: &AppState, headers: &HeaderMap) -> Result<AuthClaims, Reject> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    let claims = {
        let config = state.config.read().await;
        crate::mixer_ws::claims_of(token, &config.jwt_secret)?
    };
    if !crate::auth::subject_known(state, &claims) {
        return Err(crate::auth::subject_gone());
    }
    Ok(claims)
}

fn not_configured() -> Reject {
    (
        StatusCode::NOT_FOUND,
        Json(ApiError::new("NOT_CONFIGURED", "this site has no roster")),
    )
}

fn reference(l: &Lineup) -> LineupRef {
    LineupRef {
        id: l.id.clone(),
        name: l.name.clone(),
    }
}

/// What `by` learns of `roster` and may do (pure).
pub fn lineup_info(roster: &RosterState, by: &AuthClaims) -> LineupInfo {
    let active = roster.lineup();
    LineupInfo {
        active: reference(active),
        lineups: roster.roster.lineups.iter().map(reference).collect(),
        playing: by.engineer || active.plays(&by.sub),
        leader: active.leads(&by.sub),
        can_switch: may_switch(&roster.roster, by),
    }
}

/// The answer to a switch that did not happen (yet): a refusal, or 202
/// `SWITCH_PENDING` for one that finishes once the engine answers.
pub fn reject(e: &SwitchError) -> Reject {
    let (status, code) = match e {
        SwitchError::NotConfigured => (StatusCode::NOT_FOUND, "NOT_CONFIGURED"),
        SwitchError::Unknown(_) => (StatusCode::NOT_FOUND, "UNKNOWN_LINEUP"),
        SwitchError::Forbidden => (StatusCode::FORBIDDEN, "FORBIDDEN"),
        SwitchError::Busy => (StatusCode::CONFLICT, "BUSY"),
        SwitchError::Same => (StatusCode::CONFLICT, "ALREADY_PLAYING"),
        SwitchError::NotSynced => (StatusCode::SERVICE_UNAVAILABLE, "ENGINE_UNAVAILABLE"),
        SwitchError::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "IO_ERROR"),
        SwitchError::Engine(_) => (StatusCode::BAD_GATEWAY, "SWITCH_FAILED"),
        // Not refused: it finishes once the engine answers (#58 decision).
        SwitchError::Pending(_) => (StatusCode::ACCEPTED, "SWITCH_PENDING"),
    };
    (status, Json(ApiError::new(code, e.to_string())))
}

/// `GET /api/lineup` → [`LineupInfo`].
async fn get_lineup(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<LineupInfo>, Reject> {
    let claims = bearer(&state, &headers).await?;
    let roster = state.roster().ok_or_else(not_configured)?;
    Ok(Json(lineup_info(&roster, &claims)))
}

/// `POST /api/lineup/switch {"to": id}` → 204, or [`reject`]'s answer.
/// The switch runs on a task of its own: a client that goes away drops
/// this handler, never a switch half-way (#58 review).
async fn post_switch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SwitchRequest>,
) -> Result<StatusCode, Reject> {
    let claims = bearer(&state, &headers).await?;
    let switch = tokio::spawn(async move { switch_lineup(&state, &req.to, &claims).await });
    switch
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "the switch's task failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("SWITCH_FAILED", "the switch's task failed")),
            )
        })?
        .map_err(|e| reject(&e))?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests;
