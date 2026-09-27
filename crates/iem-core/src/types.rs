//! Common types used across the IEM mixer

use serde::{Deserialize, Serialize};

/// Why a PIN change or reset is refused before the cutover (P9, design note
/// §5.4): the predecessor is still the only place PINs change. The server
/// sends it with 409, the PIN dialog shows it.
pub const PIN_CHANGES_FROZEN: &str = "PIN sa zatiaľ mení v pôvodnej aplikácii";

/// Mixer state for a band member
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixerState {
    /// Member ID
    pub member_id: String,
    /// Channels with their levels
    pub channels: Vec<Channel>,
}

/// One channel strip of a mixer page (F5): an engine input or a mix the
/// page's mix hears (the Mixes tab), keyed by that id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Channel {
    /// The engine input id or heard mix id (one namespace).
    pub id: String,
    /// Label, as the band knows it (e.g. "MEMBER3 mic").
    pub name: String,
    /// Level in dB; −60 is off (−∞ on the fader).
    pub level_db: f32,
    /// Pan position in UI range: 0.0 = left, 0.5 = center, 1.0 = right.
    pub pan: f32,
    /// Muted, or silenced by a solo on another channel (X2).
    pub muted: bool,
    /// Tab: "mics", "stems", "tech" or "mixes".
    #[serde(default)]
    pub category: String,
    /// The viewer may open this channel's EQ (X7).
    #[serde(default)]
    pub eq: bool,
    /// The page member's own channel (shown first on Main).
    #[serde(default)]
    pub own: bool,
}

/// Batch control request (`POST /api/mixer/{page}/batch`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchControlRequest {
    /// Operation type
    pub operation: BatchOperation,
}

/// Batch operation types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchOperation {
    /// MuteAll: mute every level of the engineer's mix (F15)
    MuteAll,
}

/// Authentication token payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthClaims {
    /// Subject (member ID or "engineer")
    pub sub: String,
    /// Is engineer (full access)
    pub engineer: bool,
    /// Expiration timestamp (Unix seconds)
    pub exp: u64,
    /// Issued at timestamp
    pub iat: u64,
}

/// Per-member channel pins and hides (F8), by channel id.
/// Stored server-side so preferences follow the member to any device
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Customization {
    /// Channel ids pinned to the Main tab
    #[serde(default)]
    pub pinned: Vec<String>,
    /// Channel ids hidden from the category tabs
    #[serde(default)]
    pub hidden: Vec<String>,
}

/// Merge incoming channels into existing list.
/// If the set of ids changed (structural change), fully replace.
/// Otherwise, merge values for non-touched channels.
pub fn merge_or_replace_channels(
    existing: &mut Vec<Channel>,
    incoming: Vec<Channel>,
    touched: &std::collections::HashMap<String, bool>,
) {
    if existing.is_empty() {
        *existing = incoming;
        return;
    }
    let old_ids: std::collections::HashSet<&str> = existing.iter().map(|c| c.id.as_str()).collect();
    let new_ids: std::collections::HashSet<&str> = incoming.iter().map(|c| c.id.as_str()).collect();
    if old_ids != new_ids {
        *existing = incoming;
        return;
    }
    for new_ch in incoming {
        if touched.get(&new_ch.id).copied().unwrap_or(false) {
            continue;
        }
        if let Some(ch) = existing.iter_mut().find(|c| c.id == new_ch.id) {
            *ch = new_ch;
        }
    }
}

/// Returns true when `value` is a UI pan value within [0.0, 1.0]; NaN and
/// infinities are not.
pub fn is_valid_ui_pan(value: f32) -> bool {
    (0.0..=1.0).contains(&value)
}

/// API error response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    /// Error code
    pub code: String,
    /// Human-readable message
    pub message: String,
}

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn unauthorized() -> Self {
        Self::new("UNAUTHORIZED", "Authentication required")
    }

    pub fn forbidden() -> Self {
        Self::new("FORBIDDEN", "Access denied")
    }

    pub fn not_found(what: &str) -> Self {
        Self::new("NOT_FOUND", format!("{} not found", what))
    }

    pub fn bad_request(msg: &str) -> Self {
        Self::new("BAD_REQUEST", msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_api_error_new() {
        let err = ApiError::new("TEST_CODE", "Test message");
        assert_eq!(err.code, "TEST_CODE");
        assert_eq!(err.message, "Test message");
    }

    #[test]
    fn test_api_error_unauthorized() {
        let err = ApiError::unauthorized();
        assert_eq!(err.code, "UNAUTHORIZED");
    }

    #[test]
    fn test_api_error_forbidden() {
        let err = ApiError::forbidden();
        assert_eq!(err.code, "FORBIDDEN");
    }

    #[test]
    fn test_api_error_not_found() {
        let err = ApiError::not_found("Member");
        assert_eq!(err.code, "NOT_FOUND");
        assert!(err.message.contains("Member"));
    }

    #[test]
    fn test_api_error_bad_request() {
        let err = ApiError::bad_request("invalid input");
        assert_eq!(err.code, "BAD_REQUEST");
        assert!(err.message.contains("invalid input"));
    }

    fn channel(id: &str, level_db: f32) -> Channel {
        Channel {
            id: id.to_string(),
            name: id.to_uppercase(),
            level_db,
            pan: 0.5,
            muted: false,
            category: "mics".to_string(),
            eq: false,
            own: false,
        }
    }

    #[test]
    fn a_channel_serialises_its_id_and_defaults_the_flags() {
        let json = serde_json::to_string(&channel("mic1", -6.0)).unwrap();
        assert!(json.starts_with(r#"{"id":"mic1","name":"MIC1""#), "{json}");
        let old: Channel = serde_json::from_str(
            r#"{"id":"keys","name":"KEYS","level_db":0.0,"pan":0.5,"muted":true}"#,
        )
        .unwrap();
        assert!(old.muted && !old.eq && !old.own && old.category.is_empty());
    }

    #[test]
    fn test_batch_operation_mute_all_serialization() {
        let op = BatchOperation::MuteAll;
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(json, "\"mute_all\"");
        let op: BatchOperation = serde_json::from_str("\"mute_all\"").unwrap();
        assert!(matches!(op, BatchOperation::MuteAll));
        assert!(serde_json::from_str::<BatchOperation>("\"reset\"").is_err());
    }

    #[test]
    fn test_auth_claims() {
        let claims = AuthClaims {
            sub: "member3".to_string(),
            engineer: false,
            exp: 1234567890,
            iat: 1234567800,
        };
        assert_eq!(claims.sub, "member3");
        assert!(!claims.engineer);
    }

    #[test]
    fn ui_pan_is_zero_to_one_and_finite() {
        for ok in [0.0, 0.5, 1.0] {
            assert!(is_valid_ui_pan(ok), "{ok}");
        }
        for bad in [
            -0.0001,
            1.0001,
            -1.0,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            assert!(!is_valid_ui_pan(bad), "{bad}");
        }
    }

    #[test]
    fn customization_uses_ids_and_defaults_to_empty() {
        let cust = Customization {
            pinned: vec!["mic1".into(), "member2".into()],
            hidden: vec!["keys".into()],
        };
        let json = serde_json::to_string(&cust).unwrap();
        assert_eq!(json, r#"{"pinned":["mic1","member2"],"hidden":["keys"]}"#);
        assert_eq!(serde_json::from_str::<Customization>(&json).unwrap(), cust);
        assert_eq!(
            serde_json::from_str::<Customization>("{}").unwrap(),
            Customization::default()
        );
    }

    #[test]
    fn merge_replaces_when_the_ids_change() {
        let mut existing = vec![channel("mic1", 0.0), channel("mic2", 0.0)];
        merge_or_replace_channels(
            &mut existing,
            vec![channel("mic1", -3.0), channel("keys", -9.0)],
            &HashMap::new(),
        );
        let ids: Vec<&str> = existing.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["mic1", "keys"]);
        assert_eq!(existing[1].level_db, -9.0);
    }

    #[test]
    fn merge_keeps_touched_channels_when_the_ids_are_the_same() {
        let mut existing = vec![channel("mic1", -10.0), channel("mic2", -5.0)];
        let mut incoming = vec![channel("mic1", -20.0), channel("mic2", -15.0)];
        incoming[0].muted = true;
        let touched = HashMap::from([("mic2".to_string(), true), ("mic1".to_string(), false)]);
        merge_or_replace_channels(&mut existing, incoming, &touched);
        assert_eq!(existing[0].level_db, -20.0);
        assert!(existing[0].muted);
        assert_eq!(existing[1].level_db, -5.0);
    }

    #[test]
    fn merge_populates_an_empty_list_and_replaces_on_a_shift_even_when_touched() {
        let mut existing: Vec<Channel> = vec![];
        merge_or_replace_channels(&mut existing, vec![channel("mic1", 0.0)], &HashMap::new());
        assert_eq!(existing.len(), 1);
        let touched = HashMap::from([("mic1".to_string(), true)]);
        merge_or_replace_channels(&mut existing, vec![channel("mic9", -15.0)], &touched);
        assert_eq!(existing[0].id, "mic9");
    }
}
