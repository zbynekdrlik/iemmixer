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
use crate::lifecycle::Lifecycle;
use crate::plan::{Mode, Step};
use crate::switch_log::LastSwitch;

/// A switch in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Switching {
    pub from: Mode,
    pub to: Mode,
    /// The steps finished so far, in order (a step an older guard saved
    /// that this one no longer has is left out: `plan::known_steps`).
    #[serde(default, deserialize_with = "crate::plan::known_steps")]
    pub done: Vec<Step>,
    /// Seconds since the epoch.
    pub started: u64,
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
    /// The record a guard before S8 kept: it promoted every entry's and
    /// every activation's build (S8 design note §3.4), so `current` was its
    /// active bundle and `previous` the one before. Read only for a state
    /// such a guard saved ([`GuardState::active_bundle`],
    /// [`GuardState::way_back_bundle`]). Written as a mirror since S8 lane 2
    /// ([`GuardState::set_active`]: `current` = the active bundle,
    /// `previous` = the way back): an older guard that takes over (a
    /// rollback to an older bundle) runs the active bundle, not the one an
    /// older guard last activated (the PC, 2026-10-10). Never the pin: that
    /// is the lifecycle's, so the mirror cannot promote one early.
    pub pins: Pins,
    /// The active bundle (S8, #11): the build the engine and the server run
    /// in dev and live, and the one `bin\`'s guard came from unless an entry
    /// named another. Set by `activate` and by an entry's build (a prod
    /// entry: the pin) through [`GuardState::set_active`], never the pin
    /// itself (`lifecycle`).
    pub active: Option<String>,
    /// The bundle active before the active one (another build): the way
    /// back, whose Defender exclusions an activation keeps
    /// (`lifecycle::kept`), as `pins.previous` was before S8.
    pub way_back: Option<String>,
    /// Before the cutover, after it, or rolling back (S8 design note §3.1).
    /// A state an older guard saved has none: `Trial`
    /// (`lifecycle::lenient`); a reset keeps it.
    #[serde(deserialize_with = "crate::lifecycle::lenient")]
    pub lifecycle: Lifecycle,
    /// The cutover in progress (S8 lane 2, design note §3.2): saved before
    /// each of its steps, dropped once it is done or unwound (or kept with
    /// the steps whose undo failed). A starting guard that finds one unwinds
    /// it before anything else (`daemon::cutover::recover`); a reset keeps
    /// it. An older guard drops it on its next save.
    pub cutover: Option<crate::cutover::Run>,
    /// The rollback in progress (S8 lane 3, design note §3.3): saved with
    /// `RollingBack` before its first step and after each, dropped when it
    /// ends in trial (or kept with what is left). A starting guard that
    /// finds one continues it (`daemon::rollback::resume`); a reset keeps
    /// it. An older guard drops it on its next save, so `activate` is
    /// refused while it is kept.
    pub rollback: Option<crate::rollback::Run>,
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
    /// The last switch that ended (S7 design note §5); a reset keeps it. A
    /// record this guard cannot read loads as none, never as an unreadable
    /// state (`switch_log::lenient`).
    #[serde(deserialize_with = "crate::switch_log::lenient")]
    pub last_switch: Option<LastSwitch>,
}

/// Whether a starting guard must forget its saved mode: after a reboot, or
/// when the band's system is up without our engine, the PC is in `event`.
pub fn reset_to_event(st: &GuardState, boot_time: u64, reaper_or_app: bool, engine: bool) -> bool {
    boot_time > st.written_at || (reaper_or_app && !engine)
}

impl GuardState {
    /// The active bundle: [`GuardState::active`], or for a state an older
    /// guard saved, its `pins.current`.
    pub fn active_bundle(&self) -> Option<&str> {
        self.active.as_deref().or(self.pins.current.as_deref())
    }

    /// The way back: [`GuardState::way_back`] once this guard set an active
    /// bundle, else (a state an older guard saved) its `pins.previous`.
    pub fn way_back_bundle(&self) -> Option<&str> {
        match self.active {
            Some(_) => self.way_back.as_deref(),
            None => self.pins.previous.as_deref(),
        }
    }

    /// Makes `sha` the active bundle; the one active before it becomes the
    /// way back when it is another build, else the way back stays (the rule
    /// `Pins::promote` had for the active bundle before S8). The legacy
    /// `pins` mirror both, for an older guard that takes over.
    pub fn set_active(&mut self, sha: &str) {
        let before = self.active_bundle().map(str::to_owned);
        let kept = self.way_back_bundle().map(str::to_owned);
        self.way_back = if before.as_deref() == Some(sha) {
            kept
        } else {
            before
        };
        self.active = Some(sha.to_owned());
        self.pins = Pins {
            current: self.active.clone(),
            previous: self.way_back.clone(),
        };
    }

    /// The mode after [`reset_to_event`]: `event`, no switch in progress and
    /// no HIL job (`pref_held` and `logon_seen` stay: the next check reads
    /// the preference again; `last_switch` stays until the next switch,
    /// the start's checks included, ends).
    pub fn reset(&mut self) {
        self.mode = Mode::Event;
        self.switching = None;
        self.job = None;
    }

    /// Loads the state. A missing file gives the defaults (mode `event`); an
    /// unreadable one gives the defaults and the text of the alarm to raise.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let (st, err) = load_json::<Self>(path);
        let err = match err {
            Some(e) => Some(format!("guard state unreadable ({e}); starting in event")),
            // A lifecycle this guard cannot read loads as trial: alarmed.
            None => crate::lifecycle::unreadable_in(path),
        };
        (st, err)
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
    use crate::switch_log::{StepTime, SwitchOutcome};

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
            // S8 (#11): the lifecycle's own round trips are in
            // `lifecycle/tests.rs`.
            active: None,
            way_back: None,
            lifecycle: Lifecycle::Trial,
            cutover: Some(crate::cutover::Run {
                build: "a".repeat(40),
                since: 1_790_000_250,
                begun: vec![crate::cutover::CutStep::Import],
            }),
            rollback: Some(crate::rollback::Run {
                pin: "a".repeat(40),
                since: Some(1_790_000_250),
                at: 1_790_000_300,
                done: vec![crate::rollback::RollStep::Stop],
                exported: false,
                on_export: false,
            }),
            job: Some(4242),
            pref_held: Some(
                "REAPER runs with the preferred buffer at 32; it is restored at REAPER's next start"
                    .into(),
            ),
            logon_seen: Some("2026-09-28T06:00:00.1234567Z".into()),
            last_switch: Some(LastSwitch::new(
                &Switching {
                    from: Mode::Event,
                    to: Mode::Dev,
                    done: Vec::new(),
                    started: 1_790_000_150,
                },
                Mode::Dev,
                SwitchOutcome::Done,
                1_790_000_170,
                vec![
                    StepTime {
                        step: Step::ReaperSaveQuit,
                        ms: 8000,
                    },
                    StepTime {
                        step: Step::EngineArm,
                        ms: 10_500,
                    },
                ],
            )),
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

    /// #38: an older guard's state, saved with a pending interlock retry
    /// and an interlock step done, still loads; both are left out (no stage
    /// is read any more), the rest is kept.
    #[test]
    fn an_older_guards_interlock_retry_and_step_are_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guard-state.json");
        let old = r#"{"mode": "dev", "written_at": 7,
            "switching": {"from": "event", "to": "dev", "started": 6,
                          "done": ["precheck", "interlock", "app_stop"]},
            "interlock_retry": {"target": "dev", "refusals": 2, "next_at": 9},
            "job": 42}"#;
        fs::write(&path, old).unwrap();
        let (st, err) = GuardState::load(&path);
        assert_eq!(err, None);
        assert_eq!(
            st,
            GuardState {
                mode: Mode::Dev,
                written_at: 7,
                switching: Some(Switching {
                    from: Mode::Event,
                    to: Mode::Dev,
                    done: vec![Step::Precheck, Step::AppStop],
                    started: 6,
                }),
                job: Some(42),
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
    fn reset_forgets_the_mode_the_switch_and_the_job() {
        let mut st = sample();
        st.reset();
        assert_eq!(st.mode, Mode::Event);
        assert_eq!(st.switching, None);
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
        // A cutover cut off by the reboot is the start's to unwind, a
        // rollback the start's to continue.
        assert_eq!(st.cutover, before.cutover);
        assert_eq!(st.rollback, before.rollback);
    }

    /// S7 (#10): the last switch is saved with the state, and a reset (a
    /// reboot, or the band's system up) leaves it as it is; the start's
    /// checks that follow a reset are a switch of their own and replace it
    /// with their record when they end (`daemon::start`).
    #[test]
    fn the_last_switch_round_trips_and_a_reset_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guard-state.json");
        let mut st = sample();
        assert!(st.last_switch.is_some());
        st.save(&path, 1_790_000_300).unwrap();
        let (back, err) = GuardState::load(&path);
        assert_eq!(err, None);
        assert_eq!(back.last_switch, st.last_switch);
        st.reset();
        assert_eq!(st.last_switch, sample().last_switch);
    }

    #[test]
    fn an_older_guards_state_without_a_last_switch_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guard-state.json");
        fs::write(&path, br#"{"mode": "dev", "written_at": 7, "job": 42}"#).unwrap();
        let (st, err) = GuardState::load(&path);
        assert_eq!(err, None);
        assert_eq!(
            (st.mode, st.written_at, st.job, st.last_switch),
            (Mode::Dev, 7, Some(42), None)
        );
    }

    /// S7 part 3 (#10): an unwind's record names the entry it unwinds and
    /// keeps it across a save; an older guard's record, without the key,
    /// loads whole, with none.
    #[test]
    fn an_unwound_record_round_trips_and_an_older_one_loads_without_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guard-state.json");
        let mut st = sample();
        st.last_switch = st.last_switch.map(|r| LastSwitch {
            unwound: Some(Mode::Live),
            ..r
        });
        st.save(&path, 1_790_000_300).unwrap();
        let (back, err) = GuardState::load(&path);
        assert_eq!(err, None);
        assert_eq!(back.last_switch, st.last_switch);
        let mut older = serde_json::to_value(&st).unwrap();
        let record = older["last_switch"].as_object_mut().unwrap();
        assert_eq!(record.remove("unwound"), Some(serde_json::json!("live")));
        fs::write(&path, serde_json::to_vec(&older).unwrap()).unwrap();
        let (back, err) = GuardState::load(&path);
        assert_eq!(err, None);
        assert_eq!(back.last_switch, sample().last_switch);
    }

    /// A record this guard cannot read (a newer guard's shape) is dropped:
    /// the state loads, no alarm, nothing else is lost.
    #[test]
    fn a_last_switch_this_guard_cannot_read_is_dropped_not_the_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guard-state.json");
        for body in [
            &br#"{"mode":"dev","last_switch":{"from":"dev"}}"#[..],
            &br#"{"mode":"dev","last_switch":null}"#[..],
            &br#"{"mode":"dev","last_switch":"later"}"#[..],
        ] {
            fs::write(&path, body).unwrap();
            let (st, err) = GuardState::load(&path);
            assert_eq!(err, None, "{body:?}");
            assert_eq!(
                st,
                GuardState {
                    mode: Mode::Dev,
                    ..GuardState::default()
                },
                "{body:?}"
            );
        }
    }
}
