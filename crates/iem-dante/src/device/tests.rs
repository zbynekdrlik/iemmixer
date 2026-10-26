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
