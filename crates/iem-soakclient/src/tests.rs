//! The pure core's tests (`lib.rs`): the arguments, the build check, the
//! URLs, the gap clock, the event classes and the summary.

use super::*;

fn args(list: &[&str]) -> Result<Args, String> {
    let list: Vec<String> = list.iter().map(|s| (*s).to_owned()).collect();
    parse_args(&list)
}

/// A commit the server must name (40 lower-case hex digits), and the
/// short hash it names it by.
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const SHORT: &str = "0123456";

/// The four required flags and nothing else.
const MINIMAL: [&str; 8] = [
    "--member",
    "member9",
    "--seconds",
    "600",
    "--out",
    "s.json",
    "--expect-build",
    COMMIT,
];

/// [`MINIMAL`] followed by `extra`.
fn with(extra: &[&str]) -> Result<Args, String> {
    let mut list = MINIMAL.to_vec();
    list.extend_from_slice(extra);
    args(&list)
}

/// [`MINIMAL`] with the value of `flag` replaced by `v`.
fn replaced(flag: &str, v: &str) -> Result<Args, String> {
    let mut list = MINIMAL.to_vec();
    let at = list.iter().position(|a| *a == flag).unwrap();
    list[at + 1] = v;
    args(&list)
}

/// [`MINIMAL`] without `flag` and its value.
fn without(flag: &str) -> Result<Args, String> {
    let mut list = MINIMAL.to_vec();
    let at = list.iter().position(|a| *a == flag).unwrap();
    list.remove(at);
    list.remove(at);
    args(&list)
}

fn at(t0: Instant, ms: u64) -> Instant {
    t0 + Duration::from_millis(ms)
}

fn count(gaps: u64, max_gap_ms: u64) -> GapCount {
    GapCount { gaps, max_gap_ms }
}

/// `origin` of [`MINIMAL`] (not `--direct`) for the server's `lan_url`.
fn lan(url: Option<&str>) -> Result<String, Reason> {
    origin(&args(&MINIMAL).unwrap(), url)
}

#[test]
fn the_arguments_and_their_defaults() {
    assert_eq!(
        args(&MINIMAL).unwrap(),
        Args {
            base: "http://127.0.0.1".to_owned(),
            direct: false,
            member: "member9".to_owned(),
            seconds: 600,
            out: PathBuf::from("s.json"),
            expect_build: COMMIT.to_owned(),
            cpu_sets: Vec::new(),
        }
    );
    // Every flag, in any order; the base is kept as its origin.
    let all = args(&[
        "--expect-build",
        COMMIT,
        "--out",
        "s.json",
        "--direct",
        "--seconds",
        "1",
        "--base",
        "http://127.0.0.1:8080/",
        "--member",
        "member9",
    ])
    .unwrap();
    assert_eq!(all.base, "http://127.0.0.1:8080");
    assert!(all.direct);
    assert_eq!(all.member, "member9");
    assert_eq!(all.seconds, 1);
    assert_eq!(all.out, PathBuf::from("s.json"));
    assert_eq!(all.expect_build, COMMIT);
    assert_eq!(replaced("--seconds", "36000").unwrap().seconds, MAX_SECONDS);
}

#[test]
fn every_bad_argument_is_a_usage_error() {
    for flag in ["--member", "--seconds", "--out", "--expect-build"] {
        assert!(without(flag).unwrap_err().contains(flag), "{flag}");
    }
    for seconds in ["0", "36001", "x", "-1", ""] {
        assert!(replaced("--seconds", seconds).is_err(), "{seconds:?}");
    }
    assert!(replaced("--out", "").is_err());
    // A flag without its value, and a flag taken as a value.
    assert!(args(&["--member", "member9", "--seconds", "600", "--out"]).is_err());
    assert!(replaced("--member", "--direct").is_err());
    // A flag given twice.
    assert!(with(&["--member", "member8"]).is_err());
    assert!(with(&["--seconds", "600"]).is_err());
    assert!(with(&["--bogus"]).unwrap_err().contains("unknown argument"));
    assert!(with(&["positional"]).is_err());
    // The base is plain HTTP with a host.
    assert!(with(&["--base", "https://mixer.example.org"]).is_err());
    assert!(with(&["--base", "http://"]).is_err());
    assert!(with(&["--cpu-sets", "256,x"]).is_err());
    assert!(with(&["--cpu-sets", ""]).is_err());
    assert!(with(&["--expect-build", COMMIT]).is_err(), "given twice");
}

#[test]
fn the_expected_build_is_a_commits_40_lower_case_hex_digits() {
    let other = "fedcba9876543210fedcba9876543210fedcba98";
    assert_eq!(
        replaced("--expect-build", other).unwrap().expect_build,
        other
    );
    let upper = COMMIT.to_ascii_uppercase();
    let (short, long) = (&COMMIT[..39], format!("{COMMIT}0"));
    let not_hex = format!("{}g", &COMMIT[..39]);
    let spaced = format!(" {}", &COMMIT[..39]);
    for bad in [
        "",
        SHORT,
        short,
        long.as_str(),
        upper.as_str(),
        not_hex.as_str(),
        spaced.as_str(),
    ] {
        let e = replaced("--expect-build", bad).unwrap_err();
        assert!(e.contains("--expect-build"), "{bad:?}: {e}");
        assert!(bad.is_empty() || !e.contains(bad), "never echoed: {e}");
    }
}

#[test]
fn the_version_names_the_build_by_a_prefix_of_at_least_7_hex_digits() {
    let names = |git_hash: &str| {
        let answer = serde_json::json!({"version": "2.0.0-dev.18", "git_hash": git_hash,
            "branch": "dev", "build_time": "0", "deployed_at": "x", "full_version": "y"});
        names_build(&answer.to_string(), COMMIT)
    };
    // git's short hash, a longer one, the whole commit.
    for good in [SHORT, &COMMIT[..12], COMMIT] {
        assert!(names(good), "{good}");
    }
    // Too short, another commit, a build without its hash, a hash with
    // anything around it.
    let other = format!("{}0", &COMMIT[..6]);
    let upper = COMMIT[..12].to_ascii_uppercase();
    for bad in [
        &SHORT[..6],
        "",
        "fedcba9",
        other.as_str(),
        "unknown",
        upper.as_str(),
        " 0123456",
        "0123456 ",
    ] {
        assert!(!names(bad), "{bad:?}");
    }
    // An answer that is not the server's version at all.
    for answer in [
        "",
        "not json",
        "{}",
        r#"{"version":"2.0.0"}"#,
        r#"{"git_hash":123456789}"#,
        r#"{"git_hash":null}"#,
        r#"["0123456"]"#,
    ] {
        assert!(!names_build(answer, COMMIT), "{answer}");
    }
}

#[test]
fn the_member_is_1_to_64_letters_digits_underscores_or_hyphens() {
    let longest = "m".repeat(64);
    for good in ["m", "member9", "Member_9-x", "_", "-", longest.as_str()] {
        assert_eq!(replaced("--member", good).unwrap().member, good);
    }
    let too_long = "m".repeat(65);
    for bad in [
        "",
        "member.9",
        "member 9",
        "member/9",
        "m?",
        too_long.as_str(),
    ] {
        assert!(replaced("--member", bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_pin_never_comes_from_argv() {
    for extra in [&["--pin", "4321"][..], &["--pin=4321"][..]] {
        let e = with(extra).unwrap_err();
        assert!(e.contains(PIN_ENV), "{e}");
        assert!(!e.contains("4321"), "the value is never echoed: {e}");
    }
    // Another unknown flag gets the plain refusal, its value never echoed.
    let e = with(&["--bogus=4321"]).unwrap_err();
    assert!(!e.contains(PIN_ENV), "{e}");
    assert!(!e.contains("4321"), "{e}");
}

#[test]
fn cpu_set_lists_are_comma_separated_ids() {
    assert_eq!(parse_cpu_sets("256,257"), Some(vec![256, 257]));
    assert_eq!(parse_cpu_sets("256"), Some(vec![256]));
    for bad in ["", "256,", ",256", "256,x", "256 257", "-1", "4294967296"] {
        assert_eq!(parse_cpu_sets(bad), None, "{bad:?}");
    }
}

#[test]
fn cpu_sets_are_windows_only() {
    let parsed = with(&["--cpu-sets", "256,257"]);
    if cfg!(windows) {
        assert_eq!(parsed.unwrap().cpu_sets, vec![256, 257]);
    } else {
        assert!(parsed.unwrap_err().contains("Windows only"));
    }
    // A bad list is refused on every OS.
    let bad = with(&["--cpu-sets", "256,x"]).unwrap_err();
    assert!(bad.contains("CPU Set ids"), "{bad}");
}

#[test]
fn the_pin_comes_from_the_environment_as_4_to_12_digits() {
    for good in ["1234", "123456789012"] {
        assert_eq!(pin_from(Some(good.to_owned())), Ok(good.to_owned()));
    }
}

#[test]
fn a_bad_pin_is_refused_without_echoing_it() {
    assert!(pin_from(None).unwrap_err().contains(PIN_ENV));
    for bad in ["", "123", "1234567890123", "12a4", "12 34", "+123", "abcd"] {
        let e = pin_from(Some(bad.to_owned())).unwrap_err();
        assert!(e.contains(PIN_ENV), "{e}");
        assert!(bad.is_empty() || !e.contains(bad), "never echoed: {e}");
    }
}

#[test]
fn the_origin_is_the_named_lan_url_or_with_direct_the_base() {
    let ok = |s: &str| Ok(s.to_owned());
    assert_eq!(lan(Some("http://10.0.0.10/")), ok("http://10.0.0.10"));
    assert_eq!(
        lan(Some("http://10.0.0.10:8080")),
        ok("http://10.0.0.10:8080")
    );
    assert_eq!(lan(Some("HTTP://10.0.0.10/x?y#z")), ok("http://10.0.0.10"));
    assert_eq!(lan(None), Err(Reason::SiteUnreadable));
    for url in [
        "https://mixer.example.org",
        "http://",
        "http:///x",
        "10.0.0.10",
        "",
    ] {
        assert_eq!(lan(Some(url)), Err(Reason::NotHttp), "{url:?}");
    }
    let direct = with(&["--direct"]).unwrap();
    assert_eq!(
        origin(&direct, Some("http://10.0.0.10")),
        ok("http://127.0.0.1")
    );
    assert_eq!(origin(&direct, None), ok("http://127.0.0.1"));
}

#[test]
fn the_socket_urls_and_the_listen_start() {
    let audio = ws_url("http://10.0.0.10:8080", "/ws/audio?token=t");
    assert_eq!(audio, "ws://10.0.0.10:8080/ws/audio?token=t");
    assert_eq!(mixer_path("member9", "t"), "/ws/member9?token=t&proto=2");
    assert_eq!(listen_path("t"), "/ws/audio?token=t");
    let start = listen_start("member9");
    assert_eq!(start, r#"{"cmd":"ListenStart","member_id":"member9"}"#);
}

#[test]
fn a_wait_of_exactly_60_ms_is_no_gap_and_61_is() {
    let t0 = Instant::now();
    let mut g = Gaps::default();
    g.frame(t0);
    g.frame(at(t0, 60));
    assert_eq!(g.end(t0, at(t0, 60)), count(0, 60));
    g.frame(at(t0, 121));
    assert_eq!(g.end(t0, at(t0, 121)), count(1, 61));
    g.frame(at(t0, 141));
    assert_eq!(g.end(t0, at(t0, 141)), count(1, 61));
}

#[test]
fn the_end_counts_the_wait_since_the_last_frame() {
    let t0 = Instant::now();
    let mut g = Gaps::default();
    g.frame(t0);
    g.frame(at(t0, 100));
    assert_eq!(g.end(t0, at(t0, 160)), count(1, 100));
    assert_eq!(g.end(t0, at(t0, 161)), count(2, 100));
    assert_eq!(g.end(t0, at(t0, 400)), count(2, 300));
    // `end` only reads: the frame that ends the wait counts it once.
    g.frame(at(t0, 400));
    assert_eq!(g.end(t0, at(t0, 400)), count(2, 300));
}

#[test]
fn a_run_without_a_frame_is_one_gap_as_long_as_the_run() {
    let t0 = Instant::now();
    let g = Gaps::default();
    assert_eq!(g.end(t0, at(t0, 3_000)), count(1, 3_000));
    assert_eq!(g.end(t0, at(t0, 20)), count(1, 20));
    assert_eq!(g.expected(at(t0, 3_000)), 0);
    assert_eq!(g.first(), None);
}

#[test]
fn a_long_wait_is_one_gap_and_the_first_frame_stays() {
    // The stream stalled after the frame at 0 ms and its next frame came
    // at 500 ms: one 500 ms gap, and the first frame stays the first.
    let t0 = Instant::now();
    let mut g = Gaps::default();
    g.frame(t0);
    g.frame(at(t0, 500));
    g.frame(at(t0, 520));
    assert_eq!(g.end(t0, at(t0, 520)), count(1, 500));
    assert_eq!(g.first(), Some(t0));
    assert_eq!(g.expected(at(t0, 520)), 27);
}

#[test]
fn expected_frames_count_one_per_20_ms_from_the_first_frame() {
    let t0 = Instant::now();
    let mut g = Gaps::default();
    assert_eq!(g.expected(at(t0, 1_000)), 0);
    g.frame(at(t0, 100));
    assert_eq!(g.first(), Some(at(t0, 100)));
    assert_eq!(g.expected(at(t0, 100)), 1);
    assert_eq!(g.expected(at(t0, 119)), 1);
    assert_eq!(g.expected(at(t0, 120)), 2);
    assert_eq!(g.expected(at(t0, 1_100)), 51);
    assert_eq!(g.expected(at(t0, 100 + 8 * 3_600_000)), 1_440_001);
}

#[test]
fn events_are_meters_audio_status_or_other() {
    let meters = r#"{"event":"Meters","data":{"meters":{"mic1":[0.1,0.1]}}}"#;
    assert_eq!(classify(meters), Event::Meters);
    for (text, status) in [
        (
            r#"{"event":"AudioStatus","data":{"status":"no_source"}}"#,
            "no_source",
        ),
        (
            r#"{"event":"AudioStatus","data":{"status":"listening","target":"member9"}}"#,
            "listening",
        ),
    ] {
        assert_eq!(classify(text), Event::AudioStatus(status.to_owned()));
    }
    for other in [
        r#"{"event":"Hello","data":{"proto":2,"build":"local","min_client_proto":2}}"#,
        r#"{"event":"State","data":{"channels":[],"connected":true}}"#,
        r#"{"event":"AudioStatus","data":{}}"#,
        r#"{"event":"meters","data":{"meters":{}}}"#,
        r#"{"data":{"status":"no_source"}}"#,
        "not json",
        "",
    ] {
        assert_eq!(classify(other), Event::Other, "{other}");
    }
}

#[test]
fn a_meters_frame_of_any_data_shape_is_a_meter_frame() {
    // The tag decides: a field of another shape in a Meters frame's
    // data (here a numeric `status`) never makes the frame unreadable.
    for meters in [
        r#"{"event":"Meters","data":{"status":1,"meters":{}}}"#,
        r#"{"event":"Meters","data":[1]}"#,
        r#"{"event":"Meters"}"#,
    ] {
        assert_eq!(classify(meters), Event::Meters, "{meters}");
    }
    let numeric = r#"{"event":"AudioStatus","data":{"status":1}}"#;
    assert_eq!(classify(numeric), Event::Other);
}

#[test]
fn the_summary_names_its_error_by_the_reason_code() {
    use Reason::*;
    for reason in [
        SiteUnreadable,
        NotHttp,
        WrongServer,
        LoginRefused,
        NotEngineer,
        ServerGone,
        ConnectionLost,
        CpuSets,
    ] {
        let summary = Summary {
            error: Some(reason),
            ..Summary::default()
        };
        let v = serde_json::to_value(&summary).unwrap();
        assert_eq!(v["error"], reason.code());
        let back: Summary = serde_json::from_value(v).unwrap();
        assert_eq!(back.error, Some(reason));
    }
    // Any other text is no reason (P6: the summary carries no free text).
    let free = r#"{"error":"http://10.0.0.10 refused"}"#;
    assert!(serde_json::from_str::<Summary>(free).is_err());
}

#[test]
fn the_wire_forms_are_the_servers_own() {
    use iem_core::{ClientMsg, ServerMsg, tunnel::SiteLinks};
    // The mixer socket's protocol is one the server serves.
    let served = iem_core::MIN_CLIENT_PROTO..=iem_core::UI_PROTO;
    assert!(served.contains(&UI_PROTO));
    let start = ClientMsg::ListenStart {
        member_id: "member9".to_owned(),
    };
    assert_eq!(
        listen_start("member9"),
        serde_json::to_string(&start).unwrap()
    );
    let wire = |m: &ServerMsg| serde_json::to_string(m).unwrap();
    let meters = ServerMsg::Meters {
        meters: [("mic1".to_owned(), [0.1, 0.1])].into(),
    };
    assert_eq!(classify(&wire(&meters)), Event::Meters);
    let status = ServerMsg::AudioStatus {
        status: "no_source".to_owned(),
        target: Some("member9".to_owned()),
    };
    assert_eq!(
        classify(&wire(&status)),
        Event::AudioStatus("no_source".to_owned())
    );
    let hello = ServerMsg::Hello {
        proto: iem_core::UI_PROTO,
        build: "local".to_owned(),
        min_client_proto: iem_core::MIN_CLIENT_PROTO,
    };
    assert_eq!(classify(&wire(&hello)), Event::Other);
    // `/api/site`'s body names the LAN URL `lan_url`.
    let site = serde_json::to_value(SiteLinks {
        lan_url: Some("http://10.0.0.10/".to_owned()),
        public_host: Some("mixer.example.org".to_owned()),
    })
    .unwrap();
    assert_eq!(
        lan(site["lan_url"].as_str()),
        Ok("http://10.0.0.10".to_owned())
    );
}

#[test]
fn reasons_are_fixed_codes() {
    use Reason::*;
    let codes = [
        SiteUnreadable,
        NotHttp,
        WrongServer,
        LoginRefused,
        NotEngineer,
        ServerGone,
        ConnectionLost,
        CpuSets,
    ];
    let codes = codes.map(Reason::code).join(" ");
    assert_eq!(
        codes,
        "site-unreadable not-http wrong-server login-refused not-engineer server-gone \
         connection-lost cpu-sets"
    );
}

#[test]
fn the_summary_has_its_schema_and_no_site_value() {
    let v = serde_json::to_value(Summary::default()).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys.join(" "),
        "build complete decode_errors error expected_frames first_frame_ms frames gaps \
         max_gap_ms meter_frames no_source reconnects schema seconds"
    );
    // Numbers and reason codes only (P6).
    for site in ["member", "url", "pin", "host", "token"] {
        assert!(v.get(site).is_none(), "{site}");
    }
    assert_eq!(v["schema"], 1);
    assert_eq!(v["build"], BUILD);
    assert_eq!(v["complete"], false);
    assert!(v["error"].is_null());
    assert!(v["first_frame_ms"].is_null());
    // A missing field reads as its default (additive, as the guard's records).
    let empty: Summary = serde_json::from_str("{}").unwrap();
    assert_eq!(empty, Summary::default());
}

#[test]
fn write_summary_replaces_the_file_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("soak.json");
    let read =
        |p: &Path| -> Summary { serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap() };
    let first = Summary {
        frames: 1,
        ..Summary::default()
    };
    write_summary(&out, &first).unwrap();
    assert_eq!(read(&out), first);
    let second = Summary {
        frames: 2,
        complete: true,
        error: Some(Reason::ServerGone),
        ..Summary::default()
    };
    // A reader holding the old file keeps reading it whole: the new
    // summary replaces the file, it never rewrites it in place.
    let mut held = std::fs::File::open(&out).unwrap();
    write_summary(&out, &second).unwrap();
    let mut old = String::new();
    std::io::Read::read_to_string(&mut held, &mut old).unwrap();
    assert_eq!(serde_json::from_str::<Summary>(&old).unwrap(), first);
    assert_eq!(read(&out), second);
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["soak.json"], "no .tmp is left behind");
    // A file that cannot be written is an error, never a silent skip.
    let missing = dir.path().join("missing").join("soak.json");
    assert!(write_summary(&missing, &first).is_err());
}
