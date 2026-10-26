use super::*;

use std::cell::{Cell, RefCell};
use std::net::{SocketAddr, SocketAddrV4};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use mdns_sd::ServiceInfo;
use tokio::sync::oneshot;

use super::media::tests::{Unready, media_on, sub};
use crate::announce::CHAN_SERVICE;
use crate::arc::op;
use crate::device::DeviceConfig;
use crate::wire::{Header, STATUS_OK, request};

const TEST_DEVICE: &str = include_str!("../../../../config/test-dante-device.toml");
/// Synthetic addresses (public repository, P6), and a locally
/// administered MAC.
const IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 10);
const OTHER: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 11);
const MASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 0);
const GATEWAY: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);
const MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x0a];
/// The tests' bound on every wait, so a mutant that hangs fails fast.
const BOUND: Duration = Duration::from_secs(5);

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn run(config: &str, place: Place) -> Command {
    Command::Run(Options {
        config: PathBuf::from(config),
        place,
        control: PathBuf::from("/run/iem-dante/control.sock"),
    })
}

#[test]
fn a_run_takes_another_control_socket() {
    let want = Command::Run(Options {
        config: PathBuf::from("c"),
        place: Place::Iface("eth9".into()),
        control: PathBuf::from("/tmp/x/control.sock"),
    });
    assert_eq!(
        parse_args(&args(&[
            "--control",
            "/tmp/x/control.sock",
            "--config",
            "c",
            "--iface",
            "eth9"
        ])),
        Ok(want)
    );
    for (a, want) in [
        (
            &["--config", "c", "--iface", "e", "--control"][..],
            "--control needs a value",
        ),
        (
            &["--config", "c", "--iface", "e", "--control", ""],
            "--control needs a path",
        ),
        (
            &[
                "--config",
                "c",
                "--iface",
                "e",
                "--control",
                "a",
                "--control",
                "b",
            ],
            "given twice",
        ),
    ] {
        let got = parse_args(&args(a)).expect_err(&format!("{a:?} is refused"));
        assert!(got.contains(want), "{a:?}: {got}");
    }
}

#[test]
fn ctl_takes_a_socket_and_the_request_s_words() {
    let ctl = |socket: &str, request: &str| Command::Ctl {
        socket: PathBuf::from(socket),
        request: request.into(),
    };
    let default = "/run/iem-dante/control.sock";
    for (a, want) in [
        (&["ctl", "status"][..], ctl(default, "status")),
        (
            &["ctl", "source", "1", "sine", "1000", "-20"],
            ctl(default, "source 1 sine 1000 -20"),
        ),
        (
            &["ctl", "--socket", "/tmp/c.sock", "check", "2", "counter"],
            ctl("/tmp/c.sock", "check 2 counter"),
        ),
        // The daemon judges the words, not the client.
        (
            &["ctl", "bogus", "--version"],
            ctl(default, "bogus --version"),
        ),
        (&["ctl", "--help", "status"], Command::Help),
        (&["ctl", "-h"], Command::Help),
    ] {
        assert_eq!(parse_args(&args(a)), Ok(want), "{a:?}");
    }
    for (a, want) in [
        (&["ctl"][..], "the request is missing"),
        (
            &["ctl", "--socket", "/tmp/c.sock"],
            "the request is missing",
        ),
        (&["ctl", "--socket"], "--socket needs a path"),
        (&["ctl", "--socket", "", "status"], "--socket needs a path"),
    ] {
        let got = parse_args(&args(a)).expect_err(&format!("{a:?} is refused"));
        assert!(got.contains(want), "{a:?}: {got}");
    }
}

#[test]
fn a_ctl_answer_succeeds_when_it_is_ok_or_the_status() {
    for good in ["ok", "ok\n", "{\"clock\":{}}", "{}\n"] {
        assert!(ctl_succeeded(good), "{good:?}");
    }
    for bad in [
        "",
        "\n",
        "error: unknown request",
        "okay",
        " ok",
        "error: ok",
    ] {
        assert!(!ctl_succeeded(bad), "{bad:?}");
    }
}

#[test]
fn the_version_line_names_the_workspace_version_and_the_build() {
    assert_eq!(version_line(), format!("iem-dante-dev {VERSION} {BUILD}"));
    assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    match option_env!("GITHUB_SHA") {
        Some(sha) => assert_eq!(BUILD, sha),
        None => assert_eq!(BUILD, "local"),
    }
}

#[test]
fn a_run_names_its_config_and_one_place() {
    let cfg = "/etc/iemmixer/dante-device.toml";
    assert_eq!(
        parse_args(&args(&["--config", cfg, "--iface", "eth9"])),
        Ok(run(cfg, Place::Iface("eth9".into())))
    );
    assert_eq!(
        parse_args(&args(&["--ip", "10.0.0.10", "--config", cfg])),
        Ok(run(cfg, Place::Ip(IP)))
    );
}

#[test]
fn version_and_help_win_wherever_they_stand() {
    for a in [
        &["--version"][..],
        &["-V"],
        &["--config", "x", "--version"],
        &["--version", "--bogus"],
    ] {
        assert_eq!(parse_args(&args(a)), Ok(Command::Version), "{a:?}");
    }
    for a in [&["--help"][..], &["-h"], &["--iface", "eth9", "--help"]] {
        assert_eq!(parse_args(&args(a)), Ok(Command::Help), "{a:?}");
    }
}

#[test]
fn a_bad_command_line_is_a_usage_error() {
    let cases: &[(&[&str], &str)] = &[
        (&[], "--config is missing"),
        (&["--iface", "eth9"], "--config is missing"),
        (&["--config", "c"], "one of --iface and --ip is missing"),
        (
            &["--config", "c", "--iface", "eth9", "--ip", "10.0.0.10"],
            "together",
        ),
        (&["--config", "c", "--iface", ""], "needs an interface name"),
        (&["--config"], "--config needs a value"),
        (&["--config", "c", "--ip"], "--ip needs a value"),
        (
            &["--config", "c", "--config", "d", "--iface", "e"],
            "given twice",
        ),
        (
            &["--config", "c", "--iface", "e", "--iface", "f"],
            "given twice",
        ),
        (
            &["--config", "c", "--ip", "10.0.0.10", "--ip", "10.0.0.11"],
            "given twice",
        ),
        (
            &["--config", "c", "--iface", "e", "--port", "1"],
            "unknown argument \"--port\"",
        ),
        (&["c"], "unknown argument"),
        (&["--config", "c", "--ip", "10.0.0"], "not an IPv4 address"),
        (&["--config", "c", "--ip", "fe80::1"], "not an IPv4 address"),
    ];
    for (a, want) in cases {
        let got = parse_args(&args(a)).expect_err(&format!("{a:?} is refused"));
        assert!(got.contains(want), "{a:?}: {got}");
    }
}

#[test]
fn an_address_the_dante_network_cannot_reach_is_refused() {
    for bad in [
        "0.0.0.0",
        "255.255.255.255",
        "224.0.0.251",
        "239.1.2.3",
        "127.0.0.1",
        "127.9.9.9",
    ] {
        let got = parse_args(&args(&["--config", "c", "--ip", bad])).expect_err(bad);
        assert!(
            got.contains("not one the Dante network reaches"),
            "{bad}: {got}"
        );
    }
    // A link-local address is a Dante network's usual one.
    for good in ["169.254.10.20", "10.0.0.10", "192.0.2.254"] {
        let want = run("c", Place::Ip(good.parse().unwrap()));
        assert_eq!(
            parse_args(&args(&["--config", "c", "--ip", good])),
            Ok(want)
        );
    }
}

#[test]
fn clock_takes_its_seconds() {
    for (a, want) in [
        (
            &["clock", "--seconds", "1800"][..],
            Command::Clock { seconds: 1800 },
        ),
        (&["clock", "--seconds", "1"], Command::Clock { seconds: 1 }),
        (
            &["clock", "--seconds", "4294967295"],
            Command::Clock { seconds: u32::MAX },
        ),
        (&["clock", "--seconds", "5", "--version"], Command::Version),
        (&["clock", "-V"], Command::Version),
        (&["clock", "--help", "--bogus"], Command::Help),
        (&["clock", "-h"], Command::Help),
    ] {
        assert_eq!(parse_args(&args(a)), Ok(want), "{a:?}");
    }
}

#[test]
fn a_bad_clock_command_line_is_a_usage_error() {
    let cases: &[(&[&str], &str)] = &[
        (&["clock"], "clock: --seconds is missing"),
        (&["clock", "--seconds"], "--seconds needs a value"),
        (
            &["clock", "--seconds", "0"],
            "\"0\" is not a whole number from 1",
        ),
        (&["clock", "--seconds", "-1"], "not a whole number"),
        (&["clock", "--seconds", "1.5"], "not a whole number"),
        (&["clock", "--seconds", "4294967296"], "not a whole number"),
        (
            &["clock", "--seconds", "5", "--seconds", "6"],
            "given twice",
        ),
        (
            &["clock", "--config", "c"],
            "clock: unknown argument \"--config\"",
        ),
        (&["clock", "5"], "clock: unknown argument \"5\""),
        // Only the first word selects the clock.
        (
            &["--config", "c", "--iface", "e", "clock"],
            "unknown argument \"clock\"",
        ),
    ];
    for (a, want) in cases {
        let got = parse_args(&args(a)).expect_err(&format!("{a:?} is refused"));
        assert!(got.contains(want), "{a:?}: {got}");
    }
}

fn reading(locked: bool, mode: u8, freq_ppb: i32) -> crate::clock::Reading {
    crate::clock::Reading {
        wall_ns: 1_790_000_100_000_000_000,
        locked,
        settled: true,
        mode,
        freq_ppb,
        date: None,
    }
}

fn polled(
    reading: Option<crate::clock::Reading>,
    sample: Sample,
    event: Event,
    k_ns: Option<i64>,
) -> Polled {
    Polled {
        reading,
        sample,
        event,
        k_ns,
    }
}

/// A run from a silent dantesync to a re-anchor and a holdover, line by
/// line: every field pinned.
#[test]
fn the_clock_lines_follow_the_clock_from_waiting_to_a_re_anchor() {
    let mut lines = ClockLines::new();
    let usable = |k_ns| Sample::Usable { k_ns };
    let steps = [
        (
            polled(None, Sample::NoReply, Event::NotReady, None),
            r#"{"t_s":0,"ready":0,"k_ns":null,"dk_ns":null,"off_ns":null,"re_anchors":0,"re_anchor_ns":null,"holdover":0,"sample":5,"locked":null,"mode":null,"freq_ppb":null}"#,
        ),
        (
            polled(
                Some(reading(false, 1, 12_000)),
                Sample::NotLocked,
                Event::NotReady,
                None,
            ),
            r#"{"t_s":1,"ready":0,"k_ns":null,"dk_ns":null,"off_ns":null,"re_anchors":0,"re_anchor_ns":null,"holdover":0,"sample":1,"locked":0,"mode":1,"freq_ppb":12000}"#,
        ),
        (
            polled(
                Some(reading(true, 3, -750)),
                usable(5_000_000),
                Event::Ready { k_ns: 5_000_000 },
                Some(5_000_000),
            ),
            r#"{"t_s":2,"ready":1,"k_ns":5000000,"dk_ns":null,"off_ns":0,"re_anchors":0,"re_anchor_ns":null,"holdover":0,"sample":0,"locked":1,"mode":3,"freq_ppb":-750}"#,
        ),
        (
            polled(
                Some(reading(true, 4, -751)),
                usable(5_003_000),
                Event::Held,
                Some(5_000_000),
            ),
            r#"{"t_s":3,"ready":1,"k_ns":5003000,"dk_ns":3000,"off_ns":3000,"re_anchors":0,"re_anchor_ns":null,"holdover":0,"sample":0,"locked":1,"mode":4,"freq_ppb":-751}"#,
        ),
        (
            polled(
                Some(reading(true, 4, -751)),
                usable(9_000_000),
                Event::Held,
                Some(5_000_000),
            ),
            r#"{"t_s":4,"ready":1,"k_ns":9000000,"dk_ns":3997000,"off_ns":4000000,"re_anchors":0,"re_anchor_ns":null,"holdover":0,"sample":0,"locked":1,"mode":4,"freq_ppb":-751}"#,
        ),
        (
            polled(
                Some(reading(true, 4, -751)),
                usable(9_000_050),
                Event::ReAnchored {
                    delta_ns: 4_000_050,
                },
                Some(9_000_050),
            ),
            r#"{"t_s":5,"ready":1,"k_ns":9000050,"dk_ns":50,"off_ns":0,"re_anchors":1,"re_anchor_ns":4000050,"holdover":0,"sample":0,"locked":1,"mode":4,"freq_ppb":-751}"#,
        ),
        (
            polled(
                Some(reading(true, 4, -752)),
                usable(9_000_010),
                Event::Held,
                Some(9_000_050),
            ),
            r#"{"t_s":6,"ready":1,"k_ns":9000010,"dk_ns":-40,"off_ns":-40,"re_anchors":1,"re_anchor_ns":null,"holdover":0,"sample":0,"locked":1,"mode":4,"freq_ppb":-752}"#,
        ),
        (
            polled(
                Some(reading(false, 1, -700)),
                Sample::NotLocked,
                Event::Holdover,
                Some(9_000_050),
            ),
            r#"{"t_s":7,"ready":1,"k_ns":null,"dk_ns":null,"off_ns":null,"re_anchors":1,"re_anchor_ns":null,"holdover":1,"sample":1,"locked":0,"mode":1,"freq_ppb":-700}"#,
        ),
        // The line before had no k: no change to name.
        (
            polled(
                Some(reading(true, 3, -760)),
                usable(9_000_060),
                Event::Held,
                Some(9_000_050),
            ),
            r#"{"t_s":8,"ready":1,"k_ns":9000060,"dk_ns":null,"off_ns":10,"re_anchors":1,"re_anchor_ns":null,"holdover":0,"sample":0,"locked":1,"mode":3,"freq_ppb":-760}"#,
        ),
        // A second re-anchor, backwards.
        (
            polled(
                Some(reading(true, 3, -760)),
                usable(1_000),
                Event::ReAnchored {
                    delta_ns: -8_999_050,
                },
                Some(1_000),
            ),
            r#"{"t_s":9,"ready":1,"k_ns":1000,"dk_ns":-8999060,"off_ns":0,"re_anchors":2,"re_anchor_ns":-8999050,"holdover":0,"sample":0,"locked":1,"mode":3,"freq_ppb":-760}"#,
        ),
    ];
    for (t_s, (p, want)) in steps.iter().enumerate() {
        assert_eq!(lines.line(t_s as u32, p), *want, "line {t_s}");
    }
}

#[test]
fn every_sample_has_its_number() {
    for (sample, code) in [
        (Sample::Usable { k_ns: 1 }, 0),
        (Sample::NotLocked, 1),
        (Sample::NoDateState, 2),
        (Sample::StepNear, 3),
        (Sample::Slewing, 4),
        (Sample::NoReply, 5),
    ] {
        assert_eq!(sample_code(sample), code, "{sample:?}");
        let line = ClockLines::new().line(0, &polled(None, sample, Event::NotReady, None));
        assert!(line.contains(&format!("\"sample\":{code},")), "{line}");
    }
}

/// The clock's lines name numbers only (P6: they may be pasted).
#[test]
fn a_clock_line_holds_numbers_only() {
    let line = ClockLines::new().line(
        7,
        &polled(
            Some(reading(true, 3, -750)),
            Sample::Usable { k_ns: -42 },
            Event::Ready { k_ns: -42 },
            Some(-42),
        ),
    );
    let body = line
        .strip_prefix('{')
        .and_then(|l| l.strip_suffix('}'))
        .unwrap();
    for field in body.split(',') {
        let (key, value) = field.split_once(':').unwrap();
        assert!(
            key.starts_with('"')
                && key.ends_with('"')
                && key[1..key.len() - 1]
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_'),
            "{field}"
        );
        assert!(value == "null" || value.parse::<i64>().is_ok(), "{field}");
    }
}

fn if_addr(name: &str, ip: Ipv4Addr, netmask: Ipv4Addr) -> IfAddr {
    IfAddr {
        name: name.into(),
        ip,
        netmask,
    }
}

fn addrs() -> Vec<IfAddr> {
    vec![
        if_addr("lo", Ipv4Addr::LOCALHOST, Ipv4Addr::new(255, 0, 0, 0)),
        if_addr("eth9", IP, MASK),
        if_addr("wlan9", OTHER, Ipv4Addr::new(255, 255, 0, 0)),
        if_addr("br9", Ipv4Addr::new(192, 0, 2, 1), MASK),
        if_addr("br9", Ipv4Addr::new(192, 0, 2, 2), MASK),
        // The same address on a second interface: the first one is picked.
        if_addr("eth10", OTHER, MASK),
    ]
}

#[test]
fn an_interface_gives_its_one_ipv4_address() {
    let list = addrs();
    assert_eq!(
        pick_interface(&Place::Iface("eth9".into()), &list),
        Ok(if_addr("eth9", IP, MASK))
    );
    assert_eq!(
        pick_interface(&Place::Iface("wlan9".into()), &list),
        Ok(list[2].clone())
    );
    let none = pick_interface(&Place::Iface("eth8".into()), &list).unwrap_err();
    assert!(none.contains("no such interface"), "{none}");
    let two = pick_interface(&Place::Iface("br9".into()), &list).unwrap_err();
    assert!(two.contains("2 IPv4 addresses"), "{two}");
    let lo = pick_interface(&Place::Iface("lo".into()), &list).unwrap_err();
    assert!(lo.contains("loopback"), "{lo}");
    assert!(pick_interface(&Place::Iface("eth9".into()), &[]).is_err());
}

#[test]
fn an_address_must_be_one_of_the_box() {
    let list = addrs();
    assert_eq!(pick_interface(&Place::Ip(IP), &list), Ok(list[1].clone()));
    assert_eq!(
        pick_interface(&Place::Ip(OTHER), &list),
        Ok(list[2].clone())
    );
    let missing = pick_interface(&Place::Ip(Ipv4Addr::new(10, 0, 0, 12)), &list).unwrap_err();
    assert!(missing.contains("no interface"), "{missing}");
    assert!(pick_interface(&Place::Ip(Ipv4Addr::LOCALHOST), &list).is_err());
}

#[test]
fn a_mac_is_six_two_digit_hex_bytes_and_not_all_zeros() {
    assert_eq!(parse_mac("02:00:00:00:00:0a\n"), Some(MAC));
    assert_eq!(parse_mac("02:00:00:00:00:0A"), Some(MAC));
    assert_eq!(
        parse_mac("fe:dc:ba:98:76:54"),
        Some([0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54])
    );
    for bad in [
        "",
        "\n",
        "00:00:00:00:00:00",
        "02:00:00:00:00",
        "02:00:00:00:00:0a:0b",
        "02-00-00-00-00-0a",
        "zz:00:00:00:00:0a",
        "2:0:0:0:0:a",
        "+2:00:00:00:00:0a",
        "002:00:00:00:00:0a",
        "02:00:00:00:00:",
    ] {
        assert_eq!(parse_mac(bad), None, "{bad:?}");
    }
}

#[test]
fn a_link_speed_is_its_mbit_s_or_0_and_at_most_10000() {
    assert_eq!(link_speed(Some("1000\n")), 1000);
    assert_eq!(link_speed(Some("100")), 100);
    assert_eq!(link_speed(Some("10000")), 10_000);
    assert_eq!(link_speed(Some("25000")), 10_000);
    // The kernel's -1 (unknown), a link that is down (no file) and noise.
    for unknown in [Some("-1\n"), Some(""), Some("fast"), None] {
        assert_eq!(link_speed(unknown), 0, "{unknown:?}");
    }
}

/// `/proc/net/route` with these lines after its header (little-endian
/// hex, as the kernel prints them on x86-64).
fn routes(lines: &[&str]) -> String {
    let mut text = String::from(
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n",
    );
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    text
}

/// A default route via 10.0.0.1 on eth9 and its subnet's route.
const DEFAULT_ETH9: &str = "eth9\t00000000\t0100000A\t0003\t0\t0\t100\t00000000\t0\t0\t0";
const SUBNET_ETH9: &str = "eth9\t0000000A\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0";

#[test]
#[cfg(target_endian = "little")]
fn the_gateway_is_the_interface_s_default_route_in_its_subnet() {
    let eth9 = if_addr("eth9", IP, MASK);
    let gw = |lines: &[&str]| default_gateway(&routes(lines), &eth9);
    assert_eq!(gw(&[DEFAULT_ETH9, SUBNET_ETH9]), GATEWAY);
    assert_eq!(gw(&[SUBNET_ETH9, DEFAULT_ETH9]), GATEWAY);
    // None: no route, another interface's, a gateway outside the subnet, a
    // route that is not up or has no gateway, one that is not the default.
    let none = Ipv4Addr::UNSPECIFIED;
    assert_eq!(gw(&[]), none);
    assert_eq!(gw(&[SUBNET_ETH9]), none);
    assert_eq!(
        gw(&["wlan9\t00000000\t0100000A\t0003\t0\t0\t600\t00000000\t0\t0\t0"]),
        none
    );
    assert_eq!(
        gw(&["eth9\t00000000\t010200C0\t0003\t0\t0\t100\t00000000\t0\t0\t0"]),
        none
    );
    assert_eq!(
        gw(&["eth9\t00000000\t0100000A\t0002\t0\t0\t100\t00000000\t0\t0\t0"]),
        none
    );
    assert_eq!(
        gw(&["eth9\t00000000\t0100000A\t0001\t0\t0\t100\t00000000\t0\t0\t0"]),
        none
    );
    assert_eq!(
        gw(&["eth9\t0000000A\t0100000A\t0003\t0\t0\t100\t00FFFFFF\t0\t0\t0"]),
        none
    );
    assert_eq!(
        gw(&["eth9\t00000000\t0100000A\t0003\t0\t0\t100\t000000FF\t0\t0\t0"]),
        none
    );
    // The first default route in the subnet, after one outside it; other
    // flags beside up and gateway do not matter.
    assert_eq!(
        gw(&[
            "eth9\t00000000\t010200C0\t0003\t0\t0\t50\t00000000\t0\t0\t0",
            "eth9\t00000000\t0100000A\t0013\t0\t0\t100\t00000000\t0\t0\t0",
        ]),
        GATEWAY
    );
    // Lines that are not routes are passed over, never a panic.
    assert_eq!(
        gw(&[
            "",
            "eth9",
            "eth9\t00000000",
            "eth9\tzz\tzz\tzz\t0\t0\t0\tzz",
            DEFAULT_ETH9
        ]),
        GATEWAY
    );
    assert_eq!(default_gateway("", &eth9), none);
}

#[test]
#[cfg(target_endian = "little")]
fn the_network_info_is_the_interface_s() {
    let eth9 = if_addr("eth9", IP, MASK);
    let table = routes(&[DEFAULT_ETH9]);
    assert_eq!(
        net_info(&eth9, "02:00:00:00:00:0a\n", Some("1000\n"), Some(&table)),
        Ok(NetInfo {
            mac: MAC,
            ip: IP,
            netmask: MASK,
            gateway: GATEWAY,
            link_speed: 1000,
        })
    );
    // No speed and no route table: zeros.
    assert_eq!(
        net_info(&eth9, "02:00:00:00:00:0a", None, None),
        Ok(NetInfo {
            mac: MAC,
            ip: IP,
            netmask: MASK,
            gateway: Ipv4Addr::UNSPECIFIED,
            link_speed: 0,
        })
    );
    let err = net_info(&eth9, "00:00:00:00:00:00", None, None).unwrap_err();
    assert!(err.contains("MAC"), "{err}");
    assert!(net_info(&eth9, "", Some("1000"), Some(&table)).is_err());
}

#[test]
fn the_records_point_at_the_device_names_local_host() {
    assert_eq!(host_of("test-device"), "test-device.local.");
}

/// The mDNS daemon's place: logs every registration and withdrawal, and
/// refuses registrations after `allow` of them.
#[derive(Clone)]
struct Log {
    calls: Rc<RefCell<Vec<String>>>,
    allow: Rc<Cell<usize>>,
}

impl Log {
    fn new(allow: usize) -> Self {
        Self {
            calls: Rc::default(),
            allow: Rc::new(Cell::new(allow)),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

/// A browse (the channel records, the ARC and flow-control records):
/// logged with its service, no record.
impl Browse for Log {
    type Events = std::iter::Empty<mdns_sd::ServiceEvent>;
    fn browse(&self, service: &str) -> Result<Self::Events, mdns_sd::Error> {
        self.calls.borrow_mut().push(format!("browse {service}"));
        Ok(std::iter::empty())
    }
}

impl Registry for Log {
    fn register(&self, info: ServiceInfo) -> Result<(), mdns_sd::Error> {
        if self.allow.get() == 0 {
            return Err(mdns_sd::Error::Msg("refused by the test".into()));
        }
        self.allow.set(self.allow.get() - 1);
        let line = format!("+{} -> {}", info.get_fullname(), info.get_hostname());
        self.calls.borrow_mut().push(line);
        Ok(())
    }

    fn unregister(&self, fullname: &str) -> Result<(), mdns_sd::Error> {
        self.calls.borrow_mut().push(format!("-{fullname}"));
        Ok(())
    }
}

/// The test device, its state file in `dir`.
fn device_in(dir: &Path) -> Arc<Mutex<Device>> {
    let cfg = DeviceConfig {
        state_file: dir.join("state.toml"),
        ..DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-dante-device.toml")
    };
    Arc::new(Mutex::new(Device::load(cfg).unwrap()))
}

/// The interface the tests serve on.
fn net() -> NetInfo {
    NetInfo {
        mac: MAC,
        ip: IP,
        netmask: MASK,
        gateway: GATEWAY,
        link_speed: 1000,
    }
}

fn v4(addr: SocketAddr) -> SocketAddrV4 {
    match addr {
        SocketAddr::V4(v4) => v4,
        SocketAddr::V6(_) => panic!("a loopback IPv4 socket"),
    }
}

/// The daemon's sockets on loopback and their addresses, and receivers
/// standing in for the info and heartbeat groups (a heartbeat period no
/// test waits for: only the first, immediate, beat).
struct Loopback {
    sockets: Sockets,
    arc: SocketAddr,
    cmc: SocketAddr,
    info: SocketAddr,
    flows: SocketAddr,
    info_rx: UdpSocket,
    beat_rx: UdpSocket,
    multicast: Multicast,
}

async fn loopback() -> Loopback {
    let bind = || UdpSocket::bind("127.0.0.1:0");
    let sockets = Sockets {
        arc: bind().await.unwrap(),
        cmc: bind().await.unwrap(),
        info: bind().await.unwrap(),
        flows: bind().await.unwrap(),
    };
    let (info_rx, beat_rx) = (bind().await.unwrap(), bind().await.unwrap());
    let multicast = Multicast {
        info: v4(info_rx.local_addr().unwrap()),
        heartbeat: v4(beat_rx.local_addr().unwrap()),
        every: Duration::from_secs(3600),
    };
    Loopback {
        arc: sockets.arc.local_addr().unwrap(),
        cmc: sockets.cmc.local_addr().unwrap(),
        info: sockets.info.local_addr().unwrap(),
        flows: sockets.flows.local_addr().unwrap(),
        sockets,
        info_rx,
        beat_rx,
        multicast,
    }
}

/// The device's audio side on loopback, its clock never ready.
fn media_of(shared: &Arc<Mutex<Device>>) -> Arc<Media> {
    media_on(&shared.lock().unwrap(), Arc::new(Unready))
}

async fn recv(sock: &UdpSocket) -> Vec<u8> {
    let mut buf = [0u8; 2048];
    let (n, _) = tokio::time::timeout(BOUND, sock.recv_from(&mut buf))
        .await
        .expect("a packet within the bound")
        .unwrap();
    buf[..n].to_vec()
}

/// An info request as Dante Controller sends it (#72, 2026-10-26), with a
/// synthetic MAC, asking `what` (0x61 board info, 0x13 network info).
fn info_request(what: u8) -> Vec<u8> {
    let mut req = vec![0xff, 0xff, 0x00, 0x20, 0x00, 0xe4, 0x00, 0x00];
    req.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00]);
    req.extend_from_slice(b"Audinate");
    req.extend_from_slice(&[0x07, 0x3e, 0x00, what, 0x00, 0x00, 0x00, 0x00]);
    req
}

/// An info packet's sequence and opcode.
fn seq_op(packet: &[u8]) -> (u16, &[u8]) {
    (u16::from_be_bytes([packet[4], packet[5]]), &packet[24..32])
}

async fn ask(peer: &UdpSocket, to: std::net::SocketAddr, packet: &[u8]) -> Vec<u8> {
    peer.send_to(packet, to).await.unwrap();
    let mut buf = [0u8; 2048];
    let (n, _) = tokio::time::timeout(BOUND, peer.recv_from(&mut buf))
        .await
        .expect("an answer within the bound")
        .unwrap();
    buf[..n].to_vec()
}

/// netaudio's TX rename of `channel` to `name`.
fn tx_rename(channel: u8, name: &[u8]) -> Vec<u8> {
    let mut body = vec![0x02, 0x01, 0, 0, 0, channel, 0x00, 0x18, 0, 0, 0, 0, 0, 0];
    body.extend_from_slice(name);
    body.push(0);
    request(0x27ff, 9, op::TX_RENAME, 0, &body)
}

fn chan(name: &str) -> String {
    format!("+{name}@test-device.{CHAN_SERVICE} -> test-device.local.")
}

#[tokio::test]
async fn the_daemon_announces_answers_and_announces_a_rename_again_until_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let shared = device_in(dir.path());
    let log = Log::new(usize::MAX);
    let mut announcer = Announcer::new(log.clone());
    let Loopback {
        sockets,
        arc: arc_at,
        cmc: cmc_at,
        info: info_at,
        flows: flows_at,
        info_rx,
        beat_rx,
        multicast,
    } = loopback().await;
    let media = media_of(&shared);
    let control_path = dir.path().join("control.sock");
    let control = crate::ctl::serve_control(
        crate::ctl::bind_control(&control_path).unwrap(),
        Arc::clone(&media),
    );
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let stop = async move {
        stop_rx.await.ok();
    };
    let watch = log.clone();
    let peer = async move {
        let reply = ask(
            &peer_socket().await,
            cmc_at,
            &request(0x27ff, 1, cmc::ADVERTISE, 0, &[]),
        )
        .await;
        let (h, body) = Header::parse(&reply).unwrap();
        assert_eq!((h.seq, h.op2), (1, STATUS_OK));
        assert_eq!(
            body[14..18],
            IP.octets(),
            "CMC advertises the served address"
        );
        // The info server: the board and product info at start and the
        // first heartbeat, then the board and network info asked for, to
        // the info group, from the device's id.
        let board = [0x07, 0x2a, 0x00, 0x60, 0x00, 0x00, 0x00, 0x00];
        let network = [0x07, 0x2a, 0x00, 0x11, 0x00, 0x00, 0x00, 0x00];
        let product = [0x07, 0x2a, 0x00, 0xc0, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(seq_op(&recv(&info_rx).await), (1, &board[..]));
        assert_eq!(seq_op(&recv(&info_rx).await), (2, &product[..]));
        let beat = recv(&beat_rx).await;
        assert_eq!((beat.len(), seq_op(&beat).0), (32, 3));
        let asker = peer_socket().await;
        asker.send_to(&info_request(0x61), info_at).await.unwrap();
        let reply = recv(&info_rx).await;
        assert_eq!(seq_op(&reply), (4, &board[..]));
        assert_eq!(reply[8..16], [0, 0, 10, 0, 0, 10, 0, 0], "the device id");
        asker.send_to(&info_request(0x13), info_at).await.unwrap();
        let reply = recv(&info_rx).await;
        assert_eq!(seq_op(&reply), (5, &network[..]));
        assert_eq!(reply[32 + 10..32 + 16], MAC, "the interface's MAC");
        assert_eq!(reply[32 + 16..32 + 20], IP.octets());
        assert_eq!(reply[32 + 24..32 + 28], GATEWAY.octets());
        let reply = ask(&peer_socket().await, arc_at, &tx_rename(3, b"Mix 3")).await;
        let (h, _) = Header::parse(&reply).unwrap();
        assert_eq!((h.seq, h.op2), (9, STATUS_OK));
        // The rename's record follows within the bound.
        let deadline = tokio::time::Instant::now() + BOUND;
        while !watch.calls().contains(&chan("Mix 3")) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "no new announcement"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Flow control: a stop of a flow we never granted is 0x0103.
        let stop_req = crate::flows::control::build_stop(4, &[0, 0, 0, 9, 0, 1]);
        let reply = ask(&peer_socket().await, flows_at, &stop_req).await;
        assert_eq!(Header::parse(&reply).unwrap().0.op2, 0x0103);
        // The control socket: a source set, and the status counting every
        // request above by port and opcode (one peer: loopback).
        assert_eq!(control_ask(&control_path, "source 1 counter").await, "ok\n");
        let status = control_ask(&control_path, "status").await;
        for counted in [
            "{\"port\":4440,\"peer\":1,\"op\":8211,\"n\":1}",
            "{\"port\":4455,\"peer\":1,\"op\":257,\"n\":1}",
            "{\"port\":8700,\"peer\":1,\"op\":19,\"n\":1}",
            "{\"port\":8700,\"peer\":1,\"op\":97,\"n\":1}",
            "{\"port\":8800,\"peer\":1,\"op\":4097,\"n\":1}",
            "\"sources\":[{\"tx\":1,\"source\":\"counter\"}]",
        ] {
            assert!(status.contains(counted), "{counted} in {status}");
        }
        stop_tx.send(()).unwrap();
    };
    let (served, ()) = tokio::time::timeout(BOUND, async {
        tokio::join!(
            serve(
                sockets,
                Arc::clone(&shared),
                &mut announcer,
                net(),
                multicast,
                Arc::clone(&media),
                control,
                stop
            ),
            peer
        )
    })
    .await
    .expect("the daemon ends within the bound once stopped");
    served.unwrap();
    // The stop ended the media threads.
    assert!(media.tx.stopping() && media.rx.stopping());
    let calls = log.calls();
    assert_eq!(calls.len(), 2 + 16 + 1, "{calls:#?}");
    assert_eq!(
        calls[0],
        "+test-device._netaudio-arc._udp.local. -> test-device.local."
    );
    assert_eq!(
        calls[1],
        "+test-device._netaudio-cmc._udp.local. -> test-device.local."
    );
    assert_eq!(calls[2], chan("TX 1"));
    assert_eq!(calls[17], chan("TX 16"));
    assert_eq!(
        calls[18],
        chan("Mix 3"),
        "only the new record is sent again"
    );
    let saved = std::fs::read_to_string(dir.path().join("state.toml")).unwrap();
    assert!(saved.contains("Mix 3"), "{saved}");
    assert_eq!(shared.lock().unwrap().state.tx[2], "Mix 3");
}

async fn peer_socket() -> UdpSocket {
    UdpSocket::bind("127.0.0.1:0").await.unwrap()
}

/// One request line to the control socket at `path`; its answer, bounded.
async fn control_ask(path: &Path, line: &str) -> String {
    tokio::time::timeout(BOUND, async {
        let s = tokio::net::UnixStream::connect(path).await.unwrap();
        s.writable().await.unwrap();
        s.try_write(format!("{line}\n").as_bytes()).unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            s.readable().await.unwrap();
            match s.try_read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("{e}"),
            }
        }
        String::from_utf8(out).unwrap()
    })
    .await
    .expect("an answer within the bound")
}

#[tokio::test]
async fn a_stop_that_has_already_come_ends_the_daemon_after_its_announcement() {
    let dir = tempfile::tempdir().unwrap();
    let log = Log::new(usize::MAX);
    let mut announcer = Announcer::new(log.clone());
    let lo = loopback().await;
    let shared = device_in(dir.path());
    let media = media_of(&shared);
    let served = tokio::time::timeout(
        BOUND,
        serve(
            lo.sockets,
            shared,
            &mut announcer,
            net(),
            lo.multicast,
            Arc::clone(&media),
            std::future::pending(),
            std::future::ready(()),
        ),
    )
    .await
    .expect("within the bound");
    served.unwrap();
    assert_eq!(log.calls().len(), 2 + 16);
    assert!(media.tx.stopping() && media.rx.stopping());
}

/// The daemon's browses, in the order it starts them.
const BROWSES: [&str; 3] = [
    "browse _netaudio-chan._udp.local.",
    "browse _netaudio-arc._udp.local.",
    "browse _netaudio-dbc._udp.local.",
];

#[tokio::test]
async fn the_daemon_browses_at_the_first_subscription_and_stops_after_any_end() {
    let dir = tempfile::tempdir().unwrap();
    let shared = device_in(dir.path());
    {
        let mut device = shared.lock().unwrap();
        let cfg = device.cfg.clone();
        device
            .state
            .set_subscription(&cfg, 0, Some(sub("TX 1", "peer-device")));
    }
    let log = Log::new(usize::MAX);
    let mut announcer = Announcer::new(log.clone());
    let lo = loopback().await;
    let media = media_of(&shared);
    // The control server ends once the browses have started (the channel
    // records' at the subscription; the peer has none, so its lookup goes
    // on to the ARC directory, whose browses start at the next tick):
    // serving ends with its error, and the stop still runs.
    let watch = log.clone();
    let control = async move {
        let deadline = tokio::time::Instant::now() + BOUND;
        while !watch.calls().contains(&BROWSES[2].to_string()) {
            assert!(tokio::time::Instant::now() < deadline, "no browse");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Err(io::Error::other("the control test's end"))
    };
    let served = tokio::time::timeout(
        BOUND,
        serve(
            lo.sockets,
            shared,
            &mut announcer,
            net(),
            lo.multicast,
            Arc::clone(&media),
            control,
            std::future::pending(),
        ),
    )
    .await
    .expect("within the bound");
    let err = served.unwrap_err();
    assert!(matches!(err, ServeError::Control(_)), "{err}");
    assert_eq!(
        err.to_string(),
        "the control socket stopped: the control test's end"
    );
    let calls = log.calls();
    let browses: Vec<&String> = calls.iter().filter(|c| c.starts_with("browse")).collect();
    // Each browse once, the channel records' first.
    assert_eq!(browses, BROWSES);
    // The unresolved subscription asked nothing; the threads ended.
    assert!(media.requests.counts().sent.is_empty());
    assert!(media.tx.stopping() && media.rx.stopping());
}

#[tokio::test]
async fn a_first_announcement_that_fails_ends_the_daemon_with_its_error() {
    let dir = tempfile::tempdir().unwrap();
    let log = Log::new(0);
    let mut announcer = Announcer::new(log.clone());
    let lo = loopback().await;
    let shared = device_in(dir.path());
    let media = media_of(&shared);
    let served = tokio::time::timeout(
        BOUND,
        serve(
            lo.sockets,
            shared,
            &mut announcer,
            net(),
            lo.multicast,
            Arc::clone(&media),
            std::future::pending(),
            std::future::pending(),
        ),
    )
    .await
    .expect("within the bound");
    let err = served.unwrap_err();
    assert!(matches!(err, ServeError::Announce(_)), "{err}");
    assert!(log.calls().is_empty());
    // Nothing was started.
    assert!(!media.tx.stopping());
}

#[tokio::test]
async fn an_announcement_that_fails_after_a_rename_ends_the_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let shared = device_in(dir.path());
    // The first set goes out; the rename's new record is refused.
    let log = Log::new(2 + 16);
    let mut announcer = Announcer::new(log.clone());
    let lo = loopback().await;
    let arc_at = lo.arc;
    let peer = async move {
        let reply = ask(&peer_socket().await, arc_at, &tx_rename(5, b"Mix 5")).await;
        assert_eq!(Header::parse(&reply).unwrap().0.op2, STATUS_OK);
    };
    let media = media_of(&shared);
    let (served, ()) = tokio::time::timeout(BOUND, async {
        tokio::join!(
            serve(
                lo.sockets,
                Arc::clone(&shared),
                &mut announcer,
                net(),
                lo.multicast,
                Arc::clone(&media),
                std::future::pending(),
                std::future::pending()
            ),
            peer
        )
    })
    .await
    .expect("the failed announcement ends the daemon within the bound");
    // A failure after the start still ends the media threads.
    assert!(media.tx.stopping() && media.rx.stopping());
    let err = served.unwrap_err();
    assert!(matches!(err, ServeError::Announce(_)), "{err}");
    assert!(
        err.to_string().starts_with("announcing the device: "),
        "{err}"
    );
    // The rename was saved before it was answered: a restart announces it.
    let saved = std::fs::read_to_string(dir.path().join("state.toml")).unwrap();
    assert!(saved.contains("Mix 5"), "{saved}");
    assert_eq!(log.calls().len(), 2 + 16);
}

#[test]
fn announce_device_counts_the_records_it_made_the_announced_set() {
    let dir = tempfile::tempdir().unwrap();
    let shared = device_in(dir.path());
    let log = Log::new(usize::MAX);
    let mut announcer = Announcer::new(log.clone());
    assert_eq!(announce_device(&shared, &mut announcer, IP).unwrap(), 18);
    shared.lock().unwrap().state.tx[0] = "Mix 1".into();
    assert_eq!(announce_device(&shared, &mut announcer, IP).unwrap(), 19);
    assert_eq!(log.calls().last().unwrap(), &chan("Mix 1"));
    assert_eq!(log.calls().len(), 19);
}

#[test]
fn a_server_that_ends_is_an_error_whatever_it_returned() {
    let e = ended_error(Err(io::Error::other("recv failed")));
    assert_eq!(e.to_string(), "recv failed");
    let e = ended_error(Ok(()));
    assert_eq!(e.to_string(), "ended without an error");
    assert_eq!(
        ServeError::Arc(io::Error::other("x")).to_string(),
        "the ARC server stopped: x"
    );
    assert_eq!(
        ServeError::Cmc(io::Error::other("y")).to_string(),
        "the CMC server stopped: y"
    );
    assert_eq!(
        ServeError::Info(io::Error::other("z")).to_string(),
        "the info server stopped: z"
    );
    assert_eq!(
        ServeError::Flows(io::Error::other("f")).to_string(),
        "the flow-control server stopped: f"
    );
    assert_eq!(
        ServeError::Threads(io::Error::other("t")).to_string(),
        "the media threads did not start: t"
    );
}
