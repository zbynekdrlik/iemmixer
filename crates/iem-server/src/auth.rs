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
    state
        .discovered_members
        .read()
        .await
        .iter()
        .any(|m| m.id() == member)
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
    let client = ClientKey::from_request(peer.ip(), &headers);
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
        let client = ClientKey::from_request(peer.ip(), &headers);
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
mod tests {
    use super::*;
    use iem_core::Config;

    fn test_config() -> Config {
        Config {
            jwt_secret: "test_secret_for_auth_testing".to_string(),
            ..Config::default()
        }
    }

    #[test]
    fn test_member_token_expiry_7d() {
        let config = test_config();
        let result = issue_token(&config, "oldmember1", false);

        assert!(result.is_ok());
        let response = result.unwrap().0;

        // Member tokens should have 7-day expiry
        assert!(!response.engineer);
        assert_eq!(response.expires_in, 7 * 24 * 60 * 60);

        // Verify the token claims
        let claims = extract_claims(&response.token, &config.jwt_secret).unwrap();
        assert_eq!(claims.sub, "oldmember1");
        assert!(!claims.engineer);

        // Verify expiry is approximately 7 days from now (within 5 sec tolerance)
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let expected_exp = now + 7 * 24 * 60 * 60;
        assert!((claims.exp as i64 - expected_exp as i64).abs() < 5);
    }

    #[test]
    fn test_engineer_token_expiry_7d() {
        let config = test_config();
        let result = issue_token(&config, "engineer", true);

        assert!(result.is_ok());
        let response = result.unwrap().0;

        // Engineer tokens should have 7-day expiry
        assert!(response.engineer);
        assert_eq!(response.expires_in, 7 * 24 * 60 * 60);

        // Verify the token claims
        let claims = extract_claims(&response.token, &config.jwt_secret).unwrap();
        assert_eq!(claims.sub, "engineer");
        assert!(claims.engineer);

        // Verify expiry is approximately 7 days from now (within 5 sec tolerance)
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let expected_exp = now + 7 * 24 * 60 * 60;
        assert!((claims.exp as i64 - expected_exp as i64).abs() < 5);
    }

    #[test]
    fn test_token_expiry_constants() {
        // Verify expiry constants are correct — both 7 days
        assert_eq!(MEMBER_TOKEN_EXPIRY_SECS, 7 * 24 * 60 * 60);
        assert_eq!(ENGINEER_TOKEN_EXPIRY_SECS, 7 * 24 * 60 * 60);

        // Both roles should have the same expiry
        assert_eq!(ENGINEER_TOKEN_EXPIRY_SECS, MEMBER_TOKEN_EXPIRY_SECS);
    }

    /// Helper to create a JWT token for testing
    fn make_token(config: &Config, member: &str, engineer: bool) -> String {
        issue_token(config, member, engineer)
            .unwrap()
            .0
            .token
            .clone()
    }

    /// Helper to build headers with a Bearer token
    fn headers_with_token(token: &str) -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {}", token).parse().unwrap(),
        );
        headers
    }

    #[test]
    fn test_verify_member_access_own_member() {
        let config = test_config();
        let token = make_token(&config, "oldmember1", false);
        let headers = headers_with_token(&token);

        let result = verify_member_access(&headers, "oldmember1", &config.jwt_secret);
        assert!(result.is_ok());
        let claims = result.unwrap();
        assert_eq!(claims.sub, "oldmember1");
        assert!(!claims.engineer);
    }

    #[test]
    fn test_verify_member_access_other_member_forbidden() {
        let config = test_config();
        let token = make_token(&config, "oldmember1", false);
        let headers = headers_with_token(&token);

        let result = verify_member_access(&headers, "member3", &config.jwt_secret);
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_verify_member_access_engineer_any_member() {
        let config = test_config();
        let token = make_token(&config, "engineer", true);
        let headers = headers_with_token(&token);

        // Engineer can access any member
        let result = verify_member_access(&headers, "oldmember1", &config.jwt_secret);
        assert!(result.is_ok());
        assert!(result.unwrap().engineer);

        let result = verify_member_access(&headers, "member3", &config.jwt_secret);
        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_member_access_no_token() {
        let config = test_config();
        let headers = axum::http::HeaderMap::new();

        let result = verify_member_access(&headers, "oldmember1", &config.jwt_secret);
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_verify_member_access_invalid_token() {
        let config = test_config();
        let headers = headers_with_token("invalid.jwt.token");

        let result = verify_member_access(&headers, "oldmember1", &config.jwt_secret);
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_verify_member_access_expired_token() {
        let config = test_config();
        // Create an expired token manually
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = AuthClaims {
            sub: "oldmember1".to_string(),
            engineer: false,
            exp: now - 3600, // expired 1 hour ago
            iat: now - 7200,
        };
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(config.jwt_secret.as_bytes()),
        )
        .unwrap();
        let headers = headers_with_token(&token);

        let result = verify_member_access(&headers, "oldmember1", &config.jwt_secret);
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

#[cfg(test)]
mod login_tests {
    use super::*;
    use crate::login_guard::{HashGate, Origin};
    use crate::pin_hash::{PEPPER_LEN, PinHasher};
    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use axum::http::Request;
    use axum::routing::post;
    use std::sync::Arc;
    use tower::util::ServiceExt;

    const ENGINEER_PIN: &str = "2468";
    const MEMBER_PIN: &str = "1357";
    const WRONG_PIN: &str = "9753";
    const NEW_PIN: &str = "8642";
    const SECRET: &str = "login-test-secret";
    const LAN: [u8; 4] = [10, 0, 0, 50];
    const LOOPBACK: [u8; 4] = [127, 0, 0, 1];

    fn discovered(name: &str) -> iem_core::DiscoveredMember {
        iem_core::DiscoveredMember {
            name: name.to_string(),
            track_index: 0,
            dante_output_l: 71,
            dante_output_r: 72,
            send_index: 0,
            mix_send_index: None,
            mix_send_indices: std::collections::HashMap::new(),
        }
    }

    /// member1 (PIN set), member2 (no PIN yet), engineer; fast test hasher.
    async fn test_state(dir: &std::path::Path) -> AppState {
        let config = iem_core::Config {
            jwt_secret: SECRET.to_string(),
            ..iem_core::Config::default()
        };
        let mut state = AppState::new(config, dir);
        state.pin_hasher = PinHasher::for_tests([5u8; PEPPER_LEN]);
        {
            let mut members = state.discovered_members.write().await;
            members.push(discovered("MEMBER1"));
            members.push(discovered("MEMBER2"));
            members.push(discovered("ENGINEER"));
        }
        let engineer_hash = state.pin_hasher.hash(ENGINEER_PIN);
        let member_hash = state.pin_hasher.hash(MEMBER_PIN);
        {
            let mut store = state.pin_store.write().await;
            store.set_engineer_hash(engineer_hash).unwrap();
            store.set_member_hash("member1", member_hash).unwrap();
        }
        state
    }

    fn app(state: AppState, peer: [u8; 4]) -> axum::Router {
        axum::Router::new()
            .route("/api/auth", post(login))
            .route("/api/auth/change-pin", post(change_pin))
            .with_state(state)
            .layer(MockConnectInfo(SocketAddr::from((peer, 40000))))
    }

    async fn post_json(
        app: &axum::Router,
        uri: &str,
        body: serde_json::Value,
        headers: &[(&str, &str)],
    ) -> Response {
        let mut req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        app.clone()
            .oneshot(req.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap()
    }

    async fn login_as(
        app: &axum::Router,
        member: &str,
        pin: &str,
        headers: &[(&str, &str)],
    ) -> Response {
        post_json(
            app,
            "/api/auth",
            serde_json::json!({ "member": member, "pin": pin }),
            headers,
        )
        .await
    }

    async fn json_body(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn bearer(member: &str, engineer: bool) -> String {
        let config = iem_core::Config {
            jwt_secret: SECRET.to_string(),
            ..iem_core::Config::default()
        };
        format!(
            "Bearer {}",
            issue_token(&config, member, engineer).unwrap().0.token
        )
    }

    async fn change(app: &axum::Router, auth: &str, body: serde_json::Value) -> Response {
        post_json(
            app,
            "/api/auth/change-pin",
            body,
            &[("authorization", auth)],
        )
        .await
    }

    #[tokio::test]
    async fn member_pin_logs_the_member_in() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let resp = login_as(&app, "member1", MEMBER_PIN, &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = json_body(resp).await;
        assert_eq!(body["member"], "member1");
        assert_eq!(body["engineer"], false);
    }

    #[tokio::test]
    async fn engineer_pin_works_from_any_member_login() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let body = json_body(login_as(&app, "member1", ENGINEER_PIN, &[]).await).await;
        assert_eq!(body["engineer"], true);
        assert_eq!(body["member"], "engineer");
    }

    #[tokio::test]
    async fn engineer_login_needs_the_engineer_pin() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        assert_eq!(
            login_as(&app, "engineer", MEMBER_PIN, &[]).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            login_as(&app, "engineer", ENGINEER_PIN, &[]).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn wrong_pin_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let resp = login_as(&app, "member1", WRONG_PIN, &[]).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(json_body(resp).await["code"], "INVALID_PIN");
    }

    #[tokio::test]
    async fn member_without_a_pin_cannot_log_in() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        assert_eq!(
            login_as(&app, "member2", MEMBER_PIN, &[]).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            login_as(&app, "member2", "0000", &[]).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn unknown_member_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        assert_eq!(
            login_as(&app, "member7", MEMBER_PIN, &[]).await.status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn fourth_attempt_after_three_failures_is_throttled_even_with_the_right_pin() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        for _ in 0..3 {
            assert_eq!(
                login_as(&app, "member1", WRONG_PIN, &[]).await.status(),
                StatusCode::UNAUTHORIZED
            );
        }
        let resp = login_as(&app, "member1", MEMBER_PIN, &[]).await;
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(resp.headers()[header::RETRY_AFTER], "1");
        assert_eq!(json_body(resp).await["code"], "TOO_MANY_ATTEMPTS");
    }

    #[tokio::test]
    async fn failures_of_one_client_do_not_slow_another() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path()).await;
        let first = app(state.clone(), LAN);
        for _ in 0..3 {
            login_as(&first, "member1", WRONG_PIN, &[]).await;
        }
        let other = app(state, [10, 0, 0, 51]);
        assert_eq!(
            login_as(&other, "member1", MEMBER_PIN, &[]).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn tunnel_failures_do_not_slow_lan_logins() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path()).await;
        let tunnel = app(state.clone(), LOOPBACK);
        let cf = [("cf-connecting-ip", "203.0.113.9")];
        for _ in 0..3 {
            assert_eq!(
                login_as(&tunnel, "member1", WRONG_PIN, &cf).await.status(),
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(
            login_as(&tunnel, "member1", MEMBER_PIN, &cf).await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        let lan = app(state, LAN);
        assert_eq!(
            login_as(&lan, "member1", MEMBER_PIN, &[]).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn forged_cf_header_from_a_lan_peer_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        for n in 0..3 {
            let ip = format!("203.0.113.{n}");
            let status = login_as(
                &app,
                "member1",
                WRONG_PIN,
                &[("cf-connecting-ip", ip.as_str())],
            )
            .await
            .status();
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
        let resp = login_as(
            &app,
            "member1",
            MEMBER_PIN,
            &[("cf-connecting-ip", "203.0.113.99")],
        )
        .await;
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn success_resets_the_failure_streak() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        for _ in 0..2 {
            login_as(&app, "member1", WRONG_PIN, &[]).await;
        }
        assert_eq!(
            login_as(&app, "member1", MEMBER_PIN, &[]).await.status(),
            StatusCode::OK
        );
        for _ in 0..2 {
            assert_eq!(
                login_as(&app, "member1", WRONG_PIN, &[]).await.status(),
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(
            login_as(&app, "member1", MEMBER_PIN, &[]).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn full_hashing_gate_answers_429_without_counting_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_state(dir.path()).await;
        state.hash_gate = Arc::new(HashGate::new(0, 0));
        let app = app(state.clone(), LAN);
        let resp = tokio::time::timeout(
            Duration::from_secs(5),
            login_as(&app, "member1", MEMBER_PIN, &[]),
        )
        .await
        .expect("a full gate answers at once instead of queueing");
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(resp.headers()[header::RETRY_AFTER], "1");
        assert_eq!(state.login_guard.stats().lan_failures, 0);
    }

    #[tokio::test]
    #[tracing_test::traced_test]
    async fn exhausting_the_engineer_budget_is_logged() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path()).await;
        let now = Instant::now();
        let client = |last: u8| ClientKey {
            origin: Origin::Lan,
            ip: std::net::IpAddr::from([10, 0, 1, last]),
        };
        for i in 0..30u8 {
            record_failure(&state, &client(i), "member1", now);
        }
        assert!(
            !logs_contain("exhausted the engineer budget"),
            "30 failures stay within the budget"
        );
        record_failure(&state, &client(30), "member1", now);
        assert!(logs_contain("exhausted the engineer budget"));
    }

    #[test]
    fn retry_after_is_whole_seconds_rounded_up() {
        for (wait, expected) in [
            (Duration::from_millis(1), "1"),
            (Duration::from_millis(1001), "2"),
            (Duration::from_secs(60), "60"),
        ] {
            assert_eq!(
                too_many_attempts(wait).headers()[header::RETRY_AFTER],
                expected
            );
        }
    }

    #[tokio::test]
    async fn member_changes_own_pin_with_the_current_pin() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let resp = change(
            &app,
            &bearer("member1", false),
            serde_json::json!({ "old_pin": MEMBER_PIN, "new_pin": NEW_PIN }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            login_as(&app, "member1", NEW_PIN, &[]).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            login_as(&app, "member1", MEMBER_PIN, &[]).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn wrong_current_pin_is_rejected_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let auth = bearer("member1", false);
        for _ in 0..3 {
            let resp = change(
                &app,
                &auth,
                serde_json::json!({ "old_pin": WRONG_PIN, "new_pin": NEW_PIN }),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
        let resp = change(
            &app,
            &auth,
            serde_json::json!({ "old_pin": MEMBER_PIN, "new_pin": NEW_PIN }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn member_token_changes_only_its_own_pin() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let body =
            serde_json::json!({ "old_pin": MEMBER_PIN, "new_pin": NEW_PIN, "member": "member2" });
        assert_eq!(
            change(&app, &bearer("member1", false), body).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            login_as(&app, "member2", NEW_PIN, &[]).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            login_as(&app, "member1", NEW_PIN, &[]).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn engineer_resets_a_member_pin_without_the_old_pin() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let resp = change(
            &app,
            &bearer("engineer", true),
            serde_json::json!({ "new_pin": NEW_PIN, "member": "member2" }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            login_as(&app, "member2", NEW_PIN, &[]).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn engineer_can_rotate_the_engineer_pin() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let resp = change(
            &app,
            &bearer("engineer", true),
            serde_json::json!({ "new_pin": NEW_PIN, "member": "engineer" }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            json_body(login_as(&app, "engineer", NEW_PIN, &[]).await).await["engineer"],
            true
        );
        assert_eq!(
            login_as(&app, "engineer", ENGINEER_PIN, &[]).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn new_pin_must_be_four_digits() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let resp = change(
            &app,
            &bearer("member1", false),
            serde_json::json!({ "old_pin": MEMBER_PIN, "new_pin": "12a4" }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn engineer_reset_of_an_unknown_member_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let resp = change(
            &app,
            &bearer("engineer", true),
            serde_json::json!({ "new_pin": NEW_PIN, "member": "member7" }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn change_pin_requires_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let resp = post_json(
            &app,
            "/api/auth/change-pin",
            serde_json::json!({ "new_pin": NEW_PIN }),
            &[],
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
