//! The media hot path allocates nothing (Gen 3 phase B, plan Tasks 3, 4
//! and 5): a packet written and read, a ring written and read, every TX
//! source and both checkers, the TX thread's passes (the plans, the
//! sources, the keepalives, the packets and their sends) and the RX
//! thread's (the wait on every flow socket, the plans and routes, the
//! receive, the placing in the rings, the keepalives), under
//! `assert_no_alloc` (warn mode: the violation count must stay zero). The
//! first test proves the detector sees an allocation. Its own test binary,
//! because it installs the global allocator; every buffer, ring, checker,
//! socket, the transmitter and the receiver are built before the detector
//! runs.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use iem_dante::clock;
use iem_dante::device::DeviceConfig;
use iem_dante::flows::control::FlowRequest;
use iem_dante::media::check::{CounterCheck, SineCheck};
use iem_dante::media::packet::{self, MEDIA_HEADER};
use iem_dante::media::ring::Ring;
use iem_dante::media::rx::{Receiver, RxPlan, RxRoute, RxShared};
use iem_dante::media::source::Source;
use iem_dante::media::tx::{Transmitter, TxShared, ns_of};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

/// Ten years of a grandmaster's uptime at 96 kHz, as a position.
const T: u64 = 10 * 365 * 86_400 * 96_000;
const RATE: u32 = 96_000;
const FPP: usize = 32;
const SLOTS: usize = 8;

#[test]
fn the_detector_sees_an_allocation() {
    reset_violation_count();
    let v = assert_no_alloc(|| vec![1u8; 4]);
    assert!(violation_count() > 0);
    assert_eq!(v.len(), 4);
}

#[test]
fn the_media_path_does_not_allocate() {
    let rings: Vec<Ring> = (0..SLOTS).map(|_| Ring::new()).collect();
    let mut sources = [
        Source::Silence,
        Source::Counter,
        Source::Sine {
            hz: 1_000.0,
            dbfs: -20.0,
        },
        Source::Echo { rx: 0, offset: 192 },
        Source::Echo { rx: 1, offset: 229 },
        Source::Echo { rx: 99, offset: 0 },
        Source::Counter,
        Source::Sine {
            hz: 997.0,
            dbfs: 0.0,
        },
    ];
    let mut counter = CounterCheck::new(u64::from(RATE) * 10);
    let mut sine = SineCheck::new(1_000.0, RATE, 24);
    let mut frames = [[0i32; FPP]; SLOTS];
    let mut out = [0u8; 1500];
    let mut channel = [0i32; FPP];
    let mut written = 0usize;
    let mut holes = 0usize;
    reset_violation_count();
    assert_no_alloc(|| {
        for p in 0..2_000u64 {
            let stamp = T + p * FPP as u64;
            // TX: each source fills its slot, the packet is written.
            for (src, frame) in sources.iter_mut().zip(frames.iter_mut()) {
                holes += src.fill(stamp, RATE, frame, &rings).holes;
            }
            let (secs, sub) = (stamp / u64::from(RATE), stamp % u64::from(RATE));
            let n = packet::write(&mut out, secs as u32, sub as u32, SLOTS, 3, FPP, |f, s| {
                frames[s][f]
            });
            assert_eq!(n, MEDIA_HEADER + FPP * SLOTS * 3);
            // RX: the header, then each slot's samples into its ring.
            let (secs, sub) = packet::read_header(&out[..n]).unwrap();
            let pos = u64::from(secs) * u64::from(RATE) + u64::from(sub);
            for (slot, ring) in rings.iter().enumerate() {
                packet::read_frames(&out[..n], SLOTS, 3, |f, s, v| {
                    if s == slot {
                        channel[f] = v;
                    }
                })
                .unwrap();
                written += ring.write_at(pos, &channel).written;
            }
            // The checkers read the counter's and the sine's rings back,
            // two packets behind (written by then).
            if p >= 2 {
                let back = stamp - 2 * FPP as u64;
                rings[1].read_at(back, &mut channel);
                counter.check(back, &channel);
                rings[2].read_at(back, &mut channel);
                sine.check(back, &channel);
            }
        }
    });
    assert_eq!(violation_count(), 0, "the media path allocated");
    assert_eq!(written, 2_000 * FPP * SLOTS);
    assert!(holes > 0, "the echoes ran, the missing RX channel too");
    let c = counter.stats();
    assert_eq!((c.mismatch, c.dropout, c.slips), (0, 0, 0), "{c:?}");
    assert!(c.checked > 60_000, "{c:?}");
    let s = sine.stats();
    assert_eq!(s.discontinuities, 0, "{s:?}");
    assert!(s.checked > 60_000, "{s:?}");
}

/// A receiver's socket on loopback and its address.
fn receiver() -> (UdpSocket, SocketAddrV4) {
    let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    s.set_nonblocking(true).unwrap();
    let SocketAddr::V4(at) = s.local_addr().unwrap() else {
        panic!("an IPv4 socket");
    };
    (s, at)
}

#[test]
fn the_tx_thread_s_passes_do_not_allocate() {
    let cfg = DeviceConfig::from_toml(include_str!("../../../config/test-dante-device.toml"))
        .expect("config/test-dante-device.toml");
    let rings: Arc<[Ring]> = (0..2).map(|_| Ring::new()).collect::<Vec<_>>().into();
    let shared = Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, rings, 1).unwrap());
    assert!(shared.set_source(0, Source::Counter));
    assert!(shared.set_source(1, Source::Echo { rx: 0, offset: 37 }));
    assert!(shared.set_source(
        2,
        Source::Sine {
            hz: 1_000.0,
            dbfs: -20.0
        }
    ));
    let (a, at_a) = receiver();
    let (b, at_b) = receiver();
    {
        let mut c = shared.lock();
        for (rx, channels, bits, fpp) in [
            (at_a, vec![Some(1), Some(2), Some(3)], 24, 32),
            (at_b, vec![Some(3), None, Some(1)], 32, 16),
        ] {
            let req = FlowRequest {
                rx_host: "peer-device".to_string(),
                rx_flow_name: "flow-1".to_string(),
                sample_rate: RATE,
                bits,
                channels,
                fpp,
                rx,
            };
            c.table.add(&req, shared.now_ns()).unwrap();
        }
        shared.publish(&c);
    }
    // A keepalive waiting on the first flow's socket.
    a.send_to(&[0x13, 0x37], shared.local_addr(0).unwrap())
        .unwrap();
    let mut tx = Transmitter::new(Arc::clone(&shared));
    let start = T / u64::from(RATE) * 1_000_000_000;
    reset_violation_count();
    assert_no_alloc(|| {
        // 100 ms in passes 250 µs apart: three reads of the plans, the
        // sources and the keepalives, and every packet in between.
        for i in 0..400 {
            tx.step(Some(start + i * 250_000), 0);
        }
    });
    assert_eq!(violation_count(), 0, "the TX passes allocated");
    let (la, lb) = (shared.lane(0), shared.lane(1));
    // 100 ms at 96 kHz: 300 packets of 32 frames, 600 of 16, less the first.
    assert!((295..=300).contains(&la.packets), "{la:?}");
    assert!((595..=600).contains(&lb.packets), "{lb:?}");
    assert_eq!((la.send_errors, lb.send_errors), (0, 0));
    assert_eq!(la.keepalives, 1);
    assert!(la.holes > 0, "the echo of an empty ring");
    let mut buf = [0u8; 2048];
    assert_eq!(a.recv(&mut buf).unwrap(), MEDIA_HEADER + 3 * 3 * 32);
    assert_eq!(b.recv(&mut buf).unwrap(), MEDIA_HEADER + 3 * 4 * 16);
}

#[test]
fn the_rx_thread_s_passes_do_not_allocate() {
    let rings: Arc<[Ring]> = (0..4).map(|_| Ring::new()).collect::<Vec<_>>().into();
    let shared = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, rings).unwrap());
    let plan = RxPlan {
        src: Some(Ipv4Addr::LOCALHOST),
        bps: 3,
        nslots: 2,
        latency: 192,
        live: true,
        generation: 1,
    };
    // RX 1 and 3 from slot 0, RX 2 from slot 1, RX 4 from nothing.
    shared.publish(
        |slot| if slot == 0 { plan } else { RxPlan::default() },
        |rx| match rx {
            0 | 2 => Some(RxRoute { flow: 0, slot: 0 }),
            1 => Some(RxRoute { flow: 0, slot: 1 }),
            _ => None,
        },
    );
    let mut rx = Receiver::new(Arc::clone(&shared)).unwrap();
    let (transmitter, _) = receiver();
    let dest = shared.local_addr(0).unwrap();
    // 300 packets of 32 frames, 24-bit, two slots: 100 ms of audio.
    let packets: Vec<Vec<u8>> = (0..300u64)
        .map(|k| {
            let mut out = vec![0u8; 1500];
            let (secs, sub) = clock::stamp(T + 32 * k, RATE);
            let n = packet::write(&mut out, secs, sub, 2, 3, FPP, |f, s| {
                ((f as i32 + 1) << 8) * if s == 0 { 1 } else { -1 }
            });
            out.truncate(n);
            out
        })
        .collect();
    let now = ns_of(T + 32 * 300 + 64, RATE);
    reset_violation_count();
    assert_no_alloc(|| {
        // A pass a millisecond: 300 ms, one keepalive due at 250 ms.
        for (k, p) in packets.iter().enumerate() {
            transmitter.send_to(p, dest).unwrap();
            rx.wait(Duration::ZERO).unwrap();
            rx.pass(Some(now), k as u64 * 1_000_000);
        }
    });
    assert_eq!(violation_count(), 0, "the RX passes allocated");
    let c = shared.lane(0);
    assert_eq!((c.packets, c.refused, c.stray), (300, 0, 0), "{c:?}");
    assert_eq!(c.keepalives, 1);
    let mut got = [0i32; FPP];
    for (ring, sign) in [(0, 1), (1, -1), (2, 1)] {
        assert_eq!(
            shared.rings()[ring].read_at(T + 32 * 299, &mut got).holes,
            0
        );
        assert_eq!(got[FPP - 1], ((FPP as i32) << 8) * sign);
    }
    assert_eq!(shared.rings()[3].read_at(T, &mut got).holes, FPP);
    let mut buf = [0u8; 16];
    assert_eq!(transmitter.recv(&mut buf).unwrap(), 2);
}
