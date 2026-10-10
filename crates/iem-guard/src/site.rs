//! The guard's settings on the PC (S6 plan Task 9, Task 13): the site's
//! `[guard]` and `[card]` tables and the paths in
//! `%LOCALAPPDATA%\iemmixer\guard\pc.toml`. Real values live only in the
//! private ops repository and on the PC (P6); the tests use synthetic ones.
//!
//! The guard reads the site whole but owns only `[guard]` (unknown keys
//! refused); `[card]` is the engine's table (S6 plan Task 5), so its other
//! keys are ignored here. An older site's `[activity]` table (the server's
//! band-activity alarm, removed) is ignored too: nothing reads the stage
//! (#38).

use std::fs;
use std::path::{Path, PathBuf};

use iem_win::prefwin::{Kind, Pref};
use serde::Deserialize;

use crate::effects::argv;
use crate::pc::Images;
use crate::plan::{Mode, PrefFail};

/// The bundle's binaries the guard starts (`bundle::REQUIRED`).
pub const ENGINE_EXE: &str = "iem-engine.exe";
pub const SERVER_EXE: &str = "iem-server.exe";
pub const TRAY_EXE: &str = "iem-tray.exe";

/// The card's period for iemmixer (I2).
pub const FRAMES: u32 = 32;

/// The engine's flags for a HIL job (design §7): its test signals and its
/// fault injection. Only in dev while a job runs, never in live.
pub const HIL_FLAGS: [&str; 2] = ["--test-signal", "--fault-injection"];

/// Where `pc.toml` lives: `<LOCALAPPDATA>\iemmixer\guard\pc.toml`.
pub fn pc_toml_path(local_app_data: &Path) -> PathBuf {
    local_app_data
        .join("iemmixer")
        .join("guard")
        .join("pc.toml")
}

/// The last component of a Windows or Unix path.
pub fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

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

fn section_key(s: &str) -> bool {
    s.split_once('/')
        .is_some_and(|(section, key)| !section.is_empty() && !key.is_empty() && !key.contains('/'))
}

fn ends_with(name: &str, suffix: &str) -> bool {
    name.to_ascii_lowercase().ends_with(suffix)
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
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

/// A registry value's kind as the site names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KindSite {
    Dword,
    #[serde(alias = "string", alias = "sz")]
    Text,
}

/// `[card] pref_original`: the value REAPER keeps (design §3).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PrefSite {
    pub kind: KindSite,
    pub raw: String,
}

impl PrefSite {
    pub fn pref(&self) -> Pref {
        Pref {
            kind: match self.kind {
                KindSite::Dword => Kind::Dword,
                KindSite::Text => Kind::Text,
            },
            raw: self.raw.clone(),
        }
    }
}

/// The parts of the engine's `[card]` the guard reads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CardSite {
    /// The driver's DLL: its holders are read before REAPER or the engine
    /// may open the card (I3).
    pub module: String,
    pub frames: u32,
    /// `HKEY_CURRENT_USER` key and value name of the preferred buffer.
    pub pref_key: String,
    pub pref_name: String,
    pub pref_original: PrefSite,
}

impl CardSite {
    pub fn problems(&self) -> Vec<String> {
        let mut bad = Vec::new();
        let mut check = |ok: bool, why: &str| {
            if !ok {
                bad.push(format!("[card] {why}"));
            }
        };
        check(ends_with(&self.module, ".dll"), "module must be a .dll");
        check(self.frames == FRAMES, "frames must be 32 (I2)");
        check(!self.pref_key.is_empty(), "pref_key is empty");
        check(!self.pref_name.is_empty(), "pref_name is empty");
        let raw = &self.pref_original.raw;
        check(
            self.pref_original.kind == KindSite::Text
                || (!raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit())),
            "pref_original of kind dword must be decimal digits",
        );
        bad
    }
}

#[derive(Debug, Deserialize)]
struct SiteTables {
    guard: Option<GuardSite>,
    card: Option<CardSite>,
}

fn default_engine_pipe() -> String {
    "iemmixer-engine".to_owned()
}

fn default_tunnel_ready() -> String {
    "http://127.0.0.1:20241/ready".to_owned()
}

fn default_elevated_root() -> PathBuf {
    PathBuf::from(r"C:\ProgramData\iemmixer")
}

/// `pc.toml`: where things are on this PC (written at bootstrap). Command
/// arguments may name `{root}`, `{bundle}`, `{site}` and `{server_config}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PcToml {
    /// `%LOCALAPPDATA%\iemmixer`: `bundles\<sha>\`, `guard\`, `logs\`.
    pub root: PathBuf,
    /// The site the engine and the guard read.
    pub site: PathBuf,
    /// `iem-server`'s config (`IEMMIXER_CONFIG`).
    pub server_config: PathBuf,
    #[serde(default = "default_engine_pipe")]
    pub engine_pipe: String,
    /// The engine's arguments after its exe (`run`, `--site`,
    /// `--state-dir`, …); the guard appends `--pipe <engine_pipe>`, `--hold`
    /// on request, and the HIL flags inside a HIL job only.
    pub engine_args: Vec<String>,
    pub reaper_exe: PathBuf,
    pub app_exe: PathBuf,
    /// The HIL runner's program and arguments, and its directory.
    pub runner: Vec<String>,
    pub runner_dir: PathBuf,
    /// The data refresh of a dev entry: commands run in order, each exit 0.
    #[serde(default)]
    pub data_dev: Vec<Vec<String>>,
    #[serde(default)]
    pub data_live: Vec<Vec<String>>,
    /// cloudflared's readiness URL (the tunnel watchdog's).
    #[serde(default = "default_tunnel_ready")]
    pub tunnel_ready: String,
    /// The elevated tasks' root (`Register-IemTasks -ElevatedRoot`: the
    /// ProgramData known folder + `iemmixer`, only Administrators and SYSTEM
    /// may change it): their results and S1c's tuning record. From here,
    /// never from an environment variable the user could change.
    #[serde(default = "default_elevated_root")]
    pub elevated_root: PathBuf,
}

impl PcToml {
    pub fn problems(&self) -> Vec<String> {
        let mut bad = Vec::new();
        let mut check = |ok: bool, why: &str| {
            if !ok {
                bad.push(format!("pc.toml: {why}"));
            }
        };
        check(!self.root.as_os_str().is_empty(), "root is empty");
        check(
            !self.engine_pipe.is_empty() && !self.engine_pipe.contains('\\'),
            "engine_pipe must be a plain name",
        );
        let names = |flag: &str| self.engine_args.iter().any(|a| a == flag);
        check(!self.engine_args.is_empty(), "engine_args is empty");
        check(
            !names("--pipe") && !names("--hold"),
            "engine_args must not name --pipe or --hold: the guard adds them",
        );
        check(
            !HIL_FLAGS.iter().any(|f| names(f)),
            "engine_args must not name the HIL flags: the guard adds them inside a HIL job only",
        );
        check(
            self.engine_args.is_empty() || (names("--site") && names("--state-dir")),
            "engine_args must name --site and --state-dir (iem-engine run)",
        );
        check(!self.runner.is_empty(), "runner is empty");
        check(
            !self.data_dev.is_empty(),
            "data_dev is empty: every dev entry refreshes the band's data (P9)",
        );
        check(
            self.data_dev
                .iter()
                .chain(&self.data_live)
                .all(|c| !c.is_empty()),
            "a data command is empty",
        );
        check(
            self.tunnel_ready.starts_with("http://"),
            "tunnel_ready must be plain http://",
        );
        check(
            !self.elevated_root.as_os_str().is_empty() && self.elevated_root != self.root,
            "elevated_root must be the elevated tasks' own folder",
        );
        bad
    }
}

/// Everything the Windows effects read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub guard: GuardSite,
    pub card: CardSite,
    pub pc: PcToml,
}

impl Settings {
    /// `pc.toml`'s and the site's text; every problem at once.
    pub fn parse(pc_text: &str, site_text: &str) -> Result<Self, String> {
        let pc: PcToml = toml::from_str(pc_text).map_err(|e| format!("pc.toml: {e}"))?;
        Self::with_site(pc, site_text)
    }

    /// Reads `pc.toml` and the site it names.
    pub fn load(pc_toml: &Path) -> Result<Self, String> {
        let read = |p: &Path| fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
        let pc: PcToml =
            toml::from_str(&read(pc_toml)?).map_err(|e| format!("{}: {e}", pc_toml.display()))?;
        let site = read(&pc.site)?;
        Self::with_site(pc, &site)
    }

    fn with_site(pc: PcToml, site_text: &str) -> Result<Self, String> {
        let t: SiteTables = toml::from_str(site_text).map_err(|e| format!("site: {e}"))?;
        let s = Settings {
            guard: t.guard.ok_or("the site has no [guard] table")?,
            card: t.card.ok_or("the site has no [card] table")?,
            pc,
        };
        let bad = s.problems();
        if bad.is_empty() {
            Ok(s)
        } else {
            Err(bad.join("; "))
        }
    }

    pub fn problems(&self) -> Vec<String> {
        let mut bad = self.guard.problems();
        bad.extend(self.card.problems());
        bad.extend(self.pc.problems());
        let app = self.pc.app_exe.to_string_lossy();
        if !file_name(&app).eq_ignore_ascii_case(&self.guard.app_image) {
            bad.push("pc.toml: app_exe is not [guard] app_image".to_owned());
        }
        bad
    }

    /// A new site for this PC (`install-site`, F30): the site file is shared
    /// with the engine, and the guard reads its own tables from it at every
    /// start, so a site the guard could not load is refused before it
    /// replaces the old one.
    pub fn check_new_site(&self, site_text: &str) -> Result<(), String> {
        Self::with_site(self.pc.clone(), site_text).map(|_| ())
    }

    /// The image names of the process list.
    pub fn images(&self) -> Images {
        let name = |p: &Path| file_name(&p.to_string_lossy()).to_owned();
        Images {
            reaper: name(&self.pc.reaper_exe),
            app: self.guard.app_image.clone(),
            engine: ENGINE_EXE.to_owned(),
            server: SERVER_EXE.to_owned(),
            tray: TRAY_EXE.to_owned(),
            runner: self
                .pc
                .runner
                .first()
                .map(|r| file_name(r).to_owned())
                .unwrap_or_default(),
        }
    }

    pub fn bundle_dir(&self, sha: &str) -> PathBuf {
        self.pc.root.join("bundles").join(sha)
    }

    pub fn guard_dir(&self) -> PathBuf {
        self.pc.root.join("guard")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.pc.root.join("logs")
    }

    /// Where the guard writes the elevated tasks' requests (the user's root).
    pub fn requests_dir(&self) -> PathBuf {
        self.guard_dir().join("tasks")
    }

    /// Where the elevated tasks answer (the elevated root; read only).
    pub fn results_dir(&self) -> PathBuf {
        self.pc.elevated_root.join("tasks").join("out")
    }

    /// S1c's record of what its tuning applied (the drift check).
    pub fn tuning_record(&self) -> PathBuf {
        self.pc
            .elevated_root
            .join("tuning")
            .join(crate::effects::tuning::EXPECT)
    }

    /// The data commands of an entry into `mode` (none for `event`).
    pub fn data_commands(&self, mode: Mode) -> &[Vec<String>] {
        match mode {
            Mode::Event => &[],
            Mode::Dev => &self.pc.data_dev,
            Mode::Live => &self.pc.data_live,
        }
    }

    /// The engine's arguments for the bundle in `bundle`: `engine_args`
    /// with its placeholders, then `--pipe <engine_pipe>` (the one name the
    /// supervisor client and the server use too), `--hold` on request and
    /// [`HIL_FLAGS`] inside a HIL job (`hil`).
    pub fn engine_argv(&self, bundle: &Path, hold: bool, hil: bool) -> Result<Vec<String>, String> {
        let mut args = argv::expand(&self.pc.engine_args, &self.vars(bundle))?;
        args.extend(["--pipe".to_owned(), self.pc.engine_pipe.clone()]);
        if hold {
            args.push("--hold".to_owned());
        }
        if hil {
            args.extend(HIL_FLAGS.map(str::to_owned));
        }
        Ok(args)
    }

    /// The placeholders of command arguments, for the bundle in `bundle`.
    pub fn vars(&self, bundle: &Path) -> Vec<(&'static str, String)> {
        let text = |p: &Path| p.to_string_lossy().into_owned();
        vec![
            ("root", text(&self.pc.root)),
            ("bundle", text(bundle)),
            ("site", text(&self.pc.site)),
            ("server_config", text(&self.pc.server_config)),
        ]
    }
}

#[cfg(test)]
pub(crate) mod tests;
