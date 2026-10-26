use super::*;

use std::cell::RefCell;
use std::collections::HashMap;

use crate::flows::control::handle;

const TEST_DEVICE: &str = include_str!("../../../../config/test-dante-device.toml");
const OUR_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 10);
const PEER_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 11);
const OTHER_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 12);
const S: u64 = 1_000_000_000;
const MS: u64 = 1_000_000;

fn cfg() -> DeviceConfig {
    DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-dante-device.toml")
}

fn ports() -> [u16; MAX_FLOWS] {
    std::array::from_fn(|slot| 50_000 + slot as u16)
}

fn manager() -> Manager {
    Manager::new(&cfg(), "test-device", OUR_IP, ports())
}

fn sub(channel: &str, device: &str) -> Subscription {
    Subscription {
        tx_channel: channel.to_string(),
        tx_device: device.to_string(),
    }
}

/// A transmitter's channel record: 4 slots a flow, 24 bits at 96 kHz,
/// 4–16 frames a packet, a 2 ms latency.
fn adv(ip: Ipv4Addr, id: u16) -> Advertised {
    Advertised {
        control: SocketAddrV4::new(ip, 4455),
        tx_channel_id: id,
        nchan: 4,
        bits: 24,
        rate: 96_000,
        dbcp1: 0x1102,
        fpp_min: 4,
        fpp_max: 16,
        latency_ns: 2_000_000,
    }
}

/// The records a test's network holds, changed as the test goes.
#[derive(Default)]
struct Fake(RefCell<HashMap<(String, String), Advertised>>);

impl Fake {
    fn add(&self, device: &str, channel: &str, a: Advertised) {
        self.0
            .borrow_mut()
            .insert((device.to_string(), channel.to_string()), a);
    }

    fn remove(&self, device: &str, channel: &str) {
        self.0
            .borrow_mut()
            .remove(&(device.to_string(), channel.to_string()));
    }
}

impl Resolver for Fake {
    fn lookup(&self, tx_device: &str, tx_channel: &str) -> Option<Advertised> {
        self.0
            .borrow()
            .get(&(tx_device.to_string(), tx_channel.to_string()))
            .copied()
    }
}

/// The peer's TX 1…8 as ids 1…8.
fn peer_network() -> Fake {
    let f = Fake::default();
    for id in 1..=8 {
        f.add("peer-device", &format!("TX {id}"), adv(PEER_IP, id));
    }
    f
}

fn requests(actions: &[Action]) -> Vec<(FlowId, SocketAddrV4, FlowRequest)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Request { flow, to, req } => Some((*flow, *to, req.clone())),
            Action::Stop { .. } => None,
        })
        .collect()
}

fn stops(actions: &[Action]) -> Vec<(SocketAddrV4, Handle)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Stop { to, handle } => Some((*to, *handle)),
            Action::Request { .. } => None,
        })
        .collect()
}

const NONE: [u64; MAX_FLOWS] = [0; MAX_FLOWS];

#[test]
fn a_subscription_asks_its_transmitter_for_one_flow_of_nchan_slots() {
    let net = peer_network();
    let mut m = manager();
    assert!(m.set(0, Some(sub("TX 3", "peer-device"))));
    assert!(!m.set(32, Some(sub("TX 3", "peer-device"))));
    assert!(m.subscribed());
    let out = m.tick(0, &NONE, &net);
    let r = requests(&out);
    assert_eq!(r.len(), 1, "{out:?}");
    let (flow, to, req) = &r[0];
    assert_eq!(
        *flow,
        FlowId {
            slot: 0,
            generation: 1
        }
    );
    assert_eq!(*to, SocketAddrV4::new(PEER_IP, 4455));
    assert_eq!(
        *req,
        FlowRequest {
            rx_host: "test-device".to_string(),
            rx_flow_name: "1_0".to_string(),
            sample_rate: 96_000,
            bits: 24,
            channels: vec![Some(3), None, None, None],
            fpp: 16,
            rx: SocketAddrV4::new(OUR_IP, 50_000),
        }
    );
    // The RX thread's view: the transmitter, 3 bytes, 4 slots, the larger
    // latency (2 ms = 192 samples), not granted yet.
    assert_eq!(
        m.rx_plan(0),
        RxPlan {
            src: Some(PEER_IP),
            bps: 3,
            nslots: 4,
            latency: 192,
            live: false,
            generation: 1,
        }
    );
    assert_eq!(m.rx_route(0), Some(RxRoute { flow: 0, slot: 0 }));
    assert_eq!(m.rx_route(1), None);
    assert_eq!(m.rx_plan(1), RxPlan::default());
    assert_eq!(m.status(0), Some(SubStatus::InProgress));
    assert_eq!(m.status(1), None);
    // Asked once: nothing more while the request is in flight.
    for t in 1..30 {
        assert_eq!(m.tick(t * 100 * MS, &NONE, &net), []);
    }
    let h = handle(0, 7);
    assert_eq!(m.answered(*flow, Answer::Granted(h), 3 * S), []);
    assert!(m.rx_plan(0).live);
    let v = m.flow(0).unwrap();
    assert_eq!(
        v,
        FlowView {
            id: *flow,
            port: 50_000,
            to: *to,
            handle: Some(h),
            channels: vec![Some(3), None, None, None],
            rx: vec![vec![0], vec![], vec![], vec![]],
            bits: 24,
            fpp: 16,
            latency_ns: 2_000_000,
        }
    );
    assert_eq!(m.flow(1), None);
}

#[test]
fn channels_group_by_transmitter_and_share_a_slot_per_tx_channel() {
    let net = peer_network();
    net.add("other-device", "TX 1", adv(OTHER_IP, 1));
    let mut m = manager();
    // RX 1 and 2 both want TX 3; then TX 1, 2, 4, 5 of the peer and one of
    // another transmitter.
    for (rx, ch) in [
        (0, "TX 3"),
        (1, "TX 3"),
        (2, "TX 1"),
        (3, "TX 2"),
        (4, "TX 4"),
        (5, "TX 5"),
    ] {
        m.set(rx, Some(sub(ch, "peer-device")));
    }
    m.set(6, Some(sub("TX 1", "other-device")));
    let out = m.tick(0, &NONE, &net);
    let r = requests(&out);
    assert_eq!(r.len(), 2, "one request per transmitter: {out:?}");
    assert_eq!(r[0].2.channels, [Some(3), Some(1), Some(2), Some(4)]);
    assert_eq!(r[0].1.ip(), &PEER_IP);
    assert_eq!(r[1].2.channels, [Some(1), None, None, None]);
    assert_eq!(r[1].1.ip(), &OTHER_IP);
    // The two flows take two slots, so two ports.
    assert_eq!(r[1].0.slot, 1);
    assert_eq!(r[1].2.rx.port(), 50_001);
    assert_eq!(r[1].2.rx_flow_name, "2_0");
    for (rx, slot) in [(0, 0), (1, 0), (2, 1), (3, 2), (4, 3)] {
        assert_eq!(m.rx_route(rx), Some(RxRoute { flow: 0, slot }), "RX {rx}");
    }
    // TX 5 does not fit the first flow: it waits for the peer's next
    // request, 9 s after the first.
    assert_eq!(m.rx_route(5), None);
    assert_eq!(m.status(5), Some(SubStatus::Unresolved));
    m.answered(r[0].0, Answer::Granted(handle(0, 1)), 10 * MS);
    m.answered(r[1].0, Answer::Granted(handle(0, 2)), 10 * MS);
    let mut heard = NONE;
    let mut t = 0;
    while t < 9 * S {
        t += 100 * MS;
        heard[0] += 1;
        heard[1] += 1;
        let out = m.tick(t, &heard, &net);
        if t < 9 * S {
            assert_eq!(out, [], "at {t}");
        } else {
            let r = requests(&out);
            assert_eq!(r.len(), 1, "{out:?}");
            assert_eq!(r[0].2.channels, [Some(5), None, None, None]);
            assert_eq!(r[0].0.slot, 2);
        }
    }
    // A later channel whose TX channel a flow carries already joins it
    // without a request.
    m.set(7, Some(sub("TX 4", "peer-device")));
    assert_eq!(requests(&m.tick(t + 100 * MS, &heard, &net)), []);
    assert_eq!(m.rx_route(7), Some(RxRoute { flow: 0, slot: 3 }));
    assert_eq!(
        m.flow(0).unwrap().rx,
        [vec![0, 1], vec![2], vec![3], vec![4, 7]]
    );
}

#[test]
fn a_flow_has_at_most_8_slots_and_never_more_than_our_rx_channels() {
    let net = Fake::default();
    let mut wide = adv(PEER_IP, 1);
    wide.nchan = 64;
    for id in 1..=10 {
        net.add(
            "peer-device",
            &format!("TX {id}"),
            Advertised {
                tx_channel_id: id,
                ..wide
            },
        );
    }
    let mut m = manager();
    for rx in 0..10 {
        m.set(rx, Some(sub(&format!("TX {}", rx + 1), "peer-device")));
    }
    let r = requests(&m.tick(0, &NONE, &net));
    assert_eq!(r[0].2.channels, (1..=8u16).map(Some).collect::<Vec<_>>());
    // A device with two RX channels asks for flows of two slots.
    let mut small = cfg();
    small.rx.truncate(2);
    let mut m = Manager::new(&small, "test-device", OUR_IP, ports());
    m.set(0, Some(sub("TX 1", "peer-device")));
    let r = requests(&m.tick(0, &NONE, &net));
    assert_eq!(r[0].2.channels, [Some(1), None]);
}

/// Runs `m` from `from` to `to` in 100 ms ticks, answering each request
/// with `answer` (or never), and returns every action with its time.
fn run(
    m: &mut Manager,
    net: &Fake,
    from: u64,
    to: u64,
    answer: Option<Answer>,
) -> Vec<(u64, Action)> {
    let mut all = Vec::new();
    let mut t = from;
    while t <= to {
        for a in m.tick(t, &NONE, net) {
            if let (Action::Request { flow, .. }, Some(ans)) = (&a, answer) {
                for b in m.answered(*flow, ans, t) {
                    all.push((t, b));
                }
            }
            all.push((t, a));
        }
        t += 100 * MS;
    }
    all
}

fn request_times(all: &[(u64, Action)]) -> Vec<u64> {
    all.iter()
        .filter(|(_, a)| matches!(a, Action::Request { .. }))
        .map(|(t, _)| t / S)
        .collect()
}

#[test]
fn a_refusing_transmitter_gets_one_request_per_9_s() {
    let net = peer_network();
    let mut m = manager();
    m.set(0, Some(sub("TX 3", "peer-device")));
    // Ten simulated minutes, every request refused (another sample rate).
    let all = run(
        &mut m,
        &net,
        0,
        600 * S,
        Some(Answer::Refused(status::SAMPLE_RATE_MISMATCH)),
    );
    let times = request_times(&all);
    // 9 s after one failure, then doubling to 72 s.
    assert_eq!(
        times,
        [0, 9, 27, 63, 135, 207, 279, 351, 423, 495, 567],
        "{all:?}"
    );
    assert!(times.windows(2).all(|w| w[1] - w[0] >= 9));
    assert!(times.len() as u64 <= 600 / 9 + 1);
    assert_eq!(
        stops(&all.iter().map(|(_, a)| a.clone()).collect::<Vec<_>>()),
        []
    );
    assert_eq!(m.status(0), Some(SubStatus::TxFail));
    // Too many TX flows reads as such.
    let mut m = manager();
    m.set(0, Some(sub("TX 3", "peer-device")));
    run(
        &mut m,
        &net,
        0,
        0,
        Some(Answer::Refused(status::TOO_MANY_TX_FLOWS)),
    );
    assert_eq!(m.status(0), Some(SubStatus::TooManyTxFlows));
    assert_eq!(m.rx_plan(0), RxPlan::default());
    assert_eq!(m.rx_route(0), None);
}

#[test]
fn the_wait_doubles_from_9_to_72_s() {
    assert_eq!(
        (0..7).map(retry_after).collect::<Vec<_>>(),
        [9, 9, 18, 36, 72, 72, 72].map(|s| s * S)
    );
    assert_eq!(retry_after(u32::MAX), 72 * S);
}

#[test]
fn a_silent_transmitter_never_has_two_requests_in_flight() {
    let net = peer_network();
    let mut m = manager();
    m.set(0, Some(sub("TX 3", "peer-device")));
    m.set(1, Some(sub("TX 4", "peer-device")));
    let mut asked = Vec::new();
    let mut t = 0;
    while t <= 600 * S {
        for a in m.tick(t, &NONE, &net) {
            if let Action::Request { flow, .. } = a {
                asked.push((t, flow));
            }
        }
        let in_flight = (0..MAX_FLOWS)
            .filter_map(|s| m.flow(s))
            .filter(|f| f.handle.is_none())
            .count();
        assert!(in_flight <= 1, "at {t}");
        if t < 4 * S {
            assert_eq!(m.status(0), Some(SubStatus::InProgress));
        } else if t < 9 * S {
            // Given up after 4 s.
            assert_eq!(m.status(0), Some(SubStatus::TxFail), "at {t}");
        }
        t += 100 * MS;
    }
    let times: Vec<u64> = asked.iter().map(|(t, _)| t / S).collect();
    assert_eq!(times, [0, 9, 27, 63, 135, 207, 279, 351, 423, 495, 567]);
    // The first request's grant after all: released at once.
    let first = asked[0].1;
    assert_eq!(
        m.answered(first, Answer::Granted(handle(0, 9)), t),
        [Action::Stop {
            to: SocketAddrV4::new(PEER_IP, 4455),
            handle: handle(0, 9),
        }]
    );
    // Once only; and a refusal of it changes nothing.
    assert_eq!(m.answered(first, Answer::Granted(handle(0, 9)), t), []);
    assert_eq!(m.answered(asked[1].1, Answer::Refused(1), t), []);
}

#[test]
fn a_flow_without_packets_is_stopped_and_resolved_again() {
    let net = peer_network();
    let mut m = manager();
    m.set(0, Some(sub("TX 3", "peer-device")));
    let r = requests(&m.tick(0, &NONE, &net));
    let h = handle(0, 5);
    m.answered(r[0].0, Answer::Granted(h), 10 * MS);
    // Nothing comes: stopped 6 s after the request, never before.
    for t in 1..60 {
        assert_eq!(m.tick(t * 100 * MS, &NONE, &net), [], "at {t}");
    }
    let out = m.tick(6 * S, &NONE, &net);
    assert_eq!(
        out,
        [Action::Stop {
            to: SocketAddrV4::new(PEER_IP, 4455),
            handle: h,
        }]
    );
    assert_eq!(m.status(0), Some(SubStatus::Unresolved));
    assert_eq!(m.rx_plan(0), RxPlan::default());
    assert_eq!(m.rx_route(0), None);
    // Resolved again, asked again only after the wait (9 s from the last
    // request), in another flow slot.
    for t in 61..90 {
        assert_eq!(m.tick(t * 100 * MS, &NONE, &net), [], "at {t}");
    }
    let r = requests(&m.tick(9 * S, &NONE, &net));
    assert_eq!(r.len(), 1);
    assert_eq!(
        r[0].0,
        FlowId {
            slot: 1,
            generation: 2
        }
    );
    // This time packets come, then stop: the flow is stopped 2 s after the
    // last one (its transmitter's failures ended with the audio).
    let h = handle(1, 6);
    m.answered(r[0].0, Answer::Granted(h), 9 * S + 10 * MS);
    let mut heard = NONE;
    let mut t = 9 * S;
    while t < 20 * S {
        t += 100 * MS;
        heard[1] += 1;
        assert_eq!(m.tick(t, &heard, &net), []);
        assert_eq!(m.status(0), Some(SubStatus::Receiving));
    }
    let last = t;
    while t < last + 2 * S - 100 * MS {
        t += 100 * MS;
        assert_eq!(m.tick(t, &heard, &net), [], "at {t}");
        assert_eq!(m.status(0), Some(SubStatus::Receiving));
    }
    t += 100 * MS;
    let out = m.tick(t, &heard, &net);
    assert_eq!(stops(&out), [(SocketAddrV4::new(PEER_IP, 4455), h)]);
    // Resolved again at once, and asked at once: the last request was 13 s
    // ago, past the wait after one failure (9 s).
    let r = requests(&out);
    assert_eq!(r.len(), 1, "{out:?}");
    assert_eq!(r[0].0.slot, 2);
    assert_eq!(m.status(0), Some(SubStatus::InProgress));
}

#[test]
fn removing_the_last_channel_stops_its_flow() {
    let net = peer_network();
    let mut m = manager();
    m.set(0, Some(sub("TX 3", "peer-device")));
    m.set(1, Some(sub("TX 1", "peer-device")));
    let r = requests(&m.tick(0, &NONE, &net));
    let h = handle(0, 3);
    m.answered(r[0].0, Answer::Granted(h), MS);
    let mut heard = [1; MAX_FLOWS];
    assert_eq!(m.tick(100 * MS, &heard, &net), []);
    // One channel goes: the flow stays for the other, its slot unrouted.
    m.set(0, None);
    heard[0] += 1;
    assert_eq!(m.tick(200 * MS, &heard, &net), []);
    assert_eq!(m.rx_route(0), None);
    assert_eq!(m.status(0), None);
    assert_eq!(m.rx_route(1), Some(RxRoute { flow: 0, slot: 1 }));
    // The last goes: stopped, once.
    m.set(1, None);
    heard[0] += 1;
    assert_eq!(
        m.tick(300 * MS, &heard, &net),
        [Action::Stop {
            to: SocketAddrV4::new(PEER_IP, 4455),
            handle: h,
        }]
    );
    assert_eq!(m.tick(400 * MS, &heard, &net), []);
    assert_eq!(m.rx_plan(0), RxPlan::default());
    assert!(!m.subscribed());
}

#[test]
fn a_changed_subscription_leaves_its_flow_for_a_new_one() {
    let net = peer_network();
    let mut m = manager();
    m.set(0, Some(sub("TX 3", "peer-device")));
    let r = requests(&m.tick(0, &NONE, &net));
    let h = handle(0, 3);
    m.answered(r[0].0, Answer::Granted(h), MS);
    let mut heard = NONE;
    heard[0] = 1;
    m.tick(S, &heard, &net);
    assert_eq!(m.status(0), Some(SubStatus::Receiving));
    // The same channel set to another TX channel of the same transmitter.
    m.set(0, Some(sub("TX 1", "peer-device")));
    heard[0] += 1;
    let out = m.tick(2 * S, &heard, &net);
    assert_eq!(stops(&out), [(SocketAddrV4::new(PEER_IP, 4455), h)]);
    assert_eq!(requests(&out), []);
    assert_eq!(m.status(0), Some(SubStatus::Unresolved));
    let all = run(&mut m, &net, 2 * S + 100 * MS, 12 * S, None);
    assert_eq!(request_times(&all), [9]);
    let Action::Request { req, .. } = &all[0].1 else {
        panic!("{all:?}");
    };
    assert_eq!(req.channels, [Some(1), None, None, None]);
}

#[test]
fn statuses_move_unresolved_in_progress_receiving() {
    let net = Fake::default();
    let mut m = manager();
    m.set(4, Some(sub("TX 3", "peer-device")));
    assert_eq!(m.changed(), [4]);
    assert_eq!(m.status(4), Some(SubStatus::Unresolved));
    assert_eq!(SubStatus::Unresolved.code(), 1);
    // No record: nothing asked, still unresolved.
    assert_eq!(m.tick(0, &NONE, &net), []);
    assert_eq!(m.changed(), Vec::<usize>::new());
    net.add("peer-device", "TX 3", adv(PEER_IP, 3));
    let r = requests(&m.tick(100 * MS, &NONE, &net));
    assert_eq!(m.status(4), Some(SubStatus::InProgress));
    assert_eq!(SubStatus::InProgress.code(), 8);
    assert_eq!(m.changed(), [4]);
    m.answered(r[0].0, Answer::Granted(handle(0, 1)), 200 * MS);
    assert_eq!(m.status(4), Some(SubStatus::InProgress));
    let mut heard = NONE;
    heard[0] = 3;
    m.tick(300 * MS, &heard, &net);
    assert_eq!(m.status(4), Some(SubStatus::Receiving));
    assert_eq!(SubStatus::Receiving.code(), 0x0101_0009);
    assert_eq!(m.changed(), [4]);
    assert_eq!(m.changed(), Vec::<usize>::new());
    // Quiet: in progress again; then stopped and unresolved.
    m.tick(2 * S + 300 * MS, &heard, &net);
    assert_eq!(m.status(4), Some(SubStatus::InProgress));
    m.tick(6 * S + 100 * MS, &heard, &net);
    assert_eq!(m.status(4), Some(SubStatus::Unresolved));
    // Unsubscribed: no status.
    m.set(4, None);
    assert_eq!(m.status(4), None);
    assert_eq!(m.changed(), [4]);
    assert_eq!(SubStatus::TooManyTxFlows.code(), 0x14);
    assert_eq!(SubStatus::TxFail.code(), 0x15);
}

#[test]
fn a_changed_subscription_reads_unresolved_before_the_next_tick() {
    // ARC 0x3010 sets a subscription and Dante Controller reads 0x3000
    // right after (plan Task 6): until the next tick the channel's route
    // still points at the flow of its old subscription, whose status is
    // not the new one's.
    let net = peer_network();
    let mut m = manager();
    m.set(4, Some(sub("TX 3", "peer-device")));
    let r = requests(&m.tick(0, &NONE, &net));
    m.answered(r[0].0, Answer::Granted(handle(0, 1)), 100 * MS);
    let mut heard = NONE;
    heard[0] = 3;
    m.tick(200 * MS, &heard, &net);
    assert_eq!(m.status(4), Some(SubStatus::Receiving));
    assert_eq!(m.changed(), [4]);
    // Another TX channel of the same transmitter: not received yet.
    m.set(4, Some(sub("TX 5", "peer-device")));
    assert_eq!(m.status(4), Some(SubStatus::Unresolved));
    assert_eq!(m.changed(), [4]);
    // The same subscription again changes nothing.
    m.set(4, Some(sub("TX 5", "peer-device")));
    assert_eq!(m.changed(), Vec::<usize>::new());
}

#[test]
fn a_transmitter_we_cannot_receive_is_never_asked() {
    let net = Fake::default();
    // Another sample rate.
    net.add(
        "peer-device",
        "TX 1",
        Advertised {
            rate: 48_000,
            ..adv(PEER_IP, 1)
        },
    );
    // Records of one transmitter that disagree on the width.
    net.add("other-device", "TX 1", adv(OTHER_IP, 1));
    net.add(
        "other-device",
        "TX 2",
        Advertised {
            bits: 32,
            ..adv(OTHER_IP, 2)
        },
    );
    // No frame count fits one datagram above its least.
    net.add(
        "third-device",
        "TX 1",
        Advertised {
            fpp_min: 200,
            fpp_max: 256,
            bits: 32,
            nchan: 8,
            ..adv(Ipv4Addr::new(10, 0, 0, 13), 1)
        },
    );
    let mut m = manager();
    m.set(0, Some(sub("TX 1", "peer-device")));
    m.set(1, Some(sub("TX 1", "other-device")));
    m.set(2, Some(sub("TX 2", "other-device")));
    m.set(3, Some(sub("TX 1", "third-device")));
    assert_eq!(run(&mut m, &net, 0, 60 * S, None), []);
    assert_eq!(m.status(0), Some(SubStatus::TxFail));
    assert_eq!(m.status(1), Some(SubStatus::Unresolved));
    assert_eq!(m.status(2), Some(SubStatus::Unresolved));
    assert_eq!(m.status(3), Some(SubStatus::TxFail));
    // A record that goes away leaves the channel unresolved, its status
    // kept until it is asked again.
    net.remove("other-device", "TX 2");
    let r = requests(&m.tick(61 * S, &NONE, &net));
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].1.ip(), &OTHER_IP);
}

#[test]
fn fpp_is_the_transmitter_s_most_within_one_datagram() {
    let a = adv(PEER_IP, 1);
    assert_eq!(fpp_for(&a, 4), Some(16));
    // 8 slots of 4 bytes: (1472 − 9) / 32 = 45 frames.
    let wide = Advertised {
        fpp_max: 256,
        bits: 32,
        ..a
    };
    assert_eq!(fpp_for(&wide, 8), Some(45));
    // One slot of 2 bytes: our FPP_MAX (256) bounds it, not the datagram.
    let narrow = Advertised {
        fpp_max: 1000,
        bits: 16,
        ..a
    };
    assert_eq!(fpp_for(&narrow, 1), Some(256));
    // The least is above what fits; no slot; no width.
    assert_eq!(
        fpp_for(
            &Advertised {
                fpp_min: 46,
                ..wide
            },
            8
        ),
        None
    );
    assert_eq!(
        fpp_for(
            &Advertised {
                fpp_min: 45,
                ..wide
            },
            8
        ),
        Some(45)
    );
    assert_eq!(fpp_for(&a, 0), None);
    assert_eq!(fpp_for(&Advertised { bits: 0, ..a }, 4), None);
    assert_eq!(
        fpp_for(
            &Advertised {
                fpp_max: 0,
                fpp_min: 0,
                ..a
            },
            4
        ),
        None
    );
}

#[test]
fn the_latency_is_the_larger_of_the_transmitter_s_and_ours() {
    let net = Fake::default();
    net.add(
        "peer-device",
        "TX 1",
        Advertised {
            latency_ns: 500_000,
            ..adv(PEER_IP, 1)
        },
    );
    let mut m = manager();
    m.set(0, Some(sub("TX 1", "peer-device")));
    m.tick(0, &NONE, &net);
    // Ours: 1 ms, 96 samples.
    assert_eq!(m.rx_plan(0).latency, 96);
    assert_eq!(m.flow(0).unwrap().latency_ns, 1_000_000);
}

#[test]
fn stop_all_stops_every_granted_flow_once() {
    let net = peer_network();
    net.add("other-device", "TX 1", adv(OTHER_IP, 1));
    let mut m = manager();
    m.set(0, Some(sub("TX 1", "peer-device")));
    m.set(1, Some(sub("TX 1", "other-device")));
    m.set(2, Some(sub("TX 9", "peer-device")));
    let r = requests(&m.tick(0, &NONE, &net));
    assert_eq!(r.len(), 2);
    // One granted, one still asked for.
    m.answered(r[0].0, Answer::Granted(handle(0, 4)), MS);
    assert_eq!(
        m.stop_all(),
        [Action::Stop {
            to: SocketAddrV4::new(PEER_IP, 4455),
            handle: handle(0, 4),
        }]
    );
    assert_eq!(m.stop_all(), []);
    for s in 0..MAX_FLOWS {
        assert_eq!(m.rx_plan(s), RxPlan::default());
    }
    assert!((0..32).all(|rx| m.rx_route(rx).is_none()));
    // The one still asked for: a grant that comes after all is released.
    assert_eq!(
        m.answered(r[1].0, Answer::Granted(handle(0, 5)), 2 * MS),
        [Action::Stop {
            to: SocketAddrV4::new(OTHER_IP, 4455),
            handle: handle(0, 5),
        }]
    );
}

/// A flow of a transmitter no test asks, holding a slot (the slot
/// choice's tests).
fn held() -> Option<Flow> {
    Some(Flow {
        generation: 1,
        to: SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 99), 4455),
        slots: vec![Some(1)],
        bits: 24,
        fpp: 16,
        latency_ns: 1_000_000,
        since: 0,
        handle: None,
        heard: None,
        packets: 0,
    })
}

#[test]
fn a_new_flow_takes_the_slot_freed_longest_ago() {
    let mut m = manager();
    assert_eq!(m.free_slot(), Some(0));
    // Never used first, lowest first.
    m.freed[0] = Some(5);
    assert_eq!(m.free_slot(), Some(1));
    // Then the one freed longest ago.
    for s in 0..MAX_FLOWS {
        m.freed[s] = Some(100 - s as u64);
    }
    assert_eq!(m.free_slot(), Some(31));
    m.flows[31] = held();
    assert_eq!(m.free_slot(), Some(30));
    m.freed[3] = Some(1);
    assert_eq!(m.free_slot(), Some(3));
    for s in 0..MAX_FLOWS {
        m.flows[s] = held();
    }
    assert_eq!(m.free_slot(), None);
    // Every slot taken: a subscription waits, unresolved, nothing asked.
    let net = peer_network();
    m.set(0, Some(sub("TX 1", "peer-device")));
    assert_eq!(m.tick(0, &NONE, &net), []);
    assert_eq!(m.status(0), Some(SubStatus::Unresolved));
    // In use: a failed request's slot is not the next one's.
    let mut m = manager();
    m.set(0, Some(sub("TX 1", "peer-device")));
    let r = requests(&m.tick(0, &NONE, &net));
    assert_eq!(r[0].0.slot, 0);
    m.answered(r[0].0, Answer::Failed, 0);
    let r = requests(&m.tick(9 * S, &NONE, &net));
    assert_eq!(r[0].0.slot, 1, "not the slot just freed");
}

#[test]
fn an_answer_maps_the_client_s_result() {
    assert_eq!(Answer::of(&Ok(handle(1, 2))), Answer::Granted(handle(1, 2)));
    assert_eq!(
        Answer::of(&Err(ClientError::Refused(0x0315))),
        Answer::Refused(0x0315)
    );
    assert_eq!(Answer::of(&Err(ClientError::Timeout)), Answer::Failed);
    assert_eq!(Answer::of(&Err(ClientError::NoHandle)), Answer::Failed);
}

#[test]
fn a_loaded_state_subscribes_every_channel_it_lists() {
    let net = peer_network();
    let mut subs = vec![None; 32];
    subs[2] = Some(sub("TX 2", "peer-device"));
    subs[31] = Some(sub("TX 2", "peer-device"));
    let mut m = manager();
    m.load(&subs);
    assert_eq!(m.subscription(2), Some(&sub("TX 2", "peer-device")));
    assert_eq!(m.subscription(0), None);
    let r = requests(&m.tick(0, &NONE, &net));
    assert_eq!(r[0].2.channels, [Some(2), None, None, None]);
    assert_eq!(m.rx_route(31), Some(RxRoute { flow: 0, slot: 0 }));
    // Setting the same subscription again changes nothing.
    assert!(m.set(2, Some(sub("TX 2", "peer-device"))));
    assert_eq!(m.tick(100 * MS, &NONE, &net), []);
    assert_eq!(m.rx_route(2), Some(RxRoute { flow: 0, slot: 0 }));
}

#[test]
fn transmitters_with_nothing_pending_are_forgotten_after_their_wait() {
    let net = peer_network();
    let mut m = manager();
    m.set(0, Some(sub("TX 3", "peer-device")));
    let r = requests(&m.tick(0, &NONE, &net));
    m.answered(r[0].0, Answer::Granted(handle(0, 1)), MS);
    let mut heard = NONE;
    heard[0] = 1;
    m.tick(S, &heard, &net);
    m.set(0, None);
    heard[0] = 2;
    assert_eq!(stops(&m.tick(2 * S, &heard, &net)).len(), 1);
    assert!(
        m.peers.contains_key(&PEER_IP),
        "kept until its wait is over"
    );
    m.tick(9 * S - 1, &heard, &net);
    assert!(m.peers.contains_key(&PEER_IP));
    m.tick(9 * S, &heard, &net);
    assert!(!m.peers.contains_key(&PEER_IP));
    // A new subscription asks at once.
    m.set(0, Some(sub("TX 3", "peer-device")));
    assert_eq!(requests(&m.tick(9 * S + 1, &heard, &net)).len(), 1);
}

#[test]
fn a_transmitter_is_kept_while_it_has_a_flow_or_failures() {
    let net = peer_network();
    net.add("other-device", "TX 1", adv(OTHER_IP, 1));
    let mut m = manager();
    m.set(0, Some(sub("TX 3", "peer-device")));
    m.set(1, Some(sub("TX 1", "other-device")));
    let r = requests(&m.tick(0, &NONE, &net));
    assert_eq!(r.len(), 2);
    // The peer's flow runs; the other refuses, then is unsubscribed.
    m.answered(r[0].0, Answer::Granted(handle(0, 1)), MS);
    m.answered(r[1].0, Answer::Refused(0x0302), MS);
    m.set(1, None);
    let mut heard = NONE;
    for t in 1..=60u64 {
        heard[0] = t;
        assert_eq!(m.tick(t * S, &heard, &net), [], "at {t} s");
    }
    // A minute on: the peer for its flow, the other for its failure (its
    // wait grows if it is asked again).
    assert!(m.peers.contains_key(&PEER_IP));
    assert_eq!(m.peers[&OTHER_IP].failures, 1);
    // Another transmitter's flow never keeps one that has nothing.
    let mut m = manager();
    m.set(0, Some(sub("TX 3", "peer-device")));
    m.set(1, Some(sub("TX 1", "other-device")));
    let r = requests(&m.tick(0, &NONE, &net));
    m.answered(r[0].0, Answer::Granted(handle(0, 1)), MS);
    m.answered(r[1].0, Answer::Granted(handle(0, 2)), MS);
    let mut heard = [1; MAX_FLOWS];
    m.tick(S, &heard, &net);
    m.set(1, None);
    heard = [2; MAX_FLOWS];
    assert_eq!(stops(&m.tick(2 * S, &heard, &net)).len(), 1);
    heard = [3; MAX_FLOWS];
    m.tick(10 * S, &heard, &net);
    assert!(m.peers.contains_key(&PEER_IP));
    assert!(!m.peers.contains_key(&OTHER_IP));
}

#[test]
fn an_answer_for_another_flow_of_the_slot_changes_nothing() {
    let net = peer_network();
    let mut m = manager();
    m.set(0, Some(sub("TX 3", "peer-device")));
    let r = requests(&m.tick(0, &NONE, &net));
    let id = r[0].0;
    let other = FlowId {
        slot: id.slot,
        generation: id.generation + 1,
    };
    assert_eq!(m.answered(other, Answer::Granted(handle(0, 1)), MS), []);
    assert!(!m.rx_plan(0).live);
    assert_eq!(m.answered(other, Answer::Refused(0x0315), MS), []);
    assert_eq!(m.status(0), Some(SubStatus::InProgress));
    // Its own answer counts, once.
    m.answered(id, Answer::Granted(handle(0, 2)), MS);
    assert!(m.rx_plan(0).live);
    assert_eq!(m.answered(id, Answer::Granted(handle(0, 3)), MS), []);
    assert_eq!(m.flow(0).unwrap().handle, Some(handle(0, 2)));
}

#[test]
fn flows_that_stay_silent_twice_double_the_wait() {
    let net = peer_network();
    let mut m = manager();
    m.set(0, Some(sub("TX 3", "peer-device")));
    // Granted at once, silent every time: stopped 6 s after each request.
    let mut asked = Vec::new();
    let mut stopped = Vec::new();
    let mut t = 0;
    while t <= 60 * S {
        for a in m.tick(t, &NONE, &net) {
            match a {
                Action::Request { flow, .. } => {
                    asked.push(t / S);
                    m.answered(flow, Answer::Granted(handle(0, 1)), t);
                }
                Action::Stop { .. } => stopped.push(t / S),
            }
        }
        t += 100 * MS;
    }
    // 9 s after one failure, 18 s after two, 36 s after three.
    assert_eq!(asked, [0, 9, 27]);
    assert_eq!(stopped, [6, 15, 33]);
}
