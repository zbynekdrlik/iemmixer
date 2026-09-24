//! Common types used across the IEM mixer

use serde::{Deserialize, Serialize};

/// Mixer state for a band member
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixerState {
    /// Member ID
    pub member_id: String,
    /// Channels with their levels
    pub channels: Vec<Channel>,
}

/// A single channel in the mixer
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Channel {
    /// Input track index (1-based)
    pub track_index: usize,
    /// Input track name
    pub name: String,
    /// Level in dB
    pub level_db: f32,
    /// Pan position in UI range: 0.0 = left, 0.5 = center, 1.0 = right.
    /// The poller converts REAPER's native -1.0..1.0 to this range on read
    /// (`reaper_pan_to_ui`); the restore path converts it back
    /// (`restore_send_pan`/`ui_pan_to_reaper`) before writing to REAPER. (reaperiem#203)
    pub pan: f32,
    /// Muted state
    pub muted: bool,
    /// Track category (mics, stems, tech)
    #[serde(default)]
    pub category: String,
    /// Stereo pair name (if part of a pair)
    #[serde(default)]
    pub stereo_pair: Option<String>,
    /// Stereo side ("L" or "R")
    #[serde(default)]
    pub stereo_side: Option<String>,
}

/// Polling response with channels and meters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollResponse {
    /// Member ID
    pub member_id: String,
    /// Channel states
    pub channels: Vec<Channel>,
    /// Meter levels (track_index -> [left, right] peak levels 0.0-1.0)
    pub meters: std::collections::HashMap<usize, [f32; 2]>,
    /// Connection status
    pub connected: bool,
}

/// Batch control request for Reset
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchControlRequest {
    /// Operation type
    pub operation: BatchOperation,
}

/// Batch operation types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchOperation {
    /// Reset: all to 0dB, unmuted, centered pan
    Reset,
    /// MuteAll: mute all input channels (engineer default state — safe, produces silence)
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

/// Per-member channel customization (pin/hide preferences)
/// Stored server-side so preferences follow the member to any device
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Customization {
    /// Track indices that are pinned to the Main tab
    #[serde(default)]
    pub pinned: Vec<usize>,
    /// Track indices that are hidden from all category tabs
    #[serde(default)]
    pub hidden: Vec<usize>,
}

/// Merge incoming channels into existing list.
/// If the set of track_indices changed (structural shift), fully replace.
/// Otherwise, merge values for non-touched channels.
pub fn merge_or_replace_channels(
    existing: &mut Vec<Channel>,
    incoming: Vec<Channel>,
    touched: &std::collections::HashMap<usize, bool>,
) {
    if existing.is_empty() {
        *existing = incoming;
        return;
    }

    // Detect structural change: different set of track_indices
    let old_indices: std::collections::HashSet<usize> =
        existing.iter().map(|c| c.track_index).collect();
    let new_indices: std::collections::HashSet<usize> =
        incoming.iter().map(|c| c.track_index).collect();

    if old_indices != new_indices {
        // Structural change (track inserted/removed/shifted) — full replace
        *existing = incoming;
        return;
    }

    // Same structure — merge values for non-touched channels
    for new_ch in &incoming {
        if !touched.get(&new_ch.track_index).copied().unwrap_or(false)
            && let Some(ch) = existing
                .iter_mut()
                .find(|c| c.track_index == new_ch.track_index)
        {
            ch.level_db = new_ch.level_db;
            ch.muted = new_ch.muted;
            ch.pan = new_ch.pan;
        }
    }
}

/// Returns true when `value` is a pan value within [-1.0, 1.0].
/// NaN and infinities return false: `Range::contains` uses ordered
/// comparisons, which always return false for NaN, and infinities are
/// not ≤ 1.0, so the single range check suffices.
pub fn is_valid_pan(value: f32) -> bool {
    (-1.0..=1.0).contains(&value)
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

    #[test]
    fn test_channel_default_values() {
        let channel = Channel {
            track_index: 1,
            name: "Test".to_string(),
            level_db: 0.0,
            pan: 0.0,
            muted: false,
            category: String::new(),
            stereo_pair: None,
            stereo_side: None,
        };
        assert_eq!(channel.track_index, 1);
        assert!(!channel.muted);
    }

    #[test]
    fn test_batch_operation_serialization() {
        let op = BatchOperation::Reset;
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(json, "\"reset\"");
    }

    #[test]
    fn test_batch_operation_deserialization() {
        let op: BatchOperation = serde_json::from_str("\"reset\"").unwrap();
        assert!(matches!(op, BatchOperation::Reset));
    }

    #[test]
    fn test_batch_operation_mute_all_serialization() {
        let op = BatchOperation::MuteAll;
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(json, "\"mute_all\"");
    }

    #[test]
    fn test_batch_operation_mute_all_deserialization() {
        let op: BatchOperation = serde_json::from_str("\"mute_all\"").unwrap();
        assert!(matches!(op, BatchOperation::MuteAll));
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

    // ================================================================
    // is_valid_pan tests — designed to kill all cargo-mutants mutants:
    // body replacement (true, false), range-endpoint mutations, and
    // boundary drift. The implementation is a single Range::contains
    // call, so the mutation surface is small.
    // ================================================================

    #[test]
    fn test_is_valid_pan_true_in_range() {
        assert!(is_valid_pan(0.0));
        assert!(is_valid_pan(0.5));
        assert!(is_valid_pan(-0.5));
    }

    #[test]
    fn test_is_valid_pan_true_at_boundaries() {
        // These kill `<=` → `<` and `>=` → `>` mutants.
        assert!(is_valid_pan(-1.0));
        assert!(is_valid_pan(1.0));
    }

    #[test]
    fn test_is_valid_pan_false_just_outside_range() {
        // These kill body-replace-with-true mutants and catch boundary drift.
        assert!(!is_valid_pan(-1.0001));
        assert!(!is_valid_pan(1.0001));
        assert!(!is_valid_pan(-2.0));
        assert!(!is_valid_pan(2.0));
    }

    #[test]
    fn test_is_valid_pan_false_for_non_finite() {
        // NaN: Range::contains returns false because NaN comparisons
        // always return false. Infinities: ±INFINITY is never ≤ 1.0 and
        // NEG_INFINITY is never ≥ -1.0. So the range check alone
        // rejects all non-finite inputs without a separate is_finite guard.
        assert!(!is_valid_pan(f32::NAN));
        assert!(!is_valid_pan(f32::INFINITY));
        assert!(!is_valid_pan(f32::NEG_INFINITY));
    }

    // ================================================================
    // Customization type tests
    // ================================================================

    #[test]
    fn test_customization_default_is_empty() {
        let cust = Customization::default();
        assert!(cust.pinned.is_empty());
        assert!(cust.hidden.is_empty());
    }

    #[test]
    fn test_customization_serialization_roundtrip() {
        let cust = Customization {
            pinned: vec![1, 5, 8],
            hidden: vec![3, 7, 12],
        };
        let json = serde_json::to_string(&cust).unwrap();
        let decoded: Customization = serde_json::from_str(&json).unwrap();
        assert_eq!(cust, decoded);
    }

    #[test]
    fn test_customization_deserialize_missing_fields() {
        // Old data without pinned/hidden should default to empty
        let json = "{}";
        let cust: Customization = serde_json::from_str(json).unwrap();
        assert!(cust.pinned.is_empty());
        assert!(cust.hidden.is_empty());
    }

    // ================================================================
    // "Me" fader input detection - case-sensitive comparison regression
    // ================================================================

    #[test]
    fn test_my_input_detection_case_match() {
        // Reproduces the bug: format must produce uppercase "MIC" to match
        // channel names that are compared via to_uppercase()
        let member = "oldmember1";
        let my_input = format!("{} MIC", member.to_uppercase());
        let channel_name = "OLDMEMBER1 mic";

        assert_eq!(
            channel_name.to_uppercase(),
            my_input,
            "is_my_input must match when channel is member's mic"
        );
    }

    #[test]
    fn test_my_input_detection_non_member() {
        let member = "oldmember1";
        let my_input = format!("{} MIC", member.to_uppercase());
        let other_channel = "MEMBER2 mic";

        assert_ne!(
            other_channel.to_uppercase(),
            my_input,
            "Other member's mic must NOT match"
        );
    }

    // ================================================================
    // merge_or_replace_channels tests
    // ================================================================

    fn make_channel(track_index: usize, name: &str, level_db: f32) -> Channel {
        Channel {
            track_index,
            name: name.to_string(),
            level_db,
            pan: 0.0,
            muted: false,
            category: String::new(),
            stereo_pair: None,
            stereo_side: None,
        }
    }

    #[test]
    fn test_merge_replaces_on_index_shift() {
        let mut existing = vec![
            make_channel(1, "OLDMEMBER1 mic", 0.0),
            make_channel(2, "MEMBER2 mic", 0.0),
            make_channel(3, "MEMBER3 mic", 0.0),
        ];
        let incoming = vec![
            make_channel(1, "OLDMEMBER1 mic", 0.0),
            make_channel(3, "MEMBER2 mic", 0.0),
            make_channel(4, "MEMBER3 mic", 0.0),
        ];
        let touched = HashMap::new();
        merge_or_replace_channels(&mut existing, incoming, &touched);
        // Must be fully replaced — MEMBER2 at 3, MEMBER3 at 4
        assert_eq!(existing.len(), 3);
        assert_eq!(existing[1].name, "MEMBER2 mic");
        assert_eq!(existing[1].track_index, 3);
        assert_eq!(existing[2].name, "MEMBER3 mic");
        assert_eq!(existing[2].track_index, 4);
    }

    #[test]
    fn test_merge_preserves_touched_when_same_structure() {
        let mut existing = vec![
            make_channel(1, "OLDMEMBER1 mic", -10.0),
            make_channel(2, "MEMBER2 mic", -5.0),
        ];
        let incoming = vec![
            make_channel(1, "OLDMEMBER1 mic", -20.0),
            make_channel(2, "MEMBER2 mic", -15.0),
        ];
        let mut touched = HashMap::new();
        touched.insert(2_usize, true); // MEMBER2 fader is being dragged
        merge_or_replace_channels(&mut existing, incoming, &touched);
        // OLDMEMBER1 updated, MEMBER2 preserved (touched)
        assert_eq!(existing[0].level_db, -20.0);
        assert_eq!(existing[1].level_db, -5.0); // preserved
    }

    #[test]
    fn test_merge_populates_empty() {
        let mut existing: Vec<Channel> = vec![];
        let incoming = vec![make_channel(1, "OLDMEMBER1 mic", 0.0)];
        merge_or_replace_channels(&mut existing, incoming, &HashMap::new());
        assert_eq!(existing.len(), 1);
        assert_eq!(existing[0].name, "OLDMEMBER1 mic");
    }

    #[test]
    fn test_full_replace_on_shift_ignores_touched() {
        let mut existing = vec![make_channel(2, "MEMBER2 mic", -5.0)];
        let incoming = vec![make_channel(3, "MEMBER2 mic", -15.0)];
        let mut touched = HashMap::new();
        touched.insert(2_usize, true);
        merge_or_replace_channels(&mut existing, incoming, &touched);
        // Structural change — full replace even though old index was touched
        assert_eq!(existing[0].track_index, 3);
        assert_eq!(existing[0].level_db, -15.0);
    }
}
