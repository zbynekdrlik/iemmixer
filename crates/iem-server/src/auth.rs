//! Authentication middleware and JWT handling

use axum::{
    Json,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::Response,
};
use iem_core::config::constant_time_eq;
use iem_core::{ApiError, AuthClaims};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::AppState;

/// Login request payload
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub member: String,
    pub pin: String,
}

/// Change PIN request payload
#[derive(Debug, Deserialize)]
pub struct ChangePinRequest {
    /// Required for members (verify current PIN), skipped for engineers
    pub old_pin: Option<String>,
    pub new_pin: String,
    /// Target member — required for engineers, ignored for regular members (uses JWT sub)
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

/// Handle login and return JWT
pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;
    let pin_store = state.pin_store.read().await;

    // Helper: check if member exists in discovered members
    let member_exists = {
        let discovered = state.discovered_members.read().await;
        discovered.iter().any(|m| m.id() == req.member)
    };

    // 1. Check the engineer PIN — only a configured one; there is no default.
    if let Some(eng_pin) = config.engineer_pin.as_deref()
        && constant_time_eq(&req.pin, eng_pin)
    {
        // Verify member exists (engineer still needs a valid member target)
        if !req.member.is_empty() && !member_exists {
            return Err((StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))));
        }
        return issue_token(&config, "engineer", true);
    }

    // 2. Check PinStore for custom PIN (member changed their PIN)
    // Uses constant-time comparison to prevent timing attacks
    if let Some(custom_pin) = pin_store.get_pin(&req.member) {
        if !constant_time_eq(&req.pin, custom_pin) {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(ApiError::new("INVALID_PIN", "Invalid PIN")),
            ));
        }
        if !member_exists {
            return Err((StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))));
        }
        return issue_token(&config, &req.member, false);
    }

    // 3. No stored PIN matched.
    drop(pin_store);

    // First verify member exists in discovered members (REAPER source of truth)
    if !member_exists {
        return Err((StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))));
    }

    // Block "engineer" from using default member PIN — engineer MUST use engineer PIN (step 1)
    // If we reach here, the engineer PIN didn't match, so reject.
    if req.member == "engineer" {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiError::new("INVALID_PIN", "Invalid PIN")),
        ));
    }

    // Invalid PIN
    Err((
        StatusCode::UNAUTHORIZED,
        Json(ApiError::new("INVALID_PIN", "Invalid PIN")),
    ))
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

/// Change PIN for a member.
///
/// - **Engineers**: can change any member's PIN. `member` field required, `old_pin` skipped.
/// - **Members**: can only change their own PIN. `old_pin` required, `member` field ignored.
pub async fn change_pin(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ChangePinRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let config = state.config.read().await;

    // Extract caller identity from JWT
    let claims = extract_claims_from_header(&headers, &config.jwt_secret)?;

    // Validate new_pin is exactly 4 digits
    if req.new_pin.len() != 4 || !req.new_pin.chars().all(|c| c.is_ascii_digit()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError::new(
                "INVALID_FORMAT",
                "PIN must be exactly 4 digits",
            )),
        ));
    }

    // Determine target member and verify authorization
    let target_member = if claims.engineer {
        // Engineers must specify which member's PIN to change
        let member = req.member.as_deref().unwrap_or("").to_string();
        if member.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ApiError::new(
                    "MISSING_MEMBER",
                    "Engineer must specify target member",
                )),
            ));
        }
        // Verify member exists
        let discovered = state.discovered_members.read().await;
        if !discovered.iter().any(|m| m.id() == member) {
            return Err((StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))));
        }
        // Engineers skip old_pin verification
        member
    } else {
        // Regular members change their own PIN — old_pin required
        let old_pin = req.old_pin.as_deref().unwrap_or("");
        if old_pin.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ApiError::new("MISSING_OLD_PIN", "Current PIN is required")),
            ));
        }

        let pin_store = state.pin_store.read().await;
        let old_pin_valid = if let Some(custom_pin) = pin_store.get_pin(&claims.sub) {
            constant_time_eq(old_pin, custom_pin)
        } else if let Some(config_pin) = config.pins.get(&claims.sub) {
            constant_time_eq(old_pin, config_pin)
        } else {
            false
        };
        drop(pin_store);

        if !old_pin_valid {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(ApiError::new("INVALID_PIN", "Current PIN is incorrect")),
            ));
        }

        claims.sub.clone()
    };

    // Save new PIN
    let mut pin_store = state.pin_store.write().await;
    pin_store
        .set_pin(&target_member, &req.new_pin)
        .map_err(|e| {
            tracing::error!("Failed to save PIN: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("IO_ERROR", "Failed to save PIN")),
            )
        })?;

    tracing::info!(
        "PIN changed for member: {} (by: {})",
        target_member,
        if claims.engineer {
            "engineer"
        } else {
            &claims.sub
        }
    );
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
            engineer_pin: Some("9999".to_string()),
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
