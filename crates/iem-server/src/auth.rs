//! Authentication: PIN login → JWT, PIN changes, token checks.
//!
//! Login protection (program spec §5.3): admission by `LoginGuard` before any
//! hashing, a bounded hashing gate, argon2id verification of PIN hashes,
//! failure-only budgets, never a lockout.

use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use iem_core::{ApiError, AuthClaims};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::login_guard::{ClientKey, FailureEffect};
use crate::pin_hash::{PinHasher, is_valid_pin_format};
use crate::pin_store::ENGINEER_ID;

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

/// Token expiration for members (7 days)
const MEMBER_TOKEN_EXPIRY_SECS: u64 = 7 * 24 * 60 * 60;
/// Token expiration for engineers (7 days — same as members)
const ENGINEER_TOKEN_EXPIRY_SECS: u64 = 7 * 24 * 60 * 60;

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

/// Issue a JWT token for the given member/engineer
fn issue_token(
    config: &iem_core::Config,
    member_id: &str,
    engineer: bool,
) -> Result<Json<LoginResponse>, (StatusCode, Json<ApiError>)> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Use shorter expiry for engineer tokens (elevated access)
    let expiry_secs = if engineer {
        ENGINEER_TOKEN_EXPIRY_SECS
    } else {
        MEMBER_TOKEN_EXPIRY_SECS
    };

    let claims = AuthClaims {
        sub: member_id.to_string(),
        engineer,
        exp: now + expiry_secs,
        iat: now,
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(config.jwt_secret.as_bytes()),
    )
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("TOKEN_ERROR", "Failed to create token")),
        )
    })?;

    Ok(Json(LoginResponse {
        token,
        member: member_id.to_string(),
        engineer,
        expires_in: expiry_secs,
    }))
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

/// Extract claims from Authorization header
fn extract_claims_from_header(
    headers: &axum::http::HeaderMap,
    jwt_secret: &str,
) -> Result<AuthClaims, (StatusCode, Json<ApiError>)> {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let token = match auth_header {
        Some(h) if h.starts_with("Bearer ") => &h[7..],
        _ => {
            return Err((StatusCode::UNAUTHORIZED, Json(ApiError::unauthorized())));
        }
    };

    extract_claims(token, jwt_secret)
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(ApiError::unauthorized())))
}

/// Verify JWT and return claims
pub async fn verify_token(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;

    // Extract token from Authorization header
    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());

    let token = match auth_header {
        Some(header) if header.starts_with("Bearer ") => &header[7..],
        _ => {
            return Err((StatusCode::UNAUTHORIZED, Json(ApiError::unauthorized())));
        }
    };

    // Validate token
    let token_data = decode::<AuthClaims>(
        token,
        &DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|_| (StatusCode::UNAUTHORIZED, Json(ApiError::unauthorized())))?;

    // Check expiration
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    if token_data.claims.exp < now {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiError::new("TOKEN_EXPIRED", "Token has expired")),
        ));
    }

    // Continue with request
    drop(config);
    Ok(next.run(req).await)
}

/// Verify that the authenticated user has access to the specified member's mixer.
///
/// Rules:
/// - Engineer tokens (`claims.engineer == true`) can access any member
/// - Member tokens can only access their own mixer (`claims.sub == member_id`)
/// - Missing or invalid tokens return 401 Unauthorized
/// - Access to another member's mixer returns 403 Forbidden
pub fn verify_member_access(
    headers: &axum::http::HeaderMap,
    member_id: &str,
    jwt_secret: &str,
) -> Result<AuthClaims, (StatusCode, Json<ApiError>)> {
    let claims = extract_claims_from_header(headers, jwt_secret)?;
    if claims.engineer || claims.sub == member_id {
        Ok(claims)
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(ApiError::new(
                "FORBIDDEN",
                "Access denied to this member's mixer",
            )),
        ))
    }
}

/// Extract claims from a valid JWT token
pub fn extract_claims(token: &str, secret: &str) -> Option<AuthClaims> {
    decode::<AuthClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .ok()
    .map(|data| data.claims)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod login_tests;
