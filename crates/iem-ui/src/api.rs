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

fn yes() -> bool {
    true
}

/// Member info from server
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub has_photo: bool,
    /// Plays in the band that plays (#58); a server that does not say:
    /// everyone plays.
    #[serde(default = "yes")]
    pub playing: bool,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    /// The error a refused answer carries: the server's `{code, message}`
    /// when its body is one, else `HTTP_<status>`.
    pub fn of(status: u16, body: &str) -> Self {
        serde_json::from_str(body).unwrap_or_else(|_| Self {
            code: format!("HTTP_{status}"),
            message: format!("Server error: {status}"),
        })
    }
}

/// `GET /api/lineup`'s answer (#58) by its status and body: the band that
/// plays and what the bearer may do, or `None` on a site without a roster
/// (204, or the 404 `NOT_CONFIGURED` of a server from before Part C).
pub fn lineup_answer(status: u16, body: &str) -> Result<Option<iem_core::LineupInfo>, ApiError> {
    match status {
        200 => serde_json::from_str(body).map(Some).map_err(|e| ApiError {
            code: "UNREADABLE".to_string(),
            message: e.to_string(),
        }),
        204 => Ok(None),
        _ => {
            let e = ApiError::of(status, body);
            if status == 404 && e.code == "NOT_CONFIGURED" {
                Ok(None)
            } else {
                Err(e)
            }
        }
    }
}

/// `GET /api/lineup` (#58): which band plays and what the signed-in person
/// may do; `None` on a site without a roster.
pub async fn get_lineup() -> Result<Option<iem_core::LineupInfo>, ApiError> {
    let token = crate::auth::get_token().ok_or_else(|| ApiError {
        code: "NO_TOKEN".to_string(),
        message: "Not authenticated".to_string(),
    })?;
    let resp = Request::get(&format!("{API_BASE}/lineup"))
        .header("Authorization", &format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| ApiError {
            code: "NETWORK".to_string(),
            message: e.to_string(),
        })?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    lineup_answer(status, &body)
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

/// What a band switch came to (#58).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switched {
    /// The band plays now (204).
    Now,
    /// The mixer dropped in the middle: the switch finishes once it is back
    /// (202 `SWITCH_PENDING`). Not a failure.
    Pending,
}

/// A switch that did not happen: the answer's status (0: no answer) and
/// the server's code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchFailure {
    pub status: u16,
    pub code: String,
}

/// `POST /api/lineup/switch`'s answer by its status and body.
pub fn switch_answer(status: u16, body: &str) -> Result<Switched, SwitchFailure> {
    match status {
        202 => Ok(Switched::Pending),
        200..=299 => Ok(Switched::Now),
        _ => Err(SwitchFailure {
            status,
            code: ApiError::of(status, body).code,
        }),
    }
}

/// `POST /api/lineup/switch` (#58): band `to` plays from now on.
pub async fn switch_lineup(to: &str) -> Result<Switched, SwitchFailure> {
    let no_answer = |code: &str| SwitchFailure {
        status: 0,
        code: code.to_string(),
    };
    let token = crate::auth::get_token().ok_or_else(|| no_answer("NO_TOKEN"))?;
    let resp = Request::post(&format!("{API_BASE}/lineup/switch"))
        .header("Authorization", &format!("Bearer {token}"))
        .json(&iem_core::SwitchRequest { to: to.to_string() })
        .map_err(|_| no_answer("REQUEST"))?
        .send()
        .await
        .map_err(|_| no_answer("NETWORK"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    switch_answer(status, &body)
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
    fn lineup_info_reads_and_a_missing_playing_means_playing() {
        let member: MemberInfo =
            serde_json::from_str(r#"{"id":"member1","name":"Member 1"}"#).unwrap();
        assert!(member.playing, "a server without rosters: everyone plays");
        assert!(!member.has_photo);
        let away: MemberInfo = serde_json::from_str(
            r#"{"id":"member10","name":"Member 10","has_photo":true,"playing":false}"#,
        )
        .unwrap();
        assert!(!away.playing && away.has_photo);
        let info: iem_core::LineupInfo = serde_json::from_str(concat!(
            r#"{"active":{"id":"main","name":"Main band"},"#,
            r#""lineups":[{"id":"main","name":"Main band"},{"id":"youth","name":"Youth band"}],"#,
            r#""playing":true,"leader":true,"can_switch":true}"#
        ))
        .unwrap();
        assert_eq!(info.active.id, "main");
        assert_eq!(info.lineups[1].name, "Youth band");
        assert!(info.playing && info.leader && info.can_switch);
    }

    #[test]
    fn the_lineup_answer_reads_the_band_or_no_roster() {
        let json = concat!(
            r#"{"active":{"id":"youth","name":"Youth band"},"#,
            r#""lineups":[{"id":"main","name":"Main band"},{"id":"youth","name":"Youth band"}],"#,
            r#""playing":false,"leader":false,"can_switch":true}"#
        );
        let info = lineup_answer(200, json).unwrap().unwrap();
        assert_eq!(info.active.name, "Youth band");
        assert_eq!(info.lineups.len(), 2);
        assert!(!info.playing && !info.leader && info.can_switch);
        // A site without a roster: 204, or a server from before Part C.
        assert_eq!(lineup_answer(204, ""), Ok(None));
        let legacy = r#"{"code":"NOT_CONFIGURED","message":"this site has no roster"}"#;
        assert_eq!(lineup_answer(404, legacy), Ok(None));
        // Anything else is no answer on the band.
        let gone = r#"{"code":"UNAUTHORIZED","message":"gone"}"#;
        assert_eq!(
            lineup_answer(401, gone),
            Err(ApiError {
                code: "UNAUTHORIZED".into(),
                message: "gone".into()
            })
        );
        assert_eq!(lineup_answer(404, "").unwrap_err().code, "HTTP_404");
        assert_eq!(
            lineup_answer(500, r#"{"code":"NOT_CONFIGURED","message":"x"}"#)
                .unwrap_err()
                .code,
            "NOT_CONFIGURED"
        );
        assert_eq!(
            lineup_answer(502, "<html>").unwrap_err(),
            ApiError {
                code: "HTTP_502".into(),
                message: "Server error: 502".into()
            }
        );
        assert_eq!(lineup_answer(200, "{}").unwrap_err().code, "UNREADABLE");
    }

    #[test]
    fn a_switch_answer_is_done_pending_or_refused() {
        assert_eq!(switch_answer(204, ""), Ok(Switched::Now));
        // The mixer dropped mid-switch: it finishes later, no failure.
        let pending = r#"{"code":"SWITCH_PENDING","message":"the engine went away"}"#;
        assert_eq!(switch_answer(202, pending), Ok(Switched::Pending));
        let busy = r#"{"code":"BUSY","message":"a switch runs"}"#;
        assert_eq!(
            switch_answer(409, busy),
            Err(SwitchFailure {
                status: 409,
                code: "BUSY".into()
            })
        );
        assert_eq!(
            switch_answer(502, "Bad Gateway"),
            Err(SwitchFailure {
                status: 502,
                code: "HTTP_502".into()
            })
        );
        assert_eq!(switch_answer(199, "").unwrap_err().status, 199);
        assert_eq!(switch_answer(300, "").unwrap_err().status, 300);
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
