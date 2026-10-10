//! API and static file routes

use axum::{
    Json, Router,
    extract::Path,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::{delete, get, post, put},
};
use serde::Serialize;

use crate::{AppState, auth, backup_routes, preset_routes, snapshot_routes};
use axum::extract::State;
use iem_core::ApiError;

mod audio;
mod photos;
mod push;
mod static_files;

use self::audio::{
    audio_diagnostics_handler, talkback_diagnostics_handler, ws_audio_handler, ws_talkback_handler,
};
use self::photos::{delete_photo, get_photo, post_photo};
use self::push::{get_vapid_key, push_subscribe, push_unsubscribe};
pub use self::static_files::static_routes;

/// Version information for deployment verification
#[derive(Serialize)]
pub struct VersionInfo {
    pub version: &'static str,
    pub git_hash: &'static str,
    pub branch: &'static str,
    pub build_time: &'static str,
    pub deployed_at: String,
    pub full_version: String,
}

/// Get build version info - used by CI to verify correct version is deployed
async fn get_version() -> Json<VersionInfo> {
    Json(VersionInfo {
        version: iem_core::VERSION,
        git_hash: iem_core::git_hash(),
        branch: iem_core::git_branch(),
        build_time: iem_core::build_time(),
        deployed_at: iem_core::deployed_at(),
        full_version: iem_core::full_version(),
    })
}

/// `GET /api/site` — where the mixer is reachable (LAN URL, public host), for
/// the UI's tunnel banner and reconnect hint. Public: it holds no secret.
async fn get_site_links(State(state): State<AppState>) -> Json<iem_core::tunnel::SiteLinks> {
    Json(state.config.read().await.site_links())
}

/// API routes
pub fn api_routes(_state: AppState) -> Router<AppState> {
    Router::new()
        // Version endpoint (used by CI for deployment verification)
        .route("/api/version", get(get_version))
        // Where the mixer is reachable (LAN URL, public host)
        .route("/api/site", get(get_site_links))
        // How this request was classified (HIL v2's tunnel and LAN peer)
        .route("/api/peer", get(crate::peer_route::get_peer))
        // Auth login (returns JWT)
        .route("/api/auth", post(auth::login))
        // Member list (landing page)
        .route("/api/members", get(get_members))
        .route("/api/auth/change-pin", post(auth::change_pin))
        // A page's mixer state (also the UI's token check)
        .route("/api/mixer/{page}", get(get_mixer_state))
        // Mute All (F15)
        .route("/api/mixer/{page}/batch", post(batch_control))
        // Network mode detection (local LAN vs remote internet)
        .route("/api/network-mode", get(get_network_mode))
        // Pins and hides (F8)
        .route("/api/mixer/{page}/customization", get(get_customization))
        .route("/api/mixer/{page}/customization", put(put_customization))
        // Member photo (F22)
        .route("/api/members/{member_id}/photo", get(get_photo))
        .route("/api/members/{member_id}/photo", post(post_photo))
        .route("/api/members/{member_id}/photo", delete(delete_photo))
        // Listen (engineer) and talkback — before /ws/{page}
        .route("/ws/audio", get(ws_audio_handler))
        .route("/ws/talkback", get(ws_talkback_handler))
        .route("/api/audio/diagnostics", get(audio_diagnostics_handler))
        .route(
            "/api/talkback/diagnostics",
            get(talkback_diagnostics_handler),
        )
        // Web Push (F21)
        .route("/api/push/vapid-key", get(get_vapid_key))
        .route(
            "/api/client-error",
            // 10_240 bytes = 10 KiB, written as a literal so cargo-mutants has
            // no operator to mutate; pinned by the router tests below.
            post(client_error).layer(axum::extract::DefaultBodyLimit::max(10_240)),
        )
        .route("/api/push/subscribe", post(push_subscribe))
        .route("/api/push/unsubscribe", post(push_unsubscribe))
        // The engineer's "Back to REAPER" (§4.3)
        .route("/api/mode/event", post(crate::console::back_to_reaper))
        // The mixer page's WebSocket
        .route("/ws/{page}", get(crate::mixer_ws::ws_mixer))
        .merge(snapshot_routes::snapshot_routes())
        .merge(preset_routes::preset_routes())
        .merge(backup_routes::backup_routes())
        .merge(crate::tunnel_watch::tunnel_routes())
}

/// The site's members (the landing page's tiles).
async fn get_members(State(state): State<AppState>) -> impl IntoResponse {
    let members: Vec<MemberInfo> = state
        .site_config
        .members
        .iter()
        .map(|m| MemberInfo {
            id: m.id.clone(),
            name: m.name.clone(),
            has_photo: state.photo_store.exists(&m.id),
        })
        .collect();
    Json(members)
}

#[derive(serde::Serialize)]
struct MemberInfo {
    id: String,
    name: String,
    has_photo: bool,
}

type Reject = (StatusCode, Json<ApiError>);

/// The page `page` if the request's token may open it.
pub(crate) async fn page_for(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    page: &str,
) -> Result<(crate::site_view::Page, crate::view::Viewer), Reject> {
    let claims = {
        let config = state.config.read().await;
        auth::verify_member_access(headers, page, &config.jwt_secret)?
    };
    let page = state
        .page(page)
        .ok_or((StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))))?;
    if !crate::mixer_ws::may_open(&claims, &page) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiError::new("FORBIDDEN", "This page is the engineer's")),
        ));
    }
    Ok((
        page,
        crate::view::Viewer {
            sub: claims.sub,
            engineer: claims.engineer,
        },
    ))
}

fn engine_unavailable() -> Reject {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiError::new(
            "ENGINE_UNAVAILABLE",
            "The engine is not connected",
        )),
    )
}

/// `GET /api/mixer/{page}` — the page's channels.
async fn get_mixer_state(
    State(state): State<AppState>,
    Path(page): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<iem_core::MixerState>, Reject> {
    let (page, viewer) = page_for(&state, &headers, &page).await?;
    let channels = match state.site() {
        Some(site) => crate::view::channels(&site, &state.engine.mirror(), &page, &viewer),
        None => Vec::new(),
    };
    Ok(Json(iem_core::MixerState {
        member_id: page.id,
        channels,
    }))
}

/// `POST /api/mixer/{page}/batch` — Mute All (F15): one engine batch.
async fn batch_control(
    State(state): State<AppState>,
    Path(page): Path<String>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<iem_core::BatchControlRequest>,
) -> Result<StatusCode, Reject> {
    let (page, _) = page_for(&state, &headers, &page).await?;
    let site = state.site().ok_or_else(engine_unavailable)?;
    match payload.operation {
        iem_core::BatchOperation::MuteAll => {
            let cmd = crate::view::mute_all(&site, &page);
            state.apply(&page, vec![cmd], None).await.map_err(|e| {
                tracing::error!(page = %page.id, error = %e, "Mute All failed");
                (
                    StatusCode::BAD_GATEWAY,
                    Json(ApiError::new("ENGINE_ERROR", e.to_string())),
                )
            })?;
            tracing::info!(page = %page.id, "Mute All");
        }
    }
    Ok(StatusCode::OK)
}

/// Error report sent by the WASM client when a panic occurs.
///
/// All fields except `panic_message` are optional so that degraded clients
/// (e.g. broken Leptos graph, missing window globals) can still send a report.
#[derive(Debug, serde::Deserialize)]
pub struct ClientErrorReport {
    pub panic_message: String,
    pub version: Option<String>,
    pub git_hash: Option<String>,
    pub url: Option<String>,
    pub user_agent: Option<String>,
    pub location: Option<String>,
    pub backtrace: Option<String>,
}

/// POST /api/client-error — receive a client-side panic report and log it
/// (F25). Public (panics may occur when auth itself is broken); the body is
/// capped at 10 KiB by the route's `DefaultBodyLimit`.
pub async fn client_error(
    axum::Json(report): axum::Json<ClientErrorReport>,
) -> axum::http::StatusCode {
    tracing::warn!(
        target: "iem_server::client_error",
        version = report.version.as_deref().unwrap_or("?"),
        git_hash = report.git_hash.as_deref().unwrap_or("?"),
        url = report.url.as_deref().unwrap_or("?"),
        user_agent = report.user_agent.as_deref().unwrap_or("?"),
        location = report.location.as_deref().unwrap_or("?"),
        panic = %report.panic_message,
        "client_error",
    );
    if let Some(bt) = report.backtrace.as_deref() {
        tracing::warn!(
            target: "iem_server::client_error",
            backtrace = %bt,
            "client_error_backtrace",
        );
    }
    axum::http::StatusCode::NO_CONTENT
}

/// Detect network mode from request headers.
///
/// Since all traffic goes through Cloudflare Tunnel (mixer.example.org),
/// CF-Connecting-IP is always a public IP. We compare it against the
/// configured `local_public_ip` (the church network's public IP).
/// Match = on church WiFi, different = remote.
/// No proxy headers = direct connection = local.
///
/// Used by both the HTTP endpoint and WebSocket handler.
pub fn detect_network_mode(
    headers: &axum::http::HeaderMap,
    local_public_ip: &Option<String>,
) -> String {
    // Extract client IP from Cloudflare headers
    let client_ip = headers
        .get("cf-connecting-ip")
        .or_else(|| headers.get("x-forwarded-for"))
        .or_else(|| headers.get("x-real-ip"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or(s).trim().to_string());

    match (&client_ip, local_public_ip) {
        // If we have both client IP and configured local IP, compare them
        (Some(client), Some(local)) => {
            if client == local {
                "local".to_string()
            } else {
                "remote".to_string()
            }
        }
        // No proxy headers = direct connection (not through Cloudflare) = local
        (None, _) => "local".to_string(),
        // No local_public_ip configured = fall back to private IP check
        (Some(ip), None) => {
            if is_private_ip(ip) {
                "local".to_string()
            } else {
                "remote".to_string()
            }
        }
    }
}

/// HTTP endpoint for network mode detection (kept for backwards compatibility)
async fn get_network_mode(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let config = state.config.read().await;
    let local_ip = config.local_public_ip.clone();
    drop(config);

    let mode = detect_network_mode(&headers, &local_ip);

    Json(NetworkModeResponse { mode })
}

#[derive(Serialize)]
struct NetworkModeResponse {
    mode: String,
}

/// Check if an IP address is in a private/local range
fn is_private_ip(ip: &str) -> bool {
    // Parse the IP and check against private ranges
    if let Ok(addr) = ip.parse::<std::net::IpAddr>() {
        match addr {
            std::net::IpAddr::V4(v4) => {
                v4.is_private()         // 10.x, 172.16-31.x, 192.168.x
                    || v4.is_loopback() // 127.x
                    || v4.is_link_local() // 169.254.x
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback() // ::1
            }
        }
    } else {
        false
    }
}

/// Get the pins and hides of a page's member (ids)
async fn get_customization(
    State(state): State<AppState>,
    Path(page): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<iem_core::Customization>, Reject> {
    let (page, _) = page_for(&state, &headers, &page).await?;
    let Some(member) = page.member else {
        return Ok(Json(iem_core::Customization::default()));
    };
    let c = state.band.customization(&member).map_err(|e| {
        tracing::error!(%member, error = %e, "pins and hides unreadable");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("IO_ERROR", "Pins and hides unreadable")),
        )
    })?;
    Ok(Json(iem_core::Customization {
        pinned: c.pinned.iter().map(ToString::to_string).collect(),
        hidden: c.hidden.iter().map(ToString::to_string).collect(),
    }))
}

/// Replace the pins and hides of a page's member (unknown ids are dropped)
async fn put_customization(
    State(state): State<AppState>,
    Path(page): Path<String>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<iem_core::Customization>,
) -> Result<(StatusCode, Json<iem_core::Customization>), Reject> {
    let (page, _) = page_for(&state, &headers, &page).await?;
    let site = state.site().ok_or_else(engine_unavailable)?;
    let Some(member) = page.member.clone() else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError::bad_request("This page has no member")),
        ));
    };
    let keep = |ids: &[String]| -> (Vec<String>, Vec<iem_engine_proto::Source>) {
        ids.iter()
            .filter_map(|id| crate::view::source(&site, &page, id).map(|s| (id.clone(), s)))
            .unzip()
    };
    let (pinned, pinned_src) = keep(&payload.pinned);
    let (hidden, hidden_src) = keep(&payload.hidden);
    state
        .band
        .save_customization(&member, pinned_src, hidden_src)
        .map_err(|e| {
            tracing::error!(%member, error = %e, "saving pins and hides failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("IO_ERROR", "Failed to save customization")),
            )
        })?;
    let c = iem_core::Customization { pinned, hidden };
    state.broadcast(
        crate::To::Page(page.id.clone()),
        iem_core::ServerMsg::CustomizationUpdate {
            pinned: c.pinned.clone(),
            hidden: c.hidden.clone(),
        },
    );
    Ok((StatusCode::OK, Json(c)))
}

/// The engineer's token from the Authorization header, or the rejection.
pub async fn require_engineer(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Result<iem_core::AuthClaims, Reject> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    let claims = {
        let config = state.config.read().await;
        crate::mixer_ws::claims_of(token, &config.jwt_secret)?
    };
    if !claims.engineer {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiError::new("FORBIDDEN", "Engineer only")),
        ));
    }
    Ok(claims)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod site_links_tests;

/// The API as the browser sees it (also used by other modules' route tests).
#[cfg(test)]
pub(crate) mod api_tests;
