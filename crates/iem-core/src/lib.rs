//! IEM Mixer Core Library
//!
//! Shared types, configuration, and constants for the IEM mixing system.

pub mod backup;
#[cfg(feature = "config")]
pub mod config;
pub mod preset;
pub mod snapshot;
pub mod tunnel;
pub mod types;
pub mod ws;

pub use backup::{
    BACKUP_VERSION, BackupInfo, CaptureAudit, EqBandBackup, LimiterBackup, MixerBackup,
    RestoreCategory, RestoreChange, RestorePreview, RestoreProgress, RestoreResult, SendBackup,
    SkippedEntry,
};
#[cfg(feature = "config")]
pub use config::{BandMember, Config, DiscoveredMember, InputTrack};
pub use preset::{ChannelPreset, MAX_PRESETS, PresetEntry};
pub use snapshot::{ChannelSnapshot, MAX_SNAPSHOTS, MixSnapshot};
pub use types::{
    ApiError, AuthClaims, BatchControlRequest, BatchOperation, Channel, Customization, MixerState,
    PollResponse, is_valid_pan, merge_or_replace_channels,
};

pub use tunnel::{TunnelState, TunnelStatusInfo};
pub use ws::{AlertInfo, ClientMsg, EqBand, ServerMsg};

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

/// Full version string for display
/// On main: "1.6.0 (27.02.2026 12:07)"
/// On dev:  "1.6.0-dev (27.02.2026 12:07)"
pub fn full_version() -> String {
    let branch = git_branch();
    let version_base = if branch != "main" && branch != "unknown" {
        format!("{}-{}", VERSION, branch)
    } else {
        VERSION.to_string()
    };

    let timestamp = build_time().parse::<i64>().unwrap_or(0);
    if timestamp == 0 {
        format!("{} (local)", version_base)
    } else {
        let datetime = chrono::DateTime::from_timestamp(timestamp, 0)
            .map(|dt| dt.format("%d.%m.%Y %H:%M").to_string())
            .unwrap_or_else(|| "unknown".to_string());
        format!("{} ({})", version_base, datetime)
    }
}

/// Version label for display (e.g., "v1.16.0" or "v1.16.0-dev")
pub fn version_label() -> String {
    let branch = git_branch();
    if branch != "main" && branch != "unknown" {
        format!("v{}-{}", VERSION, branch)
    } else {
        format!("v{}", VERSION)
    }
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
