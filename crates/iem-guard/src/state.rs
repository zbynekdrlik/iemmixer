//! The guard's persistent state (S6 design note §5.2).
//!
//! Written atomically before every switch step, so a guard restart resumes or
//! unwinds a switch; a missing or unreadable file starts in `event`.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::bundle::{Pins, Record};
use crate::plan::{Mode, Step};

/// A switch in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Switching {
    pub from: Mode,
    pub to: Mode,
    /// The steps finished so far, in order.
    #[serde(default)]
    pub done: Vec<Step>,
    /// Seconds since the epoch.
    pub started: u64,
}

/// A dev/live entry the interlock refused (activity on stage): the guard
/// tries again every 15 min (design §5.2 step 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterlockRetry {
    pub target: Mode,
    /// The bundle the refused request named (`--build`), if any.
    #[serde(default)]
    pub build: Option<String>,
    pub refusals: u32,
    /// Seconds since the epoch.
    pub next_at: u64,
}

/// A child the guard started. Adoption after a guard restart needs all three
/// to match, so a recycled pid never passes for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Child {
    pub pid: u32,
    pub start_time: u64,
    pub image: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Children {
    pub engine: Option<Child>,
    pub server: Option<Child>,
    pub tray: Option<Child>,
    pub runner: Option<Child>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuardState {
    pub mode: Mode,
    pub switching: Option<Switching>,
    /// When this state was last written, seconds since the epoch.
    pub written_at: u64,
    pub pids: Children,
    /// Installed bundles by SHA.
    pub bundles: BTreeMap<String, Record>,
    pub pins: Pins,
    pub interlock_retry: Option<InterlockRetry>,
    /// The HIL job that began and has not ended (its run id). Kept here so
    /// a guard that hands over to a new exe inside the job (HIL activates
    /// the bundle it tests) or restarts still serves the job (design §7).
    pub job: Option<u64>,
    /// What the last `PrefCheck` found and left alone: the preference not
    /// REAPER's original while the driver module was held (REAPER started
    /// at 32 after a power loss in dev time; #9 2026-09-28). Alarmed once,
    /// named in the status, dropped when a check finds or restores the
    /// original. A fact of the registry: a reset keeps it.
    pub pref_held: Option<String>,
    /// The `at` of the logon task's last result the guard took (G1): each
    /// run of it is taken once, also across guard restarts; a reset keeps it.
    pub logon_seen: Option<String>,
}

/// Whether a starting guard must forget its saved mode: after a reboot, or
/// when the band's system is up without our engine, the PC is in `event`.
pub fn reset_to_event(st: &GuardState, boot_time: u64, reaper_or_app: bool, engine: bool) -> bool {
    boot_time > st.written_at || (reaper_or_app && !engine)
}

impl GuardState {
    /// The mode after [`reset_to_event`]: `event`, no switch in progress, no
    /// pending interlock retry and no HIL job (`pref_held` and `logon_seen`
    /// stay: the next check reads the preference again).
    pub fn reset(&mut self) {
        self.mode = Mode::Event;
        self.switching = None;
        self.interlock_retry = None;
        self.job = None;
    }

    /// Loads the state. A missing file gives the defaults (mode `event`); an
    /// unreadable one gives the defaults and the text of the alarm to raise.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let (st, err) = load_json::<Self>(path);
        (
            st,
            err.map(|e| format!("guard state unreadable ({e}); starting in event")),
        )
    }

    /// Stamps `written_at` with `now` and saves atomically.
    pub fn save(&mut self, path: &Path, now: u64) -> io::Result<()> {
        self.written_at = now;
        save_json::<Self>(path, self)
    }
}

/// Writes `bytes` to `path` atomically: a temp file next to it, flushed to
/// disk, then renamed over it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f = File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path)
}

/// Saves `value` as JSON (pretty: people read it on the PC) with
/// [`write_atomic`].
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    write_atomic(path, &bytes)
}

/// Loads JSON saved by [`save_json`]. A missing file is the default without a
/// complaint; an unreadable or unparsable one is the default and the reason.
pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> (T, Option<String>) {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(value) => (value, None),
            Err(e) => (T::default(), Some(format!("{}: {e}", path.display()))),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => (T::default(), None),
        Err(e) => (T::default(), Some(format!("{}: {e}", path.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::Hil;

    fn sample() -> GuardState {
        let mut bundles = BTreeMap::new();
        bundles.insert(
            "a".repeat(40),
            Record {
                sha: "a".repeat(40),
                branch: "dev".into(),
                run: 4242,
                installed_at: 1_790_000_100,
                hil: Hil::Green,
            },
        );
        GuardState {
            mode: Mode::Dev,
            switching: Some(Switching {
                from: Mode::Dev,
                to: Mode::Event,
                done: vec![Step::JobsCancel, Step::RunnerStop],
                started: 1_790_000_200,
            }),
            written_at: 0,
            pids: Children {
                engine: Some(Child {
                    pid: 101,
                    start_time: 1_790_000_150,
                    image: "iem-engine.exe".into(),
                }),
                ..Children::default()
            },
            bundles,
            pins: Pins {
                current: Some("a".repeat(40)),
                previous: None,
            },
            interlock_retry: Some(InterlockRetry {
                target: Mode::Dev,
                build: None,
                refusals: 2,
                next_at: 1_790_000_900,
            }),
            job: Some(4242),
            pref_held: Some(
                "REAPER runs with the preferred buffer at 32; it is restored at REAPER's next start"
                    .into(),
            ),
            logon_seen: Some("2026-09-28T06:00:00.1234567Z".into()),
        }
    }

    #[test]
    fn the_state_round_trips_and_is_stamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guard-state.json");
        let mut st = sample();
        st.save(&path, 1_790_000_300).unwrap();
        assert_eq!(st.written_at, 1_790_000_300);
        let (back, err) = GuardState::load(&path);
        assert_eq!(err, None);
        assert_eq!(back, st);
        // No temp file is left behind.
        assert!(!dir.path().join("guard-state.tmp").exists());
        // A second save replaces the first.
        st.mode = Mode::Event;
        st.save(&path, 1_790_000_400).unwrap();
        assert_eq!(GuardState::load(&path).0.mode, Mode::Event);
        assert_eq!(GuardState::load(&path).0.written_at, 1_790_000_400);
    }

    #[test]
    fn a_missing_file_starts_in_event_without_an_alarm() {
        let dir = tempfile::tempdir().unwrap();
        let (st, err) = GuardState::load(&dir.path().join("none.json"));
        assert_eq!(st, GuardState::default());
        assert_eq!(st.mode, Mode::Event);
        assert_eq!(err, None);
    }

    #[test]
    fn a_corrupt_file_gives_defaults_and_an_alarm() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guard-state.json");
        fs::write(&path, b"{\"mode\": \"dev\", ").unwrap();
        let (st, err) = GuardState::load(&path);
        assert_eq!(st, GuardState::default());
        let err = err.expect("an alarm");
        assert!(err.starts_with("guard state unreadable ("), "{err}");
        assert!(err.ends_with("); starting in event"), "{err}");
        // A directory where the file should be cannot be read either.
        let (st, err) = GuardState::load(dir.path());
        assert_eq!(st, GuardState::default());
        assert!(err.is_some());
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guard-state.json");
        fs::write(&path, br#"{"mode": "live", "written_at": 5}"#).unwrap();
        let (st, err) = GuardState::load(&path);
        assert_eq!(err, None);
        assert_eq!(
            st,
            GuardState {
                mode: Mode::Live,
                written_at: 5,
                ..GuardState::default()
            }
        );
    }

    #[test]
    fn write_atomic_replaces_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        write_atomic(&path, b"a much longer first version").unwrap();
        write_atomic(&path, b"short").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"short");
        // A missing directory is an error, not a silent success.
        assert!(write_atomic(&dir.path().join("no/such/dir/x.json"), b"x").is_err());
    }

    // The F30 install-site path (win/engine.rs::install_site) writes the new
    // site through write_atomic, so these pin its guarantees (gen1 parity:
    // scripts/test_merge_deployed_config.py, #25). gen2 has no config-merge:
    // the site file is replaced whole, so there is no per-key merge to test.

    #[test]
    fn write_atomic_leaves_no_tmp_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("site.toml");
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        // The temp file write_atomic renamed from must not survive the rename.
        assert!(
            !path.with_extension("tmp").exists(),
            "a .tmp sibling was left behind"
        );
        // Nothing but the target file is left in the directory.
        let count = fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(count, 1, "only the site file should remain, no temp");
    }

    #[test]
    fn a_failed_write_leaves_the_original_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("site.toml");
        let original = b"the original site, kept whole";
        fs::write(&path, original).unwrap();
        // A directory at the temp path blocks write_atomic's temp file, so it
        // fails before the atomic rename that would replace the site: the
        // crash-before-replace guarantee leaves the site untouched.
        fs::create_dir(path.with_extension("tmp")).unwrap();
        assert!(write_atomic(&path, b"a new site that must not land").is_err());
        assert_eq!(
            fs::read(&path).unwrap(),
            original,
            "the original site changed"
        );
    }

    #[test]
    fn the_bytes_are_installed_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("site.toml");
        // Non-UTF-8 bytes with NULs, CRLF and high bytes prove write_atomic
        // copies its input verbatim, never re-encoded or newline-normalised
        // (the mechanism a fresh install-site deploy relies on to copy source).
        let bytes = b"\x00\xff\x01[engine]\r\nchannels = 160\n\xfe\x80";
        write_atomic(&path, bytes).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn an_unchanged_site_is_rewritten_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("site.toml");
        let bytes = b"[engine]\nchannels = 160\n";
        write_atomic(&path, bytes).unwrap();
        let first = fs::read(&path).unwrap();
        // gen2 replaces the whole file (no per-key merge), so installing the
        // same site again is a no-op for the file's bytes: byte-identical, no
        // temp left (install-site still re-enters dev; that is out of scope here).
        write_atomic(&path, bytes).unwrap();
        assert_eq!(fs::read(&path).unwrap(), first);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn a_reboot_resets_the_mode_to_event() {
        let st = GuardState {
            mode: Mode::Dev,
            written_at: 1000,
            ..GuardState::default()
        };
        // Booted after the state was written: a reboot.
        assert!(reset_to_event(&st, 1001, false, false));
        assert!(reset_to_event(&st, 1001, false, true));
        // Booted before: a guard restart. REAPER or the app up without our
        // engine means the band's system is up.
        assert!(reset_to_event(&st, 999, true, false));
        assert!(!reset_to_event(&st, 999, true, true));
        assert!(!reset_to_event(&st, 999, false, false));
        assert!(!reset_to_event(&st, 999, false, true));
        // Booted in the same second the state was written: not a reboot.
        assert!(!reset_to_event(&st, 1000, false, false));
    }

    #[test]
    fn reset_forgets_the_mode_the_switch_and_the_retry() {
        let mut st = sample();
        st.reset();
        assert_eq!(st.mode, Mode::Event);
        assert_eq!(st.switching, None);
        assert_eq!(st.interlock_retry, None);
        assert_eq!(st.job, None, "no HIL job after a reboot");
        // Everything else stays: bundles, pins and children are facts, and
        // so is a preference left alone under a holder (#9 2026-09-28): the
        // next check reads it again.
        let before = sample();
        assert_eq!(st.bundles, before.bundles);
        assert_eq!(st.pins, before.pins);
        assert_eq!(st.pids, before.pids);
        assert_eq!(st.pref_held, before.pref_held);
        assert_eq!(st.logon_seen, before.logon_seen);
    }
}
