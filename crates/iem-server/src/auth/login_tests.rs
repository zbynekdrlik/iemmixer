//! The login and PIN-change handlers' tests: PINs, budgets, the gate and the PIN freeze.

use super::*;
use crate::login_guard::{HashGate, HostAddrs, LoginGuard, Origin};
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

fn member(id: &str) -> iem_core::SiteMember {
    iem_core::SiteMember {
        id: id.to_string(),
        name: id.to_uppercase(),
        mix: id.to_string(),
    }
}

/// member1 (PIN set), member2 (no PIN yet), engineer; fast test hasher.
async fn test_state(dir: &std::path::Path) -> AppState {
    test_state_with(dir, true).await
}

/// [`test_state`] with PIN changes frozen, as every `dev` site is
/// before the cutover.
async fn frozen_state(dir: &std::path::Path) -> AppState {
    test_state_with(dir, false).await
}

async fn test_state_with(dir: &std::path::Path, pin_changes: bool) -> AppState {
    let config = iem_core::Config {
        jwt_secret: SECRET.to_string(),
        members: vec![member("member1"), member("member2"), member("engineer")],
        pin_changes,
        ..iem_core::Config::default()
    };
    let mut state = AppState::new(config, dir);
    state.pin_hasher = PinHasher::for_tests([5u8; PEPPER_LEN]);
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
async fn an_empty_pin_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(test_state(dir.path()).await, LAN);
    // member1 and the engineer have PINs, member2 has none yet: an empty
    // PIN opens none of them.
    for member in ["member1", "engineer", "member2"] {
        let resp = login_as(&app, member, "", &[]).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{member}");
        let body = json_body(resp).await;
        assert_eq!(body["code"], "INVALID_PIN", "{member}");
        assert!(body.get("token").is_none(), "{member}: {body}");
    }
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
async fn a_tunnel_on_the_hosts_own_address_is_keyed_by_its_client() {
    // cloudflared's origin may target the host's LAN address instead of
    // 127.0.0.1: its connections then come from that address.
    const HOST: [u8; 4] = [192, 0, 2, 10];
    let dir = tempfile::tempdir().unwrap();
    let mut state = test_state(dir.path()).await;
    state.login_guard = Arc::new(LoginGuard::for_host(HostAddrs::new([
        std::net::IpAddr::from(HOST),
    ])));
    let tunnel = app(state.clone(), HOST);
    let first = [("cf-connecting-ip", "198.51.100.7")];
    for _ in 0..3 {
        assert_eq!(
            login_as(&tunnel, "member1", WRONG_PIN, &first)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        login_as(&tunnel, "member1", MEMBER_PIN, &first)
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS,
        "limited per 198.51.100.7"
    );
    let second = [("cf-connecting-ip", "198.51.100.8")];
    assert_eq!(
        login_as(&tunnel, "member1", MEMBER_PIN, &second)
            .await
            .status(),
        StatusCode::OK,
        "another client behind the tunnel"
    );
    assert_eq!(state.login_guard.stats().tunnel_failures, 3);
    // A peer that is not the host: the same header changes nothing.
    let lan = app(state.clone(), [192, 0, 2, 50]);
    assert_eq!(
        login_as(&lan, "member1", MEMBER_PIN, &first).await.status(),
        StatusCode::OK,
        "keyed by its own address, not by the header"
    );
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

async fn assert_frozen(resp: Response) {
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = json_body(resp).await;
    assert_eq!(body["code"], "PIN_CHANGES_FROZEN");
    assert_eq!(body["message"], iem_core::PIN_CHANGES_FROZEN);
}

#[tokio::test]
async fn pin_change_is_refused_while_frozen() {
    let dir = tempfile::tempdir().unwrap();
    let state = frozen_state(dir.path()).await;
    let app = app(state.clone(), LAN);
    let auth = bearer("member1", false);
    let body = serde_json::json!({ "old_pin": MEMBER_PIN, "new_pin": NEW_PIN });
    assert_frozen(change(&app, &auth, body).await).await;
    // No PIN is checked while frozen: a wrong current PIN counts nothing.
    let wrong = serde_json::json!({ "old_pin": WRONG_PIN, "new_pin": NEW_PIN });
    assert_frozen(change(&app, &auth, wrong).await).await;
    assert_eq!(state.login_guard.stats().lan_failures, 0);
    assert_eq!(
        login_as(&app, "member1", NEW_PIN, &[]).await.status(),
        StatusCode::UNAUTHORIZED,
        "the PIN did not change"
    );
    assert_eq!(
        login_as(&app, "member1", MEMBER_PIN, &[]).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn pin_reset_is_refused_while_frozen() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(frozen_state(dir.path()).await, LAN);
    let engineer = bearer("engineer", true);
    let reset = serde_json::json!({ "new_pin": NEW_PIN, "member": "member2" });
    assert_frozen(change(&app, &engineer, reset).await).await;
    let rotate = serde_json::json!({ "new_pin": NEW_PIN, "member": "engineer" });
    assert_frozen(change(&app, &engineer, rotate).await).await;
    assert_eq!(
        login_as(&app, "member2", NEW_PIN, &[]).await.status(),
        StatusCode::UNAUTHORIZED,
        "member2 still has no PIN"
    );
    assert_eq!(
        json_body(login_as(&app, "engineer", ENGINEER_PIN, &[]).await).await["engineer"],
        true,
        "the engineer PIN is unchanged"
    );
}

#[tokio::test]
async fn login_still_works_while_frozen() {
    let dir = tempfile::tempdir().unwrap();
    let state = frozen_state(dir.path()).await;
    let app = app(state.clone(), LAN);
    assert_eq!(
        json_body(login_as(&app, "member1", MEMBER_PIN, &[]).await).await["member"],
        "member1"
    );
    assert_eq!(
        json_body(login_as(&app, "member2", ENGINEER_PIN, &[]).await).await["engineer"],
        true
    );
    let peer = SocketAddr::from((LAN, 40000));
    assert!(
        verify_engineer_pin(&state, peer, &HeaderMap::new(), ENGINEER_PIN)
            .await
            .is_ok(),
        "the engineer's switch PIN check too"
    );
}

#[tokio::test]
async fn the_engineer_pin_check_is_a_guarded_login() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let peer = SocketAddr::from((LAN, 40000));
    let headers = HeaderMap::new();
    assert!(
        verify_engineer_pin(&state, peer, &headers, ENGINEER_PIN)
            .await
            .is_ok()
    );
    for _ in 0..3 {
        let r = verify_engineer_pin(&state, peer, &headers, MEMBER_PIN)
            .await
            .expect_err("a member PIN is not the engineer PIN")
            .into_response();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }
    let r = verify_engineer_pin(&state, peer, &headers, ENGINEER_PIN)
        .await
        .expect_err("throttled after three failures")
        .into_response();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(state.login_guard.stats().lan_failures, 3);
}
