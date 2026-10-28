use super::*;

use crate::clock::ManualClock;
use crate::media::packet::write;
use crate::media::tx::ns_of;
use crate::wire::tests::Rng;

const RATE: u32 = 96_000;
const MS: u64 = 1_000_000;
const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;
/// Every wait in these tests ends.
const BOUND: Duration = Duration::from_secs(5);

fn shared(rings: usize) -> Arc<RxShared> {
    let rings: Arc<[Ring]> = (0..rings).map(|_| Ring::new()).collect::<Vec<_>>().into();
    Arc::new(RxShared::new(RATE, LOOPBACK, rings).unwrap())
}

fn plan(src: Ipv4Addr, nslots: u8, live: bool, generation: u32) -> RxPlan {
    RxPlan {
        src: Some(src),
        bps: 3,
        nslots,
        latency: 192,
        live,
        generation,
    }
}

/// Publishes `plans` (slot, plan) and `routes` (RX channel, route).
fn publish(s: &RxShared, plans: &[(usize, RxPlan)], routes: &[(usize, RxRoute)]) {
    s.publish(
        |slot| {
            plans
                .iter()
                .find(|(s, _)| *s == slot)
                .map_or(RxPlan::default(), |(_, p)| *p)
        },
        |rx| routes.iter().find(|(r, _)| *r == rx).map(|(_, r)| *r),
    );
}

/// A transmitter's socket at `ip`, its reads bounded.
fn transmitter(ip: Ipv4Addr) -> UdpSocket {
    let s = UdpSocket::bind((ip, 0)).unwrap();
    s.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    s
}

fn slot_addr(s: &RxShared, slot: usize) -> SocketAddr {
    s.local_addr(slot).unwrap()
}

/// A 24-bit packet of `nslots` slots and `frames` frames whose first frame
/// is position `pos`; slot `s` of frame `f` holds `value(pos + f, s)`.
fn packet_at(pos: u64, nslots: usize, frames: usize) -> Vec<u8> {
    let mut out = vec![0u8; MAX_DATAGRAM];
    let (secs, sub) = clock::stamp(pos, RATE);
    let n = write(&mut out, secs, sub, nslots, 3, frames, |f, s| {
        value(pos + f as u64, s)
    });
    out.truncate(n);
    out
}

/// A left-justified 24-bit sample naming its position and slot.
fn value(p: u64, s: usize) -> i32 {
    let v = ((p as u32)
        .wrapping_mul(7)
        .wrapping_add(s as u32 * 1_000_003))
        & 0x00ff_ffff;
    (v << 8) as i32
}

/// Every slot's counts summed: what the thread read of `slot`.
fn read_of(c: RxCounts) -> u64 {
    c.packets + c.refused + c.stray
}

/// Passes until slot `slot` has read `n` datagrams (bounded).
fn pump(rx: &mut Receiver, slot: usize, n: u64, now: Option<u64>, mono: u64) {
    let deadline = Instant::now() + BOUND;
    loop {
        rx.wait(Duration::from_millis(20)).unwrap();
        rx.pass(now, mono);
        if read_of(rx.shared.lane(slot)) >= n {
            return;
        }
        assert!(Instant::now() < deadline, "only {:?}", rx.shared.lane(slot));
    }
}

#[test]
fn a_plan_and_a_route_round_trip_through_their_words() {
    for p in [
        plan(Ipv4Addr::new(10, 0, 0, 11), 8, true, u32::MAX),
        RxPlan {
            src: Some(Ipv4Addr::new(255, 255, 255, 255)),
            bps: 4,
            nslots: 1,
            latency: u32::MAX,
            live: false,
            generation: 0,
        },
        plan(Ipv4Addr::UNSPECIFIED, 0, false, 7),
    ] {
        assert_eq!(RxPlan::from_words(p.words()), p, "{p:?}");
    }
    assert_eq!(RxPlan::default().words(), [0, 0]);
    assert_eq!(RxPlan::from_words([u64::MAX, 0]), RxPlan::default());
    for r in [
        RxRoute { flow: 0, slot: 0 },
        RxRoute { flow: 31, slot: 7 },
        RxRoute { flow: 5, slot: 2 },
    ] {
        assert_eq!(RxRoute::from_word(RxRoute::word(Some(r))), Some(r));
    }
    assert_eq!(RxRoute::word(None), 0);
    assert_eq!(RxRoute::from_word(0), None);
    assert_eq!(RxRoute::from_word(0x7fff_ffff_ffff_ffff), None);
}

#[test]
fn publish_hands_over_what_changed_and_the_thread_reads_it() {
    let s = shared(4);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    let v0 = s.routes_version.load(Ordering::SeqCst);
    let p = plan(LOOPBACK, 2, true, 1);
    publish(
        &s,
        &[(3, p)],
        &[
            (1, RxRoute { flow: 3, slot: 1 }),
            (2, RxRoute { flow: 3, slot: 0 }),
        ],
    );
    assert_eq!(s.routes_version.load(Ordering::SeqCst), v0 + 1);
    // The same again changes nothing.
    publish(
        &s,
        &[(3, p)],
        &[
            (1, RxRoute { flow: 3, slot: 1 }),
            (2, RxRoute { flow: 3, slot: 0 }),
        ],
    );
    assert_eq!(s.routes_version.load(Ordering::SeqCst), v0 + 1);
    rx.pass(None, 0);
    assert_eq!(rx.lanes[3].plan, p);
    assert_eq!(rx.lanes[3].targets, [(1, 1), (0, 2)]);
    assert!(rx.lanes[0].targets.is_empty());
    // A route moves: one version more; the old slot loses it.
    publish(
        &s,
        &[(3, p), (4, p)],
        &[
            (1, RxRoute { flow: 4, slot: 0 }),
            (2, RxRoute { flow: 3, slot: 0 }),
        ],
    );
    assert_eq!(s.routes_version.load(Ordering::SeqCst), v0 + 2);
    rx.pass(None, 0);
    assert_eq!(rx.lanes[3].targets, [(0, 2)]);
    assert_eq!(rx.lanes[4].targets, [(0, 1)]);
    // Every route gone, every plan cleared.
    publish(&s, &[], &[]);
    rx.pass(None, 0);
    assert!(
        rx.lanes
            .iter()
            .all(|l| l.targets.is_empty() && l.plan == RxPlan::default())
    );
    // A route past our slots or channel slots is never followed.
    s.routes[0].store(
        RxRoute::word(Some(RxRoute { flow: 40, slot: 0 })),
        Ordering::SeqCst,
    );
    s.routes[1].store(
        RxRoute::word(Some(RxRoute { flow: 0, slot: 8 })),
        Ordering::SeqCst,
    );
    s.routes_version.fetch_add(1, Ordering::SeqCst);
    rx.pass(None, 0);
    assert!(rx.lanes.iter().all(|l| l.targets.is_empty()));
}

#[test]
fn frames_of_24_bit_big_endian_land_at_their_stamps_across_the_seconds_wrap() {
    // Three uptimes: ten years, just before the 32-bit seconds wrap, and
    // the wrap itself crossed inside the packets.
    let wrap = (u64::from(u32::MAX) + 1) * u64::from(RATE);
    for p0 in [10 * 365 * 86_400 * u64::from(RATE), wrap - 2_000, wrap - 64] {
        let s = shared(3);
        let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
        // Slot 0 feeds RX 1 and RX 2, slot 1 RX 3; slot 2 nobody.
        publish(
            &s,
            &[(5, plan(LOOPBACK, 3, true, 1))],
            &[
                (0, RxRoute { flow: 5, slot: 0 }),
                (1, RxRoute { flow: 5, slot: 0 }),
                (2, RxRoute { flow: 5, slot: 1 }),
            ],
        );
        let t = transmitter(LOOPBACK);
        for k in 0..3 {
            t.send_to(&packet_at(p0 + 32 * k, 3, 32), slot_addr(&s, 5))
                .unwrap();
        }
        // Our clock: 100 samples after the first packet's first sample.
        let now = ns_of(p0 + 100, RATE);
        pump(&mut rx, 5, 3, Some(now), 0);
        let c = s.lane(5);
        assert_eq!(
            (
                c.packets,
                c.refused,
                c.stray,
                c.late,
                c.out_of_order,
                c.unplaced
            ),
            (3, 0, 0, 0, 0, 0),
            "{c:?}"
        );
        let rings = s.rings();
        let mut got = [0i32; 96];
        for (ring, slot) in [(0, 0), (1, 0), (2, 1)] {
            assert_eq!(rings[ring].read_at(p0, &mut got).holes, 0);
            let want: Vec<i32> = (0..96).map(|f| value(p0 + f, slot)).collect();
            assert_eq!(got.to_vec(), want, "ring {ring} at {p0}");
        }
        // The largest lateness: the first packet's, 100 samples; then reset.
        // The status reads what the last take took, and takes nothing.
        assert_eq!(s.last_lateness(5), 0);
        assert_eq!(s.take_max_lateness(5), 100);
        assert_eq!((s.last_lateness(5), s.last_lateness(5)), (100, 100));
        assert_eq!(s.take_max_lateness(5), 0);
        assert_eq!(s.last_lateness(5), 0);
        assert_eq!(s.take_max_lateness(99), 0);
        assert_eq!(s.last_lateness(99), 0);
        // The manager's view of the packets; nothing failed; slot 5 read
        // out.
        assert_eq!(s.packets()[5], 3);
        assert_eq!(s.packets()[4], 0);
        assert_eq!(c.recv_errors, 0);
        assert_eq!(rx.ready, 0);
    }
}

#[test]
fn the_least_lateness_of_a_second_is_kept_in_ns() {
    // 1000 s of media time: whole ns at every sample position used here.
    let p0 = 1_000 * u64::from(RATE);
    let s = shared(1);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    publish(
        &s,
        &[(2, plan(LOOPBACK, 1, true, 1))],
        &[(0, RxRoute { flow: 2, slot: 0 })],
    );
    let t = transmitter(LOOPBACK);
    // Our clock 7 ns past sample p0 + 100 (ns_of rounds up: 1 041 667).
    let now = ns_of(p0 + 100, RATE) + 7;
    assert_eq!(now, 1_000_001_041_674);
    // Packets 100, 36 and 68 samples late: the least is p0 + 64's, its
    // stamp 666 667 ns into the second (rounded up), in ns, not samples.
    for pos in [p0, p0 + 64, p0 + 32] {
        t.send_to(&packet_at(pos, 1, 1), slot_addr(&s, 2)).unwrap();
    }
    pump(&mut rx, 2, 3, Some(now), 0);
    // Nothing taken yet: the status's second is none.
    assert_eq!(s.last_min_lateness_ns(2), None);
    assert_eq!(s.take_max_lateness(2), 100);
    assert_eq!(s.last_min_lateness_ns(2), Some(375_007));
    // Read twice, taken once: the status takes nothing.
    assert_eq!(s.last_min_lateness_ns(2), Some(375_007));
    // The next second has its own least, even when it is larger.
    t.send_to(&packet_at(p0 + 10, 1, 1), slot_addr(&s, 2))
        .unwrap();
    pump(&mut rx, 2, 4, Some(now), 0);
    assert_eq!(s.last_min_lateness_ns(2), Some(375_007));
    assert_eq!(s.take_max_lateness(2), 90);
    assert_eq!(s.last_min_lateness_ns(2), Some(1_041_674 - 104_167));
    // A packet stamped after our now is 0 late: a least of 0, not none.
    for pos in [p0 + 50, p0 + 200] {
        t.send_to(&packet_at(pos, 1, 1), slot_addr(&s, 2)).unwrap();
    }
    pump(&mut rx, 2, 6, Some(now), 0);
    assert_eq!(s.take_max_lateness(2), 50);
    assert_eq!(s.last_min_lateness_ns(2), Some(0));
    // Only its own slot judged.
    assert_eq!(s.take_max_lateness(1), 0);
    assert_eq!(s.last_min_lateness_ns(1), None);
}

#[test]
fn a_second_without_packets_has_no_least_lateness() {
    let s = shared(1);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    // A fresh slot, a taken one and one past the slots: none.
    assert_eq!(s.last_min_lateness_ns(0), None);
    assert_eq!(s.take_max_lateness(0), 0);
    assert_eq!(s.last_min_lateness_ns(0), None);
    assert_eq!(s.take_max_lateness(MAX_FLOWS), 0);
    assert_eq!(s.last_min_lateness_ns(MAX_FLOWS), None);
    publish(
        &s,
        &[(0, plan(LOOPBACK, 1, true, 1))],
        &[(0, RxRoute { flow: 0, slot: 0 })],
    );
    let t = transmitter(LOOPBACK);
    // A packet read before our clock is ready is not placed: not judged.
    t.send_to(&packet_at(5_000, 1, 1), slot_addr(&s, 0))
        .unwrap();
    pump(&mut rx, 0, 1, None, 0);
    s.take_max_lateness(0);
    assert_eq!(s.last_min_lateness_ns(0), None);
    // A placed one is; the second after it, without a packet, is none.
    t.send_to(&packet_at(5_000, 1, 1), slot_addr(&s, 0))
        .unwrap();
    pump(&mut rx, 0, 2, Some(ns_of(5_096, RATE)), 0);
    s.take_max_lateness(0);
    assert_eq!(
        s.last_min_lateness_ns(0),
        Some(ns_of(5_096, RATE) - ns_of(5_000, RATE))
    );
    s.take_max_lateness(0);
    assert_eq!(s.last_min_lateness_ns(0), None);
    // A stray datagram (another address's) is no packet of the flow.
    let other = transmitter(Ipv4Addr::new(127, 0, 0, 2));
    other
        .send_to(&packet_at(5_000, 1, 1), slot_addr(&s, 0))
        .unwrap();
    pump(&mut rx, 0, 3, Some(ns_of(5_096, RATE)), 0);
    assert_eq!(s.lane(0).stray, 1);
    s.take_max_lateness(0);
    assert_eq!(s.last_min_lateness_ns(0), None);
}

#[test]
fn routes_leave_before_the_plans_change_and_join_after() {
    let s = shared(2);
    let r = |flow| RxRoute { flow, slot: 0 };
    publish(&s, &[(3, plan(LOOPBACK, 1, true, 1))], &[(0, r(3))]);
    // RX 1 moves from flow 3 to flow 4 (a new flow), RX 2 joins flow 4:
    // while the plans are written, RX 1's old route is gone and neither new
    // route is there yet.
    let seen = std::cell::RefCell::new(Vec::new());
    s.publish(
        |slot| {
            seen.borrow_mut().push((
                s.routes[0].load(Ordering::SeqCst),
                s.routes[1].load(Ordering::SeqCst),
            ));
            if slot == 4 {
                plan(LOOPBACK, 1, true, 2)
            } else {
                RxPlan::default()
            }
        },
        |rx| (rx < 2).then_some(r(4)),
    );
    assert!(
        seen.borrow().iter().all(|&w| w == (0, 0)),
        "{:?}",
        seen.borrow()
    );
    assert_eq!(seen.borrow().len(), MAX_FLOWS);
    for rx in 0..2 {
        assert_eq!(
            RxRoute::from_word(s.routes[rx].load(Ordering::SeqCst)),
            Some(r(4))
        );
    }
}

#[test]
fn a_route_change_alone_is_read() {
    let s = shared(3);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    let p = plan(LOOPBACK, 2, true, 1);
    publish(&s, &[(0, p)], &[(0, RxRoute { flow: 0, slot: 0 })]);
    rx.pass(None, 0);
    assert_eq!(rx.lanes[0].targets, [(0, 0)]);
    // The same plan, one more route.
    publish(
        &s,
        &[(0, p)],
        &[
            (0, RxRoute { flow: 0, slot: 0 }),
            (2, RxRoute { flow: 0, slot: 1 }),
        ],
    );
    rx.pass(None, 0);
    assert_eq!(rx.lanes[0].targets, [(0, 0), (1, 2)]);
}

/// Phase C: the engine's pump reads only a routed channel's ring.
#[test]
fn routed_follows_the_published_routes() {
    let s = shared(3);
    assert!((0..4).all(|rx| !s.routed(rx)), "nothing routed at start");
    let p = plan(LOOPBACK, 2, true, 1);
    publish(
        &s,
        &[(0, p)],
        &[
            (0, RxRoute { flow: 0, slot: 0 }),
            (2, RxRoute { flow: 0, slot: 1 }),
        ],
    );
    let routed: Vec<bool> = (0..4).map(|rx| s.routed(rx)).collect();
    assert_eq!(routed, [true, false, true, false], "RX 4 does not exist");
    // A route that leaves.
    publish(&s, &[(0, p)], &[(2, RxRoute { flow: 0, slot: 0 })]);
    let routed: Vec<bool> = (0..3).map(|rx| s.routed(rx)).collect();
    assert_eq!(routed, [false, false, true]);
}

#[test]
fn a_channel_slot_the_flow_does_not_have_is_never_read() {
    let s = shared(2);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    // A one-slot flow; RX 2 routed to its (missing) second slot.
    publish(
        &s,
        &[(0, plan(LOOPBACK, 1, true, 1))],
        &[
            (0, RxRoute { flow: 0, slot: 0 }),
            (1, RxRoute { flow: 0, slot: 1 }),
        ],
    );
    let t = transmitter(LOOPBACK);
    let now = Some(ns_of(10_032, RATE));
    t.send_to(&packet_at(10_000, 1, 16), slot_addr(&s, 0))
        .unwrap();
    pump(&mut rx, 0, 1, now, 0);
    let mut got = [0i32; 16];
    assert_eq!(s.rings()[0].read_at(10_000, &mut got).holes, 0);
    assert_eq!(s.rings()[1].read_at(10_000, &mut got).holes, 16);
}

#[test]
fn the_longest_packet_fits() {
    // One 16-bit slot: (1472 − 9) / 2 frames, the most a datagram holds.
    assert_eq!(FRAMES_MAX, 731);
    let s = shared(1);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    let mut p = plan(LOOPBACK, 1, true, 1);
    p.bps = 2;
    publish(&s, &[(0, p)], &[(0, RxRoute { flow: 0, slot: 0 })]);
    let mut out = vec![0u8; MAX_DATAGRAM];
    let (secs, sub) = clock::stamp(50_000, RATE);
    let n = write(&mut out, secs, sub, 1, 2, FRAMES_MAX, |f, _| {
        ((f as i32) - 300) << 16
    });
    assert_eq!(n, MAX_DATAGRAM - 1);
    let t = transmitter(LOOPBACK);
    t.send_to(&out[..n], slot_addr(&s, 0)).unwrap();
    pump(&mut rx, 0, 1, Some(ns_of(51_000, RATE)), 0);
    assert_eq!(s.lane(0).packets, 1);
    let mut got = vec![0i32; FRAMES_MAX];
    assert_eq!(s.rings()[0].read_at(50_000, &mut got).holes, 0);
    assert_eq!(got[FRAMES_MAX - 1], 430 << 16);
    assert_eq!(got[0], -300 << 16);
}

#[test]
fn two_slots_waiting_are_both_read_in_one_pass() {
    let s = shared(2);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    publish(
        &s,
        &[
            (0, plan(LOOPBACK, 1, true, 1)),
            (1, plan(LOOPBACK, 1, true, 2)),
        ],
        &[],
    );
    let t = transmitter(LOOPBACK);
    t.send_to(&packet_at(1_000, 1, 4), slot_addr(&s, 0))
        .unwrap();
    t.send_to(&packet_at(1_000, 1, 4), slot_addr(&s, 1))
        .unwrap();
    let deadline = Instant::now() + BOUND;
    while rx.ready != 0b11 {
        rx.wait(Duration::from_millis(20)).unwrap();
        assert!(Instant::now() < deadline, "ready {:b}", rx.ready);
    }
    rx.pass(Some(ns_of(1_004, RATE)), 0);
    assert_eq!((s.lane(0).packets, s.lane(1).packets), (1, 1));
    assert_eq!(rx.ready, 0);
}

/// Bounded (in the nextest `mutants` profile's priority list): a mutant
/// that makes a pass loop forever fails here in 5 s, before any test that
/// hangs on it reaches its timeout.
#[test]
fn a_pass_returns_with_every_slot_waiting() {
    let s = shared(1);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    publish(
        &s,
        &[
            (0, plan(LOOPBACK, 1, true, 1)),
            (3, plan(LOOPBACK, 1, true, 2)),
        ],
        &[],
    );
    let t = transmitter(LOOPBACK);
    for slot in [0, 3, 31] {
        for k in 0..100 {
            t.send_to(&packet_at(1_000 + k, 1, 1), slot_addr(&s, slot))
                .unwrap();
        }
    }
    // Every slot marked waiting, most with nothing: one pass returns.
    rx.ready = u32::MAX;
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        rx.pass(Some(ns_of(2_000, RATE)), 0);
        done.send(rx).ok();
    });
    let rx = finished.recv_timeout(BOUND).expect("the pass returned");
    // At most 64 of each: the two flows' and the strays of a slot without
    // one; their slots still waiting, every other cleared.
    assert_eq!((s.lane(0).packets, s.lane(3).packets), (64, 64));
    assert_eq!(s.lane(31).stray, 64);
    assert_eq!(rx.ready, 1 | 1 << 3 | 1 << 31);
}

#[test]
fn a_failed_wait_is_counted_and_napped_on_an_interrupt_is_nothing() {
    let s = shared(1);
    assert!(!failed_wait(&s, &io::Error::from(ErrorKind::Interrupted)));
    assert_eq!(s.poll_errors(), 0);
    assert!(failed_wait(&s, &io::Error::other("poll failed")));
    assert_eq!(s.poll_errors(), 1);
}

#[test]
fn the_thread_s_clock_runs() {
    let s = shared(1);
    let a = s.now_ns();
    std::thread::sleep(Duration::from_millis(5));
    let b = s.now_ns();
    assert!(b >= a + 5 * MS, "{a} {b}");
}

#[test]
fn late_and_out_of_order_packets_are_counted_and_still_placed() {
    let s = shared(1);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    let mut p = plan(LOOPBACK, 1, true, 1);
    p.latency = 96;
    publish(&s, &[(0, p)], &[(0, RxRoute { flow: 0, slot: 0 })]);
    let t = transmitter(LOOPBACK);
    let base = 1_000_000_000u64;
    let now = ns_of(base + 1_000, RATE);
    // In time (lateness 96: not late), then one 97 behind (late), then one
    // before the end of the furthest (out of order).
    for pos in [base + 1_000 - 96, base + 1_000 - 97, base + 1_000 - 200] {
        t.send_to(&packet_at(pos, 1, 1), slot_addr(&s, 0)).unwrap();
    }
    pump(&mut rx, 0, 3, Some(now), 0);
    let c = s.lane(0);
    assert_eq!((c.packets, c.late, c.out_of_order), (3, 2, 2), "{c:?}");
    assert_eq!(s.take_max_lateness(0), 200);
    // A packet from our future reads as no lateness.
    t.send_to(&packet_at(base + 5_000, 1, 1), slot_addr(&s, 0))
        .unwrap();
    pump(&mut rx, 0, 4, Some(now), 0);
    assert_eq!(s.take_max_lateness(0), 0);
    assert_eq!(s.lane(0).out_of_order, 2);
    let mut one = [0i32; 1];
    for pos in [base + 904, base + 903, base + 800, base + 5_000] {
        assert_eq!(s.rings()[0].read_at(pos, &mut one).holes, 0, "{pos}");
        assert_eq!(one[0], value(pos, 0));
    }
}

#[test]
fn only_the_flow_s_transmitter_counts() {
    let s = shared(1);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    publish(
        &s,
        &[(0, plan(LOOPBACK, 1, true, 1))],
        &[(0, RxRoute { flow: 0, slot: 0 })],
    );
    let base = 1_000_000_000u64;
    let now = ns_of(base, RATE);
    // Another address (loopback's 127.0.0.2), and a slot without a flow.
    let other = transmitter(Ipv4Addr::new(127, 0, 0, 2));
    other
        .send_to(&packet_at(base, 1, 4), slot_addr(&s, 0))
        .unwrap();
    pump(&mut rx, 0, 1, Some(now), 0);
    let t = transmitter(LOOPBACK);
    t.send_to(&packet_at(base, 1, 4), slot_addr(&s, 1)).unwrap();
    pump(&mut rx, 1, 1, Some(now), 0);
    assert_eq!(s.lane(0).stray, 1);
    assert_eq!(s.lane(0).packets, 0);
    assert_eq!(s.lane(1).stray, 1);
    let mut got = [0i32; 4];
    assert_eq!(s.rings()[0].read_at(base, &mut got).holes, 4);
    // Neither gets a keepalive.
    for mono in (0..=1_000).step_by(10) {
        rx.pass(Some(now), mono * MS);
    }
    assert_eq!(s.lane(0).keepalives + s.lane(1).keepalives, 0);
    let mut buf = [0u8; 16];
    other.set_nonblocking(true).unwrap();
    assert!(other.recv(&mut buf).is_err());
}

#[test]
fn garbage_never_panics() {
    let s = shared(2);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    publish(
        &s,
        &[(0, plan(LOOPBACK, 2, true, 1))],
        &[
            (0, RxRoute { flow: 0, slot: 0 }),
            (1, RxRoute { flow: 0, slot: 1 }),
        ],
    );
    let t = transmitter(LOOPBACK);
    let mut rng = Rng::new(0x7e57_0072);
    let now = Some(ns_of(10 * 365 * 86_400 * u64::from(RATE), RATE));
    let mut sent = 0;
    for batch in 0..200 {
        for _ in 0..50 {
            let n = rng.below(1_501);
            let d: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
            t.send_to(&d, slot_addr(&s, 0)).unwrap();
            sent += 1;
        }
        pump(&mut rx, 0, sent, now, batch * 10 * MS);
    }
    let c = s.lane(0);
    assert_eq!(c.packets + c.refused, 10_000, "{c:?}");
    // About one in six random lengths is whole 6-byte frames.
    assert!(c.packets > 1_000 && c.refused > 7_000, "{c:?}");
    assert_eq!(c.stray, 0);
    // A whole packet that is not 24-bit 2-slot frames, or too long, is
    // refused too.
    let mut long = packet_at(0, 1, 1);
    long.resize(MAX_DATAGRAM + 1, 0);
    let refused = c.refused;
    for d in [packet_at(0, 1, 5), packet_at(0, 2, 0), long, vec![2; 9]] {
        t.send_to(&d, slot_addr(&s, 0)).unwrap();
    }
    pump(&mut rx, 0, sent + 4, now, 0);
    assert_eq!(s.lane(0).refused, refused + 4);
}

/// Each keepalive `13 37` waiting on `t`.
fn keepalives(t: &UdpSocket) -> usize {
    t.set_nonblocking(true).unwrap();
    let mut buf = [0u8; 16];
    let mut n = 0;
    while let Ok(len) = t.recv(&mut buf) {
        assert_eq!(&buf[..len], KEEPALIVE);
        n += 1;
    }
    t.set_nonblocking(false).unwrap();
    n
}

#[test]
fn keepalives_are_4_a_second_per_flow_while_its_audio_comes() {
    let s = shared(3);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    publish(
        &s,
        &[
            (0, plan(LOOPBACK, 1, true, 1)),
            (1, plan(LOOPBACK, 1, true, 2)),
            (2, plan(LOOPBACK, 1, false, 3)),
        ],
        &[],
    );
    let ts: Vec<UdpSocket> = (0..3).map(|_| transmitter(LOOPBACK)).collect();
    let now = Some(ns_of(1_000_000_000, RATE));
    for (slot, t) in ts.iter().enumerate() {
        t.send_to(&packet_at(1_000_000_000, 1, 1), slot_addr(&s, slot))
            .unwrap();
        pump(&mut rx, slot, 1, now, 0);
    }
    // One second in 10 ms passes: four each, to each flow's own source.
    for mono in (0..=1_000).step_by(10) {
        rx.pass(now, mono * MS);
    }
    let after_1_s: Vec<usize> = ts.iter().map(keepalives).collect();
    assert_eq!(after_1_s, [4, 4, 0], "the third flow was never granted");
    // No audio since: they end once it is 2 s old (at 250 … 1750 ms).
    for mono in (1_010..=5_000).step_by(10) {
        rx.pass(now, mono * MS);
    }
    let after_5_s: Vec<usize> = ts.iter().map(keepalives).collect();
    assert_eq!(after_5_s, [3, 3, 0]);
    assert_eq!(s.lane(0).keepalives, 7);
    assert_eq!(s.lane(2).keepalives, 0);
    // Audio again: they resume 250 ms after it, never in a burst for the
    // time missed.
    ts[0]
        .send_to(&packet_at(1_000_000_100, 1, 1), slot_addr(&s, 0))
        .unwrap();
    pump(&mut rx, 0, 2, now, 6_000 * MS);
    rx.pass(now, 6_200 * MS);
    assert_eq!(keepalives(&ts[0]), 0);
    // A pass that comes late sends one, and the next is 250 ms after it.
    rx.pass(now, 7_000 * MS);
    assert_eq!(keepalives(&ts[0]), 1);
    rx.pass(now, 7_100 * MS);
    assert_eq!(keepalives(&ts[0]), 0);
    rx.pass(now, 7_250 * MS);
    assert_eq!(keepalives(&ts[0]), 1);
}

#[test]
fn the_wait_ends_for_datagrams_the_next_keepalive_or_at_most_33_ms() {
    let s = shared(1);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    assert_eq!(rx.timeout(0), Duration::from_nanos(WAKE_NS));
    publish(&s, &[(0, plan(LOOPBACK, 1, true, 1))], &[]);
    let t = transmitter(LOOPBACK);
    t.send_to(&packet_at(1_000, 1, 1), slot_addr(&s, 0))
        .unwrap();
    pump(&mut rx, 0, 1, Some(ns_of(1_000, RATE)), 0);
    // The first keepalive is due 250 ms after the audio.
    rx.pass(Some(ns_of(1_000, RATE)), 240 * MS);
    assert_eq!(rx.timeout(240 * MS), Duration::from_millis(10));
    assert_eq!(rx.timeout(100 * MS), Duration::from_nanos(WAKE_NS));
    assert_eq!(rx.timeout(300 * MS), Duration::ZERO);
    // Datagrams waiting: no wait at all.
    rx.ready = 1;
    assert_eq!(rx.timeout(100 * MS), Duration::ZERO);
}

#[test]
fn a_flood_is_read_64_at_a_time_and_the_rest_at_once_after() {
    let s = shared(1);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    publish(&s, &[(0, plan(LOOPBACK, 1, true, 1))], &[]);
    let t = transmitter(LOOPBACK);
    for k in 0..100 {
        t.send_to(&packet_at(1_000 + k, 1, 1), slot_addr(&s, 0))
            .unwrap();
    }
    let deadline = Instant::now() + BOUND;
    while rx.ready == 0 {
        rx.wait(Duration::from_millis(20)).unwrap();
        assert!(Instant::now() < deadline);
    }
    rx.pass(Some(ns_of(2_000, RATE)), 0);
    assert_eq!(s.lane(0).packets, DRAIN as u64);
    assert_eq!(rx.ready, 1, "more wait");
    assert_eq!(rx.timeout(0), Duration::ZERO);
    rx.pass(Some(ns_of(2_000, RATE)), 0);
    assert_eq!(s.lane(0).packets, 100);
    assert_eq!(rx.ready, 0);
}

#[test]
fn nothing_is_placed_before_the_clock_is_ready() {
    let s = shared(1);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    publish(
        &s,
        &[(0, plan(LOOPBACK, 1, true, 1))],
        &[(0, RxRoute { flow: 0, slot: 0 })],
    );
    let t = transmitter(LOOPBACK);
    t.send_to(&packet_at(5_000, 1, 8), slot_addr(&s, 0))
        .unwrap();
    pump(&mut rx, 0, 1, None, 0);
    let c = s.lane(0);
    assert_eq!((c.packets, c.unplaced), (1, 1));
    let mut got = [0i32; 8];
    assert_eq!(s.rings()[0].read_at(5_000, &mut got).holes, 8);
    assert_eq!(s.take_max_lateness(0), 0);
    // Its audio still keeps the flow alive.
    rx.pass(None, 250 * MS);
    assert_eq!(keepalives(&t), 1);
}

#[test]
fn a_new_flow_in_the_slot_starts_afresh() {
    let s = shared(1);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    publish(&s, &[(0, plan(LOOPBACK, 1, true, 1))], &[]);
    let t = transmitter(LOOPBACK);
    let now = Some(ns_of(5_000, RATE));
    t.send_to(&packet_at(5_000, 1, 8), slot_addr(&s, 0))
        .unwrap();
    pump(&mut rx, 0, 1, now, 0);
    assert!(rx.lanes[0].source.is_some());
    // The next flow (another generation): no source, no keepalive, no
    // order to keep, until its own audio.
    publish(&s, &[(0, plan(LOOPBACK, 1, true, 2))], &[]);
    rx.pass(now, 250 * MS);
    assert_eq!(keepalives(&t), 0);
    assert_eq!(rx.lanes[0].source, None);
    t.send_to(&packet_at(4_000, 1, 8), slot_addr(&s, 0))
        .unwrap();
    pump(&mut rx, 0, 2, now, 300 * MS);
    assert_eq!(s.lane(0).out_of_order, 0);
    rx.pass(now, 550 * MS);
    assert_eq!(keepalives(&t), 1);
}

#[test]
fn ports_and_addresses_are_the_slots_own() {
    let s = shared(1);
    let ports = s.ports().unwrap();
    for (slot, port) in ports.iter().enumerate() {
        assert_ne!(*port, 0);
        assert_eq!(
            slot_addr(&s, slot),
            SocketAddr::V4(SocketAddrV4::new(LOOPBACK, *port))
        );
    }
    let mut sorted = ports.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), MAX_FLOWS);
    assert!(s.local_addr(MAX_FLOWS).is_err());
    assert_eq!(s.lane(MAX_FLOWS), RxCounts::default());
    assert_eq!(s.packets(), [0; MAX_FLOWS]);
    assert_eq!(s.poll_errors(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn the_receive_buffer_is_raised_as_far_as_the_kernel_allows() {
    let max: usize = std::fs::read_to_string("/proc/sys/net/core/rmem_max")
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // The kernel doubles what it grants (its bookkeeping), up to rmem_max.
    assert_eq!(shared(1).recv_buffer(), 2 * RECV_BUFFER_BYTES.min(max));
}

#[test]
fn the_rx_thread_receives_until_stopped() {
    let s = shared(1);
    let start = ns_of(10 * 365 * 86_400 * u64::from(RATE), RATE);
    let clock = Arc::new(ManualClock::new(start));
    publish(
        &s,
        &[(0, plan(LOOPBACK, 1, true, 1))],
        &[(0, RxRoute { flow: 0, slot: 0 })],
    );
    let thread = spawn(clock, Arc::clone(&s), None).unwrap();
    let t = transmitter(LOOPBACK);
    let p0 = clock::to_samples(start, RATE) - 64;
    t.send_to(&packet_at(p0, 1, 32), slot_addr(&s, 0)).unwrap();
    let deadline = Instant::now() + BOUND;
    while s.lane(0).packets == 0 {
        assert!(Instant::now() < deadline, "the thread read nothing");
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut got = [0i32; 32];
    assert_eq!(s.rings()[0].read_at(p0, &mut got).holes, 0);
    s.stop();
    let stopped = Instant::now();
    while !thread.is_finished() {
        assert!(
            stopped.elapsed() < Duration::from_secs(1),
            "the thread never ended"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    thread.join().unwrap();
}
