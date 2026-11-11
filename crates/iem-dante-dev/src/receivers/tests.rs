// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

use super::*;
use iem_dante::latency::SlotLatency;
use std::net::SocketAddrV4;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn opts(seconds: u32) -> Option<Options> {
    Some(Options {
        ip: Ipv4Addr::new(10, 0, 0, 10),
        seconds,
    })
}

#[test]
fn receivers_takes_an_ip_and_a_bounded_listen() {
    assert_eq!(parse(&args(&["--ip", "10.0.0.10"])), Ok(opts(5)));
    assert_eq!(
        parse(&args(&["--seconds", "1", "--ip", "10.0.0.10"])),
        Ok(opts(1))
    );
    assert_eq!(
        parse(&args(&["--ip", "10.0.0.10", "--seconds", "60"])),
        Ok(opts(60))
    );
    assert_eq!(parse(&args(&["--ip", "10.0.0.10", "-h"])), Ok(None));
    assert_eq!(parse(&args(&["--help"])), Ok(None));
    for bad in [
        &[][..],
        &["--seconds", "5"],
        &["--ip", "10.0.0"],
        &["--ip", "10.0.0.10", "--seconds", "0"],
        &["--ip", "10.0.0.10", "--seconds", "61"],
        &["--ip", "10.0.0.10", "--seconds", "x"],
        &["--ip", "10.0.0.10", "--ip", "10.0.0.11"],
        &["--ip"],
        &["--ip", "10.0.0.10", "--wat"],
    ] {
        assert!(parse(&args(bad)).is_err(), "{bad:?}");
    }
}

#[test]
fn the_usage_names_the_command_and_its_bounds() {
    let u = usage("iem-dante-dev");
    assert!(u.starts_with("usage: iem-dante-dev receivers --ip"), "{u}");
    assert!(u.contains("default 5, at most 60"), "{u}");
}

fn channel(n: u16, device: Option<&str>) -> RxChannel {
    RxChannel {
        channel: n,
        label: format!("in{n}"),
        tx_channel: device.map(|_| "out".to_string()),
        tx_device: device.map(str::to_string),
        status: 0,
    }
}

fn flow(slot: u16, latency_ns: u32, channels: &[u16]) -> RxFlow {
    RxFlow {
        slot,
        rate: 96_000,
        latency_ns,
        channels: channels.to_vec(),
    }
}

#[test]
fn our_flows_are_the_slots_that_feed_a_channel_subscribed_to_us() {
    let channels = [
        channel(1, Some("ZyxBox")),
        channel(2, Some("qxwother")),
        channel(3, None),
        channel(4, Some("zyxbox")),
        channel(5, Some("zyxbox-2")),
    ];
    let flows = [
        flow(7, 1_000_000, &[4]),
        flow(2, 250_000, &[1, 2]),
        flow(3, 500_000, &[2, 3]),
        flow(9, 2_000_000, &[5]),
        flow(2, 250_000, &[1]),
        flow(5, 250_000, &[]),
    ];
    assert_eq!(
        our_slots("zyxbox", &channels, &flows),
        vec![
            Ours {
                slot: 2,
                latency_ns: 250_000
            },
            Ours {
                slot: 7,
                latency_ns: 1_000_000
            },
        ]
    );
    assert_eq!(our_slots("nobody", &channels, &flows), vec![]);
}

fn target(id: u8, flows: &[(u16, u32)]) -> Target {
    Target {
        device_id: [id; 8],
        flows: flows
            .iter()
            .map(|&(slot, latency_ns)| Ours { slot, latency_ns })
            .collect(),
    }
}

fn slot(slot: u16, peaks_us: &[u32], late: u64, late_total: u32) -> SlotLatency {
    SlotLatency {
        slot,
        peaks_us: peaks_us.to_vec(),
        late,
        late_total,
    }
}

fn heard(id: u8, beats: u32, slots: Vec<SlotLatency>) -> Latency {
    Latency {
        device_id: [id; 8],
        beats,
        rate: 96_000,
        slots,
    }
}

#[test]
fn a_report_follows_each_receiver_s_own_slots() {
    let r = report(
        b"zyxsalt",
        6,
        1,
        2,
        &[
            target(1, &[(2, 250_000), (7, 1_000_000)]),
            target(2, &[(1, 500_000)]),
        ],
        &[
            heard(3, 5, vec![slot(2, &[900], 40, 40)]),
            heard(
                1,
                5,
                vec![slot(2, &[100, 180, 120], 0, 3), slot(1, &[999], 9, 9)],
            ),
        ],
    );
    let k1 = key_of(b"zyxsalt", &[1; 8]);
    let k2 = key_of(b"zyxsalt", &[2; 8]);
    assert_eq!(
        r,
        Report {
            devices: 6,
            unread: 1,
            no_id: 2,
            receivers: 2,
            heard: 1,
            flows: vec![
                FlowReport {
                    key: k1.clone(),
                    slot: 2,
                    latency_us: 250,
                    beats: 5,
                    peak_us: Some(180),
                    late: 0,
                    late_total: 3,
                },
                FlowReport {
                    key: k1,
                    slot: 7,
                    latency_us: 1_000,
                    beats: 5,
                    peak_us: None,
                    late: 0,
                    late_total: 0,
                },
                FlowReport {
                    key: k2,
                    slot: 1,
                    latency_us: 500,
                    beats: 0,
                    peak_us: None,
                    late: 0,
                    late_total: 0,
                },
            ],
            red: false,
        }
    );
}

#[test]
fn red_is_a_late_packet_in_the_listen_or_a_peak_past_the_latency() {
    let one = |s: SlotLatency| {
        report(
            b"zyxsalt",
            1,
            0,
            0,
            &[target(1, &[(2, 250_000)])],
            &[heard(1, 3, vec![s])],
        )
    };
    assert!(
        !one(slot(2, &[250], 0, 7)).red,
        "a peak at the latency is in time"
    );
    assert!(one(slot(2, &[251], 0, 7)).red, "a peak past it");
    assert!(one(slot(2, &[10], 1, 8)).red, "a late packet in the listen");
    assert!(!one(slot(2, &[], 0, 0)).red);
    assert_eq!(one(slot(2, &[10], 0, 0)).heard, 1);
    let silent = report(
        b"zyxsalt",
        1,
        0,
        0,
        &[target(1, &[(2, 250_000)])],
        &[heard(1, 0, vec![])],
    );
    assert_eq!(silent.heard, 0, "a device id with no beat is not heard");
}

#[test]
fn a_report_from_real_heartbeat_records_bins_the_receiver_s_own_numbers() {
    // Two heartbeats as a receiver sends them, read by dstack's parse: the
    // lateness record (rate 96 kHz, slot 2 at 48 then 24 samples) and the
    // late record (slot 2 at 5 then 6 since the device started).
    fn beat(seq: u16, lateness: [u32; 2], late: [u32; 2]) -> Vec<u8> {
        let mut d = vec![0u8; iem_dante::info::HEADER_LEN];
        d[0..2].copy_from_slice(&iem_dante::info::START_HEARTBEAT.to_be_bytes());
        d[8..16].copy_from_slice(&[4; 8]);
        d[24..32].copy_from_slice(&iem_dante::info::op::HEARTBEAT);
        let record = |kind: u16, head: &[u32], values: &[u32]| {
            let at = 20 + 4 * head.len();
            let len = at + 4 * values.len();
            let mut r = Vec::new();
            for v in [
                len as u16,
                kind,
                4,
                (len - 12) as u16,
                seq,
                0,
                values.len() as u16,
                0,
            ] {
                r.extend_from_slice(&v.to_be_bytes());
            }
            r.extend_from_slice(&(at as u16).to_be_bytes());
            r.extend_from_slice(&0u16.to_be_bytes());
            for v in head.iter().chain(values) {
                r.extend_from_slice(&v.to_be_bytes());
            }
            r
        };
        d.extend(record(iem_dante::latency::LATENESS, &[96_000], &lateness));
        d.extend(record(iem_dante::latency::LATE, &[], &late));
        d
    }
    let mut h = iem_dante::latency::Heard::default();
    for b in [beat(1, [0, 48], [0, 5]), beat(2, [0, 24], [0, 6])] {
        h.add(&iem_dante::latency::parse(&b).expect("a heartbeat"));
    }
    let r = report(
        b"zyxsalt",
        1,
        0,
        0,
        &[target(4, &[(2, 250_000)])],
        &h.result(),
    );
    assert_eq!(r.flows[0].peak_us, Some(500));
    assert_eq!(r.flows[0].late, 1);
    assert_eq!(r.flows[0].late_total, 6);
    assert_eq!(r.flows[0].beats, 2);
    assert!(r.red);
}

fn device(name: &str, last: u8, id: Option<u8>) -> Device {
    Device {
        name: name.to_string(),
        arc: SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, last), 4440),
        dbc: None,
        cmc: None,
        device_id: id.map(|i| [i; 8]),
        txt: Default::default(),
    }
}

#[test]
fn every_other_device_with_a_heartbeat_id_is_read() {
    let devices = [
        device("ZYXBOX", 10, Some(1)),
        device("qxwcard", 11, Some(2)),
        device("qxwrack", 12, None),
        device("qxwmic", 13, None),
        device("zyxbox-2", 14, Some(3)),
    ];
    let at = |last| SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, last), 4440);
    assert_eq!(
        candidates(&devices, "zyxbox"),
        (vec![(at(11), [2; 8]), (at(14), [3; 8])], 2)
    );
    assert_eq!(candidates(&[], "zyxbox"), (vec![], 0));
}

#[test]
fn only_a_device_with_a_channel_subscribed_to_us_has_its_flows_read() {
    let channels = [channel(1, Some("qxwother")), channel(2, Some("ZyxBox"))];
    assert!(subscribes(&channels, "zyxbox"));
    assert!(!subscribes(&channels[..1], "zyxbox"));
    assert!(!subscribes(&[channel(3, None)], "zyxbox"));
}

#[test]
fn a_key_is_sixteen_hex_digits_of_a_hash_never_the_id() {
    let k = key_of(b"zyxsalt", &[0xab; 8]);
    assert_eq!(k.len(), 16);
    assert!(k.chars().all(|c| c.is_ascii_hexdigit()), "{k}");
    assert_eq!(k, key_of(b"zyxsalt", &[0xab; 8]));
    assert_ne!(k, key_of(b"zyxsalt", &[0xac; 8]));
    assert!(!k.contains("abab"), "{k}");
    assert_ne!(k, key_of(b"zyxother", &[0xab; 8]), "another salt");
}

#[test]
fn the_line_is_numbers_and_keys_only() {
    let r = report(
        b"zyxsalt",
        2,
        0,
        0,
        &[target(1, &[(3, 1_000_000)])],
        &[heard(1, 4, vec![slot(3, &[120], 0, 2)])],
    );
    let key = key_of(b"zyxsalt", &[1; 8]);
    assert_eq!(
        r.line(),
        format!(
            "{{\"receivers\":{{\"devices\":2,\"unread\":0,\"no_id\":0,\"receivers\":1,\"heard\":1,\
             \"flows\":[{{\"key\":\"{key}\",\"slot\":3,\"latency_us\":1000,\"beats\":4,\"peak_us\":120,\
             \"late\":0,\"late_total\":2}}],\"red\":false}}}}"
        )
    );
}

#[test]
fn only_a_first_word_receivers_is_this_command() {
    assert_eq!(
        parse_command(&args(&["receivers", "--ip", "10.0.0.10"])),
        Some(Ok(opts(5)))
    );
    assert_eq!(
        parse_command(&args(&["receivers", "--help"])),
        Some(Ok(None))
    );
    for a in [
        &[][..],
        &["health"],
        &["ctl", "status"],
        &["Receivers"],
        &["--ip", "receivers"],
    ] {
        assert_eq!(parse_command(&args(a)), None, "{a:?}");
    }
}
