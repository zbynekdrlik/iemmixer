//! The guard's settings on the PC (S6 plan Task 9, Task 13): the site's
//! `[guard]`, `[card]` and `[activity]` tables and the paths in
//! `%LOCALAPPDATA%\iemmixer\guard\pc.toml`. Real values live only in the
//! private ops repository and on the PC (P6); the tests use synthetic ones.
//!
//! The guard reads the site whole but owns only `[guard]` (unknown keys
//! refused); `[card]` is the engine's table (S6 plan Task 5) and
//! `[activity]` the server's, so their other keys are ignored here.

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

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct ActivitySite {
    #[serde(default)]
    inputs: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SiteTables {
    guard: Option<GuardSite>,
    card: Option<CardSite>,
    #[serde(default)]
    activity: ActivitySite,
}

fn default_engine_pipe() -> String {
    "iemmixer-engine".to_owned()
}

fn default_tunnel_ready() -> String {
    "http://127.0.0.1:20241/ready".to_owned()
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
    /// `--state-dir`, …); the guard appends `--pipe <engine_pipe>`, and
    /// `--hold` on request.
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
        bad
    }
}

/// Everything the Windows effects read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub guard: GuardSite,
    pub card: CardSite,
    /// `[activity] inputs`: the stage inputs' engine ids.
    pub stage_inputs: Vec<String>,
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
            stage_inputs: t.activity.inputs,
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
        if self.stage_inputs.is_empty() {
            bad.push("[activity] inputs is empty: the guard needs the stage inputs".to_owned());
        }
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
    pub fn check_new_site(&self, _site_text: &str) -> Result<(), String> {
        Ok(())
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

    /// The tuning task's request, result and record files.
    pub fn tuning_dir(&self) -> PathBuf {
        self.guard_dir().join("tuning")
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
    /// supervisor client and the server use too) and `--hold` on request.
    pub fn engine_argv(&self, bundle: &Path, hold: bool) -> Result<Vec<String>, String> {
        let mut args = argv::expand(&self.pc.engine_args, &self.vars(bundle))?;
        args.extend(["--pipe".to_owned(), self.pc.engine_pipe.clone()]);
        if hold {
            args.push("--hold".to_owned());
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
pub(crate) mod tests {
    use super::*;

    pub(crate) const PC: &str = r#"
root = 'C:\IEM\iemmixer'
site = 'C:\IEM\iemmixer\site\site.toml'
server_config = 'C:\IEM\iemmixer\server\iemmixer.toml'
engine_args = ["run", "--backend", "asio", "--site", "{site}", "--state-dir", "{root}\\engine"]
reaper_exe = 'C:\Programs\REAPER\reaper.exe'
app_exe = 'C:\Programs\App\app.exe'
runner = ['C:\IEM\runner\bin\Runner.Listener.exe', "run"]
runner_dir = 'C:\IEM\runner'
data_dev = [["{bundle}\\iem-migrate.exe", "band", "--site", "{site}"]]
"#;

    pub(crate) const SITE: &str = r#"
[engine]
pipe = "iemmixer-engine"

[guard]
reaper_url = "http://127.0.0.1:8080"
reaper_project = 'C:\Band\project.rpp'
reaper_tracks = 40
stage_tracks = [1, 2, 3]
bridge_state = "bridge/state"
bridge_heartbeat = "bridge/heartbeat"
bridge_action = "_TEST_BRIDGE"
app_image = "app.exe"
app_tray_class = "TestTrayClass"
app_exit_id = 4242
app_exe_sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
app_log_dir = 'C:\Programs\App\logs'
app_exit_line = "tray exit"
app_data_dir = 'C:\Programs\App\data'
app_members = 9
public_host = "mixer.example.org"
hil_tx = [71, 72]
on_pref_fail = "start_reaper_with_alarm"

[card]
driver = "Test Card"
module = "testcard.dll"
frames = 32
pref_key = 'Software\ASIO\Test Card'
pref_name = "BufferPref"
pref_original = { kind = "dword", raw = "64" }

[activity]
inputs = ["mic1", "mic2", "mic3"]
threshold_dbfs = -50.0
"#;

    pub(crate) fn settings() -> Settings {
        Settings::parse(PC, SITE).unwrap()
    }

    #[test]
    fn the_synthetic_site_parses_with_its_defaults() {
        let s = settings();
        assert_eq!(s.guard.reaper_tracks, 40);
        assert_eq!(s.guard.stage_tracks, [1, 2, 3]);
        assert_eq!(s.guard.app_exit_id, 4242);
        assert_eq!(s.guard.hil_tx, [71, 72]);
        assert!(!s.guard.pc_tests_passed && !s.guard.start_direct);
        assert_eq!(s.guard.on_pref_fail, PrefFail::StartReaperWithAlarm);
        assert_eq!(s.card.module, "testcard.dll");
        assert_eq!(
            s.card.pref_original.pref(),
            Pref {
                kind: Kind::Dword,
                raw: "64".into()
            }
        );
        assert_eq!(s.stage_inputs, ["mic1", "mic2", "mic3"]);
        assert_eq!(s.pc.engine_pipe, "iemmixer-engine");
        assert_eq!(s.pc.tunnel_ready, "http://127.0.0.1:20241/ready");
        assert!(s.pc.data_live.is_empty());
        assert_eq!(s.problems(), Vec::<String>::new());
    }

    #[test]
    fn a_text_preference_keeps_its_kind() {
        for name in ["text", "string", "sz"] {
            let site = SITE.replace(
                r#"{ kind = "dword", raw = "64" }"#,
                &format!(r#"{{ kind = "{name}", raw = " 64" }}"#),
            );
            let s = Settings::parse(PC, &site).unwrap();
            assert_eq!(
                s.card.pref_original.pref(),
                Pref {
                    kind: Kind::Text,
                    raw: " 64".into()
                },
                "{name}"
            );
        }
    }

    #[test]
    fn missing_tables_and_unknown_guard_keys_are_refused() {
        let no_guard = SITE.replace("[guard]", "[not_guard]");
        assert_eq!(
            Settings::parse(PC, &no_guard).unwrap_err(),
            "the site has no [guard] table"
        );
        let no_card = SITE.replace("[card]", "[not_card]");
        assert_eq!(
            Settings::parse(PC, &no_card).unwrap_err(),
            "the site has no [card] table"
        );
        let unknown = SITE.replace("[guard]\n", "[guard]\nsurprise = 1\n");
        assert!(
            Settings::parse(PC, &unknown)
                .unwrap_err()
                .starts_with("site: ")
        );
        assert!(
            Settings::parse("root = 1", SITE)
                .unwrap_err()
                .starts_with("pc.toml: ")
        );
        let pc_unknown = format!("{PC}\nsurprise = 1\n");
        assert!(
            Settings::parse(&pc_unknown, SITE)
                .unwrap_err()
                .starts_with("pc.toml: ")
        );
    }

    #[test]
    fn a_new_site_must_keep_the_guards_own_tables() {
        let s = settings();
        assert_eq!(s.check_new_site(SITE), Ok(()));
        for (site, why) in [
            (
                SITE.replace("[guard]", "[not_guard]"),
                "the site has no [guard] table",
            ),
            (
                SITE.replace("[card]", "[not_card]"),
                "the site has no [card] table",
            ),
            (
                SITE.replace(r#"inputs = ["mic1", "mic2", "mic3"]"#, "inputs = []"),
                "[activity] inputs is empty: the guard needs the stage inputs",
            ),
            (
                SITE.replace(r#"app_image = "app.exe""#, r#"app_image = "other.exe""#),
                "pc.toml: app_exe is not [guard] app_image",
            ),
        ] {
            assert_eq!(s.check_new_site(&site), Err(why.to_owned()), "{why}");
        }
        let broken = s.check_new_site("[guard").unwrap_err();
        assert!(broken.starts_with("site: "), "{broken}");
        // A table the guard does not read may change freely.
        let engine = SITE.replace(r#"pipe = "iemmixer-engine""#, r#"pipe = "other-engine""#);
        assert_eq!(s.check_new_site(&engine), Ok(()));
    }

    #[test]
    fn every_guard_problem_is_named() {
        let good = settings().guard;
        assert_eq!(good.problems(), Vec::<String>::new());
        let cases: Vec<(GuardSite, &str)> = vec![
            (
                GuardSite {
                    reaper_url: "https://127.0.0.1".into(),
                    ..good.clone()
                },
                "reaper_url must be plain http:// on this PC",
            ),
            (
                GuardSite {
                    reaper_tracks: 0,
                    ..good.clone()
                },
                "reaper_tracks must be above 0",
            ),
            (
                GuardSite {
                    stage_tracks: vec![],
                    ..good.clone()
                },
                "stage_tracks is empty",
            ),
            (
                GuardSite {
                    stage_tracks: vec![1, 0],
                    ..good.clone()
                },
                "stage_tracks names track 0 (the master)",
            ),
            (
                GuardSite {
                    bridge_state: "state".into(),
                    ..good.clone()
                },
                "bridge_state must be section/key",
            ),
            (
                GuardSite {
                    bridge_heartbeat: "/heartbeat".into(),
                    ..good.clone()
                },
                "bridge_heartbeat must be section/key",
            ),
            (
                GuardSite {
                    bridge_action: String::new(),
                    ..good.clone()
                },
                "bridge_action must be one action id",
            ),
            (
                GuardSite {
                    bridge_action: "a/b".into(),
                    ..good.clone()
                },
                "bridge_action must be one action id",
            ),
            (
                GuardSite {
                    app_image: "app".into(),
                    ..good.clone()
                },
                "app_image must be an .exe",
            ),
            (
                GuardSite {
                    app_tray_class: String::new(),
                    ..good.clone()
                },
                "app_tray_class is empty",
            ),
            (
                GuardSite {
                    app_exit_id: 0,
                    ..good.clone()
                },
                "app_exit_id must not be 0",
            ),
            (
                GuardSite {
                    app_exe_sha256: "ab".repeat(31),
                    ..good.clone()
                },
                "app_exe_sha256 must be 64 hex digits",
            ),
            (
                GuardSite {
                    app_exe_sha256: "g".repeat(64),
                    ..good.clone()
                },
                "app_exe_sha256 must be 64 hex digits",
            ),
            (
                GuardSite {
                    app_log_dir: PathBuf::new(),
                    ..good.clone()
                },
                "app_log_dir is empty",
            ),
            (
                GuardSite {
                    app_exit_line: String::new(),
                    ..good.clone()
                },
                "app_exit_line is empty",
            ),
            (
                GuardSite {
                    app_data_dir: PathBuf::new(),
                    ..good.clone()
                },
                "app_data_dir is empty",
            ),
            (
                GuardSite {
                    app_members: 0,
                    ..good.clone()
                },
                "app_members must be above 0",
            ),
            (
                GuardSite {
                    public_host: String::new(),
                    ..good.clone()
                },
                "public_host must be a bare host name",
            ),
            (
                GuardSite {
                    public_host: "https://mixer.example.org".into(),
                    ..good.clone()
                },
                "public_host must be a bare host name",
            ),
            (
                GuardSite {
                    public_host: "mixer.example.org/x".into(),
                    ..good.clone()
                },
                "public_host must be a bare host name",
            ),
            (
                GuardSite {
                    public_host: "mixer.example.org:443".into(),
                    ..good.clone()
                },
                "public_host must be a bare host name",
            ),
        ];
        for (g, want) in cases {
            assert_eq!(g.problems(), [format!("[guard] {want}")], "{want}");
        }
        // Accepted edges: an unrecorded hash, upper-case hex, an .EXE.
        let edges = GuardSite {
            app_exe_sha256: String::new(),
            app_image: "APP.EXE".into(),
            ..good.clone()
        };
        assert_eq!(edges.problems(), Vec::<String>::new());
        let upper = GuardSite {
            app_exe_sha256: "AB".repeat(32),
            ..good
        };
        assert_eq!(upper.problems(), Vec::<String>::new());
    }

    #[test]
    fn section_keys_have_two_non_empty_parts() {
        assert!(section_key("a/b"));
        for bad in ["", "a", "a/", "/b", "a/b/c"] {
            assert!(!section_key(bad), "{bad:?}");
        }
    }

    #[test]
    fn every_card_problem_is_named() {
        let good = settings().card;
        assert_eq!(good.problems(), Vec::<String>::new());
        let pref = |kind, raw: &str| PrefSite {
            kind,
            raw: raw.into(),
        };
        let cases: Vec<(CardSite, &str)> = vec![
            (
                CardSite {
                    module: "testcard.sys".into(),
                    ..good.clone()
                },
                "module must be a .dll",
            ),
            (
                CardSite {
                    frames: 64,
                    ..good.clone()
                },
                "frames must be 32 (I2)",
            ),
            (
                CardSite {
                    pref_key: String::new(),
                    ..good.clone()
                },
                "pref_key is empty",
            ),
            (
                CardSite {
                    pref_name: String::new(),
                    ..good.clone()
                },
                "pref_name is empty",
            ),
            (
                CardSite {
                    pref_original: pref(KindSite::Dword, ""),
                    ..good.clone()
                },
                "pref_original of kind dword must be decimal digits",
            ),
            (
                CardSite {
                    pref_original: pref(KindSite::Dword, " 64"),
                    ..good.clone()
                },
                "pref_original of kind dword must be decimal digits",
            ),
        ];
        for (c, want) in cases {
            assert_eq!(c.problems(), [format!("[card] {want}")], "{want}");
        }
        let text = CardSite {
            module: "TESTCARD.DLL".into(),
            pref_original: pref(KindSite::Text, " 64"),
            ..good
        };
        assert_eq!(text.problems(), Vec::<String>::new());
    }

    #[test]
    fn every_pc_toml_problem_is_named() {
        let good = settings().pc;
        assert_eq!(good.problems(), Vec::<String>::new());
        let cases: Vec<(PcToml, &str)> = vec![
            (
                PcToml {
                    root: PathBuf::new(),
                    ..good.clone()
                },
                "root is empty",
            ),
            (
                PcToml {
                    engine_pipe: String::new(),
                    ..good.clone()
                },
                "engine_pipe must be a plain name",
            ),
            (
                PcToml {
                    engine_pipe: "\\\\.\\pipe\\x".into(),
                    ..good.clone()
                },
                "engine_pipe must be a plain name",
            ),
            (
                PcToml {
                    engine_args: vec![],
                    ..good.clone()
                },
                "engine_args is empty",
            ),
            (
                PcToml {
                    runner: vec![],
                    ..good.clone()
                },
                "runner is empty",
            ),
            (
                PcToml {
                    data_dev: vec![],
                    ..good.clone()
                },
                "data_dev is empty: every dev entry refreshes the band's data (P9)",
            ),
            (
                PcToml {
                    data_live: vec![vec!["x".into()], vec![]],
                    ..good.clone()
                },
                "a data command is empty",
            ),
            (
                PcToml {
                    tunnel_ready: "https://127.0.0.1/ready".into(),
                    ..good.clone()
                },
                "tunnel_ready must be plain http://",
            ),
        ];
        for (p, want) in cases {
            assert_eq!(p.problems(), [format!("pc.toml: {want}")], "{want}");
        }
    }

    /// `iem-engine run` needs `--site` and `--state-dir`; the pipe name is
    /// `engine_pipe` alone (the supervisor and the server use it too), so
    /// the guard adds `--pipe` itself, and `--hold` on request.
    #[test]
    fn engine_args_name_the_state_and_leave_pipe_and_hold_to_the_guard() {
        let good = settings().pc;
        let args = |a: &[&str]| PcToml {
            engine_args: a.iter().map(|s| (*s).to_owned()).collect(),
            ..good.clone()
        };
        let adds = "pc.toml: engine_args must not name --pipe or --hold: the guard adds them";
        let needs = "pc.toml: engine_args must name --site and --state-dir (iem-engine run)";
        assert_eq!(
            args(&["run", "--site", "s", "--state-dir", "d"]).problems(),
            Vec::<String>::new()
        );
        assert_eq!(args(&["run", "--site", "s"]).problems(), [needs]);
        assert_eq!(args(&["run", "--state-dir", "d"]).problems(), [needs]);
        assert_eq!(
            args(&["run", "--site", "s", "--state-dir", "d", "--pipe", "p"]).problems(),
            [adds]
        );
        assert_eq!(
            args(&["run", "--site", "s", "--state-dir", "d", "--hold"]).problems(),
            [adds]
        );
        assert_eq!(args(&["run", "--hold"]).problems(), [adds, needs]);
        // An empty list is named once.
        assert_eq!(args(&[]).problems(), ["pc.toml: engine_args is empty"]);
    }

    #[test]
    fn the_engine_gets_the_one_pipe_name_and_hold_on_request() {
        let s = settings();
        let bundle = s.bundle_dir("b");
        let base = [
            "run",
            "--backend",
            "asio",
            "--site",
            "C:\\IEM\\iemmixer\\site\\site.toml",
            "--state-dir",
            "C:\\IEM\\iemmixer\\engine",
            "--pipe",
            "iemmixer-engine",
        ];
        assert_eq!(s.engine_argv(&bundle, false).unwrap(), base);
        let mut held = base.to_vec();
        held.push("--hold");
        assert_eq!(s.engine_argv(&bundle, true).unwrap(), held);
        let mut other = s.clone();
        other.pc.engine_pipe = "iemmixer-engine-2".into();
        assert_eq!(
            other
                .engine_argv(&bundle, false)
                .unwrap()
                .last()
                .map(String::as_str),
            Some("iemmixer-engine-2")
        );
        other.pc.engine_args.push("{nope}".into());
        assert_eq!(
            other.engine_argv(&bundle, true).unwrap_err(),
            "unknown placeholder {nope} in \"{nope}\""
        );
    }

    #[test]
    fn settings_problems_join_the_tables_and_check_the_app() {
        let mut s = settings();
        s.stage_inputs.clear();
        s.pc.app_exe = PathBuf::from("C:\\Programs\\App\\other.exe");
        s.card.frames = 48;
        s.guard.app_members = 0;
        assert_eq!(
            s.problems(),
            [
                "[guard] app_members must be above 0",
                "[card] frames must be 32 (I2)",
                "[activity] inputs is empty: the guard needs the stage inputs",
                "pc.toml: app_exe is not [guard] app_image",
            ]
        );
        let bad_pc: String = PC
            .lines()
            .map(|l| {
                if l.starts_with("engine_args") {
                    "engine_args = []"
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            Settings::parse(&bad_pc, SITE).unwrap_err(),
            "pc.toml: engine_args is empty"
        );
        // The exe's name compares without case.
        let mut s = settings();
        s.pc.app_exe = PathBuf::from("C:\\Programs\\App\\APP.exe");
        assert_eq!(s.problems(), Vec::<String>::new());
    }

    #[test]
    fn file_names_split_on_either_separator() {
        assert_eq!(file_name("C:\\Programs\\REAPER\\reaper.exe"), "reaper.exe");
        assert_eq!(file_name("/opt/x/iem-engine"), "iem-engine");
        assert_eq!(file_name("C:\\a/b\\c.exe"), "c.exe");
        assert_eq!(file_name("plain.exe"), "plain.exe");
        assert_eq!(file_name(""), "");
    }

    #[test]
    fn images_and_directories_follow_the_settings() {
        let s = settings();
        assert_eq!(
            s.images(),
            Images {
                reaper: "reaper.exe".into(),
                app: "app.exe".into(),
                engine: "iem-engine.exe".into(),
                server: "iem-server.exe".into(),
                tray: "iem-tray.exe".into(),
                runner: "Runner.Listener.exe".into(),
            }
        );
        let root = PathBuf::from("C:\\IEM\\iemmixer");
        let sha = "a".repeat(40);
        assert_eq!(s.bundle_dir(&sha), root.join("bundles").join(&sha));
        assert_eq!(s.guard_dir(), root.join("guard"));
        assert_eq!(s.logs_dir(), root.join("logs"));
        assert_eq!(s.tuning_dir(), root.join("guard").join("tuning"));
        assert_eq!(
            pc_toml_path(Path::new("C:\\Local")),
            Path::new("C:\\Local")
                .join("iemmixer")
                .join("guard")
                .join("pc.toml")
        );
        let mut empty = s.clone();
        empty.pc.runner.clear();
        assert_eq!(empty.images().runner, "");
    }

    #[test]
    fn data_commands_and_placeholders_follow_the_mode() {
        let mut s = settings();
        s.pc.data_live = vec![vec!["live".into()]];
        assert_eq!(s.data_commands(Mode::Dev), s.pc.data_dev.as_slice());
        assert_eq!(s.data_commands(Mode::Live), [vec!["live".to_owned()]]);
        assert!(s.data_commands(Mode::Event).is_empty());
        let bundle = s.bundle_dir("b");
        let vars = s.vars(&bundle);
        let get = |k: &str| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(get("root"), "C:\\IEM\\iemmixer");
        assert_eq!(get("bundle"), bundle.to_string_lossy());
        assert_eq!(get("site"), "C:\\IEM\\iemmixer\\site\\site.toml");
        assert_eq!(
            get("server_config"),
            "C:\\IEM\\iemmixer\\server\\iemmixer.toml"
        );
        assert_eq!(vars.len(), 4);
    }

    #[test]
    fn load_reads_pc_toml_and_the_site_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("site.toml");
        fs::write(&site, SITE).unwrap();
        let pc_path = dir.path().join("pc.toml");
        let pc = PC.replace(
            "site = 'C:\\IEM\\iemmixer\\site\\site.toml'",
            &format!("site = '{}'", site.display()),
        );
        fs::write(&pc_path, &pc).unwrap();
        let s = Settings::load(&pc_path).unwrap();
        assert_eq!(s.pc.site, site);
        assert_eq!(s.guard, settings().guard);
        // A missing file names itself.
        let missing = dir.path().join("none.toml");
        assert!(
            Settings::load(&missing)
                .unwrap_err()
                .starts_with(&missing.display().to_string())
        );
        // A missing site names the site.
        fs::remove_file(&site).unwrap();
        assert!(
            Settings::load(&pc_path)
                .unwrap_err()
                .starts_with(&site.display().to_string())
        );
        fs::write(&pc_path, "root = 1").unwrap();
        assert!(
            Settings::load(&pc_path)
                .unwrap_err()
                .starts_with(&pc_path.display().to_string())
        );
    }
}
