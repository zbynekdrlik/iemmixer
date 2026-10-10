//! The tokens' tests: expiry, claims and member access.

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
