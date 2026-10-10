use super::*;

fn args(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

#[test]
fn run_arguments_parse() {
    let cmd = parse_args(&args(
        "run --site s.toml --state-dir d --pipe p --block 64 --sine 440 --test-signal --fault-injection",
    ))
    .unwrap();
    let Command::Run(cfg) = cmd else {
        panic!("{cmd:?}")
    };
    assert_eq!(cfg.site, PathBuf::from("s.toml"));
    assert_eq!(cfg.state_dir, PathBuf::from("d"));
    assert_eq!(cfg.pipe, "p");
    assert_eq!(cfg.block, 64);
    assert_eq!(
        cfg.signal,
        InputSignal::Sine {
            hz: 440.0,
            amp: 0.1
        }
    );
    assert!(cfg.flags.test_signal && cfg.flags.fault_injection);
    assert_eq!(cfg.solo_grace, Duration::from_secs(10));
    let plain = parse_args(&args("run --site s --state-dir d --pipe p")).unwrap();
    let Command::Run(cfg) = plain else {
        panic!("{plain:?}")
    };
    assert_eq!(
        (cfg.block, cfg.signal, cfg.flags),
        (32, InputSignal::Silence, Flags::default())
    );
}

#[test]
fn the_backend_the_hold_and_the_s6_commands_parse() {
    let Command::Run(cfg) = parse_args(&args(
        "run --site s --state-dir d --pipe p --backend asio --hold",
    ))
    .unwrap() else {
        panic!()
    };
    assert_eq!(
        (cfg.backend, cfg.hold, cfg.block, cfg.signal),
        (Backend::Asio, true, 32, InputSignal::Silence)
    );
    let Command::Run(cfg) = parse_args(&args(
        "run --site s --state-dir d --pipe p --backend nullrt --block 64 --sine 440",
    ))
    .unwrap() else {
        panic!()
    };
    assert_eq!(
        (cfg.backend, cfg.hold, cfg.block),
        (Backend::NullRt, false, 64)
    );
    let Command::Run(cfg) = parse_args(&args("run --site s --state-dir d --pipe p")).unwrap()
    else {
        panic!()
    };
    assert_eq!((cfg.backend, cfg.hold), (Backend::NullRt, false));
    assert_eq!(
        parse_args(&args("check-site --site s.toml")).unwrap(),
        Command::CheckSite("s.toml".into())
    );
    for (line, want) in [
        (
            "run --site s --state-dir d --pipe p --backend alsa",
            "--backend",
        ),
        (
            "run --site s --state-dir d --pipe p --backend",
            "needs a value",
        ),
        (
            "run --site s --state-dir d --pipe p --backend asio --block 32",
            "nullrt backend",
        ),
        (
            "run --site s --state-dir d --pipe p --backend asio --sine 440",
            "nullrt backend",
        ),
        (
            "run --site s --state-dir d --pipe p --seconds 5",
            "--seconds",
        ),
        (
            "run --site s --state-dir d --pipe p --stop-file x",
            "--stop-file",
        ),
        ("check-site", "--site"),
    ] {
        let e = parse_args(&args(line)).unwrap_err();
        assert!(e.contains(want), "{line}: {e}");
    }
}

#[test]
fn the_memory_lock_is_due_once_five_seconds_in() {
    assert_eq!((LOCK_AFTER, LOCK_EXTRA_MB), (Duration::from_secs(5), 64));
    let t0 = Instant::now();
    assert!(!lock_due(t0, t0, false));
    assert!(!lock_due(
        t0,
        t0 + LOCK_AFTER - Duration::from_millis(1),
        false
    ));
    assert!(lock_due(t0, t0 + LOCK_AFTER, false));
    assert!(lock_due(t0, t0 + Duration::from_secs(60), false));
    assert!(!lock_due(t0, t0 + LOCK_AFTER, true), "once");
    assert!(!lock_due(t0 + LOCK_AFTER, t0, false));
}

/// The test site with `edit` applied, as a file in `dir`.
fn edited_site(dir: &Path, name: &str, edit: impl FnOnce(String) -> String) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, edit(crate::test_support::test_site_text())).unwrap();
    path
}

/// The test site without its `[card]` table (the keys go to a table
/// nothing reads).
fn without_card(text: String) -> String {
    text.replace("[card]", "[spare]")
}

#[test]
fn check_site_names_the_topology_and_the_card() {
    let summary = check_site(&crate::test_support::test_site_path()).unwrap();
    assert_eq!(
        summary,
        SiteSummary {
            topology: crate::test_support::test_site().hash,
            inputs: 24,
            groups: 1,
            mixes: 11,
            card: true,
        }
    );
    let json = serde_json::to_value(&summary).unwrap();
    assert_eq!(json["topology"].as_str().map(str::len), Some(64));
    assert_eq!(
        (json["inputs"].as_u64(), json["card"].as_bool()),
        (Some(24), Some(true))
    );
    let dir = tempfile::tempdir().unwrap();
    let bare = edited_site(dir.path(), "bare.toml", without_card);
    assert!(!check_site(&bare).unwrap().card);
    let bad_card = edited_site(dir.path(), "card.toml", |t| {
        t.replace("frames = 32", "frames = 64")
    });
    assert!(matches!(
        check_site(&bad_card),
        Err(EngineError::Site(SiteError::CardFrames(64)))
    ));
    let bad_map = edited_site(dir.path(), "map.toml", |t| {
        t.replace("tx = [93]", "tx = [92]")
    });
    assert!(matches!(
        check_site(&bad_map),
        Err(EngineError::Site(SiteError::ChannelReused { ch: 92 }))
    ));
    assert!(matches!(
        check_site(&dir.path().join("missing.toml")),
        Err(EngineError::Site(SiteError::Io(_)))
    ));
}

/// #38 (owner, 2026-10-06): nothing measures the stage before a switch
/// (only the owner's signal decides, and other devices on the Dante
/// network feed the card's inputs). `interlock` is no command, and
/// check-site reads no stage: an older site's `[activity]` table (the
/// server's band-activity alarm, removed too) is ignored.
#[test]
fn there_is_no_interlock_and_check_site_reads_no_stage() {
    let e = parse_args(&args("interlock --site s")).unwrap_err();
    assert!(e.contains("unknown command"), "{e}");
    let dir = tempfile::tempdir().unwrap();
    let ghost = edited_site(dir.path(), "ghost.toml", |t| {
        assert!(!t.contains("[activity]"), "the test site has none");
        format!("{t}\n[activity]\ninputs = [\"ghost\"]\nwindow_s = 300\n")
    });
    assert!(check_site(&ghost).is_ok());
}

/// HIL's test signal goes only to spare card outputs outside the
/// topology (`[guard] hil_tx`; the owner's decision on #9 of
/// 2026-09-28): the D5(b) loopback pair or an unused TX, never a channel
/// a band member hears. check-site, so `install-site` (F30), takes card
/// outputs from 1 that no mix uses, above `[engine] channels` too (the
/// highest channel the topology uses, not the card's output count: the
/// card's own count is checked when the stream opens), and refuses a
/// mix's TX, channel 0, one named twice and more than a HIL mask holds,
/// instead of every HIL test signal failing later.
#[test]
fn check_site_takes_spare_hil_outputs_and_refuses_a_mixs_tx() {
    let dir = tempfile::tempdir().unwrap();
    let with = |name: &str, tx: &str| {
        edited_site(dir.path(), name, |t| {
            t.replace("hil_tx = [94, 95]", &format!("hil_tx = {tx}"))
        })
    };
    // The test site's pair (94/95), TX 89/90 between the members and the
    // engineer, the map's first and last channels, channels above
    // `[engine] channels` (160), none at all.
    assert!(check_site(&crate::test_support::test_site_path()).is_ok());
    for spare in ["[89, 90]", "[1, 160]", "[161, 500]", "[]"] {
        assert!(check_site(&with("spare.toml", spare)).is_ok(), "{spare}");
    }
    let mix_tx = |ch: u16, mix: &str| {
        format!("card output {ch} is mix {mix}'s TX: the HIL signal goes only to spare outputs")
    };
    for (tx, why) in [
        ("[94, 72]", mix_tx(72, "member1")),
        ("[88]", mix_tx(88, "member9")),
        ("[92, 94]", mix_tx(92, "engineer")),
        ("[93]", mix_tx(93, "translator")),
        (
            "[0]",
            "card output 0: card channels count from 1".to_owned(),
        ),
        ("[94, 95, 94]", "card output 94 is listed twice".to_owned()),
        (
            "[94, 95, 96, 97, 98, 99, 100, 101, 102]",
            "9 card outputs: at most 8".to_owned(),
        ),
    ] {
        let e = check_site(&with("bad.toml", tx)).unwrap_err();
        assert!(
            matches!(&e, EngineError::Site(SiteError::HilTx(_))),
            "{tx}: {e}"
        );
        assert_eq!(e.to_string(), format!("[guard] hil_tx: {why}"), "{tx}");
    }
}

/// `run` opens HIL's spare outputs only with the test-signal flag (a
/// HIL job's engine): without it no HIL signal can start, so a live
/// engine never reads `[guard] hil_tx` and opens no card output outside
/// the topology.
#[test]
fn a_run_opens_the_hil_outputs_only_with_the_test_signal_flag() {
    let text = crate::test_support::test_site_text();
    let topo = crate::test_support::test_site();
    let on = Flags {
        test_signal: true,
        fault_injection: false,
    };
    assert_eq!(run_hil(on, &text, &topo), Ok(vec![94, 95]));
    assert_eq!(run_hil(Flags::default(), &text, &topo), Ok(Vec::new()));
    let mix_tx = text.replace("hil_tx = [94, 95]", "hil_tx = [72]");
    assert!(matches!(
        run_hil(on, &mix_tx, &topo),
        Err(SiteError::HilTx(m)) if m.contains("member1")
    ));
    assert_eq!(run_hil(Flags::default(), &mix_tx, &topo), Ok(Vec::new()));
    let unread = text.replace("hil_tx = [94, 95]", "hil_tx = \"x\"");
    assert!(matches!(
        run_hil(on, &unread, &topo),
        Err(SiteError::Toml(_))
    ));
    assert_eq!(run_hil(Flags::default(), &unread, &topo), Ok(Vec::new()));
    let none = text.replace("hil_tx = [94, 95]", "");
    assert_eq!(run_hil(on, &none, &topo), Ok(Vec::new()));
}

#[cfg(not(windows))]
#[test]
fn off_windows_the_card_is_a_usage_error_before_any_state() {
    let site = crate::test_support::test_site_path();
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = RunConfig::new(
        site,
        dir.path().join("state"),
        dir.path().join("p.sock").to_string_lossy().into_owned(),
    );
    cfg.backend = Backend::Asio;
    let e = run_bounded(cfg).unwrap_err();
    assert!(
        matches!(&e, EngineError::Usage(m) if m.contains("Windows")),
        "{e}"
    );
    assert!(
        !dir.path().join("state").exists(),
        "refused before the state"
    );
}

/// The hosted Windows runner has no ASIO driver: the synthetic card of
/// the test site is refused (exit 3) by `run`.
#[cfg(windows)]
#[test]
fn without_the_driver_the_card_is_refused() {
    let site = crate::test_support::test_site_path();
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = RunConfig::new(
        site,
        dir.path().join("state"),
        format!("iem-engine-asio-test-{}", std::process::id()),
    );
    cfg.backend = Backend::Asio;
    // The first open scans every process's modules (I3) before the
    // driver list; a hung driver call is refused after 15 s
    // (`iem_audio_io::asio`'s open bound), so the bound is above it.
    let e = run_bounded_for(cfg, Duration::from_secs(20)).unwrap_err();
    assert!(matches!(&e, EngineError::Card(_)), "{e}");
}

#[test]
fn render_arguments_parse() {
    let cmd = parse_args(&args(
        "render --site s --in a.wav --out b.wav --state x.json --block 97",
    ))
    .unwrap();
    assert_eq!(
        cmd,
        Command::Render(RenderArgs {
            site: "s".into(),
            state: Some("x.json".into()),
            input: "a.wav".into(),
            output: "b.wav".into(),
            block: 97
        })
    );
    let Command::Render(r) = parse_args(&args("render --site s --in a --out b")).unwrap() else {
        panic!()
    };
    assert_eq!((r.state, r.block), (None, 32));
}

#[test]
fn bad_arguments_are_explained() {
    assert_eq!(parse_args(&[]).unwrap(), Command::Help);
    assert_eq!(parse_args(&args("--help")).unwrap(), Command::Help);
    for (line, want) in [
        ("start", "unknown command"),
        ("run --site s --state-dir d", "--pipe"),
        ("run --state-dir d --pipe p", "--site"),
        ("run --site s --pipe p", "--state-dir"),
        ("render --site s --out b", "--in"),
        ("render --site s --in a", "--out"),
        ("render --in a --out b", "--site"),
        ("run --site", "needs a value"),
        ("run --site s --state-dir d --pipe p --block 0", "--block"),
        (
            "run --site s --state-dir d --pipe p --block 5000",
            "--block",
        ),
        ("run --site s --state-dir d --pipe p --block x", "--block"),
        ("run --site s --state-dir d --pipe p --sine 0", "--sine"),
        ("run --site s --state-dir d --pipe p --sine 50000", "--sine"),
        ("run --site s --state-dir d --pipe p --sine 48000", "--sine"),
        ("run --site s --state-dir d --pipe p --sine nan", "--sine"),
        (
            "run --site s --state-dir d --pipe p --loud",
            "unknown option",
        ),
    ] {
        let e = parse_args(&args(line)).unwrap_err();
        assert!(e.contains(want), "{line}: {e}");
    }
    assert_eq!(block("4096"), Ok(4096));
    assert_eq!(block("1"), Ok(1));
}

#[test]
fn acceptors_back_off_briefly_when_idle_and_longer_on_errors() {
    let idle = std::io::Error::from(std::io::ErrorKind::WouldBlock);
    assert_eq!(backoff(&idle, "t"), Duration::from_millis(10));
    let broken = std::io::Error::other("broken");
    assert_eq!(backoff(&broken, "t"), Duration::from_millis(100));
}

#[test]
fn dropped_state_entries_are_named_once() {
    assert_eq!(dropped_note(&[]), None);
    assert_eq!(
        dropped_note(&["input ghost".into(), "bus old".into()]).as_deref(),
        Some("state entries the topology no longer has: input ghost, bus old")
    );
}

/// `run` in a thread, bounded: a refused start must return at once.
fn run_bounded(cfg: RunConfig) -> Result<Exit, EngineError> {
    run_bounded_for(cfg, Duration::from_secs(5))
}

/// `run` in a thread, refused within `limit`.
fn run_bounded_for(cfg: RunConfig, limit: Duration) -> Result<Exit, EngineError> {
    let handle = std::thread::spawn(move || run(cfg));
    let start = std::time::Instant::now();
    while !handle.is_finished() {
        assert!(start.elapsed() < limit, "run did not refuse");
        std::thread::sleep(Duration::from_millis(10));
    }
    handle.join().unwrap()
}

#[test]
fn run_refuses_a_bad_block_and_a_bad_site() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = RunConfig::new(
        crate::test_support::test_site_path(),
        dir.path().join("state"),
        dir.path().join("p.sock").to_string_lossy().into_owned(),
    );
    cfg.block = 0;
    assert!(matches!(
        run_bounded(cfg.clone()),
        Err(EngineError::Usage(_))
    ));
    cfg.block = MAX_BLOCK + 1;
    assert!(matches!(
        run_bounded(cfg.clone()),
        Err(EngineError::Usage(_))
    ));
    cfg.block = 32;
    cfg.site = dir.path().join("missing.toml");
    assert!(matches!(
        run_bounded(cfg.clone()),
        Err(EngineError::Site(SiteError::Io(_)))
    ));
    // The asio backend needs the site's [card] table.
    cfg.site = edited_site(dir.path(), "bare.toml", without_card);
    cfg.backend = Backend::Asio;
    assert!(matches!(
        run_bounded(cfg.clone()),
        Err(EngineError::Site(SiteError::NoCardTable))
    ));
    // With the test-signal flag the run opens the site's HIL outputs:
    // a hil_tx that names a mix's TX stops it before any state or pipe
    // (the owner's decision on #9, 2026-09-28).
    cfg.backend = Backend::NullRt;
    cfg.flags.test_signal = true;
    cfg.site = edited_site(dir.path(), "mix-tx.toml", |t| {
        t.replace("hil_tx = [94, 95]", "hil_tx = [94, 72]")
    });
    assert!(matches!(
        run_bounded(cfg),
        Err(EngineError::Site(SiteError::HilTx(m))) if m.contains("member1")
    ));
    assert!(
        !dir.path().join("state").exists(),
        "refused before the state"
    );
}

#[test]
fn render_checks_rate_channels_and_block() {
    let dir = tempfile::tempdir().unwrap();
    let site = crate::test_support::test_site_path();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.wav");
    let args = |block: usize| RenderArgs {
        site: site.clone(),
        state: None,
        input: input.clone(),
        output: output.clone(),
        block,
    };
    wav::write_file(&input, 48_000, &iem_audio_io::Planar::new(32, 10)).unwrap();
    let e = render(&args(32)).unwrap_err().to_string();
    assert!(e.contains("48000 Hz"), "{e}");
    wav::write_file(&input, 96_000, &iem_audio_io::Planar::new(4, 10)).unwrap();
    let e = render(&args(32)).unwrap_err().to_string();
    assert!(e.contains("4 channels"), "{e}");
    wav::write_file(&input, 96_000, &iem_audio_io::Planar::new(32, 100)).unwrap();
    assert!(matches!(render(&args(0)), Err(EngineError::Usage(_))));
    render(&args(32)).unwrap();
    let (rate, out) = wav::read_file(&output).unwrap();
    assert_eq!((rate, out.channels(), out.frames()), (96_000, 21, 100));
    // A plain mix-state JSON is accepted as --state; garbage is explained.
    let state = dir.path().join("state.json");
    std::fs::write(&state, "{}").unwrap();
    render(&RenderArgs {
        state: Some(state.clone()),
        ..args(32)
    })
    .unwrap();
    std::fs::write(&state, "[1]").unwrap();
    let e = render(&RenderArgs {
        state: Some(state),
        ..args(32)
    })
    .unwrap_err()
    .to_string();
    assert!(e.contains("plain mix state"), "{e}");
}
