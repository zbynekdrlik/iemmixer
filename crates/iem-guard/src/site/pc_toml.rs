//! `pc.toml`: where things are on this PC (S6 plan Task 9).

use std::path::PathBuf;

use serde::Deserialize;

use super::HIL_FLAGS;

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
/// arguments may name `{root}`, `{bundle}`, `{site}` and `{server_config}`;
/// `shadow` also `{project}`.
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
    /// The report-only shadow import at each entry from event (S8 lane 4,
    /// `crate::shadow`): `iem-migrate shadow --rpp {project} --aliases …
    /// --site {site} --state-dir …`, in the active bundle's folder; it
    /// writes nothing, the guard appends its report to the history. Empty
    /// (no key): no shadow.
    #[serde(default)]
    pub shadow: Vec<String>,
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
        // Only the report-only subcommand: an `import` here would write the
        // engine's state at every entry, after the cutover too.
        check(
            self.shadow.is_empty() || self.shadow.get(1).is_some_and(|a| a == "shadow"),
            "shadow must be `<iem-migrate> shadow …` (report-only, never import)",
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
