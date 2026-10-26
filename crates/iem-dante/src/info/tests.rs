use super::*;

use std::net::SocketAddr;

use crate::wire::tests::Rng;

const TEST_DEVICE: &str = include_str!("../../../../config/test-dante-device.toml");
/// Synthetic values (public repository, P6): the device's address, a
/// locally administered MAC, and Dante Controller's MAC in its request.
const IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 10);
const MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x0a];
const PEER_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
/// The tests' bound on every wait, so a mutant that hangs fails fast.
const BOUND: Duration = Duration::from_secs(5);
/// A heartbeat period no test waits for: only the first, immediate, beat.
const NEVER: Duration = Duration::from_secs(3600);

fn cfg() -> DeviceConfig {
    DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-dante-device.toml")
}

fn net() -> NetInfo {
    NetInfo {
        mac: MAC,
        ip: IP,
        netmask: Ipv4Addr::new(255, 255, 255, 0),
        gateway: Ipv4Addr::new(10, 0, 0, 1),
        link_speed: 1000,
    }
}

fn identity() -> Identity {
    Identity::new(&cfg(), net())
}

/// The workspace version as the product info carries it: major, minor,
/// then the patch as a big-endian u16.
fn workspace_version() -> [u8; 4] {
    let core = env!("CARGO_PKG_VERSION").split('-').next().unwrap();
    let n: Vec<u16> = core.split('.').map(|p| p.parse().unwrap()).collect();
    let [hi, lo] = n[2].to_be_bytes();
    [
        u8::try_from(n[0]).unwrap(),
        u8::try_from(n[1]).unwrap(),
        hi,
        lo,
    ]
}

/// The request Dante Controller sent, as captured on the box (#72,
/// 2026-10-26), its MAC replaced by a synthetic one.
fn captured() -> Vec<u8> {
    let mut req = vec![0xff, 0xff, 0x00, 0x20, 0x00, 0xe4, 0x00, 0x00];
    req.extend_from_slice(&PEER_MAC);
    req.extend_from_slice(&[0x00, 0x00]);
    req.extend_from_slice(b"Audinate");
    req.extend_from_slice(&[0x07, 0x3e, 0x00, 0x61, 0x00, 0x00, 0x00, 0x00]);
    req
}

/// A request like the captured one, with `opcode`.
fn asking(opcode: [u8; 8]) -> Vec<u8> {
    let mut req = captured();
    req[24..32].copy_from_slice(&opcode);
    req
}

/// What the device's packets start with: `start`, the length, `seq`,
/// process id 0, the device id (two zeros, the address, process id 0),
/// "Audinate", `opcode`.
fn header(start: u16, len: usize, seq: u16, opcode: [u8; 8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&start.to_be_bytes());
    out.extend_from_slice(&u16::try_from(len).unwrap().to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&[0x00, 0x00]);
    out.extend_from_slice(&[0x00, 0x00, 10, 0, 0, 10, 0x00, 0x00]);
    out.extend_from_slice(b"Audinate");
    out.extend_from_slice(&opcode);
    out
}

/// `len` zero bytes but those listed.
fn sparse(len: usize, set: &[(usize, &[u8])]) -> Vec<u8> {
    let mut out = vec![0u8; len];
    for (at, bytes) in set {
        out[*at..*at + bytes.len()].copy_from_slice(bytes);
    }
    out
}

fn packet(start: u16, seq: u16, opcode: [u8; 8], body: &[u8]) -> Vec<u8> {
    let mut out = header(start, 32 + body.len(), seq, opcode);
    out.extend_from_slice(body);
    out
}

const BOARD_INFO: [u8; 8] = [0x07, 0x2a, 0x00, 0x60, 0x00, 0x00, 0x00, 0x00];
const PRODUCT_INFO: [u8; 8] = [0x07, 0x2a, 0x00, 0xc0, 0x00, 0x00, 0x00, 0x00];
const NETWORK_INFO: [u8; 8] = [0x07, 0x2a, 0x00, 0x11, 0x00, 0x00, 0x00, 0x00];
const ANSWER_0077: [u8; 8] = [0x07, 0x2a, 0x00, 0x78, 0x00, 0x00, 0x00, 0x00];
const HEARTBEAT: [u8; 8] = [0x00, 0x08, 0x00, 0x01, 0x10, 0x00, 0x00, 0x00];

/// inferno's board info for the test device: firmware 4.1.0.6, hardware
/// 4.1.0.3, the model (the board name) cut to 8 bytes at 0x0c and to 16 at
/// 0x38, feature flags 0x10 (a manufacturer name), boot 1.0.0.0, and 0x1f
/// at 0xbb.
fn board_info_body(model: &[u8]) -> Vec<u8> {
    sparse(
        200,
        &[
            (0x00, &[4, 1, 0, 6]),
            (0x04, &[4, 1, 0, 3]),
            (0x0c, &model[..model.len().min(8)]),
            (0x16, &[0x10]),
            (0x23, &[2]),
            (0x27, &[1]),
            (0x28, &[1, 0, 0, 0]),
            (0x38, &model[..model.len().min(16)]),
            (0xbb, &[0x1f]),
        ],
    )
}

#[test]
fn the_ports_and_groups_are_inferno_s() {
    assert_eq!(REQUEST_PORT, 8700);
    assert_eq!(crate::cmc::INFO_REQUEST_PORT, REQUEST_PORT);
    assert_eq!(
        Multicast::DANTE,
        Multicast {
            info: "224.0.0.231:8702".parse().unwrap(),
            heartbeat: "224.0.0.233:8708".parse().unwrap(),
            every: Duration::from_secs(1),
        }
    );
}

#[test]
fn the_identity_is_the_config_s_and_the_interface_s() {
    let id = identity();
    assert_eq!(id.device_id, [0, 0, 10, 0, 0, 10, 0, 0]);
    assert_eq!(id.device_id, crate::cmc::device_id(IP));
    assert_eq!(id.manufacturer, "iemmixer");
    assert_eq!(id.model, "iem-dante");
    assert_eq!(id.version, workspace_version());
    assert_eq!(id.net, net());
}

#[test]
fn the_captured_request_asks_for_the_board_info() {
    assert_eq!(captured().len(), 32);
    assert_eq!(
        parse_request(&captured()),
        Some(Request {
            seq: 0x00e4,
            opcode: [0x07, 0x3e, 0x00, 0x61, 0x00, 0x00, 0x00, 0x00],
            kind: Kind::BoardInfo,
        })
    );
}

#[test]
fn the_requests_inferno_answers_are_known_by_their_opcode() {
    // Byte 1 is any (Dante Controller sent 0x3e); bytes 4-7 are zero, but
    // for 0x21, 0x13 and 0x77 the last byte is any too (0x64 seen).
    let cases: &[([u8; 8], Kind)] = &[
        ([0x07, 0x3e, 0, 0x61, 0, 0, 0, 0], Kind::BoardInfo),
        ([0x07, 0x00, 0, 0x61, 0, 0, 0, 0], Kind::BoardInfo),
        ([0x07, 0xff, 0, 0xc1, 0, 0, 0, 0], Kind::ProductInfo),
        ([0x07, 0x38, 0, 0x21, 0, 0, 0, 0x64], Kind::ClockStats),
        ([0x07, 0x38, 0, 0x21, 0, 0, 0, 0], Kind::ClockStats),
        ([0x07, 0x2a, 0, 0x13, 0, 0, 0, 0x64], Kind::NetworkInfo),
        ([0x07, 0x2a, 0, 0x13, 0, 0, 0, 0], Kind::NetworkInfo),
        ([0x07, 0x2a, 0, 0x77, 0, 0, 0, 0x01], Kind::Op0077),
        ([0x07, 0x2a, 0, 0x77, 0, 0, 0, 0], Kind::Op0077),
        // Not one of them.
        ([0x07, 0x3e, 0, 0x61, 0, 0, 0, 0x64], Kind::Unknown),
        ([0x07, 0x3e, 0, 0xc1, 0, 0, 0, 0x01], Kind::Unknown),
        ([0x07, 0x3e, 0, 0x61, 0, 0, 1, 0], Kind::Unknown),
        ([0x07, 0x3e, 0, 0x13, 0, 1, 0, 0], Kind::Unknown),
        ([0x07, 0x3e, 0, 0x21, 1, 0, 0, 0], Kind::Unknown),
        ([0x07, 0x3e, 0, 0x77, 0, 0, 1, 0], Kind::Unknown),
        ([0x07, 0x3e, 1, 0x61, 0, 0, 0, 0], Kind::Unknown),
        ([0x08, 0x3e, 0, 0x61, 0, 0, 0, 0], Kind::Unknown),
        ([0x07, 0x2a, 0, 0x60, 0, 0, 0, 0], Kind::Unknown),
        ([0x07, 0x2a, 0, 0x81, 0, 0, 0, 0x64], Kind::Unknown),
        ([0; 8], Kind::Unknown),
    ];
    for (opcode, kind) in cases {
        let got = parse_request(&asking(*opcode)).expect("a whole header");
        assert_eq!((got.opcode, got.kind), (*opcode, *kind), "{opcode:02x?}");
    }
}

#[test]
fn only_a_datagram_shorter_than_the_header_is_no_request() {
    let req = captured();
    assert_eq!(parse_request(&req[..31]), None);
    assert_eq!(parse_request(&[]), None);
    // As inferno: neither the start code nor the length field is checked,
    // and anything after the header is ignored.
    let mut longer = req.clone();
    longer.extend_from_slice(&[1, 2, 3]);
    assert_eq!(parse_request(&longer).unwrap().kind, Kind::BoardInfo);
    let mut odd = req.clone();
    odd[0..4].copy_from_slice(&[0x12, 0x34, 0x00, 0x99]);
    assert_eq!(parse_request(&odd).unwrap().kind, Kind::BoardInfo);
}

#[test]
fn the_board_info_is_inferno_s_bytes() {
    let mut mc = Multicaster::new(identity());
    let want = packet(0xffff, 1, BOARD_INFO, &board_info_body(b"iem-dante"));
    assert_eq!(want.len(), 232);
    assert_eq!(mc.board_info(), want);
    // A 31-byte model is cut at 8 and 16 bytes; nothing past 0x48 moves.
    let long = "abcdefghijklmnopqrstuvwxyz01234";
    let mut id = identity();
    id.model = long.to_string();
    let mut mc = Multicaster::new(id);
    assert_eq!(
        mc.board_info(),
        packet(0xffff, 1, BOARD_INFO, &board_info_body(long.as_bytes()))
    );
}

#[test]
fn the_product_info_is_inferno_s_bytes() {
    let mut mc = Multicaster::new(identity());
    let version = workspace_version();
    let body = sparse(
        336,
        &[
            (0x00, b"iemmixer"),
            (0x08, b"iem-dant"),
            (0x1c, &version),
            (0x2c, b"iemmixer"),
            (0xac, b"iem-dante"),
        ],
    );
    assert_eq!(mc.product_info(), packet(0xffff, 1, PRODUCT_INFO, &body));
    // The manufacturer is cut at 8 and 16 bytes, the model at 8 and 16.
    let mut id = identity();
    id.manufacturer = "ABCDEFGHIJKLMNOPQRSTUVWXYZ".to_string();
    id.model = "abcdefghijklmnopqrstuvwxyz".to_string();
    let mut mc = Multicaster::new(id);
    let body = sparse(
        336,
        &[
            (0x00, b"ABCDEFGH"),
            (0x08, b"abcdefgh"),
            (0x1c, &version),
            (0x2c, b"ABCDEFGHIJKLMNOP"),
            (0xac, b"abcdefghijklmnop"),
        ],
    );
    assert_eq!(mc.product_info(), packet(0xffff, 1, PRODUCT_INFO, &body));
}

#[test]
fn the_network_info_names_the_interface() {
    let mut mc = Multicaster::new(identity());
    #[rustfmt::skip]
    let mut body = vec![
        0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x03, 0xe8,                         // link speed, Mbit/s
        0x00, 0x01,
        0x02, 0x00, 0x00, 0x00, 0x00, 0x0a, // the MAC
        10, 0, 0, 10,                       // the address
        255, 255, 255, 0,                   // the netmask
        10, 0, 0, 1,                        // the gateway
        10, 0, 0, 1,                        // the gateway again (inferno: DNS)
        0x00, 0x18, 0x00, 0x30,
    ];
    body.extend_from_slice(&[0; 28]);
    assert_eq!(body.len(), 64);
    let req = asking([0x07, 0x2a, 0x00, 0x13, 0x00, 0x00, 0x00, 0x64]);
    assert_eq!(
        mc.answer(&req),
        Some(packet(0xffff, 1, NETWORK_INFO, &body))
    );
    // No gateway and a link of unknown speed are zeros.
    let mut id = identity();
    id.net.gateway = Ipv4Addr::UNSPECIFIED;
    id.net.link_speed = 0;
    let mut mc = Multicaster::new(id);
    body[6..8].copy_from_slice(&[0, 0]);
    body[24..32].copy_from_slice(&[0; 8]);
    assert_eq!(
        mc.answer(&req),
        Some(packet(0xffff, 1, NETWORK_INFO, &body))
    );
}

#[test]
fn each_known_request_gets_its_reply() {
    let mut mc = Multicaster::new(identity());
    let version = workspace_version();
    assert_eq!(
        mc.answer(&captured()),
        Some(packet(
            0xffff,
            1,
            BOARD_INFO,
            &board_info_body(b"iem-dante")
        ))
    );
    let product = mc
        .answer(&asking([0x07, 0x3e, 0x00, 0xc1, 0, 0, 0, 0]))
        .unwrap();
    assert_eq!(product[..32], header(0xffff, 368, 2, PRODUCT_INFO));
    assert_eq!(product[32 + 0x1c..32 + 0x20], version);
    assert_eq!(
        mc.answer(&asking([0x07, 0x3e, 0x00, 0x77, 0, 0, 0, 0x64])),
        Some(packet(0xffff, 3, ANSWER_0077, &[0, 0, 0, 3, 0, 0, 0, 0]))
    );
    let network = mc
        .answer(&asking([0x07, 0x3e, 0x00, 0x13, 0, 0, 0, 0]))
        .unwrap();
    assert_eq!(network[..32], header(0xffff, 96, 4, NETWORK_INFO));
}

#[test]
fn a_burst_of_requests_gets_at_most_ten_replies_a_kind_and_the_next_second_more() {
    // Each reply goes to the info group every Dante device hears: 1 000
    // requests in one second of each answered kind get ten replies each,
    // with sequences that skip none; the next second replies again. Byte 1
    // (and for 0x13 and 0x77 the last byte) varies, as from a sender that
    // tries to pass a bound kept per whole opcode.
    let t0 = Instant::now();
    let mut mc = Multicaster::new(identity());
    let mut limit = Limit::new();
    let kinds = [
        (0x61, BOARD_INFO),
        (0xc1, PRODUCT_INFO),
        (0x13, NETWORK_INFO),
        (0x77, ANSWER_0077),
    ];
    let mut seqs = Vec::new();
    for (byte, reply_op) in kinds {
        let mut replies = 0;
        for k in 0..1_000u16 {
            let last = if matches!(byte, 0x13 | 0x77) {
                (k >> 8) as u8
            } else {
                0
            };
            let req = asking([0x07, k as u8, 0x00, byte, 0, 0, 0, last]);
            let now = t0 + Duration::from_micros(u64::from(k) * 999);
            if let Some(p) = mc.answer_within(&mut limit, now, &req) {
                assert_eq!(p[24..32], reply_op);
                seqs.push(u16::from_be_bytes([p[4], p[5]]));
                replies += 1;
            }
        }
        assert_eq!(replies, limit::PER_SPAN, "{byte:#04x}");
    }
    assert_eq!(seqs, (1..=40).collect::<Vec<u16>>());
    let next = t0 + Duration::from_secs(1);
    assert_eq!(
        mc.answer_within(&mut limit, next, &captured()),
        Some(packet(
            0xffff,
            41,
            BOARD_INFO,
            &board_info_body(b"iem-dante")
        ))
    );
    // A request without a reply is not bounded: it still gets none.
    let clock = asking([0x07, 0x38, 0x00, 0x21, 0, 0, 0, 0x64]);
    assert_eq!(mc.answer_within(&mut limit, next, &clock), None);
    assert_eq!(mc.seq, 42);
}

#[test]
fn clock_stats_and_unknown_requests_get_no_reply_and_use_no_sequence() {
    let mut mc = Multicaster::new(identity());
    // inferno sends clock stats only with a clock's frequency offset;
    // phase A has no clock.
    assert_eq!(
        mc.answer(&asking([0x07, 0x38, 0x00, 0x21, 0, 0, 0, 0x64])),
        None
    );
    assert_eq!(
        mc.answer(&asking([0x07, 0x38, 0x00, 0x81, 0, 0, 0, 0x64])),
        None
    );
    assert_eq!(mc.answer(&captured()[..31]), None);
    assert_eq!(mc.heartbeat(None), packet(0xfffe, 1, HEARTBEAT, &[]));
}

#[test]
fn the_heartbeat_is_a_bare_header_with_no_clock() {
    // inferno before its clock has a frequency offset: no 0x8001 record,
    // no peaks, no latency; the header alone.
    let mut mc = Multicaster::new(identity());
    let beat = mc.heartbeat(None);
    #[rustfmt::skip]
    let want = [
        0xff, 0xfe, 0x00, 0x20, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x00, 10, 0, 0, 10, 0x00, 0x00,
        b'A', b'u', b'd', b'i', b'n', b'a', b't', b'e',
        0x00, 0x08, 0x00, 0x01, 0x10, 0x00, 0x00, 0x00,
    ];
    assert_eq!(beat, want);
}

#[test]
fn every_packet_takes_the_next_sequence_number_from_1_and_it_wraps() {
    let mut mc = Multicaster::new(identity());
    let seq = |p: &[u8]| u16::from_be_bytes([p[4], p[5]]);
    assert_eq!(seq(&mc.board_info()), 1);
    assert_eq!(seq(&mc.product_info()), 2);
    assert_eq!(seq(&mc.heartbeat(None)), 3);
    assert_eq!(seq(&mc.answer(&captured()).unwrap()), 4);
    assert_eq!(seq(&mc.heartbeat(None)), 5);
    mc.seq = 0xffff;
    assert_eq!(seq(&mc.heartbeat(None)), 0xffff);
    assert_eq!(seq(&mc.heartbeat(None)), 0);
    assert_eq!(seq(&mc.heartbeat(None)), 1);
}

#[test]
fn random_datagrams_never_panic() {
    let mut rng = Rng::new(0x1f0_8700);
    let mut mc = Multicaster::new(identity());
    let known = [0x61, 0xc1, 0x21, 0x13, 0x77];
    let mut answered = 0;
    for i in 0..10_000 {
        let len = rng.below(601);
        let mut buf = rng.bytes(len);
        // Every other buffer of header size gets a known opcode, so the
        // answering path is walked as often as the dropping one.
        if i % 2 == 0 && len >= HEADER_LEN {
            buf[24] = 0x07;
            buf[26] = 0x00;
            buf[27] = known[rng.below(known.len())];
            buf[28..31].copy_from_slice(&[0, 0, 0]);
            if rng.below(2) == 0 {
                buf[31] = 0;
            }
        }
        let before = mc.seq;
        match mc.answer(&buf) {
            Some(p) => {
                answered += 1;
                assert!(len >= HEADER_LEN);
                assert_eq!(usize::from(u16::from_be_bytes([p[2], p[3]])), p.len());
                assert_eq!(u16::from_be_bytes([p[4], p[5]]), before);
                assert_eq!(p[0..2], [0xff, 0xff]);
                assert_eq!(mc.seq, before.wrapping_add(1));
            }
            None => assert_eq!(mc.seq, before),
        }
    }
    assert!(answered > 2_000, "{answered}");
}

fn v4(addr: SocketAddr) -> SocketAddrV4 {
    match addr {
        SocketAddr::V4(v4) => v4,
        SocketAddr::V6(_) => panic!("a loopback IPv4 socket"),
    }
}

async fn recv(sock: &UdpSocket) -> Vec<u8> {
    let mut buf = [0u8; 2048];
    let (n, _) = tokio::time::timeout(BOUND, sock.recv_from(&mut buf))
        .await
        .expect("a packet within the bound")
        .unwrap();
    buf[..n].to_vec()
}

/// The info server on loopback, with receivers standing in for the
/// groups.
async fn loopback(every: Duration) -> (SocketAddr, UdpSocket, UdpSocket, Multicast, UdpSocket) {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let at = sock.local_addr().unwrap();
    let info_rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let beat_rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let to = Multicast {
        info: v4(info_rx.local_addr().unwrap()),
        heartbeat: v4(beat_rx.local_addr().unwrap()),
        every,
    };
    (at, info_rx, beat_rx, to, sock)
}

#[tokio::test]
async fn the_server_says_who_it_is_answers_to_the_info_group_and_beats() {
    let (at, info_rx, beat_rx, to, sock) = loopback(NEVER).await;
    let requests = Arc::new(Requests::default());
    let task = tokio::spawn(serve_info(
        sock,
        Multicaster::new(identity()),
        to,
        Arc::new(NoClock),
        Arc::new(Changes::default()),
        Arc::clone(&requests),
    ));
    // At start, as inferno: the board info, the product info, then the
    // first heartbeat at once.
    let board = recv(&info_rx).await;
    assert_eq!(board[..32], header(0xffff, 232, 1, BOARD_INFO));
    let product = recv(&info_rx).await;
    assert_eq!(product[..32], header(0xffff, 368, 2, PRODUCT_INFO));
    assert_eq!(recv(&beat_rx).await, packet(0xfffe, 3, HEARTBEAT, &[]));
    // An unknown request and a short datagram get nothing; the captured
    // request gets the board info, on the info group, not to its sender.
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let unknown = asking([0x07, 0x38, 0x00, 0x81, 0, 0, 0, 0x64]);
    for datagram in [&unknown[..], &captured()[..20], &captured()[..]] {
        peer.send_to(datagram, at).await.unwrap();
    }
    assert_eq!(
        recv(&info_rx).await,
        packet(0xffff, 4, BOARD_INFO, &board_info_body(b"iem-dante"))
    );
    let mut buf = [0u8; 64];
    let to_peer = tokio::time::timeout(Duration::from_millis(100), peer.recv_from(&mut buf)).await;
    assert!(to_peer.is_err(), "nothing goes back to the requester");
    // Every datagram counted for the status, the short one as opcode 0.
    let seen: Vec<(u16, usize, u16, u64)> = requests
        .counts()
        .received
        .iter()
        .map(|r| (r.port, r.peer, r.op, r.n))
        .collect();
    assert_eq!(
        seen,
        [
            (REQUEST_PORT, 1, 0x0000, 1),
            (REQUEST_PORT, 1, 0x0061, 1),
            (REQUEST_PORT, 1, 0x0081, 1)
        ]
    );
    assert!(!task.is_finished());
    task.abort();
}

/// Every packet that reaches `sock` until it has been quiet for 300 ms
/// (bounded by [`BOUND`] in all).
async fn drain(sock: &UdpSocket) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 2048];
    let end = tokio::time::Instant::now() + BOUND;
    while let Ok(Ok((n, _))) =
        tokio::time::timeout(Duration::from_millis(300), sock.recv_from(&mut buf)).await
    {
        out.push(buf[..n].to_vec());
        assert!(tokio::time::Instant::now() < end, "the replies never stop");
    }
    out
}

#[tokio::test]
async fn the_server_bounds_its_replies_to_the_info_group() {
    let (at, info_rx, _beat_rx, to, sock) = loopback(NEVER).await;
    let task = tokio::spawn(serve_info(
        sock,
        Multicaster::new(identity()),
        to,
        Arc::new(NoClock),
        Arc::new(Changes::default()),
        Arc::default(),
    ));
    recv(&info_rx).await;
    recv(&info_rx).await;
    // A burst of board and network info requests from one sender.
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let started = Instant::now();
    for k in 0..1_000u16 {
        for byte in [0x61, 0x13] {
            peer.send_to(&asking([0x07, k as u8, 0x00, byte, 0, 0, 0, 0]), at)
                .await
                .unwrap();
        }
    }
    let replies = drain(&info_rx).await;
    // At most ten of each kind in any second: the whole run took this long.
    let seconds = started.elapsed().saturating_sub(Duration::from_millis(300));
    let bound = limit::PER_SPAN * (1 + seconds.as_secs() as usize);
    for opcode in [BOARD_INFO, NETWORK_INFO] {
        let n = replies.iter().filter(|p| p[24..32] == opcode).count();
        assert!((1..=bound).contains(&n), "{n} replies, at most {bound}");
    }
    assert_eq!(
        replies.len(),
        replies
            .iter()
            .filter(|p| p[24..32] == BOARD_INFO || p[24..32] == NETWORK_INFO)
            .count()
    );
    // A second later it answers again.
    tokio::time::sleep(limit::SPAN).await;
    peer.send_to(&captured(), at).await.unwrap();
    let again = recv(&info_rx).await;
    assert_eq!(again[24..32], BOARD_INFO);
    assert!(!task.is_finished());
    task.abort();
}

/// Records for 2 TX and 3 RX channels: dantesync's -1234 ppb, peaks, RX
/// flow slot 1 seven samples late and slot 32 past `i32::MAX`.
fn records() -> Records {
    let mut lateness = [0u32; crate::media::rx::MAX_FLOWS];
    lateness[0] = 7;
    lateness[31] = 0x7fff_ffff;
    Records {
        freq_ppb: -1234,
        tx_peaks: vec![0, 255],
        rx_peaks: vec![40, 84, 255],
        rate: 96_000,
        lateness,
    }
}

#[test]
fn the_heartbeat_with_a_clock_holds_inferno_s_three_records() {
    let mut mc = Multicaster::new(identity());
    let beat = mc.heartbeat(Some(&records()));
    #[rustfmt::skip]
    let mut body = vec![
        // 0x8001: 16 bytes, 4, 4, this packet's sequence (1), 0, -1234 ppb.
        0x00, 0x10, 0x80, 0x01, 0x00, 0x04, 0x00, 0x04, 0x00, 0x01, 0x00, 0x00,
        0xff, 0xff, 0xfb, 0x2e,
        // 0x8002: 24 + 5 bytes, 4, 12 + 5, the sequence, 0; 2 TX, 3 RX, 24.
        0x00, 0x1d, 0x80, 0x02, 0x00, 0x04, 0x00, 0x11, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x02, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x18, 0x00, 0x00,
        // TX peaks, then RX peaks, padded to a multiple of 4.
        0x00, 0xff, 0x28, 0x54, 0xff, 0x00, 0x00, 0x00,
        // 0x8003: 24 + 128 bytes, 4, 12 + 128, the sequence, 0; 32 flow
        // slots, 0, 24, 0, 96 kHz.
        0x00, 0x98, 0x80, 0x03, 0x00, 0x04, 0x00, 0x8c, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x20, 0x00, 0x00, 0x00, 0x18, 0x00, 0x00, 0x00, 0x01, 0x77, 0x00,
        0x00, 0x00, 0x00, 0x07,
    ];
    body.extend([0; 30 * 4]);
    body.extend([0x7f, 0xff, 0xff, 0xff]);
    assert_eq!(body.len(), 200);
    assert_eq!(beat, packet(0xfffe, 1, HEARTBEAT, &body));
    // Each record carries its own packet's sequence; without a clock the
    // heartbeat is the header alone.
    mc.seq = 0x1234;
    let beat = mc.heartbeat(Some(&records()));
    for at in [32 + 8, 32 + 16 + 8, 32 + 48 + 8] {
        assert_eq!(beat[at..at + 2], [0x12, 0x34], "{at}");
    }
    assert_eq!(mc.heartbeat(None), packet(0xfffe, 0x1235, HEARTBEAT, &[]));
    // No channels: no peaks record (inferno sends it only with peaks).
    let quiet = Records {
        tx_peaks: Vec::new(),
        rx_peaks: Vec::new(),
        ..records()
    };
    let body = heartbeat_body(1, &quiet);
    assert_eq!(body.len(), 16 + 152);
    assert_eq!(body[16..20], [0x00, 0x98, 0x80, 0x03]);
    // One peak pads by three.
    let one = Records {
        tx_peaks: vec![9],
        rx_peaks: Vec::new(),
        ..records()
    };
    let body = heartbeat_body(1, &one);
    assert_eq!(body[16..18], [0x00, 25]);
    assert_eq!(body[16 + 24..16 + 28], [9, 0, 0, 0]);
    assert_eq!(body[16 + 28..16 + 32], [0x00, 0x98, 0x80, 0x03]);
    // Four peaks need no padding.
    let four = Records {
        tx_peaks: vec![1, 2],
        rx_peaks: vec![3, 4],
        ..records()
    };
    let body = heartbeat_body(1, &four);
    assert_eq!(body[16 + 24..16 + 32], [1, 2, 3, 4, 0x00, 0x98, 0x80, 0x03]);
}

#[test]
fn a_peak_is_minus_40_log10_of_full_scale_as_inferno_computes_it() {
    let full = i32::MAX as u32;
    assert_eq!(peak_byte(0), 255, "silence");
    assert_eq!(peak_byte(1), 255, "far below the scale");
    assert_eq!(peak_byte(full), 0);
    assert_eq!(peak_byte(1 << 31), 0, "i32::MIN's magnitude");
    assert_eq!(peak_byte(u32::MAX), 0);
    assert_eq!(peak_byte(full / 10), 40, "-20 dBFS");
    assert_eq!(peak_byte(full / 100), 80, "-40 dBFS");
    assert_eq!(peak_byte(65_535 << 8), 84, "the counter's top");
    assert_eq!(peak_byte(full / 2), 12);
}

#[test]
fn the_channel_change_bitmask_is_inferno_s() {
    assert_eq!(channel_change_body(&[]), [0, 1, 0]);
    assert_eq!(channel_change_body(&[0]), [0, 1, 0x01]);
    assert_eq!(channel_change_body(&[7]), [0, 1, 0x80]);
    assert_eq!(channel_change_body(&[8]), [0, 2, 0, 0x01]);
    assert_eq!(channel_change_body(&[3, 3]), [0, 1, 0x08]);
    assert_eq!(
        channel_change_body(&[31, 9, 0]),
        [0, 4, 0x01, 0x02, 0x00, 0x80]
    );
    let last = channel_change_body(&[511]);
    assert_eq!(last.len(), 66);
    assert_eq!(last[..2], [0, 64]);
    assert_eq!(last[65], 0x80);
    let mut mc = Multicaster::new(identity());
    assert_eq!(
        mc.channel_change(&[4, 12]),
        packet(
            0xffff,
            1,
            [0x07, 0x2a, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00],
            &[0, 2, 0x10, 0x10]
        )
    );
}

#[test]
fn changes_are_coalesced_and_the_notification_is_bounded() {
    let t0 = Instant::now();
    let mut mc = Multicaster::new(identity());
    let mut limit = Limit::new();
    let changes = Changes::default();
    // Nothing waits: nothing sent, no sequence used.
    assert_eq!(mc.notify_within(&mut limit, t0, &changes), None);
    assert_eq!(mc.seq, 1);
    // A channel changes every millisecond for a second: at most ten
    // notifications, and every channel is named, none lost.
    let mut sent = Vec::new();
    let mut named = BTreeSet::new();
    for k in 0..1_000u64 {
        changes.add([(k % 40) as usize]);
        let now = t0 + Duration::from_millis(k);
        if let Some(p) = mc.notify_within(&mut limit, now, &changes) {
            assert_eq!(p[24..32], [0x07, 0x2a, 0x01, 0x02, 0, 0, 0, 0]);
            for (byte, bits) in p[34..].iter().enumerate() {
                for bit in 0..8 {
                    if bits & (1 << bit) != 0 {
                        named.insert(byte * 8 + bit);
                    }
                }
            }
            sent.push(u16::from_be_bytes([p[4], p[5]]));
        }
    }
    assert_eq!(sent, (1..=10).collect::<Vec<u16>>());
    assert!(!changes.is_empty(), "the rest waits");
    // The next second: one notification names everything that waited.
    let p = mc
        .notify_within(&mut limit, t0 + Duration::from_secs(1), &changes)
        .unwrap();
    for (byte, bits) in p[34..].iter().enumerate() {
        for bit in 0..8 {
            if bits & (1 << bit) != 0 {
                named.insert(byte * 8 + bit);
            }
        }
    }
    assert_eq!(named, (0..40).collect());
    assert!(changes.is_empty());
    // Nothing of it counts as a dropped reply.
    assert_eq!(limit.flush(t0 + Duration::from_secs(120)), None);
}

#[test]
fn changes_take_every_channel_once_in_order() {
    let changes = Changes::default();
    assert!(changes.is_empty());
    changes.add([5, 1]);
    changes.add([1]);
    changes.add(std::iter::empty());
    assert!(!changes.is_empty());
    assert_eq!(changes.take(), [1, 5]);
    assert!(changes.is_empty());
    assert_eq!(changes.take(), Vec::<usize>::new());
}

#[tokio::test]
async fn adding_a_change_wakes_a_waiter_once() {
    let changes = Changes::default();
    // A change added before the wait wakes it at once.
    changes.add([3]);
    tokio::time::timeout(BOUND, changes.added())
        .await
        .expect("woken");
    // A channel already waiting wakes nobody again.
    changes.add([3]);
    let again = tokio::time::timeout(Duration::from_millis(50), changes.added()).await;
    assert!(again.is_err());
    // A new one does.
    changes.add([4]);
    tokio::time::timeout(BOUND, changes.added())
        .await
        .expect("woken");
}

/// A beat with fixed records.
struct Fixed(Option<Records>);

impl Beat for Fixed {
    fn records(&self) -> Option<Records> {
        self.0.clone()
    }
}

/// A device whose media clock is not ready: the heartbeat is the header
/// alone.
struct NoClock;

impl Beat for NoClock {
    fn records(&self) -> Option<Records> {
        None
    }
}

#[tokio::test]
async fn the_server_beats_with_its_records_and_notifies_changes() {
    let (_, info_rx, beat_rx, to, sock) = loopback(NEVER).await;
    let changes = Arc::new(Changes::default());
    let task = tokio::spawn(serve_info(
        sock,
        Multicaster::new(identity()),
        to,
        Arc::new(Fixed(Some(records()))),
        Arc::clone(&changes),
        Arc::default(),
    ));
    recv(&info_rx).await;
    recv(&info_rx).await;
    let beat = recv(&beat_rx).await;
    assert_eq!(beat.len(), 232);
    assert_eq!(beat[32..34], [0x00, 0x10]);
    // RX 2 and RX 10 changed: one notification on the info group.
    changes.add([1, 9]);
    let note = recv(&info_rx).await;
    assert_eq!(
        note,
        packet(
            0xffff,
            4,
            [0x07, 0x2a, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00],
            &[0, 2, 0x02, 0x02]
        )
    );
    assert!(changes.is_empty());
    task.abort();
}

#[tokio::test]
async fn the_server_sends_a_held_back_notification_within_a_second_or_two() {
    let (_, info_rx, _beat_rx, to, sock) = loopback(NEVER).await;
    let changes = Arc::new(Changes::default());
    let task = tokio::spawn(serve_info(
        sock,
        Multicaster::new(identity()),
        to,
        Arc::new(NoClock),
        Arc::clone(&changes),
        Arc::default(),
    ));
    recv(&info_rx).await;
    recv(&info_rx).await;
    // Twelve changes as fast as they come: ten notifications at once, the
    // rest held back by the bound, then sent at a check.
    let mut seen = BTreeSet::new();
    for rx in 0..12 {
        changes.add([rx]);
        tokio::task::yield_now().await;
    }
    let started = Instant::now();
    while seen.len() < 12 {
        let note = recv(&info_rx).await;
        assert_eq!(note[24..28], [0x07, 0x2a, 0x01, 0x02]);
        for (byte, bits) in note[34..].iter().enumerate() {
            for bit in 0..8 {
                if bits & (1 << bit) != 0 {
                    seen.insert(byte * 8 + bit);
                }
            }
        }
    }
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(seen, (0..12).collect());
    task.abort();
}

#[test]
fn the_records_come_once_the_clock_is_ready_and_each_beat_takes_its_own_second() {
    use crate::clock::{ManualClock, stamp};
    use crate::media::packet;
    use crate::media::ring::Ring;
    use crate::media::rx::{Receiver, RxPlan, RxRoute};
    use crate::subscribe::resolve::{Advertised, Resolver};
    use crate::subscribe::{Action, Answer};

    struct One(Advertised);
    impl Resolver for One {
        fn lookup(&self, _: &str, _: &str) -> Option<Advertised> {
            Some(self.0)
        }
    }

    let cfg = cfg();
    let lo = Ipv4Addr::LOCALHOST;
    let rings: Arc<[Ring]> = (0..cfg.rx.len())
        .map(|_| Ring::new())
        .collect::<Vec<_>>()
        .into();
    let tx = Arc::new(TxShared::new(cfg.clone(), lo, Arc::clone(&rings), 1, &[]).unwrap());
    let rx = Arc::new(RxShared::new(cfg.sample_rate, lo, Arc::clone(&rings)).unwrap());
    let ports = rx.ports().unwrap();
    // The manager holds a granted flow in slot 1 only.
    let mut m = Manager::new(&cfg, "test-device", lo, ports);
    m.set(
        0,
        Some(crate::device::Subscription {
            tx_channel: "TX 1".into(),
            tx_device: "peer-device".into(),
        }),
    );
    let peer = Advertised {
        control: SocketAddrV4::new(lo, 4455),
        tx_channel_id: 1,
        nchan: 1,
        bits: 24,
        rate: 96_000,
        dbcp1: 0x1102,
        fpp_min: 1,
        fpp_max: 16,
        latency_ns: 2_000_000,
    };
    let Some(Action::Request { flow, .. }) = m.tick(0, &[0; 32], &One(peer)).pop() else {
        panic!("a request");
    };
    assert_eq!(flow.slot, 0);
    m.answered(flow, Answer::Granted([0, 0, 0, 0, 0, 1]), 0);
    let clock = Arc::new(ManualClock::new(0));
    clock.set_ready(false);
    let freq = Arc::new(AtomicI32::new(-1234));
    let sources = Sources {
        clock: clock.clone(),
        freq: Arc::clone(&freq),
        tx,
        rx: Arc::clone(&rx),
        manager: Arc::new(Mutex::new(m)),
    };
    // Not ready: no records, nothing taken.
    rings[2].write_at(5, &[i32::MAX / 10]);
    assert_eq!(sources.records(), None);
    clock.set_ready(true);
    // Slots 1 and 2 each receive a packet 300 samples late.
    let mut receiver = Receiver::new(Arc::clone(&rx)).unwrap();
    let plan = RxPlan {
        src: Some(lo),
        bps: 3,
        nslots: 1,
        latency: 192,
        live: true,
        generation: 1,
    };
    rx.publish(
        |slot| if slot < 2 { plan } else { RxPlan::default() },
        |ch| (ch < 2).then_some(RxRoute { flow: ch, slot: 0 }),
    );
    let now_samples = 1_000_000u64;
    let now_ns = crate::media::tx::ns_of(now_samples, 96_000);
    let sender = std::net::UdpSocket::bind((lo, 0)).unwrap();
    let mut buf = [0u8; 64];
    let (secs, sub) = stamp(now_samples - 300, 96_000);
    let n = packet::write(&mut buf, secs, sub, 1, 3, 1, |_, _| 0);
    for slot in 0..2 {
        sender
            .send_to(&buf[..n], rx.local_addr(slot).unwrap())
            .unwrap();
    }
    let deadline = Instant::now() + BOUND;
    while rx.lane(0).packets + rx.lane(1).packets < 2 {
        receiver.wait(Duration::from_millis(20)).unwrap();
        receiver.pass(Some(now_ns), 0);
        assert!(Instant::now() < deadline, "the packets never came");
    }
    let r = sources.records().unwrap();
    assert_eq!(r.freq_ppb, -1234);
    assert_eq!(r.rate, 96_000);
    assert_eq!(r.tx_peaks, vec![255; 16], "nothing sent");
    let mut want = vec![255; 32];
    want[2] = 40;
    assert_eq!(r.rx_peaks, want);
    // Only the slot with a granted flow reports its lateness.
    let mut late = [0u32; 32];
    late[0] = 300;
    assert_eq!(r.lateness, late);
    // Every slot's second was taken, the second slot's too (no grant): the
    // largest in samples and the packets' least in ns (300 samples,
    // 3 125 000 ns).
    for slot in 0..2 {
        assert_eq!(rx.last_lateness(slot), 300, "slot {slot}");
        assert_eq!(
            rx.last_min_lateness_ns(slot),
            Some(3_125_000),
            "slot {slot}"
        );
    }
    // The next beat has its own second: the peaks and lateness were taken.
    freq.store(77, Ordering::Relaxed);
    let r = sources.records().unwrap();
    assert_eq!(r.freq_ppb, 77);
    assert_eq!(r.rx_peaks, vec![255; 32]);
    assert_eq!(r.lateness, [0; 32]);
    for slot in 0..2 {
        assert_eq!(rx.last_lateness(slot), 0, "slot {slot}");
        assert_eq!(rx.last_min_lateness_ns(slot), None, "slot {slot}");
    }
}

#[tokio::test]
async fn the_heartbeat_repeats_at_its_period() {
    let (_, info_rx, beat_rx, to, sock) = loopback(Duration::from_millis(20)).await;
    let task = tokio::spawn(serve_info(
        sock,
        Multicaster::new(identity()),
        to,
        Arc::new(NoClock),
        Arc::new(Changes::default()),
        Arc::default(),
    ));
    recv(&info_rx).await;
    recv(&info_rx).await;
    for seq in 3..6 {
        assert_eq!(recv(&beat_rx).await, packet(0xfffe, seq, HEARTBEAT, &[]));
    }
    task.abort();
}
