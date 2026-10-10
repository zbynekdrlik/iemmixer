//! `GET /api/site`: the site links come from the site config.

use super::*;
use axum::body::Body;
use axum::http::Request;
use tower::util::ServiceExt;

#[tokio::test]
async fn site_links_come_from_the_site_config() {
    let dir = tempfile::tempdir().unwrap();
    let config = iem_core::Config {
        lan_url: Some("http://10.0.0.10".to_string()),
        https_domain: Some("mixer.example.org".to_string()),
        ..iem_core::Config::default()
    };
    let state = AppState::new(config, dir.path());
    let app = Router::new()
        .route("/api/site", get(get_site_links))
        .with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/site")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"lan_url": "http://10.0.0.10", "public_host": "mixer.example.org"})
    );
}
