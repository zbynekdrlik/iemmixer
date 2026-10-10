use super::*;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request};
use std::net::SocketAddr;
use tower::util::ServiceExt;

pub(crate) const SECRET: &str = "api-test-secret";

pub(crate) fn token(sub: &str, engineer: bool) -> String {
    let claims = iem_core::AuthClaims {
        sub: sub.into(),
        engineer,
        exp: u64::MAX / 2,
        iat: 0,
    };
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(SECRET.as_bytes()),
    )
    .unwrap()
}

/// A state without an engine (tokens signed with [`SECRET`]) and its API.
pub(crate) fn app(dir: &std::path::Path) -> (AppState, Router) {
    let mut config = crate::site_view::tests::test_config();
    config.jwt_secret = SECRET.into();
    let state = AppState::new(config, dir);
    (state.clone(), router(state))
}

/// The API routes over `state`, reached from a LAN client.
pub(crate) fn router(state: AppState) -> Router {
    api_routes(state.clone())
        .with_state(state)
        .layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 50], 40000))))
}

pub(crate) async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    bearer: Option<&str>,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = bearer {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let resp = app
        .clone()
        .oneshot(
            req.body(Body::from(body.unwrap_or("").to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn members_are_the_sites() {
    let dir = tempfile::tempdir().unwrap();
    let (_s, app) = app(dir.path());
    let (status, json) = call(&app, Method::GET, "/api/members", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let list = json.as_array().unwrap();
    assert_eq!(list.len(), 10);
    assert_eq!(
        list[0],
        serde_json::json!({"id": "member1", "name": "Member1", "has_photo": false})
    );
    assert_eq!(list[9]["id"], "engineer");
}

#[tokio::test]
async fn the_mixer_state_is_guarded_per_page() {
    let dir = tempfile::tempdir().unwrap();
    let (_s, app) = app(dir.path());
    let m1 = token("member1", false);
    let eng = token("engineer", true);
    assert_eq!(
        call(&app, Method::GET, "/api/mixer/member1", None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, Method::GET, "/api/mixer/member2", Some(&m1), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, json) = call(&app, Method::GET, "/api/mixer/member1", Some(&m1), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["member_id"], "member1");
    assert_eq!(json["channels"], serde_json::json!([]), "no engine yet");
    assert_eq!(
        call(&app, Method::GET, "/api/mixer/member2", Some(&eng), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, Method::GET, "/api/mixer/nobody", Some(&eng), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn mute_all_and_pins_need_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let (_s, app) = app(dir.path());
    let eng = token("engineer", true);
    let (status, json) = call(
        &app,
        Method::POST,
        "/api/mixer/engineer/batch",
        Some(&eng),
        Some(r#"{"operation":"mute_all"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json["code"], "ENGINE_UNAVAILABLE");
    assert_eq!(
        call(
            &app,
            Method::POST,
            "/api/mixer/engineer/batch",
            Some(&eng),
            Some(r#"{"operation":"reset"}"#)
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the batch Reset is gone"
    );
    let m1 = token("member1", false);
    let (status, json) = call(
        &app,
        Method::GET,
        "/api/mixer/member1/customization",
        Some(&m1),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, serde_json::json!({"pinned": [], "hidden": []}));
    assert_eq!(
        call(
            &app,
            Method::PUT,
            "/api/mixer/member1/customization",
            Some(&m1),
            Some(r#"{"pinned":["mic1"],"hidden":[]}"#)
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        call(
            &app,
            Method::GET,
            "/api/mixer/member2/customization",
            Some(&m1),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn client_errors_are_accepted_up_to_ten_kib() {
    let dir = tempfile::tempdir().unwrap();
    let (_s, app) = app(dir.path());
    let body = |n: usize| format!(r#"{{"panic_message":"{}"}}"#, "y".repeat(n));
    let under = body(9 * 1024);
    assert!(under.len() < 10_240);
    assert_eq!(
        call(&app, Method::POST, "/api/client-error", None, Some(&under))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let over = body(11 * 1024);
    assert_eq!(
        call(&app, Method::POST, "/api/client-error", None, Some(&over))
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let full = r#"{"panic_message":"boom","version":"2.0.0","git_hash":"abc","url":"/engineer","user_agent":"UA","location":"f:1:1","backtrace":"trace"}"#;
    assert_eq!(
        call(&app, Method::POST, "/api/client-error", None, Some(full))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(
            &app,
            Method::POST,
            "/api/client-error",
            None,
            Some(r#"{"version":"1"}"#)
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

/// The report the UI's panic hook posts (`iem-ui/src/lifecycle.rs`),
/// every field set.
const FULL_REPORT: &str = r#"{"panic_message":"marker-full","version":"9.9.9","git_hash":"deadbee","url":"/trace-test","user_agent":"TraceUA","location":"trace.rs:1:1","backtrace":"marker-backtrace"}"#;

#[test]
fn client_error_reports_parse_every_field() {
    let r: ClientErrorReport = serde_json::from_str(FULL_REPORT).unwrap();
    assert_eq!(
        (
            r.panic_message.as_str(),
            r.version.as_deref(),
            r.git_hash.as_deref(),
            r.url.as_deref(),
            r.user_agent.as_deref(),
            r.location.as_deref(),
            r.backtrace.as_deref()
        ),
        (
            "marker-full",
            Some("9.9.9"),
            Some("deadbee"),
            Some("/trace-test"),
            Some("TraceUA"),
            Some("trace.rs:1:1"),
            Some("marker-backtrace")
        )
    );
    let min: ClientErrorReport = serde_json::from_str(r#"{"panic_message":"boom"}"#).unwrap();
    assert_eq!(min.panic_message, "boom");
    assert!(
        min.version.is_none()
            && min.git_hash.is_none()
            && min.url.is_none()
            && min.user_agent.is_none()
            && min.location.is_none()
            && min.backtrace.is_none()
    );
    assert!(
        serde_json::from_str::<ClientErrorReport>(r#"{"version":"1.2.3"}"#).is_err(),
        "a report without its panic message"
    );
}

#[tokio::test]
#[tracing_test::traced_test]
async fn client_errors_are_logged_with_their_fields_and_backtrace() {
    let dir = tempfile::tempdir().unwrap();
    let (_s, app) = app(dir.path());
    let post = |body: &'static str| {
        let app = app.clone();
        async move {
            call(&app, Method::POST, "/api/client-error", None, Some(body))
                .await
                .0
        }
    };
    // A degraded client sends only the message: "?" for the rest, and
    // no backtrace line.
    assert_eq!(
        post(r#"{"panic_message":"marker-min"}"#).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(post(FULL_REPORT).await, StatusCode::NO_CONTENT);
    logs_assert(|lines: &[&str]| {
        let one = |marker: &str| -> Result<String, String> {
            match lines
                .iter()
                .filter(|l| l.contains(marker))
                .collect::<Vec<_>>()
                .as_slice()
            {
                [l] => Ok(l.to_string()),
                other => Err(format!("one line with {marker}, got {other:?}")),
            }
        };
        let min = one("panic=marker-min")?;
        let full = one("panic=marker-full")?;
        let bt = one("client_error_backtrace")?;
        let wants: [(&String, &[&str]); 3] = [
            (
                &min,
                &[
                    " WARN ",
                    "iem_server::client_error: client_error ",
                    r#"version="?""#,
                    r#"git_hash="?""#,
                    r#"url="?""#,
                    r#"user_agent="?""#,
                    r#"location="?""#,
                ],
            ),
            (
                &full,
                &[
                    " WARN ",
                    "iem_server::client_error: client_error ",
                    r#"version="9.9.9""#,
                    r#"git_hash="deadbee""#,
                    r#"url="/trace-test""#,
                    r#"user_agent="TraceUA""#,
                    r#"location="trace.rs:1:1""#,
                ],
            ),
            (
                &bt,
                &[
                    " WARN ",
                    "iem_server::client_error: client_error_backtrace ",
                    "backtrace=marker-backtrace",
                ],
            ),
        ];
        for (line, parts) in wants {
            if let Some(p) = parts.iter().find(|p| !line.contains(**p)) {
                return Err(format!("{p:?} missing in {line:?}"));
            }
        }
        Ok(())
    });
}

#[tokio::test]
async fn the_switch_is_the_engineers_and_needs_the_engineer_pin() {
    let dir = tempfile::tempdir().unwrap();
    let (state, app) = app(dir.path());
    let hash = state.pin_hasher.hash("2468");
    state
        .pin_store
        .write()
        .await
        .set_engineer_hash(hash)
        .unwrap();
    let body = Some(r#"{"pin":"2468"}"#);
    assert_eq!(
        call(&app, Method::POST, "/api/mode/event", None, body)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            Method::POST,
            "/api/mode/event",
            Some(&token("member1", false)),
            body
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let eng = token("engineer", true);
    assert_eq!(
        call(
            &app,
            Method::POST,
            "/api/mode/event",
            Some(&eng),
            Some(r#"{"pin":"1111"}"#)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, json) = call(&app, Method::POST, "/api/mode/event", Some(&eng), body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{json}");
    assert_eq!(json["ok"], true);
    // A site without a switch has no button to press.
    let dir2 = tempfile::tempdir().unwrap();
    let config = iem_core::Config {
        jwt_secret: SECRET.into(),
        ..Default::default()
    };
    let bare = AppState::new(config, dir2.path());
    let app2 = api_routes(bare.clone())
        .with_state(bare)
        .layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 51], 40000))));
    assert_eq!(
        call(&app2, Method::POST, "/api/mode/event", Some(&eng), body)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

/// `GET uri` without a token: the status, the content type and the body.
async fn get_raw(app: &Router, uri: &str) -> (StatusCode, Option<String>, Vec<u8>) {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, content_type, bytes.to_vec())
}

/// F22: anyone reads a member's photo; only the member or the engineer
/// changes it, with valid base64 of at most 256 KiB.
#[tokio::test]
async fn photos_are_read_by_anyone_and_changed_by_their_member_up_to_256_kib() {
    use base64::Engine as _;
    let dir = tempfile::tempdir().unwrap();
    let (state, app) = app(dir.path());
    let m1 = token("member1", false);
    let m2 = token("member2", false);
    let eng = token("engineer", true);
    let uri = "/api/members/member1/photo";
    let photo = |bytes: &[u8]| {
        serde_json::json!({ "photo": base64::engine::general_purpose::STANDARD.encode(bytes) })
            .to_string()
    };
    let limit = vec![0xA5_u8; 256 * 1024];
    let over = vec![0x5A_u8; 256 * 1024 + 1];
    let small = photo(&b"jpeg"[..]);

    assert_eq!(get_raw(&app, uri).await.0, StatusCode::NOT_FOUND);
    for (bearer, want) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some(m2.as_str()), StatusCode::FORBIDDEN),
    ] {
        let (status, _) = call(&app, Method::POST, uri, bearer, Some(small.as_str())).await;
        assert_eq!(status, want);
    }
    let (status, json) = call(
        &app,
        Method::POST,
        uri,
        Some(&m1),
        Some(r#"{"photo":"%%%"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "INVALID_DATA");
    let too_large = photo(&over[..]);
    let (status, json) = call(&app, Method::POST, uri, Some(&m1), Some(too_large.as_str())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "TOO_LARGE");
    assert!(!state.photo_store.exists("member1"), "nothing saved yet");

    let at_limit = photo(&limit[..]);
    let (status, json) = call(&app, Method::POST, uri, Some(&m1), Some(at_limit.as_str())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, serde_json::json!({"ok": true}));
    let (status, content_type, body) = get_raw(&app, uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("image/jpeg"));
    assert_eq!(body, limit, "exactly 256 KiB is kept");

    let (status, _) = call(&app, Method::POST, uri, Some(&eng), Some(small.as_str())).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the engineer changes any member's photo"
    );
    assert_eq!(get_raw(&app, uri).await.2, b"jpeg".to_vec());

    assert_eq!(
        call(&app, Method::DELETE, uri, Some(&m2), None).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, json) = call(&app, Method::DELETE, uri, Some(&m1), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, serde_json::json!({"ok": true}));
    assert_eq!(get_raw(&app, uri).await.0, StatusCode::NOT_FOUND);
    assert!(!state.photo_store.exists("member1"));
}

/// F21: only the engineer subscribes a device, and only with its endpoint
/// and both keys.
#[tokio::test]
async fn push_subscriptions_are_the_engineers_and_need_every_field() {
    let dir = tempfile::tempdir().unwrap();
    let (state, app) = app(dir.path());
    let eng = token("engineer", true);
    let m1 = token("member1", false);
    let uri = "/api/push/subscribe";
    let full = r#"{"endpoint":"https://push.example.org/e1","keys":{"p256dh":"k1","auth":"a1"}}"#;

    for bearer in [None, Some(m1.as_str())] {
        let (status, json) = call(&app, Method::POST, uri, bearer, Some(full)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(json["error"], "engineer access required");
    }
    for missing in [
        r#"{"keys":{"p256dh":"k1","auth":"a1"}}"#,
        r#"{"endpoint":"https://push.example.org/e1","keys":{"auth":"a1"}}"#,
        r#"{"endpoint":"https://push.example.org/e1","keys":{"p256dh":"k1"}}"#,
    ] {
        let (status, json) = call(&app, Method::POST, uri, Some(&eng), Some(missing)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{missing}");
        assert_eq!(json["error"], "missing endpoint, p256dh, or auth");
    }
    assert!(state.push_store.read().await.all().is_empty());

    let (status, json) = call(&app, Method::POST, uri, Some(&eng), Some(full)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, serde_json::json!({"ok": true}));
    let store = state.push_store.read().await;
    assert_eq!(store.all().len(), 1);
    let sub = &store.all()[0];
    assert_eq!(
        (
            sub.endpoint.as_str(),
            sub.p256dh.as_str(),
            sub.auth.as_str()
        ),
        ("https://push.example.org/e1", "k1", "a1")
    );
}

/// F28: the listen and talkback diagnostics are the engineer's.
#[cfg(feature = "audio")]
#[tokio::test]
async fn the_audio_diagnostics_are_the_engineers() {
    let dir = tempfile::tempdir().unwrap();
    let (_s, app) = app(dir.path());
    let eng = token("engineer", true);
    let m1 = token("member1", false);
    for uri in ["/api/audio/diagnostics", "/api/talkback/diagnostics"] {
        for (bearer, want) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some(m1.as_str()), StatusCode::FORBIDDEN),
        ] {
            let (status, _) = call(&app, Method::GET, uri, bearer, None).await;
            assert_eq!(status, want, "{uri}");
        }
    }
    let (status, json) = call(
        &app,
        Method::GET,
        "/api/audio/diagnostics",
        Some(&eng),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["receiving_oiem"], false);
    assert_eq!(json["frames_forwarded"], 0);
    let (status, json) = call(
        &app,
        Method::GET,
        "/api/talkback/diagnostics",
        Some(&eng),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["packets_in"], 0);
    assert_eq!(json["active_talker"], serde_json::Value::Null);
}
