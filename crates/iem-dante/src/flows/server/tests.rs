use super::*;

use std::time::Duration;

use crate::flows::control::{FlowRequest, Handle, build_request, build_stop, build_update, split};
use crate::media::ring::Ring;
use crate::media::tx::{KEEPALIVE_TIMEOUT_NS, Transmitter, TxFlow};
use crate::wire::tests::Rng;
use crate::wire::{HEADER_LEN, request as datagram};

const TEST_DEVICE: &str = include_str!("../../../../../config/test-dante-device.toml");
const PEER_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 11);
/// The requester's control socket.
const FROM: SocketAddrV4 = SocketAddrV4::new(PEER_IP, 50_000);
/// Where the requester wants the audio.
const RX: SocketAddrV4 = SocketAddrV4::new(PEER_IP, 4321);
/// Ten years of a grandmaster's uptime, ns.
const T: u64 = 10 * 365 * 86_400 * 1_000_000_000;

/// The test device: 16 TX channels at 96 kHz, 24 bits, 1 ms.
fn cfg() -> DeviceConfig {
    DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-dante-device.toml")
}

fn req() -> FlowRequest {
    FlowRequest {
        rx_host: "peer-device".to_string(),
        rx_flow_name: "flow-1".to_string(),
        sample_rate: 96_000,
        bits: 24,
        channels: vec![Some(1), Some(2)],
        fpp: 32,
        rx: RX,
    }
}

/// The server's side of one test: the table and the config.
struct Dev {
    table: TxTable,
    cfg: DeviceConfig,
    seq: u16,
}

/// A reply, decoded.
#[derive(Debug)]
struct Answer {
    header: Header,
    body: Vec<u8>,
}

impl Dev {
    fn new() -> Self {
        Dev::with(cfg())
    }

    fn with(cfg: DeviceConfig) -> Self {
        Dev {
            table: TxTable::new(0x5eed),
            cfg,
            seq: 0,
        }
    }

    /// A device whose channels offered 256 frames a packet (a config sets
    /// at most 32): inferno's bound and one datagram's are reached only
    /// past `tx_fpp_max`, so their tests ask such a device.
    fn offering_256() -> Self {
        Dev::with(DeviceConfig {
            tx_fpp_max: 256,
            ..cfg()
        })
    }

    fn send(&mut self, bytes: &[u8], now: u64) -> FlowOutcome {
        handle(&mut self.table, &self.cfg, FROM, bytes, now)
    }

    fn next_seq(&mut self) -> u16 {
        self.seq += 1;
        self.seq
    }

    /// A flow request, answered.
    fn ask(&mut self, r: &FlowRequest) -> Answer {
        let seq = self.next_seq();
        let a = decode(self.send(&build_request(seq, r), 0));
        assert_eq!(
            (a.header.start, a.header.seq, a.header.op1),
            (control::START, seq, op::REQUEST),
            "the reply echoes the request"
        );
        a
    }

    fn stop(&mut self, h: &Handle) -> Answer {
        let seq = self.next_seq();
        decode(self.send(&build_stop(seq, h), 0))
    }

    fn update(&mut self, h: &Handle, channels: &[Option<u16>], now: u64) -> Answer {
        let seq = self.next_seq();
        decode(self.send(&build_update(seq, h, channels), now))
    }

    /// A request that must be answered with a handle.
    fn flow(&mut self, r: &FlowRequest) -> Handle {
        let a = self.ask(r);
        assert_eq!(a.header.op2, status::OK, "{r:?}");
        a.body.try_into().expect("a 6-byte handle")
    }

    fn flows(&self) -> Vec<(usize, TxFlow)> {
        self.table.flows().map(|(s, f)| (s, f.clone())).collect()
    }
}

fn decode(outcome: FlowOutcome) -> Answer {
    let FlowOutcome::Reply(bytes) = outcome else {
        panic!("not answered: {outcome:?}");
    };
    let (header, body) = Header::parse(&bytes).expect("a reply parses");
    Answer {
        header,
        body: body.to_vec(),
    }
}

/// A refused request: its status, no body, and the table as it was.
fn refused_with(dev: &mut Dev, r: &FlowRequest, code: u16) {
    let before = dev.flows();
    let a = dev.ask(r);
    assert_eq!((a.header.op2, a.body.len()), (code, 0), "{r:?}");
    assert_eq!(dev.flows(), before, "a refusal changes nothing");
}

#[test]
fn a_request_is_answered_with_its_handle_and_the_flow_is_in_the_table() {
    let mut dev = Dev::new();
    let a = dev.ask(&req());
    assert_eq!(a.header.op2, status::OK);
    assert_eq!(a.header.len, 16);
    let h: Handle = a.body.clone().try_into().unwrap();
    let (slot, cookie) = split(&h);
    assert_eq!(slot, 0);
    assert_eq!(
        dev.flows(),
        vec![(
            0,
            TxFlow {
                dest: RX,
                rx_host: "peer-device".to_string(),
                rx_flow_name: "flow-1".to_string(),
                channels: vec![Some(1), Some(2)],
                fpp: 32,
                bps: 3,
                cookie,
                heard_ns: 0,
                expired: false,
            }
        )]
    );
    assert_eq!(dev.table.get(&h).map(|(s, _)| s), Some(0));
}

#[test]
fn a_channel_id_past_ours_is_0x0302_once_and_changes_nothing() {
    let mut dev = Dev::new();
    // 16 TX channels: 16 is the last, 17 is past them (inferno: id − 1 <
    // the count); an empty slot is no id.
    refused_with(
        &mut dev,
        &FlowRequest {
            channels: vec![Some(1), Some(17)],
            ..req()
        },
        status::BAD_REQUEST,
    );
    // inferno checks the ids first: another rate too still reads 0x0302.
    refused_with(
        &mut dev,
        &FlowRequest {
            channels: vec![Some(17)],
            sample_rate: 48_000,
            ..req()
        },
        status::BAD_REQUEST,
    );
    dev.flow(&FlowRequest {
        channels: vec![Some(16), None, Some(1)],
        ..req()
    });
    assert_eq!(dev.flows().len(), 1);
}

#[test]
fn another_sample_rate_is_0x0301_before_the_frames_are_checked() {
    let mut dev = Dev::new();
    for rate in [48_000, 95_999, 192_000, 0] {
        refused_with(
            &mut dev,
            &FlowRequest {
                sample_rate: rate,
                ..req()
            },
            status::SAMPLE_RATE_MISMATCH,
        );
    }
    refused_with(
        &mut dev,
        &FlowRequest {
            sample_rate: 48_000,
            fpp: 300,
            ..req()
        },
        status::SAMPLE_RATE_MISMATCH,
    );
}

#[test]
fn more_than_256_frames_is_0x0302_and_256_are_taken() {
    let mut dev = Dev::offering_256();
    refused_with(
        &mut dev,
        &FlowRequest { fpp: 257, ..req() },
        status::BAD_REQUEST,
    );
    // One 16-bit slot at 256 frames: 9 + 512 bytes.
    dev.flow(&FlowRequest {
        fpp: 256,
        bits: 16,
        channels: vec![Some(1)],
        ..req()
    });
}

#[test]
fn more_frames_than_our_channels_offer_is_0x0302_and_tx_fpp_max_is_taken() {
    // The review of PR #84 (#72 phase F): the config's advertised latency
    // covers tx_fpp_max frames a packet; a receiver asking more would get
    // our packets after it plays them.
    let mut dev = Dev::with(DeviceConfig {
        tx_fpp_max: 8,
        ..cfg()
    });
    for fpp in [9, 16, 32, 256] {
        refused_with(&mut dev, &FlowRequest { fpp, ..req() }, status::BAD_REQUEST);
    }
    dev.flow(&FlowRequest { fpp: 8, ..req() });
    // The test device offers the default, 32.
    let mut dev = Dev::new();
    refused_with(
        &mut dev,
        &FlowRequest { fpp: 33, ..req() },
        status::BAD_REQUEST,
    );
    dev.flow(&FlowRequest { fpp: 32, ..req() });
    // A refusal the gap guard counts.
    assert_eq!(
        dev.send(&build_request(1, &FlowRequest { fpp: 33, ..req() }), 0)
            .gap(),
        Some((op::REQUEST, Gap::Refused))
    );
}

#[test]
fn our_safety_refusals_are_0x0302_and_change_nothing() {
    let mut dev = Dev::offering_256();
    let other_ip = SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 12), 4321);
    let cases = [
        FlowRequest { fpp: 0, ..req() },
        FlowRequest { bits: 8, ..req() },
        FlowRequest { bits: 15, ..req() },
        FlowRequest { bits: 40, ..req() },
        FlowRequest { bits: 0, ..req() },
        FlowRequest {
            channels: vec![Some(1); 9],
            ..req()
        },
        // 8 slots of 32 bits: 46 frames are 9 + 1 472 bytes, one too many.
        FlowRequest {
            channels: vec![Some(1); 8],
            bits: 32,
            fpp: 46,
            ..req()
        },
        // Audio to another device than the requester.
        FlowRequest {
            rx: other_ip,
            ..req()
        },
        FlowRequest {
            rx: SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 1), 4321),
            ..req()
        },
        FlowRequest {
            rx: SocketAddrV4::new(PEER_IP, 0),
            ..req()
        },
    ];
    for r in &cases {
        refused_with(&mut dev, r, status::BAD_REQUEST);
    }
    // The edges that are taken: 45 frames of 8 × 32 bits (9 + 1 440
    // bytes), 8 slots, bits / 8 as inferno takes it (31 bits are 3 bytes),
    // no slots at all.
    for (port, r) in [
        FlowRequest {
            channels: vec![Some(1); 8],
            bits: 32,
            fpp: 45,
            ..req()
        },
        FlowRequest { bits: 16, ..req() },
        FlowRequest { bits: 31, ..req() },
        FlowRequest {
            channels: vec![],
            ..req()
        },
    ]
    .into_iter()
    .enumerate()
    {
        dev.flow(&FlowRequest {
            rx: SocketAddrV4::new(PEER_IP, 5000 + port as u16),
            ..r
        });
    }
    let widths: Vec<(u8, u16, usize)> = dev
        .flows()
        .iter()
        .map(|(_, f)| (f.bps, f.fpp, f.channels.len()))
        .collect();
    assert_eq!(widths, [(4, 45, 8), (2, 32, 2), (3, 32, 2), (3, 32, 0)]);
}

#[test]
fn the_33rd_flow_is_too_many_until_expired_flows_are_freed() {
    let mut dev = Dev::new();
    for port in 0..MAX_FLOWS as u16 {
        let h = dev.flow(&FlowRequest {
            rx: SocketAddrV4::new(PEER_IP, 6000 + port),
            ..req()
        });
        assert_eq!(split(&h).0, u32::from(port), "slots in turn");
    }
    let r33 = FlowRequest {
        rx: SocketAddrV4::new(PEER_IP, 7000),
        ..req()
    };
    refused_with(&mut dev, &r33, status::TOO_MANY_TX_FLOWS);
    // A request for a destination already served still gets its handle.
    dev.flow(&FlowRequest {
        rx: SocketAddrV4::new(PEER_IP, 6005),
        ..req()
    });
    // Every flow expires; the 33rd request frees them all (inferno's
    // scan) and takes a slot.
    assert_eq!(dev.table.expire(KEEPALIVE_TIMEOUT_NS), u32::MAX);
    dev.flow(&r33);
    let flows = dev.flows();
    assert_eq!(flows.len(), 1);
    assert_eq!(flows[0].1.dest, r33.rx);
}

#[test]
fn a_second_request_for_one_destination_keeps_its_handle_and_sets_the_flow() {
    let mut dev = Dev::new();
    let h = dev.flow(&req());
    let again = FlowRequest {
        channels: vec![Some(5)],
        fpp: 16,
        bits: 32,
        rx_flow_name: "flow-2".to_string(),
        ..req()
    };
    assert_eq!(dev.flow(&again), h);
    let flows = dev.flows();
    assert_eq!(flows.len(), 1);
    let f = &flows[0].1;
    assert_eq!(
        (f.channels.clone(), f.fpp, f.bps, f.rx_flow_name.as_str()),
        (vec![Some(5)], 16, 4, "flow-2")
    );
    // Another port of the same device is another destination.
    assert_ne!(
        dev.flow(&FlowRequest {
            rx: SocketAddrV4::new(PEER_IP, 4322),
            ..req()
        }),
        h
    );
}

#[test]
fn update_sets_the_channels_and_revives_an_expired_flow() {
    let mut dev = Dev::new();
    let h = dev.flow(&req());
    assert_eq!(dev.table.expire(KEEPALIVE_TIMEOUT_NS), 1);
    assert_eq!(
        dev.table.plan(0).dest,
        None,
        "an expired flow sends nothing"
    );
    let a = dev.update(&h, &[Some(3), None, Some(4)], 5 * KEEPALIVE_TIMEOUT_NS);
    assert_eq!((a.header.op2, a.body.len()), (status::OK, 0));
    let f = dev.table.flow(0).unwrap();
    assert_eq!(f.channels, [Some(3), None, Some(4)]);
    assert_eq!((f.expired, f.heard_ns), (false, 5 * KEEPALIVE_TIMEOUT_NS));
    assert_eq!(dev.table.plan(0).dest, Some(RX));
}

#[test]
fn an_update_that_cannot_be_sent_is_0x0302_and_changes_nothing() {
    let mut dev = Dev::offering_256();
    // 8 slots of 24 bits at 32 frames: 9 + 768 bytes; 61 frames would not
    // fit, so a flow at 61 frames takes 7 slots only.
    let h = dev.flow(&FlowRequest {
        fpp: 61,
        channels: vec![Some(1); 7],
        ..req()
    });
    let before = dev.flows();
    for channels in [vec![Some(17)], vec![Some(1); 8], vec![None; 9]] {
        let a = dev.update(&h, &channels, 0);
        assert_eq!(
            (a.header.op2, a.body.len()),
            (status::BAD_REQUEST, 0),
            "{channels:?}"
        );
        assert_eq!(dev.flows(), before);
    }
    let a = dev.update(&h, &[Some(16)], 0);
    assert_eq!(a.header.op2, status::OK);
}

#[test]
fn a_stop_ends_the_flow_and_an_unknown_handle_is_0x0103() {
    let mut dev = Dev::new();
    let h = dev.flow(&req());
    let (slot, cookie) = split(&h);
    for unknown in [
        control::handle(slot, cookie.wrapping_add(1)),
        control::handle(slot + 1, cookie),
        control::handle(u32::MAX, cookie),
    ] {
        let a = dev.stop(&unknown);
        assert_eq!((a.header.op2, a.body.len()), (status::FLOW_NOT_FOUND, 0));
        let a = dev.update(&unknown, &[Some(1)], 0);
        assert_eq!((a.header.op2, a.body.len()), (status::FLOW_NOT_FOUND, 0));
        assert_eq!(dev.flows().len(), 1);
    }
    let a = dev.stop(&h);
    assert_eq!(
        (a.header.op1, a.header.op2, a.body.len()),
        (op::STOP, status::OK, 0)
    );
    assert!(dev.flows().is_empty());
    let a = dev.stop(&h);
    assert_eq!(a.header.op2, status::FLOW_NOT_FOUND);
    // An expired flow is stopped like a live one.
    let h = dev.flow(&req());
    dev.table.expire(KEEPALIVE_TIMEOUT_NS);
    assert_eq!(dev.stop(&h).header.op2, status::OK);
    assert!(dev.flows().is_empty());
}

#[test]
fn what_inferno_cannot_read_gets_no_reply_and_is_counted() {
    let mut dev = Dev::new();
    let good = build_request(1, &req());
    let mut cases: Vec<(Vec<u8>, u16)> = Vec::new();
    // A request body of 16 bytes, cut ids, a missing string, the
    // descriptor past the end (each with a true length field).
    for body_len in [0, 16, 25] {
        cases.push((
            datagram(
                control::START,
                1,
                op::REQUEST,
                0,
                &good[HEADER_LEN..HEADER_LEN + body_len],
            ),
            op::REQUEST,
        ));
    }
    let mut missing = good.clone();
    missing[HEADER_LEN..HEADER_LEN + 2].copy_from_slice(&500u16.to_be_bytes());
    cases.push((missing, op::REQUEST));
    let mut no_descriptor = good.clone();
    no_descriptor[HEADER_LEN + 14..HEADER_LEN + 16].copy_from_slice(&75u16.to_be_bytes());
    cases.push((no_descriptor, op::REQUEST));
    // A stop of 5 or 7 bytes, an update cut.
    cases.push((datagram(control::START, 2, op::STOP, 0, &[0; 5]), op::STOP));
    cases.push((datagram(control::START, 2, op::STOP, 0, &[0; 7]), op::STOP));
    cases.push((
        datagram(control::START, 3, op::UPDATE, 0, &[0, 0, 0, 0, 0, 0, 0]),
        op::UPDATE,
    ));
    cases.push((
        datagram(
            control::START,
            3,
            op::UPDATE,
            0,
            &[0, 0, 0, 0, 0, 0, 0, 2, 0, 1],
        ),
        op::UPDATE,
    ));
    // Any other opcode, and op2 not 0.
    cases.push((datagram(control::START, 4, 0x0103, 0, &[]), 0x0103));
    cases.push((
        datagram(control::START, 4, 0x2200, 0, &good[HEADER_LEN..]),
        0x2200,
    ));
    cases.push((
        datagram(control::START, 5, op::REQUEST, 7, &good[HEADER_LEN..]),
        op::REQUEST,
    ));
    for (bytes, op) in cases {
        let outcome = dev.send(&bytes, 0);
        assert_eq!(outcome, FlowOutcome::Unanswered { op }, "{bytes:02x?}");
        assert_eq!(outcome.gap(), Some((op, Gap::Unanswered)));
        assert!(dev.flows().is_empty());
    }
}

#[test]
fn no_reply_ever_answers_a_reply() {
    let mut dev = Dev::new();
    let ok = dev.send(&build_request(1, &req()), 0);
    let refusal = dev.send(
        &build_request(
            2,
            &FlowRequest {
                sample_rate: 1,
                ..req()
            },
        ),
        0,
    );
    let stopped = dev.send(&build_stop(3, &[0; 6]), 0);
    for outcome in [ok, refusal, stopped] {
        let FlowOutcome::Reply(bytes) = outcome else {
            panic!("answered");
        };
        // Our reply fed back to a server like ours: op2 is its status,
        // never 0, so it is never answered.
        let (h, _) = Header::parse(&bytes).unwrap();
        assert_ne!(h.op2, 0);
        assert_eq!(dev.send(&bytes, 0), FlowOutcome::Unanswered { op: h.op1 });
    }
}

#[test]
fn a_misframed_datagram_is_counted_and_a_short_one_ignored() {
    let mut dev = Dev::new();
    let mut bytes = build_request(1, &req());
    bytes.push(0);
    let outcome = dev.send(&bytes, 0);
    assert_eq!(outcome, FlowOutcome::Misframed { op: op::REQUEST });
    assert_eq!(outcome.gap(), Some((op::REQUEST, Gap::Misframed)));
    for n in 0..HEADER_LEN {
        let outcome = dev.send(&bytes[..n], 0);
        assert_eq!(outcome, FlowOutcome::Ignored);
        assert_eq!(outcome.gap(), None);
    }
    assert!(dev.flows().is_empty());
}

#[test]
fn a_source_no_reply_can_go_to_is_ignored() {
    let mut dev = Dev::new();
    let bytes = build_request(1, &req());
    for from in [
        SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 231), 4455),
        SocketAddrV4::new(Ipv4Addr::BROADCAST, 4455),
        SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 4455),
        SocketAddrV4::new(PEER_IP, 0),
    ] {
        assert_eq!(
            handle(&mut dev.table, &dev.cfg, from, &bytes, 0),
            FlowOutcome::Ignored,
            "{from}"
        );
    }
    assert!(dev.flows().is_empty());
}

#[test]
fn the_gap_guard_counts_refusals_and_drops_never_an_ok() {
    let mut dev = Dev::new();
    assert_eq!(dev.send(&build_request(1, &req()), 0).gap(), None);
    for (r, code) in [
        (
            FlowRequest {
                sample_rate: 1,
                ..req()
            },
            status::SAMPLE_RATE_MISMATCH,
        ),
        (FlowRequest { fpp: 0, ..req() }, status::BAD_REQUEST),
    ] {
        let outcome = dev.send(&build_request(2, &r), 0);
        assert_eq!(
            outcome.gap(),
            Some((op::REQUEST, Gap::Refused)),
            "{code:#x}"
        );
    }
    assert_eq!(
        dev.send(&build_stop(3, &[9; 6]), 0).gap(),
        Some((op::STOP, Gap::Refused))
    );
    assert_eq!(
        FlowOutcome::Limited { op: 0x0102 }.gap(),
        Some((0x0102, Gap::Limited))
    );
    // A reply that does not parse counts as nothing.
    assert_eq!(FlowOutcome::Reply(vec![1, 2]).gap(), None);
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn a_peer_gets_ten_requests_read_in_any_second() {
    let t0 = Instant::now();
    let mut limit = PeerLimit::new();
    let allowed = |limit: &mut PeerLimit, peer: Ipv4Addr, from: u64, to: u64| -> Vec<u64> {
        (from..to)
            .filter(|&i| limit.allow(t0 + ms(i), peer))
            .collect()
    };
    // A thousand requests in a second: the first ten; the next second ten
    // more from its first moment.
    assert_eq!(
        allowed(&mut limit, PEER_IP, 0, 1_000),
        (0..10).collect::<Vec<_>>()
    );
    assert_eq!(
        allowed(&mut limit, PEER_IP, 1_000, 2_000),
        (1_000..1_010).collect::<Vec<_>>()
    );
    // Another peer has its own window.
    let other = Ipv4Addr::new(10, 0, 0, 12);
    assert_eq!(
        allowed(&mut limit, other, 1_500, 1_600),
        (1_500..1_510).collect::<Vec<_>>()
    );
    // A request every 50 ms: never more than ten in a second.
    let mut limit = PeerLimit::new();
    let times: Vec<u64> = (0..200)
        .map(|k| k * 50)
        .filter(|&t| limit.allow(t0 + ms(t), PEER_IP))
        .collect();
    for &start in &times {
        let n = times
            .iter()
            .filter(|&&t| t >= start && t < start + 1_000)
            .count();
        assert!(n <= PER_SPAN, "{n} from {start}");
    }
    assert!(times.len() >= 95, "{}", times.len());
}

#[test]
fn peers_past_64_share_one_window_until_a_peer_goes_quiet() {
    let t0 = Instant::now();
    let mut limit = PeerLimit::new();
    let peer = |n: u32| Ipv4Addr::from_bits(0x0a01_0000 + n);
    for n in 0..MAX_PEERS as u32 {
        assert!(limit.allow(t0, peer(n)));
    }
    // Two more peers, each asking 10 times at once: 10 between them.
    let shared: usize = [MAX_PEERS as u32, MAX_PEERS as u32 + 1]
        .into_iter()
        .map(|n| (0..10).filter(|_| limit.allow(t0 + ms(1), peer(n))).count())
        .sum();
    assert_eq!(shared, PER_SPAN);
    // A second later the first 64 are quiet: a new peer takes a window of
    // its own (ten), and an old one comes back to a fresh window.
    let later = t0 + ms(1_000);
    let n = (0..20).filter(|_| limit.allow(later, peer(500))).count();
    assert_eq!(n, PER_SPAN);
    let n = (0..20).filter(|_| limit.allow(later, peer(501))).count();
    assert_eq!(n, PER_SPAN, "another quiet window given up");
    // Within the second, a busy peer keeps its window.
    assert!(!limit.allow(later + ms(999), peer(500)));
}

#[test]
fn answer_drops_a_request_over_the_bound_unread() {
    let mut dev = Dev::new();
    let mut limit = PeerLimit::new();
    let t0 = Instant::now();
    for i in 0..12u16 {
        let r = FlowRequest {
            rx: SocketAddrV4::new(PEER_IP, 8000 + i),
            ..req()
        };
        let outcome = answer(
            &mut limit,
            &mut dev.table,
            &dev.cfg,
            FROM,
            &build_request(i, &r),
            t0 + ms(u64::from(i)),
            0,
        );
        if usize::from(i) < PER_SPAN {
            assert_eq!(decode(outcome).header.op2, status::OK);
        } else {
            assert_eq!(outcome, FlowOutcome::Limited { op: op::REQUEST });
        }
    }
    assert_eq!(dev.flows().len(), PER_SPAN);
    // A datagram too short to name its opcode.
    assert_eq!(
        answer(&mut limit, &mut dev.table, &dev.cfg, FROM, &[1], t0, 0),
        FlowOutcome::Limited { op: 0 }
    );
}

#[test]
fn random_datagrams_never_panic_and_every_answer_is_one_of_inferno_s() {
    let mut rng = Rng::new(0x4455_5eed);
    let mut dev = Dev::new();
    let mut handles: Vec<Handle> = vec![[0; 6]];
    let mut answered = 0;
    for i in 0..10_000u32 {
        let pick = |rng: &mut Rng, xs: &[u32]| xs[rng.below(xs.len())];
        let mut bytes = match i % 4 {
            0 => {
                let len = rng.below(601);
                rng.bytes(len)
            }
            1 | 2 => {
                let r = FlowRequest {
                    rx_host: "peer-device".to_string(),
                    rx_flow_name: "flow".to_string(),
                    sample_rate: pick(&mut rng, &[96_000, 96_000, 48_000]),
                    bits: pick(&mut rng, &[16, 24, 32, 8, 40]),
                    channels: (0..rng.below(11))
                        .map(|_| match rng.below(20) as u16 {
                            0 => None,
                            id => Some(id),
                        })
                        .collect(),
                    fpp: pick(&mut rng, &[0, 1, 16, 32, 256, 257]) as u16,
                    rx: SocketAddrV4::new(
                        if rng.below(8) == 0 {
                            Ipv4Addr::new(10, 0, 0, 12)
                        } else {
                            PEER_IP
                        },
                        rng.below(64) as u16,
                    ),
                };
                build_request(i as u16, &r)
            }
            _ => {
                let h = handles[rng.below(handles.len())];
                if rng.below(2) == 0 {
                    build_stop(i as u16, &h)
                } else {
                    let channels: Vec<Option<u16>> = (0..rng.below(11))
                        .map(|_| Some(rng.below(18) as u16))
                        .collect();
                    build_update(i as u16, &h, &channels)
                }
            }
        };
        if i % 4 != 0 && rng.below(4) == 0 {
            let at = rng.below(bytes.len());
            bytes[at] = rng.next() as u8;
            bytes.truncate(rng.below(bytes.len() + 1));
        }
        let outcome = dev.send(&bytes, u64::from(i) * 1_000_000);
        if let FlowOutcome::Reply(reply) = &outcome {
            answered += 1;
            let (h, body) = Header::parse(reply).expect("a reply parses");
            let (req_h, _) = Header::parse(&bytes).expect("only a request is answered");
            assert_eq!((h.start, h.seq, h.op1), (req_h.start, req_h.seq, req_h.op1));
            assert!(
                [
                    status::OK,
                    status::FLOW_NOT_FOUND,
                    status::SAMPLE_RATE_MISMATCH,
                    status::BAD_REQUEST,
                    status::TOO_MANY_TX_FLOWS
                ]
                .contains(&h.op2),
                "{:#x}",
                h.op2
            );
            if h.op2 == status::OK && h.op1 == op::REQUEST {
                handles.push(body.try_into().expect("a handle"));
            } else {
                assert!(body.is_empty());
            }
        }
        assert!(dev.table.flows().count() <= MAX_FLOWS);
        for (_, f) in dev.table.flows() {
            assert_eq!(*f.dest.ip(), PEER_IP, "audio only to the requester");
            assert!(f.channels.len() <= 8);
            assert!(f.fpp <= dev.cfg.tx_fpp_max, "{}", f.fpp);
            assert!(
                f.channels
                    .iter()
                    .flatten()
                    .all(|&id| (1..=16).contains(&id))
            );
        }
        if i % 500 == 0 {
            dev.table.expire(u64::from(i) * 1_000_000);
        }
    }
    assert!(answered > 3_000, "{answered}");
    assert!(handles.len() > 32, "{}", handles.len());
}

#[tokio::test]
async fn the_server_answers_the_requester_and_hands_the_flow_to_the_tx_thread() {
    let rings: Arc<[Ring]> = Arc::from(Vec::new());
    let shared = Arc::new(TxShared::new(cfg(), Ipv4Addr::LOCALHOST, rings, 7, &[]).unwrap());
    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server = sock.local_addr().unwrap();
    let requests = Arc::new(Requests::default());
    let task = tokio::spawn(serve(sock, Arc::clone(&shared), Arc::clone(&requests)));
    let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let SocketAddr::V4(rx) = client.local_addr().unwrap() else {
        panic!("an IPv4 socket");
    };
    // A reply is never answered: no datagram comes back for it.
    client
        .send_to(&datagram(control::START, 9, op::REQUEST, 1, &[]), server)
        .await
        .unwrap();
    client
        .send_to(&build_request(5, &FlowRequest { rx, ..req() }), server)
        .await
        .unwrap();
    let mut buf = [0u8; 2048];
    let (n, from) = tokio::time::timeout(Duration::from_secs(2), client.recv_from(&mut buf))
        .await
        .expect("an answer within 2 s")
        .unwrap();
    assert_eq!(from, server);
    let (h, body) = Header::parse(&buf[..n]).unwrap();
    assert_eq!(
        (h.seq, h.op1, h.op2, body.len()),
        (5, op::REQUEST, status::OK, 6)
    );
    // Both datagrams counted for the status, the unanswered reply too.
    let seen: Vec<(u16, usize, u16, u64)> = requests
        .counts()
        .received
        .iter()
        .map(|r| (r.port, r.peer, r.op, r.n))
        .collect();
    assert_eq!(seen, [(control::PORT, 1, op::REQUEST, 2)]);
    // The plan reached the TX side: its packets come to the requester.
    // The first pass reads the plan and starts the schedule; 0.7 ms later
    // (67 samples) the first packet is due, within the 1 ms latency.
    let mut tx = Transmitter::new(Arc::clone(&shared));
    tx.step(Some(T), 0);
    tx.step(Some(T + 700_000), 0);
    let (n, from) = tokio::time::timeout(Duration::from_secs(2), client.recv_from(&mut buf))
        .await
        .expect("audio within 2 s")
        .unwrap();
    assert_eq!(from, shared.local_addr(0).unwrap());
    assert_eq!(n, 9 + 2 * 3 * 32);
    task.abort();
}
