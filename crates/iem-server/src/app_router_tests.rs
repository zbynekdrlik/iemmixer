//! The app router: same-origin only, no CORS grant.

use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::util::ServiceExt;

const FOREIGN: &str = "https://evil.example";

#[tokio::test]
async fn a_foreign_origin_gets_no_cors_grant() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_router(AppState::new(Config::default(), dir.path()));

    let simple = app
        .clone()
        .oneshot(
            Request::get("/api/version")
                .header(header::ORIGIN, FOREIGN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        simple
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
    );

    let preflight = app
        .oneshot(
            Request::options("/api/auth")
                .header(header::ORIGIN, FOREIGN)
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        preflight
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
    );
    assert!(
        preflight
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_METHODS)
            .is_none()
    );
}

#[tokio::test]
async fn a_same_origin_request_still_works() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_router(AppState::new(Config::default(), dir.path()));
    let resp = app
        .oneshot(
            Request::get("/api/version")
                .header(header::HOST, "10.0.0.10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get("x-frame-options").unwrap(), "DENY");
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["version"], iem_core::VERSION);
}

async fn headers_for(config: Config) -> axum::http::HeaderMap {
    let dir = tempfile::tempdir().unwrap();
    let app = app_router(AppState::new(config, dir.path()));
    let resp = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    resp.headers().clone()
}

#[tokio::test]
async fn no_page_may_frame_the_ui_by_default() {
    let h = headers_for(Config::default()).await;
    assert_eq!(h.get("x-frame-options").unwrap(), "DENY");
    let csp = h.get("content-security-policy").unwrap().to_str().unwrap();
    assert!(csp.ends_with("; frame-ancestors 'none'"), "{csp}");
}

#[tokio::test]
async fn a_listed_parent_may_frame_the_ui() {
    let config = Config {
        frame_ancestors: vec!["https://surface.example".into()],
        ..Config::default()
    };
    let h = headers_for(config).await;
    assert!(h.get("x-frame-options").is_none());
    let csp = h.get("content-security-policy").unwrap().to_str().unwrap();
    assert!(
        csp.ends_with("; frame-ancestors https://surface.example"),
        "{csp}"
    );
    assert!(csp.starts_with("default-src 'self'; "), "{csp}");
}
