//! API client for server communication

use gloo_net::http::Request;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{AbortController, RequestInit, Response};

use crate::auth::AuthState;

/// Base URL for API calls (same origin)
const API_BASE: &str = "/api";

/// Network timeout in milliseconds
const NETWORK_TIMEOUT_MS: i32 = 5000;

/// Member info from server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub has_photo: bool,
}

/// Re-export Channel from iem_core to avoid duplicate type
pub use iem_core::Channel;

/// Login response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginResponse {
    pub token: String,
    pub member: String,
    pub engineer: bool,
    pub expires_in: u64,
}

/// API error
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

/// Get list of band members
pub async fn get_members() -> Result<Vec<MemberInfo>, String> {
    let resp = Request::get(&format!("{}/members", API_BASE))
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if resp.ok() {
        resp.json().await.map_err(|e| format!("Parse error: {}", e))
    } else {
        Err(format!("Server error: {}", resp.status()))
    }
}

/// Where the mixer is reachable (LAN URL, public host) — `GET /api/site`.
pub async fn get_site_links() -> Result<iem_core::tunnel::SiteLinks, String> {
    let resp = Request::get(&format!("{}/site", API_BASE))
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;
    if resp.ok() {
        resp.json().await.map_err(|e| format!("Parse error: {}", e))
    } else {
        Err(format!("Server error: {}", resp.status()))
    }
}

/// Get list of band members with timeout
/// Returns NETWORK_TIMEOUT error if fetch takes longer than timeout_ms
pub async fn get_members_with_timeout() -> Result<Vec<MemberInfo>, String> {
    let window = web_sys::window().ok_or("No window")?;

    // Create abort controller for timeout
    let controller = AbortController::new().map_err(|_| "Failed to create AbortController")?;
    let signal = controller.signal();

    // Set up timeout to abort the fetch
    let controller_clone = controller.clone();
    let timeout_closure = Closure::once_into_js(move || {
        controller_clone.abort();
    });

    window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            timeout_closure.as_ref().unchecked_ref(),
            NETWORK_TIMEOUT_MS,
        )
        .map_err(|_| "Failed to set timeout")?;

    // Create fetch request with abort signal
    let opts = RequestInit::new();
    opts.set_method("GET");
    opts.set_signal(Some(&signal));

    let url = format!("{}/members", API_BASE);
    let request = web_sys::Request::new_with_str_and_init(&url, &opts)
        .map_err(|_| "Failed to create request")?;

    // Perform fetch
    let fetch_promise = window.fetch_with_request(&request);
    let result = JsFuture::from(fetch_promise).await;

    match result {
        Ok(resp) => {
            let response: Response = resp.dyn_into().map_err(|_| "Invalid response")?;
            if !response.ok() {
                return Err(format!("Server error: {}", response.status()));
            }

            let json_promise = response.json().map_err(|_| "Failed to get JSON")?;
            let json = JsFuture::from(json_promise)
                .await
                .map_err(|_| "Failed to parse JSON")?;

            let members: Vec<MemberInfo> =
                serde_wasm_bindgen::from_value(json).map_err(|e| format!("Parse error: {}", e))?;
            Ok(members)
        }
        Err(_) => {
            // Fetch was aborted (timeout) or network error
            Err("NETWORK_TIMEOUT".to_string())
        }
    }
}

/// Login with PIN
pub async fn login(member: &str, pin: &str) -> Result<AuthState, String> {
    #[derive(Serialize)]
    struct LoginRequest<'a> {
        member: &'a str,
        pin: &'a str,
    }

    let resp = Request::post(&format!("{}/auth", API_BASE))
        .json(&LoginRequest { member, pin })
        .map_err(|e| format!("Request error: {}", e))?
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if resp.ok() {
        let login_resp: LoginResponse = resp
            .json()
            .await
            .map_err(|e| format!("Parse error: {}", e))?;

        Ok(AuthState {
            token: login_resp.token,
            member: login_resp.member,
            engineer: login_resp.engineer,
        })
    } else {
        let retry_after = resp.headers().get("retry-after");
        Err(login_error_message(resp.status(), retry_after.as_deref()))
    }
}

/// User-facing text of a failed login (`status` = HTTP status,
/// `retry_after` = the `Retry-After` header).
pub fn login_error_message(status: u16, retry_after: Option<&str>) -> String {
    match status {
        401 => "Invalid PIN".to_string(),
        429 => {
            let secs = retry_after
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(1);
            format!("Too many attempts. Try again in {secs} s")
        }
        other => format!("Server error: {other}"),
    }
}

/// Mute all channels for a member (batch operation)
pub async fn batch_mute_all(member: &str) -> Result<(), String> {
    let token = crate::auth::get_token().ok_or("Not authenticated")?;

    let resp = Request::post(&format!("{}/mixer/{}/batch", API_BASE, member))
        .header("Authorization", &format!("Bearer {}", token))
        .json(&serde_json::json!({ "operation": "mute_all" }))
        .map_err(|e| format!("Request error: {}", e))?
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if resp.ok() {
        Ok(())
    } else {
        Err(format!("Server error: {}", resp.status()))
    }
}

/// "Back to REAPER" (§4.3): the engineer's switch in the console, confirmed
/// with the engineer PIN; the server starts the site's switch command (202).
pub async fn back_to_reaper(pin: &str) -> Result<(), String> {
    let token = crate::auth::get_token().ok_or("Not authenticated")?;
    let resp = Request::post(&format!("{}/mode/event", API_BASE))
        .header("Authorization", &format!("Bearer {}", token))
        .json(&serde_json::json!({ "pin": pin }))
        .map_err(|e| format!("Request error: {}", e))?
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;
    if resp.ok() {
        Ok(())
    } else {
        Err(crate::components::back_to_reaper::switch_error_message(
            resp.status(),
        ))
    }
}

/// Whether the answer to the token check (`GET /api/mixer/<page>`) is the
/// server refusing the token, which sends the page to the login: 401 (an
/// invalid or expired token) or 403 (a token not for this page). Any other
/// answer is no verdict on the token — cloudflared's 502 while the server
/// restarts or 530 with the tunnel down, a 5xx, a page the site lacks
/// (404) — so the page keeps its token and keeps retrying.
pub fn token_refused(status: u16) -> bool {
    matches!(status, 401 | 403)
}

/// Whether the stored token still holds, asked of a protected endpoint:
/// false without a token or when the server refuses it ([`token_refused`]);
/// a network error or an answer that is no verdict keeps it.
pub async fn verify_token_valid(member: &str) -> bool {
    let token = match crate::auth::get_token() {
        Some(t) => t,
        None => return false,
    };

    let url = format!("{}/mixer/{}", API_BASE, member);
    let resp = Request::get(&url)
        .header("Authorization", &format!("Bearer {}", token))
        .send()
        .await;

    match resp {
        Ok(r) => !token_refused(r.status()),
        Err(_) => true, // Network error — don't clear auth, might be transient
    }
}

/// Change PIN for a member.
///
/// - `old_pin`: current PIN (required for regular members, empty for engineers)
/// - `new_pin`: the new 4-digit PIN
/// - `member`: target member ID (used by engineers, also sent by members for consistency)
pub async fn change_pin(old_pin: &str, new_pin: &str, member: &str) -> Result<(), String> {
    let token = crate::auth::get_token().ok_or("Not authenticated")?;

    #[derive(Serialize)]
    struct ChangePinRequest<'a> {
        #[serde(skip_serializing_if = "Option::is_none")]
        old_pin: Option<&'a str>,
        new_pin: &'a str,
        member: &'a str,
    }

    let resp = Request::post(&format!("{}/auth/change-pin", API_BASE))
        .header("Authorization", &format!("Bearer {}", token))
        .json(&ChangePinRequest {
            old_pin: if old_pin.is_empty() {
                None
            } else {
                Some(old_pin)
            },
            new_pin,
            member,
        })
        .map_err(|e| format!("Request error: {}", e))?
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if resp.ok() {
        Ok(())
    } else {
        Err(change_pin_error(resp.status()))
    }
}

/// The PIN dialog's text for a refused change (`status` = HTTP status); 409
/// is the freeze before the cutover (P9).
pub fn change_pin_error(status: u16) -> String {
    match status {
        400 => "PIN must be exactly 4 digits".to_string(),
        401 => "Wrong current PIN".to_string(),
        409 => iem_core::PIN_CHANGES_FROZEN.to_string(),
        other => format!("Server error: {other}"),
    }
}

/// Upload a member's profile photo (base64 JPEG)
pub async fn upload_photo(member_id: &str, base64_jpeg: &str) -> Result<(), String> {
    let auth = crate::auth::get_auth().ok_or("Not logged in")?;
    let url = format!("/api/members/{}/photo", member_id);
    let resp = Request::post(&url)
        .header("Authorization", &format!("Bearer {}", auth.token))
        .json(&serde_json::json!({ "photo": base64_jpeg }))
        .map_err(|e| format!("Request error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    if resp.ok() {
        Ok(())
    } else {
        Err(format!("Server error: {}", resp.status()))
    }
}

/// Delete a member's profile photo
pub async fn delete_photo(member_id: &str) -> Result<(), String> {
    let auth = crate::auth::get_auth().ok_or("Not logged in")?;
    let url = format!("/api/members/{}/photo", member_id);
    let resp = Request::delete(&url)
        .header("Authorization", &format!("Bearer {}", auth.token))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    if resp.ok() {
        Ok(())
    } else {
        Err(format!("Server error: {}", resp.status()))
    }
}

// === Backup API (engineer-only) ===

/// List available backups
pub async fn list_backups(token: &str) -> Result<Vec<iem_core::BackupInfo>, String> {
    let resp = Request::get("/api/backups")
        .header("Authorization", &format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| format!("{e}"))?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json().await.map_err(|e| format!("{e}"))
}

/// Preview a backup restore
pub async fn preview_restore(
    token: &str,
    filename: &str,
) -> Result<iem_core::RestorePreview, String> {
    let resp = Request::post(&format!("/api/backups/{}/preview", filename))
        .header("Authorization", &format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| format!("{e}"))?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json().await.map_err(|e| format!("{e}"))
}

/// Apply a backup restore
pub async fn apply_restore(token: &str, filename: &str) -> Result<iem_core::RestoreResult, String> {
    let resp = Request::post(&format!("/api/backups/{}/restore", filename))
        .header("Authorization", &format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| format!("{e}"))?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json().await.map_err(|e| format!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_errors_are_readable() {
        assert_eq!(login_error_message(401, None), "Invalid PIN");
        assert_eq!(
            login_error_message(429, Some("7")),
            "Too many attempts. Try again in 7 s"
        );
        assert_eq!(
            login_error_message(429, None),
            "Too many attempts. Try again in 1 s"
        );
        assert_eq!(
            login_error_message(429, Some("soon")),
            "Too many attempts. Try again in 1 s"
        );
        assert_eq!(login_error_message(500, None), "Server error: 500");
    }

    #[test]
    fn only_the_servers_verdict_refuses_a_token() {
        // The server refuses the token (invalid or expired: 401; not for
        // this page: 403): the page goes to the login.
        assert!(token_refused(401));
        assert!(token_refused(403));
        // It took the token.
        assert!(!token_refused(200));
        // No verdict on the token: cloudflared while the server restarts
        // (502) or with the tunnel down (530), a proxy or server not ready
        // (500, 503, 504), a page the site lacks (404). A page opened
        // through the tunnel during a restart keeps its token and retries.
        for status in [404, 500, 502, 503, 504, 530] {
            assert!(!token_refused(status), "{status} is no verdict");
        }
    }

    #[test]
    fn pin_change_errors_are_readable() {
        assert_eq!(change_pin_error(400), "PIN must be exactly 4 digits");
        assert_eq!(change_pin_error(401), "Wrong current PIN");
        assert_eq!(
            change_pin_error(409),
            "PIN sa zatiaľ mení v pôvodnej aplikácii"
        );
        assert_eq!(change_pin_error(500), "Server error: 500");
    }
}
