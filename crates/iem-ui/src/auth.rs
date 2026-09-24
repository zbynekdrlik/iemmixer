//! Client-side authentication helpers

use gloo_storage::{LocalStorage, Storage};
use serde::{Deserialize, Serialize};

const TOKEN_KEY: &str = "iem_token";

/// Stored auth state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthState {
    pub token: String,
    pub member: String,
    pub engineer: bool,
}

/// JWT claims from the token payload
#[derive(Debug, Deserialize)]
struct JwtClaims {
    exp: u64,
}

/// Get current auth state from localStorage
pub fn get_auth() -> Option<AuthState> {
    LocalStorage::get::<AuthState>(TOKEN_KEY).ok()
}

/// Save auth state to localStorage
pub fn save_auth(state: &AuthState) {
    let _ = LocalStorage::set(TOKEN_KEY, state);
}

/// Clear auth state from localStorage (logout)
pub fn clear_auth() {
    LocalStorage::delete(TOKEN_KEY);
}

/// Get auth token for API calls
pub fn get_token() -> Option<String> {
    get_auth().map(|a| a.token)
}

/// Check if the current token is expired
/// Returns true if no token exists or if the token is expired
pub fn is_token_expired() -> bool {
    let auth = match get_auth() {
        Some(a) => a,
        None => return true,
    };

    // JWT format: header.payload.signature
    let parts: Vec<&str> = auth.token.split('.').collect();
    if parts.len() != 3 {
        return true;
    }

    // Decode payload (base64url)
    let payload_b64 = parts[1];

    // Convert base64url to standard base64
    let payload_b64_std = payload_b64.replace('-', "+").replace('_', "/");

    // Pad to multiple of 4
    let padding = (4 - payload_b64_std.len() % 4) % 4;
    let padded = format!("{}{}", payload_b64_std, "=".repeat(padding));

    // Use window.atob to decode base64
    let window = match web_sys::window() {
        Some(w) => w,
        None => return true,
    };

    let decoded = match window.atob(&padded) {
        Ok(d) => d,
        Err(_) => return true,
    };

    // Parse JSON
    let claims: JwtClaims = match serde_json::from_str(&decoded) {
        Ok(c) => c,
        Err(_) => return true,
    };

    // Check if expired (add 30 second buffer for clock skew)
    let now = (js_sys::Date::now() / 1000.0) as u64;
    claims.exp < now + 30
}
