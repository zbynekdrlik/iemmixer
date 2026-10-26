use super::*;

use std::collections::HashMap;
use std::sync::mpsc;

use mdns_sd::{ResolvedService, ServiceInfo};

use crate::device::DeviceConfig;

const TEST_DEVICE: &str = include_str!("../../../../../config/test-dante-device.toml");
const PEER_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 11);
const OUR_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 10);

fn cfg() -> DeviceConfig {
    DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-dante-device.toml")
}

/// A channel record as a Dante transmitter writes its TXT (inferno's keys).
fn txt(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn peer_txt() -> HashMap<String, String> {
    txt(&[
        ("txtvers", "2"),
        ("dbcp1", "0x1102"),
        ("dbcp", "0x1004"),
        ("id", "3"),
        ("rate", "48000"),
        ("pcm", "3"),
        ("enc", "24"),
        ("en", "24"),
        ("latency_ns", "1000000"),
        ("fpp", "16,4"),
        ("nchan", "4"),
    ])
}

fn parsed(t: &HashMap<String, String>) -> Result<Advertised, AdvertError> {
    parse(
        |k| t.get(k).map(String::as_str),
        SocketAddrV4::new(PEER_IP, 4455),
    )
}

#[test]
fn a_channel_record_reads_as_inferno_reads_it() {
    let a = parsed(&peer_txt()).unwrap();
    assert_eq!(
        a,
        Advertised {
            control: SocketAddrV4::new(PEER_IP, 4455),
            tx_channel_id: 3,
            nchan: 4,
            bits: 24,
            rate: 48_000,
            dbcp1: 0x1102,
            fpp_min: 4,
            fpp_max: 16,
            latency_ns: 1_000_000,
        }
    );
    // `en` when `enc` is missing; decimal or 0x hex anywhere.
    let mut t = peer_txt();
    t.remove("enc");
    t.insert("en".into(), "0x20".into());
    t.insert("id".into(), "0x10".into());
    let a = parsed(&t).unwrap();
    assert_eq!((a.bits, a.tx_channel_id), (32, 16));
}

#[test]
fn a_record_missing_or_mangling_a_key_is_refused_naming_the_key() {
    for key in ["id", "nchan", "rate", "dbcp1", "fpp", "latency_ns"] {
        let mut t = peer_txt();
        t.remove(key);
        assert_eq!(parsed(&t), Err(AdvertError::Missing(key)), "{key}");
    }
    let mut t = peer_txt();
    t.remove("enc");
    t.remove("en");
    assert_eq!(parsed(&t), Err(AdvertError::Missing("enc")));
    for (key, value) in [
        ("id", "0"),
        ("id", "65536"),
        ("id", "x"),
        ("nchan", "0"),
        ("nchan", "70000"),
        ("rate", "0"),
        ("rate", "-1"),
        ("enc", "20"),
        ("enc", ""),
        ("dbcp1", "0x"),
        ("dbcp1", "0x10000"),
        ("fpp", "16"),
        ("fpp", "16,x"),
        ("fpp", "0,0"),
        ("latency_ns", "1e6"),
    ] {
        let mut t = peer_txt();
        t.insert(key.into(), value.into());
        assert_eq!(parsed(&t), Err(AdvertError::Invalid(key)), "{key}={value}");
    }
    assert_eq!(
        AdvertError::Invalid("fpp").to_string(),
        "the channel record's fpp is not valid"
    );
}

/// Our own device's channel records, resolved as mdns-sd hands them over.
fn our_records(state: &DeviceState, cfg: &DeviceConfig) -> Vec<ResolvedService> {
    crate::announce::records(state, cfg, "test-device.local.", OUR_IP)
        .iter()
        .filter(|r| r.service == CHAN_SERVICE)
        .map(|r| r.info().unwrap().as_resolved_service())
        .collect()
}

#[test]
fn our_own_records_resolve_to_our_flow_control() {
    let cfg = cfg();
    let mut state = DeviceState::from_config(&cfg);
    state.tx[1] = "Mix 2".to_string();
    let records = our_records(&state, &cfg);
    let (name, a) = from_resolved(&records[0]).unwrap();
    assert_eq!(name, "tx 1@test-device._netaudio-chan._udp.local.");
    assert_eq!(
        a,
        Advertised {
            control: SocketAddrV4::new(OUR_IP, 4455),
            tx_channel_id: 1,
            nchan: 8,
            bits: 24,
            rate: 96_000,
            dbcp1: 0x1102,
            fpp_min: 2,
            fpp_max: 32,
            latency_ns: 1_000_000,
        }
    );
    // Own: by either name of a channel, case aside; another device's not.
    let own = Own::new(&state, &cfg, OUR_IP);
    assert_eq!(own.lookup("test-device", "TX 2").unwrap().tx_channel_id, 2);
    assert_eq!(own.lookup("TEST-device", "mix 2").unwrap().tx_channel_id, 2);
    assert_eq!(
        own.lookup("test-device", "TX 16").unwrap().tx_channel_id,
        16
    );
    assert_eq!(own.lookup("test-device", "TX 17"), None);
    assert_eq!(own.lookup("peer-device", "TX 1"), None);
}

#[test]
fn a_record_without_an_ipv4_address_is_refused() {
    let info = ServiceInfo::new(
        CHAN_SERVICE,
        "TX 1@peer-device",
        "peer-device.local.",
        "",
        4455,
        None,
    )
    .unwrap();
    let r = info.as_resolved_service();
    assert_eq!(from_resolved(&r), Err(AdvertError::NoAddress));
}

#[test]
fn several_addresses_resolve_to_the_lowest() {
    let info = ServiceInfo::new(
        CHAN_SERVICE,
        "TX 3@peer-device",
        "peer-device.local.",
        "10.0.0.12,10.0.0.11",
        4455,
        peer_txt(),
    )
    .unwrap();
    let (_, a) = from_resolved(&info.as_resolved_service()).unwrap();
    assert_eq!(a.control, SocketAddrV4::new(PEER_IP, 4455));
}

fn resolved_event(instance: &str, ip: &str) -> ServiceEvent {
    let info = ServiceInfo::new(
        CHAN_SERVICE,
        instance,
        "peer-device.local.",
        ip,
        4455,
        peer_txt(),
    )
    .unwrap();
    ServiceEvent::ServiceResolved(Box::new(info.as_resolved_service()))
}

#[test]
fn the_browse_s_events_keep_the_records_case_aside() {
    let mut a = Adverts::default();
    assert_eq!(
        a.apply(&resolved_event("TX 3@Peer-Device", "10.0.0.11")),
        Applied::Resolved
    );
    assert_eq!(a.len(), 1);
    assert_eq!(a.get("peer-device", "tx 3").unwrap().control.ip(), &PEER_IP);
    // A newer record replaces the older (a transmitter that came back).
    a.apply(&resolved_event("TX 3@peer-device", "10.0.0.12"));
    assert_eq!(a.len(), 1);
    assert_eq!(
        a.get("peer-device", "TX 3").unwrap().control.ip(),
        &Ipv4Addr::new(10, 0, 0, 12)
    );
    // Removed: gone (a transmitter's goodbye).
    let gone = ServiceEvent::ServiceRemoved(
        CHAN_SERVICE.to_string(),
        "tx 3@PEER-device._netaudio-chan._udp.local.".to_string(),
    );
    assert_eq!(a.apply(&gone), Applied::Removed);
    assert_eq!(a.get("peer-device", "TX 3"), None);
    // Other events, other services and broken records change nothing.
    let mut t = peer_txt();
    t.remove("id");
    let broken = ServiceInfo::new(
        CHAN_SERVICE,
        "TX 4@peer-device",
        "peer-device.local.",
        "10.0.0.11",
        4455,
        t,
    )
    .unwrap();
    // Another service, even with a channel record's TXT.
    let other = ServiceInfo::new(
        "_netaudio-arc._udp.local.",
        "TX 3@peer-device",
        "peer-device.local.",
        "10.0.0.11",
        4440,
        peer_txt(),
    )
    .unwrap();
    for ev in [
        ServiceEvent::ServiceResolved(Box::new(broken.as_resolved_service())),
        ServiceEvent::ServiceResolved(Box::new(other.as_resolved_service())),
        ServiceEvent::SearchStarted(CHAN_SERVICE.to_string()),
        ServiceEvent::ServiceFound(CHAN_SERVICE.to_string(), "x".to_string()),
        ServiceEvent::SearchStopped(CHAN_SERVICE.to_string()),
        gone,
    ] {
        assert_eq!(a.apply(&ev), Applied::Ignored, "{ev:?}");
    }
    assert_eq!(a.len(), 0);
    assert!(a.is_empty());
    a.apply(&resolved_event("TX 3@peer-device", "10.0.0.11"));
    assert!(!a.is_empty());
}

#[test]
fn the_records_kept_are_bounded() {
    let mut a = Adverts::default();
    for i in 0..MAX_ADVERTS {
        assert_eq!(
            a.apply(&resolved_event(&format!("TX {i}@peer-device"), "10.0.0.11")),
            Applied::Resolved
        );
    }
    assert_eq!(a.len(), MAX_ADVERTS);
    // A new one past the bound is dropped; a known one is still updated.
    assert_eq!(
        a.apply(&resolved_event("TX new@peer-device", "10.0.0.11")),
        Applied::Ignored
    );
    assert_eq!(
        a.apply(&resolved_event("TX 0@peer-device", "10.0.0.12")),
        Applied::Resolved
    );
    assert_eq!(a.len(), MAX_ADVERTS);
}

/// The mDNS daemon's browse, as a test sees it: each call hands over a
/// channel's receiving end and counts.
struct FakeDaemon {
    browses: std::cell::Cell<usize>,
    events: std::cell::RefCell<Option<mpsc::Receiver<ServiceEvent>>>,
}

impl Browse for FakeDaemon {
    type Events = mpsc::IntoIter<ServiceEvent>;
    fn browse(&self, service: &str) -> Result<Self::Events, mdns_sd::Error> {
        assert_eq!(service, CHAN_SERVICE);
        self.browses.set(self.browses.get() + 1);
        self.events
            .borrow_mut()
            .take()
            .map(mpsc::Receiver::into_iter)
            .ok_or_else(|| mdns_sd::Error::Msg("browsed twice".to_string()))
    }
}

#[test]
fn the_browse_starts_once_and_fills_the_records_until_the_daemon_ends() {
    let (send, events) = mpsc::channel();
    let daemon = FakeDaemon {
        browses: std::cell::Cell::new(0),
        events: std::cell::RefCell::new(Some(events)),
    };
    let mut mdns = Mdns::new();
    assert!(!mdns.started());
    assert_eq!(mdns.lookup("peer-device", "TX 3"), None);
    assert!(mdns.start(&daemon).unwrap());
    // At most one browse for the device's life.
    assert!(!mdns.start(&daemon).unwrap());
    assert!(mdns.started());
    assert_eq!(daemon.browses.get(), 1);
    send.send(resolved_event("TX 3@peer-device", "10.0.0.11"))
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while mdns.lookup("peer-device", "TX 3").is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the record never came"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(mdns.lookup("peer-device", "tx 3").unwrap().tx_channel_id, 3);
    // The daemon's end ends the thread (its sender dropped): nothing hangs.
    drop(send);
    mdns.join_within(std::time::Duration::from_secs(5));
}

#[test]
fn own_first_then_mdns() {
    let cfg = cfg();
    let state = DeviceState::from_config(&cfg);
    let own = Own::new(&state, &cfg, OUR_IP);
    let mut peers = Adverts::default();
    peers.apply(&resolved_event("TX 3@peer-device", "10.0.0.11"));
    // A peer that also calls itself by our name loses to our own record.
    peers.apply(&resolved_event("TX 1@test-device", "10.0.0.11"));
    let both = (own, peers);
    assert_eq!(
        both.lookup("test-device", "TX 1").unwrap().control.ip(),
        &OUR_IP
    );
    assert_eq!(
        both.lookup("peer-device", "TX 3").unwrap().control.ip(),
        &PEER_IP
    );
    assert_eq!(both.lookup("peer-device", "TX 9"), None);
}

#[test]
fn instance_names_are_dante_s_case_aside() {
    assert_eq!(
        instance("Peer-Device", "TX 3"),
        "tx 3@peer-device._netaudio-chan._udp.local."
    );
}

#[test]
fn a_device_is_named_while_any_of_its_channel_records_is_kept() {
    let mut a = Adverts::default();
    assert!(!a.names_device("peer-device"));
    a.apply(&resolved_event("TX 3@Peer-Device", "10.0.0.11"));
    a.apply(&resolved_event("TX 4@peer-device", "10.0.0.11"));
    // A record again (refreshed) is still one record of the device.
    a.apply(&resolved_event("TX 3@peer-device", "10.0.0.12"));
    assert!(a.names_device("PEER-device"));
    assert!(!a.names_device("other-device"));
    assert!(!a.names_device("peer"));
    let gone = |channel: &str| {
        ServiceEvent::ServiceRemoved(
            CHAN_SERVICE.to_string(),
            format!("{channel}@peer-device.{CHAN_SERVICE}"),
        )
    };
    assert_eq!(a.apply(&gone("TX 3")), Applied::Removed);
    assert!(a.names_device("peer-device"));
    assert_eq!(a.apply(&gone("TX 3")), Applied::Ignored);
    assert!(a.names_device("peer-device"));
    assert_eq!(a.apply(&gone("TX 4")), Applied::Removed);
    assert!(!a.names_device("peer-device"));
    // The device is the name's part after its '@', before the service.
    assert_eq!(
        device_of(&instance("peer-device", "TX 1")),
        Some("peer-device")
    );
    assert_eq!(
        device_of("tx 1@peer-device._netaudio-arc._udp.local."),
        None
    );
    assert_eq!(device_of(&format!("peer-device.{CHAN_SERVICE}")), None);
    assert_eq!(device_of(&format!("x@peer-device{CHAN_SERVICE}")), None);
}

#[test]
fn mdns_names_a_device_its_browse_found() {
    let (send, events) = mpsc::channel();
    let daemon = FakeDaemon {
        browses: std::cell::Cell::new(0),
        events: std::cell::RefCell::new(Some(events)),
    };
    let mut mdns = Mdns::new();
    assert!(!mdns.names_device("peer-device"));
    mdns.start(&daemon).unwrap();
    send.send(resolved_event("TX 3@peer-device", "10.0.0.11"))
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !mdns.names_device("peer-device") {
        assert!(std::time::Instant::now() < deadline, "never named");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    drop(send);
    mdns.join_within(std::time::Duration::from_secs(5));
}
