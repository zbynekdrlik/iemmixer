//! IEM Mixer Core Library
//!
//! Shared types, configuration, and constants for the IEM mixing system: the
//! site file and the roster, the UI protocol, band data and backups.

pub mod backup;
pub mod band;
#[cfg(feature = "config")]
pub mod config;
#[cfg(feature = "jwt")]
pub mod jwt;
pub mod lineup;
#[cfg(feature = "config")]
pub mod roster;
pub mod tunnel;
pub mod types;
pub mod ws;

pub use backup::{
    BACKUP_FORMAT, BACKUP_VERSION, BackupError, BackupInfo, MixerBackup, RestoreCategory,
    RestoreChange, RestorePreview, RestoreResult, SkippedEntry,
};
pub use band::{MAX_PRESETS, MAX_SNAPSHOTS, PresetInfo, SnapshotInfo};
#[cfg(feature = "config")]
pub use config::{Config, SiteInputMeta, SiteMember};
pub use lineup::{LineupInfo, LineupRef, SwitchRequest};
pub use types::{
    ApiError, AuthClaims, BatchControlRequest, BatchOperation, Channel, Customization, MixerState,
    is_valid_ui_pan, merge_or_replace_channels,
};

pub use tunnel::{TunnelState, TunnelStatusInfo};
pub use ws::{
    AlertInfo, ClientMsg, ConsoleInfo, ConsoleInput, ConsoleMix, EqBand, LoginFailures,
    MIN_CLIENT_PROTO, PageLink, ServerMsg, UI_PROTO,
};

/// Application version (from Cargo.toml)
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Git commit hash at build time (7 characters)
/// Returns "unknown" if not set during build
pub fn git_hash() -> &'static str {
    option_env!("GIT_HASH").unwrap_or("unknown")
}

/// Git branch at build time
/// Returns "unknown" if not set during build
pub fn git_branch() -> &'static str {
    option_env!("GIT_BRANCH").unwrap_or("unknown")
}

/// Build timestamp (unix seconds)
/// Returns "0" if not set during build
pub fn build_time() -> &'static str {
    option_env!("BUILD_TIME").unwrap_or("0")
}

/// Full version string for display (build time in UTC), e.g.
/// "2.0.0-dev.3 (24.09.2025 09:45)" for `BUILD_TIME` 1758707100.
pub fn full_version() -> String {
    full_version_at(build_time())
}

/// [`full_version`] for a `BUILD_TIME` value (unix seconds; "0" or unparsable
/// means a local build).
fn full_version_at(build_time: &str) -> String {
    let timestamp = build_time.parse::<i64>().unwrap_or(0);
    if timestamp == 0 {
        format!("{VERSION} (local)")
    } else {
        let datetime =
            utc_format(timestamp, "%d.%m.%Y %H:%M").unwrap_or_else(|| "unknown".to_string());
        format!("{VERSION} ({datetime})")
    }
}

/// The current Unix time in seconds (0 for a clock before 1970).
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// `ts` (Unix seconds) in UTC, formatted with the strftime `format`; `None`
/// for a time outside the years -9999…9999.
pub fn utc_format(ts: i64, format: &str) -> Option<String> {
    jiff::Timestamp::from_second(ts)
        .ok()
        .map(|t| t.strftime(format).to_string())
}

/// Version label for display: `v` + the Cargo version (pre-releases already
/// carry `-dev.N`), e.g. "v2.0.0-dev.3".
pub fn version_label() -> String {
    format!("v{VERSION}")
}

/// Build datetime for display in Slovak format (e.g., "28.02.2026 09:47")
pub fn build_datetime() -> String {
    build_datetime_at(build_time())
}

/// [`build_datetime`] for a `BUILD_TIME` value (unix seconds; "0" or
/// unparsable means a local build).
fn build_datetime_at(build_time: &str) -> String {
    let timestamp = build_time.parse::<i64>().unwrap_or(0);
    if timestamp == 0 {
        "local build".to_string()
    } else {
        utc_format(timestamp, "%d.%m.%Y %H:%M").unwrap_or_else(|| "unknown".to_string())
    }
}

/// Get formatted deployment timestamp (e.g., "2026-02-26 14:30:00 UTC")
pub fn deployed_at() -> String {
    deployed_at_at(build_time())
}

/// [`deployed_at`] for a `BUILD_TIME` value (unix seconds; "0" or unparsable
/// is unknown).
fn deployed_at_at(build_time: &str) -> String {
    let timestamp = build_time.parse::<i64>().unwrap_or(0);
    if timestamp == 0 {
        "unknown".to_string()
    } else {
        utc_format(timestamp, "%Y-%m-%d %H:%M:%S UTC").unwrap_or_else(|| "unknown".to_string())
    }
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn version_label_is_v_plus_the_cargo_version() {
        assert_eq!(version_label(), format!("v{VERSION}"));
    }

    #[test]
    fn full_version_starts_with_the_cargo_version() {
        assert!(full_version().starts_with(&format!("{VERSION} (")));
    }

    #[test]
    fn full_version_names_a_local_build() {
        assert_eq!(full_version_at("0"), format!("{VERSION} (local)"));
        assert_eq!(
            full_version_at("not a number"),
            format!("{VERSION} (local)")
        );
    }

    #[test]
    fn full_version_shows_the_utc_build_time() {
        assert_eq!(
            full_version_at("1758707100"),
            format!("{VERSION} (24.09.2025 09:45)")
        );
    }

    #[test]
    fn build_datetime_shows_the_utc_build_time_or_a_local_build() {
        assert_eq!(build_datetime_at("1758707100"), "24.09.2025 09:45");
        assert_eq!(build_datetime_at("0"), "local build");
        assert_eq!(build_datetime_at("not a number"), "local build");
        // Past the years jiff formats.
        assert_eq!(build_datetime_at("999999999999"), "unknown");
        assert_eq!(build_datetime(), build_datetime_at(build_time()));
    }

    #[test]
    fn deployed_at_shows_the_utc_build_time_or_unknown() {
        assert_eq!(deployed_at_at("1758707100"), "2025-09-24 09:45:00 UTC");
        assert_eq!(deployed_at_at("0"), "unknown");
        assert_eq!(deployed_at_at("not a number"), "unknown");
        assert_eq!(deployed_at_at("999999999999"), "unknown");
        assert_eq!(deployed_at(), deployed_at_at(build_time()));
    }

    #[test]
    fn utc_format_writes_every_format_the_server_uses() {
        let ts = 1_758_707_100; // 2025-09-24 09:45:00 UTC
        let f = |format| utc_format(ts, format).unwrap();
        assert_eq!(f("%d.%m.%Y %H:%M"), "24.09.2025 09:45");
        assert_eq!(f("%Y-%m-%d %H:%M:%S UTC"), "2025-09-24 09:45:00 UTC");
        assert_eq!(f("%Y-%m-%d"), "2025-09-24");
        assert_eq!(f("%Y%m%d_%H%M%S"), "20250924_094500");
        assert_eq!(f("%Y-%m-%dT%H:%M:%SZ"), "2025-09-24T09:45:00Z");
        assert_eq!(f("%Y-%m-%dT%H:%M:%S%.3fZ"), "2025-09-24T09:45:00.000Z");
        // Before the epoch and at the edge of the range.
        assert_eq!(
            utc_format(-1, "%Y-%m-%d %H:%M:%S").unwrap(),
            "1969-12-31 23:59:59"
        );
        assert_eq!(utc_format(0, "%Y-%m-%d").unwrap(), "1970-01-01");
        assert_eq!(utc_format(i64::MAX, "%Y"), None);
    }

    #[test]
    fn unix_now_is_the_clock_in_seconds() {
        let reference = jiff::Timestamp::now().as_second();
        let now = unix_now();
        assert!(now > 1_758_707_100, "{now}");
        assert!((now - reference).abs() <= 2, "{now} vs {reference}");
    }
}
