//! The routes' unit tests: private IPs, network mode, content hashes and the push gate.

use super::push::header_has_engineer_token;
use super::static_files::has_content_hash;
use super::*;

#[test]
fn test_is_private_ip_local_ranges() {
    // 10.x.x.x (Class A private)
    assert!(is_private_ip("10.0.0.1"));
    assert!(is_private_ip("10.0.0.10"));
    assert!(is_private_ip("10.255.255.255"));

    // 172.16-31.x.x (Class B private)
    assert!(is_private_ip("172.16.0.1"));
    assert!(is_private_ip("172.31.255.255"));

    // 192.168.x.x (Class C private)
    assert!(is_private_ip("192.168.1.1"));
    assert!(is_private_ip("192.168.0.100"));

    // Loopback
    assert!(is_private_ip("127.0.0.1"));
}

#[test]
fn test_is_private_ip_public_ranges() {
    assert!(!is_private_ip("8.8.8.8"));
    assert!(!is_private_ip("1.1.1.1"));
    assert!(!is_private_ip("203.0.113.50"));
    assert!(!is_private_ip("172.32.0.1")); // Just outside 172.16-31 range
}

#[test]
fn test_is_private_ip_invalid() {
    assert!(!is_private_ip("not_an_ip"));
    assert!(!is_private_ip(""));
}

#[test]
fn test_is_private_ip_ipv6() {
    assert!(is_private_ip("::1")); // loopback
    assert!(!is_private_ip("2001:db8::1")); // documentation range (public)
}

#[test]
fn test_detect_network_mode_matching_ip_is_local() {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("cf-connecting-ip", "203.0.113.50".parse().unwrap());
    let local_ip = Some("203.0.113.50".to_string());
    assert_eq!(detect_network_mode(&headers, &local_ip), "local");
}

#[test]
fn test_detect_network_mode_different_ip_is_remote() {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("cf-connecting-ip", "198.51.100.10".parse().unwrap());
    let local_ip = Some("203.0.113.50".to_string());
    assert_eq!(detect_network_mode(&headers, &local_ip), "remote");
}

#[test]
fn test_detect_network_mode_no_headers_is_local() {
    let headers = axum::http::HeaderMap::new();
    let local_ip = Some("203.0.113.50".to_string());
    assert_eq!(detect_network_mode(&headers, &local_ip), "local");
}

#[test]
fn test_detect_network_mode_no_local_ip_private_is_local() {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("cf-connecting-ip", "10.0.0.100".parse().unwrap());
    assert_eq!(detect_network_mode(&headers, &None), "local");
}

#[test]
fn test_detect_network_mode_no_local_ip_public_is_remote() {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("cf-connecting-ip", "8.8.8.8".parse().unwrap());
    assert_eq!(detect_network_mode(&headers, &None), "remote");
}

#[test]
fn test_detect_network_mode_x_forwarded_for_fallback() {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-forwarded-for", "203.0.113.50, 10.0.0.1".parse().unwrap());
    let local_ip = Some("203.0.113.50".to_string());
    // Should use first IP from x-forwarded-for
    assert_eq!(detect_network_mode(&headers, &local_ip), "local");
}

#[test]
fn test_content_hash_detection_hashed_files() {
    // Trunk-generated files with content hashes → long cache
    assert!(has_content_hash("iem-ui-c72f48fccb666eb9.js"));
    assert!(has_content_hash("iem-ui-c72f48fccb666eb9_bg.wasm"));
    assert!(has_content_hash("style-ccead50460cc69d.css"));
}

#[test]
fn test_content_hash_detection_unhashed_files() {
    // Files without content hashes → no-cache
    assert!(!has_content_hash("audio_player.js"));
    assert!(!has_content_hash("sw.js"));
    assert!(!has_content_hash("manifest.json"));
    assert!(!has_content_hash("index.html"));
    assert!(!has_content_hash("icon.svg"));
    assert!(!has_content_hash("icon-192.png"));
    assert!(!has_content_hash("icon-512.png"));
}

#[test]
fn test_content_hash_detection_with_directory() {
    // Snippet paths: directory has hash but filename doesn't → no-cache
    assert!(!has_content_hash(
        "snippets/iem-ui-fe2cd2496a8b535b/audio_player.js"
    ));
    // Full path with hashed filename → long cache
    assert!(has_content_hash("assets/iem-ui-c72f48fccb666eb9.js"));
}

/// The UI's index answers `/` and an SPA route alike, never cached, and
/// without the CDN header only the service worker carries.
#[tokio::test]
async fn the_index_answers_the_root_and_spa_routes_uncached() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(iem_core::Config::default(), dir.path());
    let app = static_routes().with_state(state);
    let mut pages = Vec::new();
    for uri in ["/", "/member1"] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri}");
        let headers = resp.headers();
        assert_eq!(
            headers.get(header::CONTENT_TYPE).unwrap(),
            "text/html",
            "{uri}"
        );
        assert_eq!(
            headers.get(header::CACHE_CONTROL).unwrap(),
            "no-cache, must-revalidate",
            "{uri}"
        );
        assert!(headers.get("CDN-Cache-Control").is_none(), "{uri}");
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        assert!(!body.is_empty(), "{uri}");
        pages.push(body);
    }
    assert_eq!(pages[0], pages[1], "an SPA route gets the index");
}

// ---- Auth gate tests for push_unsubscribe (reaperiem#188) ----
//
// Verifies the boolean returned by `header_has_engineer_token` matches
// the contract: only an engineer JWT under the configured secret may
// pass. Kills cargo-mutants flips of the engineer claim check that
// would otherwise turn the gate into "always allow" or "always deny".

fn make_test_token(secret: &str, member: &str, engineer: bool) -> String {
    use jsonwebtoken::{EncodingKey, Header, encode};
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = iem_core::AuthClaims {
        sub: member.to_string(),
        engineer,
        exp: now + 3600,
        iat: now,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .unwrap()
}

fn headers_with_bearer(token: &str) -> axum::http::HeaderMap {
    let mut h = axum::http::HeaderMap::new();
    h.insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {}", token).parse().unwrap(),
    );
    h
}

#[test]
fn header_has_engineer_token_true_for_engineer_jwt() {
    let secret = "unit-test-secret";
    let token = make_test_token(secret, "engineer", true);
    let headers = headers_with_bearer(&token);
    assert!(header_has_engineer_token(&headers, secret));
}

#[test]
fn header_has_engineer_token_false_for_member_jwt() {
    let secret = "unit-test-secret";
    let token = make_test_token(secret, "member1", false);
    let headers = headers_with_bearer(&token);
    assert!(!header_has_engineer_token(&headers, secret));
}

#[test]
fn header_has_engineer_token_false_when_no_authorization_header() {
    let headers = axum::http::HeaderMap::new();
    assert!(!header_has_engineer_token(&headers, "any-secret"));
}

#[test]
fn header_has_engineer_token_false_when_secret_mismatches() {
    // Engineer JWT signed under a DIFFERENT secret must be rejected.
    let token = make_test_token("issued-under-this", "engineer", true);
    let headers = headers_with_bearer(&token);
    assert!(!header_has_engineer_token(
        &headers,
        "but-validated-under-this"
    ));
}

// ---- Router-level integration tests for POST /api/push/unsubscribe (reaperiem#188) ----
//
// Exercises the full handler with a real `AppState` + axum Router. Kills
// mutations cargo-mutants would otherwise leave on the auth-gate negation
// (`!header_has_engineer_token(...)` → drop the `!` flips engineer→403,
// member→200). Verifies the 200/403/400 contract end-to-end.

fn push_unsubscribe_test_state(secret: &str) -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = iem_core::Config {
        jwt_secret: secret.to_string(),
        ..Default::default()
    };
    let state = AppState::new(config, dir.path());
    (state, dir)
}

fn push_unsubscribe_test_router(state: AppState) -> axum::Router {
    use axum::routing::post;
    axum::Router::new()
        .route("/api/push/unsubscribe", post(push_unsubscribe))
        .with_state(state)
}

#[tokio::test]
async fn push_unsubscribe_engineer_token_returns_200() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let secret = "router-test-secret";
    let (state, _dir) = push_unsubscribe_test_state(secret);

    // Pre-seed the store with the endpoint we'll ask to remove so the
    // handler does real work.
    {
        let mut store = state.push_store.write().await;
        store
            .add(crate::push_store::PushSubscription {
                endpoint: "https://fcm.example/abc".into(),
                p256dh: "k".into(),
                auth: "a".into(),
            })
            .unwrap();
    }

    let token = make_test_token(secret, "engineer", true);
    let router = push_unsubscribe_test_router(state.clone());
    let req = Request::builder()
        .method("POST")
        .uri("/api/push/unsubscribe")
        .header("authorization", format!("Bearer {}", token))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"endpoint":"https://fcm.example/abc"}"#))
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Side effect: endpoint actually removed.
    let store = state.push_store.read().await;
    assert!(
        store
            .all()
            .iter()
            .all(|s| s.endpoint != "https://fcm.example/abc"),
        "engineer call must remove the endpoint"
    );
}

#[tokio::test]
async fn push_unsubscribe_member_token_returns_403() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let secret = "router-test-secret";
    let (state, _dir) = push_unsubscribe_test_state(secret);

    let token = make_test_token(secret, "member1", false);
    let router = push_unsubscribe_test_router(state);
    let req = Request::builder()
        .method("POST")
        .uri("/api/push/unsubscribe")
        .header("authorization", format!("Bearer {}", token))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"endpoint":"https://fcm.example/abc"}"#))
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn push_unsubscribe_no_auth_returns_403() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let secret = "router-test-secret";
    let (state, _dir) = push_unsubscribe_test_state(secret);

    let router = push_unsubscribe_test_router(state);
    let req = Request::builder()
        .method("POST")
        .uri("/api/push/unsubscribe")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"endpoint":"https://fcm.example/abc"}"#))
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn push_unsubscribe_engineer_with_empty_endpoint_returns_400() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let secret = "router-test-secret";
    let (state, _dir) = push_unsubscribe_test_state(secret);

    let token = make_test_token(secret, "engineer", true);
    let router = push_unsubscribe_test_router(state);
    let req = Request::builder()
        .method("POST")
        .uri("/api/push/unsubscribe")
        .header("authorization", format!("Bearer {}", token))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"endpoint":""}"#))
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// X10: the raw REAPER passthrough is gone — even an engineer gets 404.
#[tokio::test]
async fn raw_reaper_passthrough_is_gone() {
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use tower::ServiceExt;

    let secret = "passthrough-test-secret";
    let dir = tempfile::tempdir().unwrap();
    let config = iem_core::Config {
        jwt_secret: secret.to_string(),
        ..iem_core::Config::default()
    };
    let state = AppState::new(config, dir.path());
    let router = api_routes(state.clone()).with_state(state);
    let token = make_test_token(secret, "engineer", true);
    for method in [Method::GET, Method::POST] {
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri("/api/reaper/_/NTRACK")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{method}");
    }
}
