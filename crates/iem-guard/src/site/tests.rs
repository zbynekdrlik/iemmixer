//! The guard's settings against a synthetic site and `pc.toml` (P6), and
//! the fixtures other modules' tests read (`PC`, `SITE`, `settings`).

use iem_win::prefwin::{Kind, Pref};

use super::*;
use crate::plan::PrefFail;

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
hil_tx = [94, 95]
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
    assert_eq!(s.guard.hil_tx, [94, 95]);
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
        (
            PcToml {
                elevated_root: PathBuf::new(),
                ..good.clone()
            },
            "elevated_root must be the elevated tasks' own folder",
        ),
        (
            PcToml {
                elevated_root: good.root.clone(),
                ..good.clone()
            },
            "elevated_root must be the elevated tasks' own folder",
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
    let hil = "pc.toml: engine_args must not name the HIL flags: the guard adds them inside a \
               HIL job only";
    for flag in HIL_FLAGS {
        assert_eq!(
            args(&["run", "--site", "s", "--state-dir", "d", flag]).problems(),
            [hil],
            "{flag}"
        );
    }
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
    assert_eq!(s.engine_argv(&bundle, false, false).unwrap(), base);
    let mut held = base.to_vec();
    held.push("--hold");
    assert_eq!(s.engine_argv(&bundle, true, false).unwrap(), held);
    // Inside a HIL job: the test signals and the fault injection.
    let mut job = held.clone();
    job.extend(["--test-signal", "--fault-injection"]);
    assert_eq!(s.engine_argv(&bundle, true, true).unwrap(), job);
    let mut respawn = base.to_vec();
    respawn.extend(["--test-signal", "--fault-injection"]);
    assert_eq!(s.engine_argv(&bundle, false, true).unwrap(), respawn);
    let mut other = s.clone();
    other.pc.engine_pipe = "iemmixer-engine-2".into();
    assert_eq!(
        other
            .engine_argv(&bundle, false, false)
            .unwrap()
            .last()
            .map(String::as_str),
        Some("iemmixer-engine-2")
    );
    other.pc.engine_args.push("{nope}".into());
    assert_eq!(
        other.engine_argv(&bundle, true, false).unwrap_err(),
        "unknown placeholder {nope} in \"{nope}\""
    );
}

#[test]
fn settings_problems_join_the_tables_and_check_the_app() {
    let mut s = settings();
    s.pc.app_exe = PathBuf::from("C:\\Programs\\App\\other.exe");
    s.card.frames = 48;
    s.guard.app_members = 0;
    assert_eq!(
        s.problems(),
        [
            "[guard] app_members must be above 0",
            "[card] frames must be 32 (I2)",
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
    assert_eq!(s.requests_dir(), root.join("guard").join("tasks"));
    let elevated = PathBuf::from("C:\\ProgramData\\iemmixer");
    assert_eq!(s.pc.elevated_root, elevated);
    assert_eq!(s.results_dir(), elevated.join("tasks").join("out"));
    assert_eq!(
        s.tuning_record(),
        elevated.join("tuning").join("expect.json")
    );
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
