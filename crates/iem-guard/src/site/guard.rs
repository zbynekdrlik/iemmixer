//! The site's `[guard]` table (S6 plan Task 13): the guard owns it, so
//! its unknown keys are refused.

use std::path::PathBuf;

use serde::Deserialize;

use super::{ends_with, is_hex64, section_key};
use crate::plan::PrefFail;

/// `[guard]`: REAPER's control, the meter bridge, the predecessor app, the
/// public host and the guard's own switches (S6 plan Task 13).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardSite {
    /// REAPER's web control on this PC (plain `http://`).
    pub reaper_url: String,
    /// The project file; its modification time proves a save.
    pub reaper_project: PathBuf,
    /// The track count REAPER must report once the project is loaded.
    pub reaper_tracks: u32,
    /// REAPER's track numbers of the stage inputs (as in its `TRACK` lines).
    pub stage_tracks: Vec<u32>,
    /// `section/key` of the meter bridge's state.
    pub bridge_state: String,
    /// `section/key` of the meter bridge's heartbeat.
    pub bridge_heartbeat: String,
    /// The action that starts the meter bridge.
    pub bridge_action: String,
    /// The predecessor app's image name.
    pub app_image: String,
    /// The class of the tray library's top-level window.
    pub app_tray_class: String,
    /// The `WM_COMMAND` id of the tray menu's Exit item (design §5.3).
    pub app_exit_id: u16,
    /// SHA-256 of the deployed exe; empty until recorded (Task 16), which
    /// refuses every entry.
    #[serde(default)]
    pub app_exe_sha256: String,
    pub app_log_dir: PathBuf,
    /// The log line the tray's Exit writes (corroboration only).
    pub app_exit_line: String,
    /// Where the app writes its data (temp-then-rename).
    pub app_data_dir: PathBuf,
    /// The member count the app's `/api/members` must list.
    pub app_members: u32,
    /// The band's public host name (no scheme, no path).
    pub public_host: String,
    /// The card outputs a HIL test signal may reach (design §4).
    #[serde(default)]
    pub hil_tx: Vec<u16>,
    /// Set after the owner-approved PC tests (design §10).
    #[serde(default)]
    pub pc_tests_passed: bool,
    /// The guard starts REAPER and the app itself (the probe task was
    /// refused, design §5.1).
    #[serde(default)]
    pub start_direct: bool,
    pub on_pref_fail: PrefFail,
}

impl GuardSite {
    pub fn problems(&self) -> Vec<String> {
        let mut bad = Vec::new();
        let mut check = |ok: bool, why: &str| {
            if !ok {
                bad.push(format!("[guard] {why}"));
            }
        };
        check(
            self.reaper_url.starts_with("http://"),
            "reaper_url must be plain http:// on this PC",
        );
        check(self.reaper_tracks > 0, "reaper_tracks must be above 0");
        check(!self.stage_tracks.is_empty(), "stage_tracks is empty");
        check(
            !self.stage_tracks.contains(&0),
            "stage_tracks names track 0 (the master)",
        );
        check(
            section_key(&self.bridge_state),
            "bridge_state must be section/key",
        );
        check(
            section_key(&self.bridge_heartbeat),
            "bridge_heartbeat must be section/key",
        );
        check(
            !self.bridge_action.is_empty() && !self.bridge_action.contains('/'),
            "bridge_action must be one action id",
        );
        check(
            ends_with(&self.app_image, ".exe"),
            "app_image must be an .exe",
        );
        check(!self.app_tray_class.is_empty(), "app_tray_class is empty");
        check(self.app_exit_id != 0, "app_exit_id must not be 0");
        check(
            self.app_exe_sha256.is_empty() || is_hex64(&self.app_exe_sha256),
            "app_exe_sha256 must be 64 hex digits",
        );
        check(
            !self.app_log_dir.as_os_str().is_empty(),
            "app_log_dir is empty",
        );
        check(!self.app_exit_line.is_empty(), "app_exit_line is empty");
        check(
            !self.app_data_dir.as_os_str().is_empty(),
            "app_data_dir is empty",
        );
        check(self.app_members > 0, "app_members must be above 0");
        check(
            !self.public_host.is_empty()
                && !self.public_host.contains('/')
                && !self.public_host.contains(':'),
            "public_host must be a bare host name",
        );
        bad
    }
}
