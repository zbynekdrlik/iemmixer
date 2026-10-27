use super::*;

use std::cell::RefCell;

use crate::device::DeviceConfig;

const TEST_DEVICE: &str = include_str!("../../../../config/test-dante-device.toml");
const HOST: &str = "test-device.local.";
/// A synthetic address (public repository, P6).
const IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 10);

fn config() -> DeviceConfig {
    DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-dante-device.toml")
}

fn chan_records(recs: &[ServiceRecord]) -> Vec<&ServiceRecord> {
    recs.iter().filter(|r| r.service == CHAN_SERVICE).collect()
}

fn has(rec: &ServiceRecord, entry: &str) -> bool {
    rec.txt.iter().any(|t| t == entry)
}

#[test]
fn one_chan_record_per_tx_channel_named_channel_at_device() {
    let cfg = config();
    let state = DeviceState::from_config(&cfg);
    let recs = records(&state, &cfg, HOST, IP);
    assert_eq!(recs.len(), 2 + 16);
    let chans = chan_records(&recs);
    assert_eq!(chans.len(), 16);
    for (i, rec) in chans.iter().enumerate() {
        let n = i + 1;
        assert_eq!(rec.instance, format!("TX {n}@test-device"));
        assert_eq!(rec.port, FLOWS_PORT);
        assert_eq!((rec.host.as_str(), rec.ip), (HOST, IP));
        assert!(has(rec, &format!("id={n}")), "{:?}", rec.txt);
        assert!(has(rec, "default"), "a default name is marked so");
    }
    let arc = &recs[0];
    assert_eq!(
        (arc.service, arc.instance.as_str(), arc.port),
        (ARC_SERVICE, "test-device", 4440)
    );
    let cmc = &recs[1];
    assert_eq!(
        (cmc.service, cmc.instance.as_str(), cmc.port),
        (CMC_SERVICE, "test-device", 8800)
    );
    for rec in [arc, cmc] {
        assert_eq!((rec.host.as_str(), rec.ip), (HOST, IP));
    }
}

#[test]
fn a_rename_changes_the_chan_records() {
    let cfg = config();
    let mut state = DeviceState::from_config(&cfg);
    state.tx[2] = "Mix 3".into();
    let recs = records(&state, &cfg, HOST, IP);
    let chans = chan_records(&recs);
    // The default name keeps its record (a subscription by it still finds
    // the channel); the new name gets one more.
    assert_eq!(chans.len(), 17);
    let default = chans
        .iter()
        .find(|r| r.instance == "TX 3@test-device")
        .unwrap();
    assert!(has(default, "default") && has(default, "id=3"));
    let renamed = chans
        .iter()
        .find(|r| r.instance == "Mix 3@test-device")
        .unwrap();
    assert!(has(renamed, "id=3"));
    assert!(!has(renamed, "default"));
    assert_eq!(renamed.port, FLOWS_PORT);
    // Every other channel is as before.
    let names: Vec<&str> = chans.iter().map(|r| r.instance.as_str()).collect();
    assert_eq!(
        names[..4],
        [
            "TX 1@test-device",
            "TX 2@test-device",
            "TX 3@test-device",
            "Mix 3@test-device"
        ]
    );
    assert_eq!(names[16], "TX 16@test-device");

    // A name that differs only in case is the same mDNS name: no second record.
    state.tx[2] = "tx 3".into();
    assert_eq!(chan_records(&records(&state, &cfg, HOST, IP)).len(), 16);
    // The device's name is in every record.
    state.name = "other-device".into();
    let recs = records(&state, &cfg, HOST, IP);
    assert!(recs[..2].iter().all(|r| r.instance == "other-device"));
    assert!(
        chan_records(&recs)
            .iter()
            .all(|r| r.instance.ends_with("@other-device"))
    );
}

#[test]
fn the_txt_records_carry_the_rate_encoding_and_latency() {
    let cfg = config();
    let state = DeviceState::from_config(&cfg);
    let recs = records(&state, &cfg, HOST, IP);
    assert_eq!(
        recs[0].txt,
        [
            "arcp_vers=2.7.41",
            "arcp_min=0.2.4",
            "router_vers=4.0.2",
            "router_info=iem-dante",
            "mf=iemmixer",
            "model=_000000000000000b",
        ]
    );
    assert_eq!(
        recs[1].txt,
        [
            "id=00000a00000a0000",
            "process=0",
            "cmcp_vers=1.2.0",
            "cmcp_min=1.0.0",
            "server_vers=4.0.2",
            "channels=0x6000004d",
            "mf=iemmixer",
            "model=_000000000000000b",
            "",
            "",
        ]
    );
    assert_eq!(
        recs[2].txt,
        [
            "txtvers=2",
            "dbcp1=0x1102",
            "dbcp=0x1004",
            "id=1",
            "rate=96000",
            "pcm=3 e",
            "enc=24",
            "en=24",
            "latency_ns=1000000",
            "fpp=32,2",
            "nchan=8",
            "default",
        ]
    );
    // Another device: the entries follow its config.
    let other = DeviceConfig {
        sample_rate: 48_000,
        encoding: 16,
        latency_ns: 5_000_000,
        tx: vec!["A".into(), "B".into(), "C".into()],
        manufacturer: "zyxmaker".into(),
        model: "zyxboard".into(),
        ..cfg
    };
    let state = DeviceState::from_config(&other);
    let recs = records(&state, &other, HOST, Ipv4Addr::new(192, 0, 2, 1));
    let c = &recs[4];
    assert_eq!(c.instance, "C@test-device");
    for entry in [
        "id=3",
        "rate=48000",
        "pcm=2 e",
        "enc=16",
        "en=16",
        "latency_ns=5000000",
        "nchan=3",
    ] {
        assert!(has(c, entry), "{entry} not in {:?}", c.txt);
    }
    assert!(has(&recs[0], "router_info=zyxboard") && has(&recs[0], "mf=zyxmaker"));
    // The TX settings: the advertised latency and the most fpp.
    let tight = DeviceConfig {
        tx_latency_ns: Some(250_000),
        tx_fpp_max: 16,
        ..other.clone()
    };
    let recs = records(&state, &tight, HOST, Ipv4Addr::new(192, 0, 2, 1));
    for entry in ["latency_ns=250000", "fpp=16,2"] {
        assert!(has(&recs[4], entry), "{entry} not in {:?}", recs[4].txt);
    }
    assert!(!has(&recs[4], "latency_ns=5000000"), "not the RX latency");
    // The device id holds the address (192.0.2.1) after two zero bytes.
    assert!(has(&recs[1], "id=0000c00002010000"), "{:?}", recs[1].txt);
}

#[test]
fn a_record_becomes_the_service_info_mdns_sd_sends() {
    let cfg = config();
    let state = DeviceState::from_config(&cfg);
    let recs = records(&state, &cfg, HOST, IP);
    let info = recs[2].info().unwrap();
    assert_eq!(
        info.get_fullname(),
        "TX 1@test-device._netaudio-chan._udp.local."
    );
    assert_eq!(info.get_type(), CHAN_SERVICE);
    assert_eq!(info.get_hostname(), HOST);
    assert_eq!(info.get_port(), FLOWS_PORT);
    assert_eq!(
        info.get_addresses_v4()
            .into_iter()
            .copied()
            .collect::<Vec<_>>(),
        [IP]
    );
    // Every TXT entry in its order: key=value, a bare key, and the cmc
    // record's two empty strings, as inferno sends them.
    let props: Vec<(String, Option<Vec<u8>>)> = info
        .get_properties()
        .iter()
        .map(|p| (p.key().to_string(), p.val().map(<[u8]>::to_vec)))
        .collect();
    assert_eq!(props.len(), recs[2].txt.len());
    assert_eq!(props[0], ("txtvers".to_string(), Some(b"2".to_vec())));
    assert_eq!(props[9], ("fpp".to_string(), Some(b"32,2".to_vec())));
    assert_eq!(props[11], ("default".to_string(), None));
    let cmc = recs[1].info().unwrap();
    assert_eq!(cmc.get_fullname(), "test-device._netaudio-cmc._udp.local.");
    let last: Vec<(String, Option<Vec<u8>>)> = cmc
        .get_properties()
        .iter()
        .skip(8)
        .map(|p| (p.key().to_string(), p.val().map(<[u8]>::to_vec)))
        .collect();
    assert_eq!(last, [(String::new(), None), (String::new(), None)]);
    // A value with '=' in it keeps it: only the first '=' splits.
    let rec = ServiceRecord {
        txt: vec!["k=a=b".into()],
        ..recs[0].clone()
    };
    assert_eq!(rec.info().unwrap().get_property_val_str("k"), Some("a=b"));
}

/// What the announcer asked of the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Register(String),
    Unregister(String),
}

/// The daemon's place in the tests: records the calls, fails on request.
#[derive(Default)]
struct FakeRegistry {
    calls: RefCell<Vec<Call>>,
    /// A fullname whose registration fails.
    refuse: RefCell<Option<String>>,
}

impl Registry for FakeRegistry {
    fn register(&self, info: ServiceInfo) -> Result<(), mdns_sd::Error> {
        let name = info.get_fullname().to_string();
        if self.refuse.borrow().as_deref() == Some(name.as_str()) {
            return Err(mdns_sd::Error::Msg("refused by the test".into()));
        }
        self.calls.borrow_mut().push(Call::Register(name));
        Ok(())
    }

    fn unregister(&self, fullname: &str) -> Result<(), mdns_sd::Error> {
        self.calls
            .borrow_mut()
            .push(Call::Unregister(fullname.to_string()));
        Ok(())
    }
}

fn take(a: &Announcer<FakeRegistry>) -> Vec<Call> {
    a.registry().calls.borrow_mut().drain(..).collect()
}

fn chan(name: &str) -> String {
    format!("{name}@test-device._netaudio-chan._udp.local.")
}

#[test]
fn an_announcer_withdraws_the_old_records_first_and_repeats_nothing() {
    let cfg = config();
    let mut state = DeviceState::from_config(&cfg);
    let mut a = Announcer::new(FakeRegistry::default());
    a.announce(&records(&state, &cfg, HOST, IP)).unwrap();
    let calls = take(&a);
    assert_eq!(calls.len(), 18);
    assert_eq!(
        calls[0],
        Call::Register("test-device._netaudio-arc._udp.local.".into())
    );
    assert_eq!(
        calls[1],
        Call::Register("test-device._netaudio-cmc._udp.local.".into())
    );
    assert_eq!(calls[2], Call::Register(chan("TX 1")));
    // The same set again: nothing to send.
    a.announce(&records(&state, &cfg, HOST, IP)).unwrap();
    assert_eq!(take(&a), []);

    // A rename: one record more.
    state.tx[2] = "Mix 3".into();
    a.announce(&records(&state, &cfg, HOST, IP)).unwrap();
    assert_eq!(take(&a), [Call::Register(chan("Mix 3"))]);
    // Renamed again: the old record goes first, then the new one comes.
    state.tx[2] = "Mix 3b".into();
    a.announce(&records(&state, &cfg, HOST, IP)).unwrap();
    assert_eq!(
        take(&a),
        [
            Call::Unregister(chan("mix 3")),
            Call::Register(chan("Mix 3b"))
        ]
    );
    // Back to the default name: only a withdrawal.
    state.tx[2] = "TX 3".into();
    a.announce(&records(&state, &cfg, HOST, IP)).unwrap();
    assert_eq!(take(&a), [Call::Unregister(chan("mix 3b"))]);

    // A changed record with the same name is sent again; the others are not.
    let mut changed = records(&state, &cfg, HOST, IP);
    changed[0].txt.push("extra=1".into());
    a.announce(&changed).unwrap();
    assert_eq!(
        take(&a),
        [Call::Register(
            "test-device._netaudio-arc._udp.local.".into()
        )]
    );

    // All withdrawn, every unregistration before any registration.
    state.name = "other-device".into();
    a.announce(&records(&state, &cfg, HOST, IP)).unwrap();
    let calls = take(&a);
    assert_eq!(calls.len(), 36);
    assert!(calls[..18].iter().all(|c| matches!(c, Call::Unregister(_))));
    assert!(
        calls[18..]
            .iter()
            .all(|c| matches!(c, Call::Register(n) if n.contains("other-device")))
    );
}

#[test]
fn a_failed_announcement_is_tried_again_and_a_bad_record_sends_nothing() {
    let cfg = config();
    let mut state = DeviceState::from_config(&cfg);
    let mut a = Announcer::new(FakeRegistry::default());
    a.announce(&records(&state, &cfg, HOST, IP)).unwrap();
    take(&a);
    state.tx[0] = "Mix 1".into();
    *a.registry().refuse.borrow_mut() = Some(chan("Mix 1"));
    let err = a.announce(&records(&state, &cfg, HOST, IP)).unwrap_err();
    let message = err.to_string();
    assert!(message.contains(CHAN_SERVICE), "{message}");
    assert!(!message.contains("Mix 1"), "{message} names the channel");
    // The record that failed is tried again by the next announcement.
    *a.registry().refuse.borrow_mut() = None;
    a.announce(&records(&state, &cfg, HOST, IP)).unwrap();
    assert_eq!(take(&a), [Call::Register(chan("Mix 1"))]);

    // A record mdns-sd cannot build (a TXT entry over 255 bytes): an error,
    // and nothing is sent, not even the withdrawals.
    state.tx[0] = "TX 1".into();
    let mut bad = records(&state, &cfg, HOST, IP);
    bad[3].txt.push("x".repeat(300));
    assert!(a.announce(&bad).is_err());
    assert_eq!(take(&a), []);
    // Two records with one name: refused before anything is sent.
    let mut twice = records(&state, &cfg, HOST, IP);
    twice.push(twice[2].clone());
    assert!(a.announce(&twice).is_err());
    assert_eq!(take(&a), []);
}

#[test]
fn hex_is_lower_case_two_digits_a_byte() {
    assert_eq!(hex(&[0x00, 0x0a, 0xff, 0x10]), "000aff10");
    assert_eq!(hex(&[]), "");
}
