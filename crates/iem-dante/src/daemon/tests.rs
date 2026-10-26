use super::*;

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use mdns_sd::ServiceInfo;
use tokio::sync::oneshot;

use crate::announce::CHAN_SERVICE;
use crate::arc::op;
use crate::device::DeviceConfig;
use crate::wire::{Header, STATUS_OK, request};

const TEST_DEVICE: &str = include_str!("../../../../config/test-dante-device.toml");
/// Synthetic addresses (public repository, P6).
const IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 10);
const OTHER: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 11);
/// The tests' bound on every wait, so a mutant that hangs fails fast.
const BOUND: Duration = Duration::from_secs(5);

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn run(config: &str, place: Place) -> Command {
    Command::Run(Options {
        config: PathBuf::from(config),
        place,
    })
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

fn addrs() -> Vec<IfAddr> {
    let a = |name: &str, ip: Ipv4Addr| IfAddr {
        name: name.into(),
        ip,
    };
    vec![
        a("lo", Ipv4Addr::LOCALHOST),
        a("eth9", IP),
        a("wlan9", OTHER),
        a("br9", Ipv4Addr::new(192, 0, 2, 1)),
        a("br9", Ipv4Addr::new(192, 0, 2, 2)),
    ]
}

#[test]
fn an_interface_gives_its_one_ipv4_address() {
    let list = addrs();
    assert_eq!(pick_ipv4(&Place::Iface("eth9".into()), &list), Ok(IP));
    assert_eq!(pick_ipv4(&Place::Iface("wlan9".into()), &list), Ok(OTHER));
    let none = pick_ipv4(&Place::Iface("eth8".into()), &list).unwrap_err();
    assert!(none.contains("no such interface"), "{none}");
    let two = pick_ipv4(&Place::Iface("br9".into()), &list).unwrap_err();
    assert!(two.contains("2 IPv4 addresses"), "{two}");
    let lo = pick_ipv4(&Place::Iface("lo".into()), &list).unwrap_err();
    assert!(lo.contains("loopback"), "{lo}");
    assert!(pick_ipv4(&Place::Iface("eth9".into()), &[]).is_err());
}

#[test]
fn an_address_must_be_one_of_the_box() {
    let list = addrs();
    assert_eq!(pick_ipv4(&Place::Ip(IP), &list), Ok(IP));
    assert_eq!(pick_ipv4(&Place::Ip(OTHER), &list), Ok(OTHER));
    let missing = pick_ipv4(&Place::Ip(Ipv4Addr::new(10, 0, 0, 12)), &list).unwrap_err();
    assert!(missing.contains("no interface"), "{missing}");
    assert!(pick_ipv4(&Place::Ip(Ipv4Addr::LOCALHOST), &list).is_err());
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

/// ARC's and CMC's sockets on loopback, and their addresses.
async fn sockets() -> (
    UdpSocket,
    UdpSocket,
    std::net::SocketAddr,
    std::net::SocketAddr,
) {
    let arc_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let cmc_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let (a, c) = (
        arc_sock.local_addr().unwrap(),
        cmc_sock.local_addr().unwrap(),
    );
    (arc_sock, cmc_sock, a, c)
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
    let (arc_sock, cmc_sock, arc_at, cmc_at) = sockets().await;
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
        stop_tx.send(()).unwrap();
    };
    let (served, ()) = tokio::time::timeout(BOUND, async {
        tokio::join!(
            serve(
                arc_sock,
                cmc_sock,
                Arc::clone(&shared),
                &mut announcer,
                IP,
                stop
            ),
            peer
        )
    })
    .await
    .expect("the daemon ends within the bound once stopped");
    served.unwrap();
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

#[tokio::test]
async fn a_stop_that_has_already_come_ends_the_daemon_after_its_announcement() {
    let dir = tempfile::tempdir().unwrap();
    let log = Log::new(usize::MAX);
    let mut announcer = Announcer::new(log.clone());
    let (arc_sock, cmc_sock, _, _) = sockets().await;
    let served = tokio::time::timeout(
        BOUND,
        serve(
            arc_sock,
            cmc_sock,
            device_in(dir.path()),
            &mut announcer,
            IP,
            std::future::ready(()),
        ),
    )
    .await
    .expect("within the bound");
    served.unwrap();
    assert_eq!(log.calls().len(), 2 + 16);
}

#[tokio::test]
async fn a_first_announcement_that_fails_ends_the_daemon_with_its_error() {
    let dir = tempfile::tempdir().unwrap();
    let log = Log::new(0);
    let mut announcer = Announcer::new(log.clone());
    let (arc_sock, cmc_sock, _, _) = sockets().await;
    let served = tokio::time::timeout(
        BOUND,
        serve(
            arc_sock,
            cmc_sock,
            device_in(dir.path()),
            &mut announcer,
            IP,
            std::future::pending(),
        ),
    )
    .await
    .expect("within the bound");
    let err = served.unwrap_err();
    assert!(matches!(err, ServeError::Announce(_)), "{err}");
    assert!(log.calls().is_empty());
}

#[tokio::test]
async fn an_announcement_that_fails_after_a_rename_ends_the_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let shared = device_in(dir.path());
    // The first set goes out; the rename's new record is refused.
    let log = Log::new(2 + 16);
    let mut announcer = Announcer::new(log.clone());
    let (arc_sock, cmc_sock, arc_at, _) = sockets().await;
    let peer = async move {
        let reply = ask(&peer_socket().await, arc_at, &tx_rename(5, b"Mix 5")).await;
        assert_eq!(Header::parse(&reply).unwrap().0.op2, STATUS_OK);
    };
    let (served, ()) = tokio::time::timeout(BOUND, async {
        tokio::join!(
            serve(
                arc_sock,
                cmc_sock,
                Arc::clone(&shared),
                &mut announcer,
                IP,
                std::future::pending()
            ),
            peer
        )
    })
    .await
    .expect("the failed announcement ends the daemon within the bound");
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
}
