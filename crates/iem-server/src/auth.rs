//! Authentication: PIN login → JWT, PIN changes, token checks.
//!
//! Login protection (program spec §5.3): admission by `LoginGuard` before any
//! hashing, a bounded hashing gate, argon2id verification of PIN hashes,
//! failure-only budgets, never a lockout.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use iem_core::ApiError;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::login_guard::{ClientKey, FailureEffect};
use crate::pin_hash::{PinHasher, is_valid_pin_format};
use crate::pin_store::ENGINEER_ID;

mod token;

pub use self::token::{extract_claims, verify_member_access, verify_token};
use self::token::{extract_claims_from_header, issue_token};

/// Login request payload
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub member: String,
    pub pin: String,
}

/// Change PIN request payload
#[derive(Debug, Deserialize)]
pub struct ChangePinRequest {
    /// Required for members (their current PIN), ignored for engineers
    pub old_pin: Option<String>,
    pub new_pin: String,
    /// Target member — required for engineers, ignored for members (JWT `sub`)
    pub member: Option<String>,
}

/// Login response with JWT token
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub member: String,
    pub engineer: bool,
    pub expires_in: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinMatch {
    Engineer,
    Member,
    None,
}

/// A rejection already rendered as a response. Boxed so the `Err` variant of
/// the login and PIN-change handlers stays pointer-sized (`Response` is 128
/// bytes; clippy `result_large_err`).
pub struct Rejection(Box<Response>);

impl From<Response> for Rejection {
    fn from(response: Response) -> Self {
        Self(Box::new(response))
    }
}

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        *self.0
    }
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(ApiError::new(code, message))).into_response()
}

fn member_not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))).into_response()
}

/// 429 with `Retry-After` in whole seconds (at least 1).
pub fn too_many_attempts(wait: Duration) -> Response {
    let secs = wait.as_millis().div_ceil(1000).max(1);
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, secs.to_string())],
        Json(ApiError::new(
            "TOO_MANY_ATTEMPTS",
            "Too many attempts, try again later",
        )),
    )
        .into_response()
}

/// Run argon2id work on the blocking pool, admitted by the hashing gate.
async fn with_hasher<T: Send + 'static>(
    state: &AppState,
    job: impl FnOnce(&PinHasher) -> T + Send + 'static,
) -> Result<T, Rejection> {
    let Some(permit) = state.hash_gate.acquire().await else {
        tracing::warn!("PIN hashing gate full — answering 429");
        return Err(Rejection::from(too_many_attempts(Duration::from_secs(1))));
    };
    let hasher = state.pin_hasher.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        job(&hasher)
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "PIN hashing task failed");
        Rejection::from(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "HASH_ERROR",
            "PIN check failed",
        ))
    })
}

async fn member_exists(state: &AppState, member: &str) -> bool {
    state.site_config.member(member).is_some()
}

/// Checks the engineer PIN like a login (admission before hashing, the
/// hashing gate, failure budgets); the "Back to REAPER" switch needs it.
pub async fn verify_engineer_pin(
    state: &AppState,
    peer: SocketAddr,
    headers: &HeaderMap,
    pin: &str,
) -> Result<(), Rejection> {
    let client = state.login_guard.client(peer.ip(), headers);
    let now = Instant::now();
    if let Err(wait) = state.login_guard.check(&client, ENGINEER_ID, now) {
        return Err(Rejection::from(too_many_attempts(wait)));
    }
    let hash = state
        .pin_store
        .read()
        .await
        .engineer_hash()
        .map(str::to_owned);
    let pin = pin.to_string();
    let ok = with_hasher(state, move |hasher| {
        hasher.verify_optional(&pin, hash.as_deref())
    })
    .await?;
    if ok {
        state.login_guard.record_success(&client, ENGINEER_ID);
        Ok(())
    } else {
        record_failure(state, &client, ENGINEER_ID, now);
        tracing::warn!(origin = ?client.origin, "engineer PIN check failed");
        Err(Rejection::from(error_response(
            StatusCode::UNAUTHORIZED,
            "INVALID_PIN",
            "Invalid PIN",
        )))
    }
}

fn record_failure(state: &AppState, client: &ClientKey, member: &str, now: Instant) {
    if state.login_guard.record_failure(client, member, now)
        == FailureEffect::EngineerBudgetExhausted
    {
        tracing::warn!(
            origin = ?client.origin,
            "login failures exhausted the engineer budget — attempts from this origin are now spaced"
        );
    }
}

/// Handle login and return a JWT.
pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, Rejection> {
    let client = state.login_guard.client(peer.ip(), &headers);
    let now = Instant::now();
    if let Err(wait) = state.login_guard.check(&client, &req.member, now) {
        tracing::info!(origin = ?client.origin, member = %req.member, wait_ms = wait.as_millis() as u64, "login throttled");
        return Err(Rejection::from(too_many_attempts(wait)));
    }
    if !req.member.is_empty() && !member_exists(&state, &req.member).await {
        return Err(Rejection::from(member_not_found()));
    }
    let (engineer_hash, member_hash) = {
        let store = state.pin_store.read().await;
        (
            store.engineer_hash().map(str::to_owned),
            store.member_hash(&req.member).map(str::to_owned),
        )
    };
    let pin = req.pin.clone();
    let matched = with_hasher(&state, move |hasher| {
        if hasher.verify_optional(&pin, engineer_hash.as_deref()) {
            PinMatch::Engineer
        } else if hasher.verify_optional(&pin, member_hash.as_deref()) {
            PinMatch::Member
        } else {
            PinMatch::None
        }
    })
    .await?;
    let config = state.config.read().await;
    match matched {
        PinMatch::Engineer => {
            state.login_guard.record_success(&client, &req.member);
            issue_token(&config, ENGINEER_ID, true).map_err(|e| Rejection::from(e.into_response()))
        }
        PinMatch::Member => {
            state.login_guard.record_success(&client, &req.member);
            issue_token(&config, &req.member, false).map_err(|e| Rejection::from(e.into_response()))
        }
        PinMatch::None => {
            record_failure(&state, &client, &req.member, now);
            tracing::info!(origin = ?client.origin, member = %req.member, "login failed: invalid PIN");
            Err(Rejection::from(error_response(
                StatusCode::UNAUTHORIZED,
                "INVALID_PIN",
                "Invalid PIN",
            )))
        }
    }
}

/// Change a PIN.
///
/// - **Engineers**: set any member's PIN (`member` required, no old PIN), or
///   the engineer PIN with `member = "engineer"`.
/// - **Members**: change their own PIN (`old_pin` required, `member` ignored);
///   a wrong current PIN counts against the login budgets.
/// - **Before the cutover** (`pin_changes = false`, every `dev` and trial
///   site; P9): 409 with [`iem_core::PIN_CHANGES_FROZEN`] for everyone, before
///   any PIN is checked; nothing changes.
pub async fn change_pin(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ChangePinRequest>,
) -> Result<StatusCode, Rejection> {
    let claims = {
        let config = state.config.read().await;
        extract_claims_from_header(&headers, &config.jwt_secret)
            .map_err(IntoResponse::into_response)?
    };
    if !state.site_config.pin_changes {
        tracing::info!(by = %claims.sub, "PIN change refused: PINs change in the predecessor until the cutover");
        return Err(Rejection::from(error_response(
            StatusCode::CONFLICT,
            "PIN_CHANGES_FROZEN",
            iem_core::PIN_CHANGES_FROZEN,
        )));
    }
    if !is_valid_pin_format(&req.new_pin) {
        return Err(Rejection::from(error_response(
            StatusCode::BAD_REQUEST,
            "INVALID_FORMAT",
            "PIN must be exactly 4 digits",
        )));
    }
    let target = if claims.engineer {
        let member = req.member.as_deref().unwrap_or("");
        if member.is_empty() {
            return Err(Rejection::from(error_response(
                StatusCode::BAD_REQUEST,
                "MISSING_MEMBER",
                "Engineer must specify target member",
            )));
        }
        if member != ENGINEER_ID && !member_exists(&state, member).await {
            return Err(Rejection::from(member_not_found()));
        }
        member.to_string()
    } else {
        let old_pin = req.old_pin.clone().unwrap_or_default();
        if old_pin.is_empty() {
            return Err(Rejection::from(error_response(
                StatusCode::BAD_REQUEST,
                "MISSING_OLD_PIN",
                "Current PIN is required",
            )));
        }
        let client = state.login_guard.client(peer.ip(), &headers);
        let now = Instant::now();
        if let Err(wait) = state.login_guard.check(&client, &claims.sub, now) {
            return Err(Rejection::from(too_many_attempts(wait)));
        }
        let current = state
            .pin_store
            .read()
            .await
            .member_hash(&claims.sub)
            .map(str::to_owned);
        let old_ok = with_hasher(&state, move |hasher| {
            hasher.verify_optional(&old_pin, current.as_deref())
        })
        .await?;
        if !old_ok {
            record_failure(&state, &client, &claims.sub, now);
            return Err(Rejection::from(error_response(
                StatusCode::UNAUTHORIZED,
                "INVALID_PIN",
                "Current PIN is incorrect",
            )));
        }
        state.login_guard.record_success(&client, &claims.sub);
        claims.sub.clone()
    };
    let new_pin = req.new_pin.clone();
    let phc = with_hasher(&state, move |hasher| hasher.hash(&new_pin)).await?;
    let saved = {
        let mut store = state.pin_store.write().await;
        if target == ENGINEER_ID {
            store.set_engineer_hash(phc)
        } else {
            store.set_member_hash(&target, phc)
        }
    };
    saved.map_err(|e| {
        tracing::error!(error = %e, member = %target, "failed to save the PIN hash");
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "IO_ERROR",
            "Failed to save PIN",
        )
    })?;
    tracing::info!(member = %target, by = %claims.sub, "PIN changed");
    Ok(StatusCode::OK)
}

#[cfg(test)]
mod login_tests;
