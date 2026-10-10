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
//!
//! Its parts: `guard.rs` (`[guard]`), `card.rs` (`[card]`) and `pc_toml.rs`
//! (`pc.toml`); the checks they share and [`Settings`] stay here.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::effects::argv;
use crate::pc::Images;
use crate::plan::Mode;

mod card;
mod guard;
mod pc_toml;

pub use self::card::{CardSite, KindSite, PrefSite};
pub use self::guard::GuardSite;
pub use self::pc_toml::PcToml;

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

#[derive(Debug, Deserialize)]
struct SiteTables {
    guard: Option<GuardSite>,
    card: Option<CardSite>,
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

    /// The shadow imports' history (S8 lane 4): `<root>\shadow\history.jsonl`.
    pub fn shadow_history(&self) -> PathBuf {
        self.pc
            .root
            .join(crate::shadow::DIR)
            .join(crate::shadow::HISTORY)
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
