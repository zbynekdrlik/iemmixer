//! Web Push (F21): the VAPID public key and the engineer's subscriptions.

use axum::{
    Json,
    http::{StatusCode, header},
    response::IntoResponse,
};

use crate::{AppState, auth};

/// Return the VAPID public key for browser push subscription (reaperiem#133).
pub(super) async fn get_vapid_key(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> impl IntoResponse {
    let config = state.config.read().await;
    if config.vapid_private_key.is_empty() {
        return (StatusCode::OK, Json(serde_json::json!({ "key": null })));
    }
    match iem_core::config::Config::vapid_public_key_base64url(&config.vapid_private_key) {
        Ok(pub_key) => (StatusCode::OK, Json(serde_json::json!({ "key": pub_key }))),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "invalid VAPID key" })),
        ),
    }
}

/// Store a push subscription (engineer-only) (reaperiem#133).
pub(super) async fn push_subscribe(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    // Verify engineer token
    let config = state.config.read().await;
    let claims = match auth::extract_claims(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .unwrap_or(""),
        &config.jwt_secret,
    ) {
        Some(c) if c.engineer => c,
        _ => {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "error": "engineer access required" })),
            );
        }
    };
    drop(config);
    // Suppress unused variable warning (claims used only for engineer check)
    let _ = claims;

    // Parse subscription
    let endpoint = body["endpoint"].as_str().unwrap_or("").to_string();
    let p256dh = body["keys"]["p256dh"].as_str().unwrap_or("").to_string();
    let auth_key = body["keys"]["auth"].as_str().unwrap_or("").to_string();

    if endpoint.is_empty() || p256dh.is_empty() || auth_key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "missing endpoint, p256dh, or auth" })),
        );
    }

    let sub = crate::push_store::PushSubscription {
        endpoint,
        p256dh,
        auth: auth_key,
    };
    let mut store = state.push_store.write().await;
    match store.add(sub) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({ "ok": true }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("save failed: {}", e) })),
        ),
    }
}

/// Returns true iff the request's `Authorization: Bearer <jwt>` header decodes
/// to a valid engineer claim under the given secret. Pulled out as a function
/// so its decision can be unit-tested independently of the handler's
/// `AppState` plumbing — the boolean check is exactly what cargo-mutants tries
/// to flip. (reaperiem#188)
pub(super) fn header_has_engineer_token(headers: &axum::http::HeaderMap, jwt_secret: &str) -> bool {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");
    matches!(auth::extract_claims(token, jwt_secret), Some(c) if c.engineer)
}

/// Remove a stored push subscription by endpoint URL (engineer-only) (reaperiem#188).
///
/// Idempotent: returns 200 even if the endpoint is not in the store. The leaving
/// client is the source of truth for which endpoint to forget — the server has
/// no per-member association to look it up otherwise.
pub(super) async fn push_unsubscribe(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let config = state.config.read().await;
    if !header_has_engineer_token(&headers, &config.jwt_secret) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "engineer access required" })),
        );
    }
    drop(config);

    let endpoint = body["endpoint"].as_str().unwrap_or("").to_string();
    if endpoint.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "missing endpoint" })),
        );
    }

    let mut store = state.push_store.write().await;
    store.remove_endpoint(&endpoint);
    (StatusCode::OK, Json(serde_json::json!({ "ok": true })))
}
