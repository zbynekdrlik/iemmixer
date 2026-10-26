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
fn a_state_that_does_not_fit_the_config_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    let path = cfg.state_file.display().to_string();

    let mut fewer = DeviceState::from_config(&cfg);
    fewer.tx.pop();
    fewer.save(&cfg.state_file).unwrap();
    let err = DeviceState::load_or_default(&cfg).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let message = err.to_string();
    assert!(message.contains(&path), "{message}");
    assert!(
        message.contains("15 TX channels, the config 16"),
        "{message}"
    );

    let mut more = DeviceState::from_config(&cfg);
    more.rx.push("RX 33".into());
    more.save(&cfg.state_file).unwrap();
    let message = DeviceState::load_or_default(&cfg).unwrap_err().to_string();
    assert!(message.contains(&path), "{message}");
    assert!(
        message.contains("33 RX channels, the config 32"),
        "{message}"
    );
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
    ];
    for (text, want) in cases {
        let found = problems(&text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with(want), "{found:?} lacks {want:?}");
    }
    // The edges that are allowed.
    for (line, ok) in [
        ("latency_ns =", "latency_ns = 4294967295"),
        ("latency_ns =", "latency_ns = 1"),
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
