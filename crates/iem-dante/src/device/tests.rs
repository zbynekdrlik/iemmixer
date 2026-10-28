use super::*;

const TEST_DEVICE: &str = include_str!("../../../../config/test-dante-device.toml");

fn config() -> DeviceConfig {
    DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-dante-device.toml")
}

/// The test device with its state file in `dir`.
fn config_in(dir: &Path) -> DeviceConfig {
    DeviceConfig {
        state_file: dir.join("state.toml"),
        ..config()
    }
}

/// The test device's config text with one line replaced.
fn config_with(line: &str, replacement: &str) -> String {
    let lines: Vec<&str> = TEST_DEVICE.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with(line))
        .unwrap_or_else(|| panic!("the test config has no line starting {line:?}"));
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    out[at] = replacement.to_string();
    out.join("\n")
}

/// The test device's config text with the list `key` (its `key = [` line
/// through the `]` line) replaced by `names`.
fn config_with_list(key: &str, names: &[String]) -> String {
    let mut out = Vec::new();
    let mut skipping = false;
    for line in TEST_DEVICE.lines() {
        if line == format!("{key} = [") {
            let quoted: Vec<String> = names.iter().map(|n| format!("\"{n}\"")).collect();
            out.push(format!("{key} = [{}]", quoted.join(", ")));
            skipping = true;
        } else if skipping {
            skipping = line != "]";
        } else {
            out.push(line.to_string());
        }
    }
    assert!(
        !skipping,
        "the test config's {key} list has no closing line"
    );
    out.join("\n")
}

fn problems(text: &str) -> Vec<String> {
    DeviceConfig::from_toml(text).unwrap_err().problems
}

#[test]
fn a_config_reads_the_test_device() {
    let cfg = config();
    assert_eq!(cfg.name, "test-device");
    assert_eq!(cfg.manufacturer, "iemmixer");
    assert_eq!(cfg.model, "iem-dante");
    assert_eq!(cfg.rx.len(), 32);
    assert_eq!(cfg.tx.len(), 16);
    assert_eq!(cfg.rx[0], "RX 1");
    assert_eq!(cfg.rx[31], "RX 32");
    assert_eq!(cfg.tx[0], "TX 1");
    assert_eq!(cfg.tx[15], "TX 16");
    assert_eq!(cfg.sample_rate, 96_000);
    assert_eq!(cfg.encoding, 24);
    assert_eq!(cfg.latency_ns, 1_000_000);
    assert_eq!(cfg.block, 32, "B defaults to 32");
    assert_eq!(cfg.work_margin, 96, "W defaults to 1 ms");
    assert_eq!(cfg.tx_latency_ns, None);
    assert_eq!(
        cfg.advertised_latency_ns(),
        1_000_000,
        "TX advertises the RX latency"
    );
    assert_eq!(cfg.tx_margin_ns, 250_000);
    assert_eq!(cfg.tx_fpp_max, 32);
    assert_eq!(
        cfg.arc_rx_fpp, 16,
        "an ARC-resolved transmitter is asked for 16"
    );
    assert_eq!((cfg.pump_cpu, cfg.io_cpu), (None, None), "no CPU pinned");
    assert_eq!(
        cfg.state_file,
        PathBuf::from("/var/lib/iem-dante/state.toml")
    );
}

#[test]
fn names_follow_the_dante_rules() {
    let d31 = "a".repeat(31);
    let d32 = "a".repeat(32);
    // Device names: A-Z a-z 0-9 and '-', 1 to 31, no '-' at either end.
    assert!(valid_device_name(&d31));
    assert!(valid_device_name("test-device"));
    assert!(valid_device_name("Box2"));
    assert!(valid_device_name("x"));
    assert!(!valid_device_name(&d32));
    assert!(!valid_device_name(""));
    for bad in [
        "a@b", "a=b", "a.b", "a b", "a_b", "-ab", "ab-", "-", "a\u{e9}",
    ] {
        assert!(!valid_device_name(bad), "{bad:?} is no device name");
    }
    // Channel names: printable ASCII, 1 to 31, none of Dante's `= . @`.
    assert!(valid_channel_name(&d31));
    assert!(valid_channel_name("TX 1"));
    assert!(valid_channel_name("x"));
    assert!(valid_channel_name("Mix-3 (L) ~#!"));
    assert!(!valid_channel_name(&d32));
    assert!(!valid_channel_name(""));
    for bad in [
        "a@b", "a=b", "a.b", "a\tb", "a\nb", "a\u{7f}", "\u{e9}", "a\u{0}",
    ] {
        assert!(!valid_channel_name(bad), "{bad:?} is no channel name");
    }
    // The 31 limit counts bytes, so a multi-byte character cannot slip under it.
    assert!(!valid_channel_name(&format!("{}\u{e9}", "a".repeat(29))));
}

#[test]
fn a_state_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    let mut state = DeviceState::load_or_default(&cfg).unwrap();
    state.tx[2] = "Mix 3".into();
    state.rx[31] = "Return 32".into();
    state.save(&cfg.state_file).unwrap();
    // A restart: the config's defaults again, the state file read anew.
    let back = DeviceState::load_or_default(&cfg).unwrap();
    assert_eq!(back, state);
    assert_eq!(back.tx[2], "Mix 3");
    assert_eq!(back.tx[1], "TX 2");
    assert_eq!(back.rx[31], "Return 32");
    // The rename is written over: no temporary file is left beside it.
    let names: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["state.toml"]);
}

#[test]
fn a_second_save_replaces_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    let mut state = DeviceState::from_config(&cfg);
    state.tx[0] = "First".into();
    state.save(&cfg.state_file).unwrap();
    state.tx[0] = "Second".into();
    state.save(&cfg.state_file).unwrap();
    assert_eq!(DeviceState::load_or_default(&cfg).unwrap().tx[0], "Second");
}

#[test]
fn a_missing_state_is_the_config() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    let state = DeviceState::load_or_default(&cfg).unwrap();
    assert_eq!(state.name, "test-device");
    assert_eq!(state.rx, cfg.rx);
    assert_eq!(state.tx, cfg.tx);
    assert_eq!(state, DeviceState::from_config(&cfg));
    // Reading writes nothing.
    assert!(!cfg.state_file.exists());
}

#[test]
fn a_state_that_cannot_follow_the_config_is_refused() {
    // A config with fewer channels cuts the state only where nothing of
    // its own would go (#72): a channel past the config's count that
    // carries a rename refuses the start, by number and kind.
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    let path = cfg.state_file.display().to_string();

    let mut more_tx = DeviceState::from_config(&cfg);
    more_tx.tx.push("zyxq 17".into());
    more_tx.save(&cfg.state_file).unwrap();
    let err = DeviceState::load_or_default(&cfg).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let message = err.to_string();
    assert!(message.contains(&path), "{message}");
    assert!(
        message.contains(
            "17 TX channels, the config 16: \
             a renamed TX channel past the config's count (channel 17)"
        ),
        "{message}"
    );
    assert!(!message.contains("zyxq"), "{message} echoes a name");

    let mut more_rx = DeviceState::from_config(&cfg);
    more_rx.rx.push("zyxq 33".into());
    more_rx.save(&cfg.state_file).unwrap();
    let message = DeviceState::load_or_default(&cfg).unwrap_err().to_string();
    assert!(message.contains(&path), "{message}");
    assert!(
        message.contains(
            "33 RX channels, the config 32: \
             a renamed RX channel past the config's count (channel 33)"
        ),
        "{message}"
    );
    assert!(!message.contains("zyxq"), "{message} echoes a name");
}

#[test]
fn a_state_with_a_bad_name_is_refused_by_number_never_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    type Edit = fn(&mut DeviceState);
    let cases: [(Edit, &str); 6] = [
        (
            |s| s.tx[2] = "zyxq.name".into(),
            "TX channel 3: not a valid",
        ),
        (|s| s.rx[4] = String::new(), "RX channel 5: not a valid"),
        (|s| s.name = "zyxq name".into(), "name: not a valid"),
        // Names compare as mDNS does, without case.
        (
            |s| s.tx[5] = "tx 1".into(),
            "TX channel 6: the same name as channel 1",
        ),
        (
            |s| s.rx[9] = "RX 2".into(),
            "RX channel 10: the same name as channel 2",
        ),
        // A TX name that is another channel's default would be its mDNS name too.
        (
            |s| s.tx[0] = "TX 2".into(),
            "TX channel 1: the default name of channel 2",
        ),
    ];
    for (edit, want) in cases {
        let mut state = DeviceState::from_config(&cfg);
        edit(&mut state);
        state.save(&cfg.state_file).unwrap();
        let message = DeviceState::load_or_default(&cfg).unwrap_err().to_string();
        assert!(message.contains(want), "{message} lacks {want:?}");
        assert!(
            message.contains(&cfg.state_file.display().to_string()),
            "{message}"
        );
        assert!(!message.contains("zyxq"), "{message} echoes a name");
    }
}

#[test]
fn a_state_file_that_is_no_state_is_refused_without_echoing_it() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    for text in [
        "name = \"zyxqname\"\nrx = [\n",
        "zyxqname = 1\n",
        "name = 3\n",
    ] {
        fs::write(&cfg.state_file, text).unwrap();
        let err = DeviceState::load_or_default(&cfg).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{text:?}");
        let message = err.to_string();
        assert!(
            message.contains(&cfg.state_file.display().to_string()),
            "{message}"
        );
        assert!(message.contains("line "), "{message}");
        assert!(!message.contains("zyxq"), "{message} echoes the file");
    }
}

#[test]
fn an_unreadable_state_file_is_an_error_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    // A directory where the file should be: it exists, yet cannot be read.
    fs::create_dir(&cfg.state_file).unwrap();
    let err = DeviceState::load_or_default(&cfg).unwrap_err();
    assert!(
        err.to_string()
            .contains(&cfg.state_file.display().to_string()),
        "{err}"
    );
}

#[test]
fn a_save_into_a_missing_directory_fails_naming_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing").join("state.toml");
    let cfg = config_in(dir.path());
    let err = DeviceState::from_config(&cfg).save(&path).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    assert!(
        err.to_string().contains(&path.display().to_string()),
        "{err}"
    );
    let err = DeviceState::from_config(&cfg)
        .save(Path::new("/"))
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

/// The names in `dir`, sorted.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_writable_state_folder_passes_the_check_and_keeps_nothing_of_it() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    // No state file yet: the check leaves the folder empty.
    DeviceState::check_writable(&cfg.state_file).unwrap();
    assert_eq!(listing(dir.path()), Vec::<String>::new());
    // A state file and a temporary file a crash left: the state stays as it
    // was, the leftover goes (save would overwrite it anyway).
    let state = DeviceState::from_config(&cfg);
    state.save(&cfg.state_file).unwrap();
    let saved = fs::read(&cfg.state_file).unwrap();
    fs::write(dir.path().join("state.toml.tmp"), b"a torn write").unwrap();
    DeviceState::check_writable(&cfg.state_file).unwrap();
    assert_eq!(listing(dir.path()), ["state.toml"]);
    assert_eq!(fs::read(&cfg.state_file).unwrap(), saved);
}

#[test]
fn a_state_folder_that_cannot_be_written_fails_the_check_naming_the_file() {
    let dir = tempfile::tempdir().unwrap();
    // A folder that is not there, and a file where the folder should be:
    // refused whoever runs the test (root too).
    let missing = dir.path().join("missing").join("state.toml");
    let blocker = dir.path().join("a-file");
    fs::write(&blocker, b"not a folder").unwrap();
    let under_a_file = blocker.join("state.toml");
    for path in [&missing, &under_a_file] {
        let err = DeviceState::check_writable(path).unwrap_err();
        assert!(
            err.to_string().contains(&path.display().to_string()),
            "{err}"
        );
    }
    assert_eq!(
        DeviceState::check_writable(&missing).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        DeviceState::check_writable(Path::new("/"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(listing(dir.path()), ["a-file"]);
}

#[test]
fn the_tx_side_settings_are_read() {
    let text = config_with(
        "latency_ns =",
        "latency_ns = 1000000\ntx_latency_ns = 250000\ntx_margin_ns = 30000\ntx_fpp_max = 16",
    );
    let cfg = DeviceConfig::from_toml(&text).unwrap();
    assert_eq!(cfg.latency_ns, 1_000_000, "the RX playout stays");
    assert_eq!(cfg.advertised_latency_ns(), 250_000);
    assert_eq!((cfg.tx_margin_ns, cfg.tx_fpp_max), (30_000, 16));
}

#[test]
fn the_fpp_asked_of_an_arc_resolved_transmitter_is_read() {
    // #72 phase F: fewer frames a packet than the card's guessed 16.
    let text = config_with("latency_ns =", "latency_ns = 1000000\narc_rx_fpp = 4");
    let cfg = DeviceConfig::from_toml(&text).unwrap();
    assert_eq!(cfg.arc_rx_fpp, 4);
    assert_eq!(cfg.tx_fpp_max, 32, "the TX side's own");
}

#[test]
fn the_advertised_latency_covers_a_packet_its_margin_and_the_send_slack() {
    // The review of PR #84 (#72 phase F): a receiver plays our audio the
    // advertised latency after its stamp, and our packet leaves tx_fpp_max
    // frames + tx_margin_ns after its first sample, give or take the send
    // time's spread on the realtime box (p99.9 50 us). A config whose sum
    // passes the latency is refused, naming fields and numbers only.
    let tx = |lines: &str| config_with("latency_ns =", &format!("latency_ns = 1000000\n{lines}"));
    let refusal = vec![
        "tx_latency_ns (latency_ns when it is left out): \
         below tx_fpp_max frames + tx_margin_ns + 50000 ns"
            .to_string(),
    ];
    // The box's settings: 8 frames at 96 kHz (83 334 ns) + 30 000 + 50 000
    // ns = 163 334 ns within 0.25 ms; at that edge too, not 1 ns below it.
    let cfg = DeviceConfig::from_toml(&tx(
        "tx_latency_ns = 250000\ntx_margin_ns = 30000\ntx_fpp_max = 8",
    ))
    .unwrap();
    assert_eq!(cfg.advertised_latency_ns(), 250_000);
    DeviceConfig::from_toml(&tx(
        "tx_latency_ns = 163334\ntx_margin_ns = 30000\ntx_fpp_max = 8",
    ))
    .unwrap();
    assert_eq!(
        problems(&tx(
            "tx_latency_ns = 163333\ntx_margin_ns = 30000\ntx_fpp_max = 8"
        )),
        refusal
    );
    // The defaults: 32 frames (333 334 ns) + 250 000 + 50 000 ns within the
    // test device's 1 ms; 0.25 ms with them is refused.
    assert_eq!(config().advertised_latency_ns(), 1_000_000);
    assert_eq!(problems(&tx("tx_latency_ns = 250000")), refusal);
    // Without tx_latency_ns our channels advertise latency_ns.
    DeviceConfig::from_toml(&config_with("latency_ns =", "latency_ns = 633334")).unwrap();
    assert_eq!(
        problems(&config_with("latency_ns =", "latency_ns = 633333")),
        refusal
    );
    // At 48 kHz 32 frames are 666 667 ns: 966 667 ns within 1 ms.
    let at_48k = config_with("sample_rate =", "sample_rate = 48000");
    DeviceConfig::from_toml(&at_48k).unwrap();
    assert_eq!(
        problems(&format!("{at_48k}\ntx_latency_ns = 966666")),
        refusal
    );
}

#[test]
fn the_threads_cpus_are_read_and_bounded() {
    let text = config_with(
        "latency_ns =",
        "latency_ns = 1000000\npump_cpu = 3\nio_cpu = 2",
    );
    let cfg = DeviceConfig::from_toml(&text).unwrap();
    assert_eq!((cfg.pump_cpu, cfg.io_cpu), (Some(3), Some(2)));
    for (line, field) in [("pump_cpu = 1024", "pump_cpu"), ("io_cpu = 1024", "io_cpu")] {
        let found = problems(&config_with(
            "latency_ns =",
            &format!("latency_ns = 1000000\n{line}"),
        ));
        assert_eq!(found, vec![format!("{field}: 0 to 1023")]);
    }
    DeviceConfig::from_toml(&config_with(
        "latency_ns =",
        "latency_ns = 1000000\nio_cpu = 1023",
    ))
    .unwrap();
}

#[test]
fn the_block_is_16_or_32_and_the_work_margin_at_least_one_block() {
    // #72: B 16 halves the block and lets W go down to 16 (mic to in-ear
    // under 1 ms); a config without `block` keeps 32.
    let with = |lines: &str| config_with("latency_ns =", &format!("latency_ns = 1000000\n{lines}"));
    let cfg = DeviceConfig::from_toml(&with("block = 16")).unwrap();
    assert_eq!((cfg.block, cfg.work_margin), (16, 96));
    let cfg = DeviceConfig::from_toml(&with("block = 32")).unwrap();
    assert_eq!(cfg.block, 32);
    // Any other block is refused naming the field, one problem.
    for block in [0, 8, 24, 31, 33, 64, 4096] {
        assert_eq!(
            problems(&with(&format!("block = {block}"))),
            vec!["block: 16 or 32 frames".to_string()],
            "{block}"
        );
    }
    // W 16 with block 16, never with block 32 (the default).
    let cfg = DeviceConfig::from_toml(&with("block = 16\nwork_margin = 16")).unwrap();
    assert_eq!((cfg.block, cfg.work_margin), (16, 16));
    assert_eq!(
        problems(&with("work_margin = 16")),
        vec!["work_margin: 32 to 4096 samples".to_string()]
    );
    assert_eq!(
        problems(&with("block = 32\nwork_margin = 16")),
        vec!["work_margin: 32 to 4096 samples".to_string()]
    );
    // Below the block at 16; the top stays 4096.
    assert_eq!(
        problems(&with("block = 16\nwork_margin = 15")),
        vec!["work_margin: 16 to 4096 samples".to_string()]
    );
    DeviceConfig::from_toml(&with("block = 16\nwork_margin = 4096")).unwrap();
    assert_eq!(
        problems(&with("block = 16\nwork_margin = 4097")),
        vec!["work_margin: 16 to 4096 samples".to_string()]
    );
    // A refused block bounds W at the default's 32: one problem a field.
    assert_eq!(
        problems(&with("block = 64\nwork_margin = 48")),
        vec!["block: 16 or 32 frames".to_string()]
    );
    assert_eq!(
        problems(&with("block = 8\nwork_margin = 16")),
        vec![
            "block: 16 or 32 frames".to_string(),
            "work_margin: 32 to 4096 samples".to_string(),
        ]
    );
}

#[test]
fn the_config_is_checked_field_by_field() {
    let cases = [
        (
            config_with("name =", "name = \"bad name\""),
            "name: not a valid Dante device name",
        ),
        (
            config_with("manufacturer =", "manufacturer = \"\""),
            "manufacturer: 1 to 31",
        ),
        (
            config_with("model =", "model = \"m\u{e9}\""),
            "model: 1 to 31",
        ),
        (
            config_with("sample_rate =", "sample_rate = 22050"),
            "sample_rate: 22050 Hz is not one of",
        ),
        (
            config_with("encoding =", "encoding = 20"),
            "encoding: 20 bits is not one of",
        ),
        (
            config_with("latency_ns =", "latency_ns = 0"),
            "latency_ns: 1 to 4294967295",
        ),
        (
            config_with("latency_ns =", "latency_ns = 4294967296"),
            "latency_ns: 1 to 4294967295",
        ),
        (
            config_with("state_file =", "state_file = \"\""),
            "state_file: empty",
        ),
        (
            config_with("latency_ns =", "latency_ns = 1000000\ntx_latency_ns = 0"),
            "tx_latency_ns: 1 to 4294967295",
        ),
        (
            config_with(
                "latency_ns =",
                "latency_ns = 1000000\ntx_margin_ns = 1000001",
            ),
            "tx_margin_ns: 0 to 1000000",
        ),
        (
            config_with("latency_ns =", "latency_ns = 1000000\ntx_fpp_max = 1"),
            "tx_fpp_max: 2 to 32",
        ),
        (
            config_with("latency_ns =", "latency_ns = 1000000\ntx_fpp_max = 33"),
            "tx_fpp_max: 2 to 32",
        ),
        // 300 frames would also pass 1 ms: one problem, the field's own.
        (
            config_with("latency_ns =", "latency_ns = 1000000\ntx_fpp_max = 300"),
            "tx_fpp_max: 2 to 32",
        ),
        (
            config_with("latency_ns =", "latency_ns = 1000000\narc_rx_fpp = 1"),
            "arc_rx_fpp: 2 to 32",
        ),
        (
            config_with("latency_ns =", "latency_ns = 1000000\narc_rx_fpp = 33"),
            "arc_rx_fpp: 2 to 32",
        ),
        (
            config_with("latency_ns =", "latency_ns = 250000"),
            "tx_latency_ns (latency_ns when it is left out): below tx_fpp_max frames",
        ),
        (
            config_with("latency_ns =", "latency_ns = 1000000\nwork_margin = 31"),
            "work_margin: 32 to 4096 samples",
        ),
        (
            config_with("latency_ns =", "latency_ns = 1000000\nwork_margin = 4097"),
            "work_margin: 32 to 4096 samples",
        ),
    ];
    for (text, want) in cases {
        let found = problems(&text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with(want), "{found:?} lacks {want:?}");
    }
    // The edges that are allowed (the advertised latency covers a packet,
    // its margin and the send slack: the RX playout's 1 ns and the margin's
    // 1 ms beside a TX latency of their own).
    for (line, ok) in [
        ("latency_ns =", "latency_ns = 4294967295"),
        ("latency_ns =", "latency_ns = 1\ntx_latency_ns = 1000000"),
        ("latency_ns =", "latency_ns = 1000000\nwork_margin = 32"),
        (
            "latency_ns =",
            "latency_ns = 1000000\ntx_latency_ns = 4294967295",
        ),
        ("latency_ns =", "latency_ns = 1000000\ntx_margin_ns = 0"),
        (
            "latency_ns =",
            "latency_ns = 1000000\ntx_latency_ns = 2000000\ntx_margin_ns = 1000000",
        ),
        ("latency_ns =", "latency_ns = 1000000\ntx_fpp_max = 2"),
        ("latency_ns =", "latency_ns = 1000000\narc_rx_fpp = 2"),
        ("latency_ns =", "latency_ns = 1000000\narc_rx_fpp = 32"),
        ("latency_ns =", "latency_ns = 1000000\nwork_margin = 4096"),
        ("sample_rate =", "sample_rate = 48000"),
        ("encoding =", "encoding = 32"),
        (
            "manufacturer =",
            "manufacturer = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"",
        ),
    ] {
        DeviceConfig::from_toml(&config_with(line, ok)).unwrap();
    }
    assert_eq!(
        problems(&config_with(
            "manufacturer =",
            "manufacturer = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\""
        ))
        .len(),
        1
    );
}

#[test]
fn a_config_channel_problem_names_its_number_never_its_name() {
    let text = TEST_DEVICE
        .replace("\"TX 3\"", "\"zyxq.three\"")
        .replace("\"RX 7\"", "\"RX 1\"");
    let found = problems(&text);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found[0].starts_with("RX channel 7: the same name as channel 1"),
        "{found:?}"
    );
    assert!(
        found[1].starts_with("TX channel 3: not a valid Dante channel name"),
        "{found:?}"
    );
    assert!(!found.join(" ").contains("zyxq"), "{found:?}");
}

#[test]
fn a_config_may_have_no_channels_but_not_too_many() {
    let cfg = DeviceConfig::from_toml(&config_with_list("tx", &[])).unwrap();
    assert!(cfg.tx.is_empty());
    assert_eq!(cfg.rx.len(), 32);

    let names = |n: usize| -> Vec<String> { (1..=n).map(|i| format!("R{i}")).collect() };
    let found = problems(&config_with_list("rx", &names(MAX_CHANNELS + 1)));
    assert_eq!(
        found,
        [format!(
            "RX: {} channels, at most {MAX_CHANNELS}",
            MAX_CHANNELS + 1
        )]
    );
    let cfg = DeviceConfig::from_toml(&config_with_list("rx", &names(MAX_CHANNELS))).unwrap();
    assert_eq!(cfg.rx.len(), MAX_CHANNELS);
}

#[test]
fn a_config_parse_error_gives_a_line_and_no_value() {
    let text = config_with("sample_rate =", "sample_rate = \"zyxqrate\"");
    let found = problems(&text);
    assert_eq!(found.len(), 1, "{found:?}");
    let line = TEST_DEVICE
        .lines()
        .position(|l| l.starts_with("sample_rate ="))
        .unwrap()
        + 1;
    assert!(found[0].starts_with(&format!("line {line}: ")), "{found:?}");
    assert!(!found[0].contains("zyxq"), "{found:?}");

    let found = problems(&format!("{TEST_DEVICE}\nzyxqfield = 1\n"));
    assert!(!found[0].contains("zyxq"), "{found:?}");
    let err = DeviceConfig::from_toml("name = \"x\"").unwrap_err();
    assert!(err.to_string().starts_with("device config: "), "{err}");
}

#[test]
fn redact_keeps_the_quote_marks_and_drops_what_they_hold() {
    assert_eq!(redact("a \"b\" c `d` e"), "a \"\u{2026}\" c `\u{2026}` e");
    assert_eq!(redact("x \"a\\\"b\" y"), "x \"\u{2026}\" y");
    assert_eq!(redact("open \"never closed"), "open \"\u{2026}\"");
    assert_eq!(redact("plain"), "plain");
}

// ---- RX subscriptions (phase B, plan Task 5) ----

fn sub(channel: &str, device: &str) -> Subscription {
    Subscription {
        tx_channel: channel.to_string(),
        tx_device: device.to_string(),
    }
}

/// A phase A state file: the names only, RX 32 renamed.
fn phase_a_state() -> String {
    let quoted = |names: Vec<String>| {
        names
            .iter()
            .map(|n| format!("\"{n}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut rx: Vec<String> = (1..=31).map(|i| format!("RX {i}")).collect();
    rx.push("Return 32".to_string());
    let tx: Vec<String> = (1..=16).map(|i| format!("TX {i}")).collect();
    format!(
        "name = \"test-device\"\nrx = [{}]\ntx = [{}]\n",
        quoted(rx),
        quoted(tx)
    )
}

#[test]
fn a_phase_a_state_file_loads_without_subscriptions() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    fs::write(&cfg.state_file, phase_a_state()).unwrap();
    let state = DeviceState::load_or_default(&cfg).unwrap();
    assert_eq!(state.rx[31], "Return 32");
    assert!(state.subscriptions.is_empty());
    assert_eq!(state.subscription(0), None);
    assert_eq!(state.subscription(99), None);
    // A state without subscriptions is written as phase A wrote it.
    state.save(&cfg.state_file).unwrap();
    let text = fs::read_to_string(&cfg.state_file).unwrap();
    assert!(!text.contains("subscriptions"), "{text}");
    assert_eq!(DeviceState::load_or_default(&cfg).unwrap(), state);
    // So is one whose subscriptions were all removed again.
    let mut emptied = state.clone();
    assert!(emptied.set_subscription(&cfg, 3, Some(sub("TX 1", "peer-device"))));
    assert!(emptied.set_subscription(&cfg, 3, None));
    assert_eq!(emptied.subscriptions.len(), 32);
    emptied.save(&cfg.state_file).unwrap();
    let text = fs::read_to_string(&cfg.state_file).unwrap();
    assert!(!text.contains("subscriptions"), "{text}");
}

#[test]
fn a_subscription_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    let mut state = DeviceState::load_or_default(&cfg).unwrap();
    assert!(state.set_subscription(&cfg, 1, Some(sub("TX 1", "peer-device"))));
    assert!(state.set_subscription(&cfg, 31, Some(sub("TX 16", "test-device"))));
    // No RX channel 33.
    assert!(!state.set_subscription(&cfg, 32, Some(sub("TX 1", "peer-device"))));
    assert!(!state.set_subscription(&cfg, 32, None));
    assert_eq!(state.subscriptions.len(), 32);
    state.save(&cfg.state_file).unwrap();
    // The file lists the subscribed channels by number, nothing else.
    let text = fs::read_to_string(&cfg.state_file).unwrap();
    assert_eq!(text.matches("[[subscriptions]]").count(), 2, "{text}");
    assert!(text.contains("rx = 2\n"), "{text}");
    assert!(text.contains("rx = 32\n"), "{text}");
    let back = DeviceState::load_or_default(&cfg).unwrap();
    assert_eq!(back, state);
    assert_eq!(back.subscription(1), Some(&sub("TX 1", "peer-device")));
    assert_eq!(back.subscription(31), Some(&sub("TX 16", "test-device")));
    assert_eq!(back.subscription(0), None);
    // A file that lists only a low channel loads as one entry per channel.
    let mut low = DeviceState::from_config(&cfg);
    assert!(low.set_subscription(&cfg, 0, Some(sub("TX 2", "peer-device"))));
    low.save(&cfg.state_file).unwrap();
    let back = DeviceState::load_or_default(&cfg).unwrap();
    assert_eq!(back.subscriptions.len(), 32);
    assert_eq!(back.subscription(0), Some(&sub("TX 2", "peer-device")));
}

#[test]
fn a_wrong_length_list_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    let path = cfg.state_file.display().to_string();
    let entry = |rx: &str| {
        format!(
            "\n[[subscriptions]]\nrx = {rx}\ntx_channel = \"zyxqchan\"\n\
             tx_device = \"zyxqdev\"\n"
        )
    };
    let base = phase_a_state();
    // Past the RX channels, past any device's, channel 0, a channel twice,
    // not a number, an unknown field.
    for (text, want) in [
        (
            format!("{base}{}", entry("33")),
            "a subscription of RX channel 33, the config has 32 RX channels",
        ),
        // Any device's most channels are read, then refused by the config.
        (
            format!("{base}{}", entry("512")),
            "a subscription of RX channel 512, the config has 32 RX channels",
        ),
        (
            format!("{base}{}", entry("513")),
            "RX channel 513: no device has it (1 to 512)",
        ),
        (format!("{base}{}", entry("100000")), "RX channel 100000"),
        (format!("{base}{}", entry("0")), "RX channel 0"),
        (
            format!("{base}{}{}", entry("2"), entry("2")),
            "RX channel 2 is subscribed twice",
        ),
        (format!("{base}{}", entry("\"zyxq\"")), "line "),
        (format!("{base}{}zyxqfield = 1\n", entry("1")), "line "),
    ] {
        fs::write(&cfg.state_file, &text).unwrap();
        let err = DeviceState::load_or_default(&cfg).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{text}");
        let message = err.to_string();
        assert!(message.contains(&path), "{message}");
        assert!(message.contains(want), "{message} lacks {want:?}");
        assert!(!message.contains("zyxq"), "{message} echoes the file");
    }
    // In memory: one entry per RX channel or none.
    let mut state = DeviceState::from_config(&cfg);
    state.subscriptions = vec![None; 5];
    assert_eq!(
        state.problems(&cfg),
        ["5 subscription entries, the config has 32 RX channels"]
    );
    state.subscriptions = vec![None; 32];
    assert!(state.problems(&cfg).is_empty());
    state.subscriptions = Vec::new();
    assert!(state.problems(&cfg).is_empty());
}

#[test]
fn a_subscription_to_an_invalid_name_is_refused_by_number() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    for (s, want) in [
        (
            sub("zyxq.chan", "peer-device"),
            "RX channel 4: a subscription to an invalid channel name",
        ),
        (
            sub("", "peer-device"),
            "RX channel 4: a subscription to an invalid channel name",
        ),
        (
            sub("TX 1", "zyxq dev"),
            "RX channel 4: a subscription to an invalid device name",
        ),
        (
            sub("TX 1", "-zyxq"),
            "RX channel 4: a subscription to an invalid device name",
        ),
    ] {
        let mut state = DeviceState::from_config(&cfg);
        // set_subscription refuses it; a hand-edited file may still hold it.
        assert!(!state.set_subscription(&cfg, 3, Some(s.clone())));
        assert!(state.subscriptions.is_empty());
        state.subscriptions = vec![None; 32];
        state.subscriptions[3] = Some(s);
        state.save(&cfg.state_file).unwrap();
        let message = DeviceState::load_or_default(&cfg).unwrap_err().to_string();
        assert!(message.contains(want), "{message} lacks {want:?}");
        assert!(!message.contains("zyxq"), "{message} echoes a name");
    }
}

// ---- A config whose channel counts changed (#72 phase C, Live 1) ----

/// The test device with `rx` RX and `tx` TX channels, named as the test
/// config names its channels (`RX n`, `TX n`), its state file in `dir`.
fn config_sized(dir: &Path, rx: usize, tx: usize) -> DeviceConfig {
    DeviceConfig {
        rx: (1..=rx).map(|n| format!("RX {n}")).collect(),
        tx: (1..=tx).map(|n| format!("TX {n}")).collect(),
        ..config_in(dir)
    }
}

#[test]
fn a_state_follows_a_config_with_more_channels() {
    // Live 1 (#72, 2026-10-26): phase B's state (32 RX, 16 TX) under phase
    // C's config (136 RX, 96 TX) refused the engine's start with exit 2.
    let dir = tempfile::tempdir().unwrap();
    let phase_b = config_in(dir.path());
    let mut state = DeviceState::from_config(&phase_b);
    state.rx[2] = "Return 3".into();
    assert!(state.set_subscription(&phase_b, 4, Some(sub("TX 2", "peer-device"))));
    state.save(&phase_b.state_file).unwrap();

    let phase_c = config_sized(dir.path(), 136, 96);
    let device = Device::load(phase_c.clone()).expect("the device starts on the grown config");
    let grown = &device.state;
    // RX 3's name and RX 5's subscription stay.
    assert_eq!(grown.rx[2], "Return 3");
    assert_eq!(grown.subscription(4), Some(&sub("TX 2", "peer-device")));
    // Every other channel, the new ones too, at the config's name.
    assert_eq!(grown.rx.len(), 136);
    for (i, name) in grown.rx.iter().enumerate().filter(|&(i, _)| i != 2) {
        assert_eq!(name, &phase_c.rx[i], "RX channel {}", i + 1);
    }
    assert_eq!(grown.tx, phase_c.tx);
    // No other subscription; one entry per RX channel.
    assert_eq!(grown.subscriptions.len(), 136);
    assert_eq!(grown.subscriptions.iter().flatten().count(), 1);
    assert_eq!(grown.name, "test-device");
    assert!(grown.problems(&phase_c).is_empty());
}

/// The state file as written, read without fitting it to any config.
fn written(cfg: &DeviceConfig) -> DeviceState {
    toml::from_str(&fs::read_to_string(&cfg.state_file).unwrap()).unwrap()
}

#[test]
fn the_grown_state_is_saved_so_the_next_start_reads_a_matching_file() {
    let dir = tempfile::tempdir().unwrap();
    let phase_b = config_in(dir.path());
    let mut state = DeviceState::from_config(&phase_b);
    state.rx[2] = "Return 3".into();
    state.tx[1] = "Mix 2".into();
    assert!(state.set_subscription(&phase_b, 4, Some(sub("TX 2", "peer-device"))));
    state.save(&phase_b.state_file).unwrap();

    let phase_c = config_sized(dir.path(), 136, 96);
    let grown = DeviceState::load_or_default(&phase_c).unwrap();
    assert_eq!(grown.tx[1], "Mix 2");
    // The file now holds the config's counts, the rename and the
    // subscription in it.
    let file = written(&phase_c);
    assert_eq!((file.rx.len(), file.tx.len()), (136, 96));
    assert_eq!(file.rx[2], "Return 3");
    assert_eq!(file.tx[1], "Mix 2");
    assert_eq!(file.rx[135], "RX 136");
    assert_eq!(file.tx[95], "TX 96");
    assert_eq!(file.subscription(4), Some(&sub("TX 2", "peer-device")));
    // The next start reads it as it is, and writes nothing.
    let saved = fs::read(&phase_c.state_file).unwrap();
    assert_eq!(DeviceState::load_or_default(&phase_c).unwrap(), grown);
    assert_eq!(fs::read(&phase_c.state_file).unwrap(), saved);
    assert_eq!(listing(dir.path()), ["state.toml"]);
}

#[test]
fn a_state_without_subscriptions_grows_without_any() {
    let dir = tempfile::tempdir().unwrap();
    let phase_b = config_in(dir.path());
    DeviceState::from_config(&phase_b)
        .save(&phase_b.state_file)
        .unwrap();
    let phase_c = config_sized(dir.path(), 136, 96);
    let grown = DeviceState::load_or_default(&phase_c).unwrap();
    assert_eq!(grown, DeviceState::from_config(&phase_c));
    assert!(grown.subscriptions.is_empty());
    let text = fs::read_to_string(&phase_c.state_file).unwrap();
    assert!(!text.contains("subscriptions"), "{text}");
}

#[test]
fn a_state_follows_a_config_whose_tx_count_alone_changed() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    let mut state = DeviceState::from_config(&cfg);
    assert!(state.set_subscription(&cfg, 4, Some(sub("TX 2", "peer-device"))));
    state.save(&cfg.state_file).unwrap();
    // More TX channels, the same RX ones.
    let more_tx = config_sized(dir.path(), 32, 96);
    let grown = DeviceState::load_or_default(&more_tx).unwrap();
    assert_eq!(grown.tx, more_tx.tx);
    assert_eq!(grown.rx, cfg.rx);
    assert_eq!(grown.subscriptions.len(), 32);
    assert_eq!(grown.subscription(4), Some(&sub("TX 2", "peer-device")));
    assert_eq!(written(&more_tx).tx.len(), 96);

    // A subscription past the RX channels is the file's own fault, refused
    // as before even while the TX count changes.
    let entry = "\n[[subscriptions]]\nrx = 33\ntx_channel = \"zyxqchan\"\n\
                 tx_device = \"zyxqdev\"\n";
    fs::write(&cfg.state_file, format!("{}{entry}", phase_a_state())).unwrap();
    let before = fs::read(&cfg.state_file).unwrap();
    let message = DeviceState::load_or_default(&more_tx)
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("a subscription of RX channel 33, the config has 32 RX channels"),
        "{message}"
    );
    assert!(!message.contains("zyxq"), "{message} echoes a name");
    assert_eq!(fs::read(&cfg.state_file).unwrap(), before);
}

#[test]
fn a_state_is_cut_to_a_config_with_fewer_channels_when_nothing_of_its_own_goes() {
    let dir = tempfile::tempdir().unwrap();
    let wide = config_sized(dir.path(), 40, 20);
    let mut state = DeviceState::from_config(&wide);
    state.rx[2] = "Return 3".into();
    state.tx[15] = "Mix 16".into();
    assert!(state.set_subscription(&wide, 31, Some(sub("TX 2", "peer-device"))));
    state.save(&wide.state_file).unwrap();

    let narrow = config_in(dir.path());
    let cut = DeviceState::load_or_default(&narrow).unwrap();
    assert_eq!(cut.rx.len(), 32);
    assert_eq!(cut.tx.len(), 16);
    assert_eq!(cut.rx[2], "Return 3");
    assert_eq!(cut.tx[15], "Mix 16");
    assert_eq!(cut.subscriptions.len(), 32);
    assert_eq!(cut.subscription(31), Some(&sub("TX 2", "peer-device")));
    // Saved: the file holds the config's counts.
    let file = written(&narrow);
    assert_eq!((file.rx.len(), file.tx.len()), (32, 16));
    assert_eq!(DeviceState::load_or_default(&narrow).unwrap(), cut);

    // In memory too: one subscription entry per RX channel after the cut.
    let mut state = DeviceState::from_config(&wide);
    assert!(state.set_subscription(&wide, 0, Some(sub("TX 1", "peer-device"))));
    assert_eq!(state.subscriptions.len(), 40);
    assert_eq!(
        state.fit(&narrow),
        Ok(Some(Refit {
            rx: (40, 32),
            tx: (20, 16)
        }))
    );
    assert_eq!(state.subscriptions.len(), 32);
    assert_eq!(state.rx, narrow.rx);
    assert_eq!(state.tx, narrow.tx);
    assert!(state.problems(&narrow).is_empty());
}

#[test]
fn a_cut_that_would_drop_a_rename_or_a_subscription_is_refused_by_kind_and_number() {
    let dir = tempfile::tempdir().unwrap();
    let wide = config_sized(dir.path(), 40, 20);
    let narrow = config_in(dir.path());
    let path = narrow.state_file.display().to_string();
    let renamed_rx = "40 RX channels, the config 32: \
                      a renamed RX channel past the config's count (channel 34)";
    let renamed_tx = "20 TX channels, the config 16: \
                      a renamed TX channel past the config's count (channel 18)";
    let subscribed = "40 RX channels, the config 32: \
                      a subscribed RX channel past the config's count (channel 35)";
    type Edit = fn(&mut DeviceState, &DeviceConfig);
    let cases: [(Edit, &[&str]); 4] = [
        (
            |s, _| {
                s.rx[33] = "zyxq 34".into();
                s.rx[37] = "zyxq 38".into();
            },
            &[renamed_rx],
        ),
        (|s, _| s.tx[17] = "zyxq 18".into(), &[renamed_tx]),
        (
            |s, c| assert!(s.set_subscription(c, 34, Some(sub("zyxq", "zyxq-dev")))),
            &[subscribed],
        ),
        (
            |s, c| {
                s.rx[33] = "zyxq 34".into();
                s.tx[17] = "zyxq 18".into();
                assert!(s.set_subscription(c, 34, Some(sub("zyxq", "zyxq-dev"))));
            },
            &[renamed_rx, renamed_tx, subscribed],
        ),
    ];
    for (edit, want) in cases {
        let mut state = DeviceState::from_config(&wide);
        edit(&mut state, &wide);
        state.save(&narrow.state_file).unwrap();
        let before = fs::read(&narrow.state_file).unwrap();
        let err = DeviceState::load_or_default(&narrow).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let message = err.to_string();
        assert_eq!(message, format!("{path}: {}", want.join("; ")));
        assert!(!message.contains("zyxq"), "{message} echoes a name");
        // The file stays as it was.
        assert_eq!(fs::read(&narrow.state_file).unwrap(), before);
        // In memory: refused, nothing changed.
        let kept = state.clone();
        assert_eq!(
            state.fit(&narrow),
            Err(want.iter().map(|w| w.to_string()).collect())
        );
        assert_eq!(state, kept);
    }
}

#[test]
fn a_state_with_the_config_s_counts_is_left_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    let mut state = DeviceState::from_config(&cfg);
    state.rx[2] = "Return 3".into();
    assert!(state.set_subscription(&cfg, 4, Some(sub("TX 2", "peer-device"))));
    let kept = state.clone();
    assert_eq!(state.fit(&cfg), Ok(None));
    assert_eq!(state, kept);
    // Loading it writes nothing: a hand-written comment stays.
    state.save(&cfg.state_file).unwrap();
    let text = format!(
        "# zyxq kept\n{}",
        fs::read_to_string(&cfg.state_file).unwrap()
    );
    fs::write(&cfg.state_file, &text).unwrap();
    assert_eq!(DeviceState::load_or_default(&cfg).unwrap(), kept);
    assert_eq!(fs::read_to_string(&cfg.state_file).unwrap(), text);
}

#[test]
fn a_channel_past_the_config_has_its_numbering_as_default_name() {
    let names = |list: &[&str]| list.iter().map(|n| n.to_string()).collect::<Vec<_>>();
    let rx = names(&["RX 1", "RX 2", "RX 3"]);
    assert_eq!(default_past(&rx, 4).as_deref(), Some("RX 4"));
    assert_eq!(default_past(&rx, 100).as_deref(), Some("RX 100"));
    // Leading zeros keep the digits' width.
    let padded = names(&["001", "002", "003"]);
    assert_eq!(default_past(&padded, 4).as_deref(), Some("004"));
    assert_eq!(default_past(&padded, 1000).as_deref(), Some("1000"));
    let ten: Vec<String> = (1..=10).map(|n| format!("Out {n}")).collect();
    assert_eq!(default_past(&ten, 11).as_deref(), Some("Out 11"));
    // No numbering to carry on: no default is known.
    for list in [
        names(&[]),
        names(&["Kick", "Snare"]),
        names(&["RX 1", "RX 5"]),
        names(&["RX 2"]),
        names(&["99999999999999999999999"]),
    ] {
        assert_eq!(default_past(&list, 9), None, "{list:?}");
        // So a cut there counts as a rename, whatever the name.
        let mut longer = list.clone();
        longer.push("RX 9".into());
        assert_eq!(renamed_past(&longer, &list), Some(list.len() + 1));
    }
    // Defaults only past the count: nothing is renamed; fewer names, none.
    assert_eq!(
        renamed_past(&names(&["RX 1", "RX 2", "RX 3", "RX 4"]), &rx),
        None
    );
    assert_eq!(renamed_past(&names(&["RX 1"]), &rx), None);
    assert_eq!(
        renamed_past(&names(&["RX 1", "RX 2", "RX 3", "RX 4", "RX 6"]), &rx),
        Some(5)
    );
}

/// A log sink for [`logged`].
#[derive(Clone, Default)]
struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl io::Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// What `f` logs at INFO or above on this thread, as text.
fn logged(f: impl FnOnce()) -> String {
    let sink = Sink::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let bytes = sink.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn a_fitted_state_is_logged_once_by_its_counts_only() {
    let dir = tempfile::tempdir().unwrap();
    let phase_b = config_in(dir.path());
    let mut state = DeviceState::from_config(&phase_b);
    state.rx[2] = "zyxq 3".into();
    assert!(state.set_subscription(&phase_b, 4, Some(sub("zyxq", "zyxq-dev"))));
    state.save(&phase_b.state_file).unwrap();
    let phase_c = config_sized(dir.path(), 136, 96);
    let log = logged(|| {
        DeviceState::load_or_default(&phase_c).unwrap();
    });
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(
        log.contains("rx_before=32 rx_after=136 tx_before=16 tx_after=96"),
        "{log}"
    );
    assert!(!log.contains("zyxq"), "{log} names a name");
    assert!(!log.contains(&dir.path().display().to_string()), "{log}");
    // The next start fits nothing and logs nothing.
    let log = logged(|| {
        DeviceState::load_or_default(&phase_c).unwrap();
    });
    assert_eq!(log, "");
}
