//! IEM Mixer Core Library
//!
//! Shared types, configuration, and constants for the IEM mixing system: the
//! site file, the UI protocol, band data and backups; the predecessor's
//! formats only in [`legacy`] (the importer's).

pub mod backup;
pub mod band;
#[cfg(feature = "config")]
pub mod config;
pub mod legacy;
pub mod tunnel;
pub mod types;
pub mod ws;

pub use backup::{
    BACKUP_FORMAT, BACKUP_VERSION, BackupError, BackupInfo, MixerBackup, RestoreCategory,
    RestoreChange, RestorePreview, RestoreResult, SkippedEntry,
};
pub use band::{MAX_PRESETS, MAX_SNAPSHOTS, PresetInfo, SnapshotInfo};
#[cfg(feature = "config")]
pub use config::{ActivityConfig, Config, SiteInputMeta, SiteMember};
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
        let datetime = chrono::DateTime::from_timestamp(timestamp, 0)
            .map(|dt| dt.format("%d.%m.%Y %H:%M").to_string())
            .unwrap_or_else(|| "unknown".to_string());
        format!("{VERSION} ({datetime})")
    }
}

/// Version label for display: `v` + the Cargo version (pre-releases already
/// carry `-dev.N`), e.g. "v2.0.0-dev.3".
pub fn version_label() -> String {
    format!("v{VERSION}")
}

/// Build datetime for display in Slovak format (e.g., "28.02.2026 09:47")
pub fn build_datetime() -> String {
    let timestamp = build_time().parse::<i64>().unwrap_or(0);
    if timestamp == 0 {
        "local build".to_string()
    } else {
        chrono::DateTime::from_timestamp(timestamp, 0)
            .map(|dt| dt.format("%d.%m.%Y %H:%M").to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

/// Get formatted deployment timestamp (e.g., "2026-02-26 14:30:00 UTC")
pub fn deployed_at() -> String {
    let timestamp = build_time().parse::<i64>().unwrap_or(0);
    if timestamp == 0 {
        "unknown".to_string()
    } else {
        chrono::DateTime::from_timestamp(timestamp, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| "unknown".to_string())
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
}
