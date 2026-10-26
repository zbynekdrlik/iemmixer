//! Our device's audio through its own flow control on loopback (Gen 3
//! phase B, plan Task 7): our flow-control client asks our own UDP 4455
//! server for a flow of TX 1 to RX 1, the TX side sends the counter, the RX
//! side places it in RX 1's ring, and RX 1's counter check reads one
//! simulated second back with no mismatch, dropout or slip. Then the stop
//! releases the flow at the transmitter (our server). The media clock is a
//! `ManualClock` the test moves, and the test steps the TX and RX passes as
//! their threads step them (the threads' loops are glue): every packet sent
//! is received before time moves on, and every wait is bounded.

#![cfg(target_os = "linux")]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iem_dante::clock::{ManualClock, PtpClock};
use iem_dante::ctl::{CheckKind, CheckStats, Received, Sent};
use iem_dante::daemon::media::{Clock, Driver, Media};
use iem_dante::device::{Device, DeviceConfig, Subscription};
use iem_dante::flows::client::Client;
use iem_dante::flows::control::{PORT, op};
use iem_dante::flows::server;
use iem_dante::info::Changes;
use iem_dante::media::rx::Receiver;
use iem_dante::media::source::Source;
use iem_dante::media::tx::Transmitter;
use iem_dante::subscribe::Answer;
use iem_dante::subscribe::resolve::{Advertised, Resolver};
use tokio::net::UdpSocket;

const TEST_DEVICE: &str = include_str!("../../../config/test-dante-device.toml");
/// Every wait's bound.
const BOUND: Duration = Duration::from_secs(5);
/// PTP time at the start: 1000 s, a whole second of samples at 96 kHz.
const START: u64 = 1_000_000_000_000;
const SECOND: u64 = 1_000_000_000;

/// Our own TX 1's record, its flow control where the test's server is.
struct OwnTx1(Advertised);

impl Resolver for OwnTx1 {
    fn lookup(&self, tx_device: &str, tx_channel: &str) -> Option<Advertised> {
        (tx_device == "test-device" && tx_channel == "TX 1").then_some(self.0)
    }
}

#[tokio::test]
async fn our_tx_counter_reaches_our_rx_clean_for_a_second_then_the_stop_releases_the_flow() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = DeviceConfig {
        state_file: dir.path().join("state.toml"),
        ..DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-dante-device.toml")
    };
    let device = Device::load(cfg).unwrap();
    let clock = Arc::new(ManualClock::new(START));
    let media = Arc::new(
        Media::new(
            &device,
            Ipv4Addr::LOCALHOST,
            Clock {
                ptp: clock.clone(),
                freq: Arc::default(),
                stats: Arc::default(),
            },
            7,
        )
        .unwrap(),
    );
    // Our flow-control server, at a port of its own (the record says
    // where): what the daemon serves on UDP 4455.
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let SocketAddr::V4(control) = sock.local_addr().unwrap() else {
        panic!("an IPv4 socket");
    };
    let serving = tokio::spawn(server::serve(
        sock,
        Arc::clone(&media.tx),
        Arc::clone(&media.requests),
    ));
    // RX 1 subscribed to our TX 1, which sends the counter; RX 1 checked.
    media.manager.lock().unwrap().set(
        0,
        Some(Subscription {
            tx_channel: "TX 1".into(),
            tx_device: "test-device".into(),
        }),
    );
    assert!(media.tx.set_source(0, Source::Counter));
    assert!(
        media
            .checks
            .lock()
            .unwrap()
            .set(0, Some(CheckKind::Counter))
    );
    let record = OwnTx1(Advertised {
        control,
        tx_channel_id: 1,
        nchan: 2,
        bits: 24,
        rate: 96_000,
        dbcp1: 0x1102,
        fpp_min: 16,
        fpp_max: 32,
        latency_ns: 1_000_000,
    });

    // Our client asks our server for the flow.
    let changes = Arc::new(Changes::default());
    let (mut driver, mut answers) = Driver::new(
        Arc::clone(&media),
        Client::new(Ipv4Addr::LOCALHOST),
        Arc::clone(&changes),
    );
    driver.tick(&record);
    let (flow, answer) = tokio::time::timeout(BOUND, answers.recv())
        .await
        .expect("an answer within the bound")
        .expect("the driver holds a sender");
    assert!(matches!(answer, Answer::Granted(_)), "{answer:?}");
    driver.answered(flow, answer);
    let tx_slots: Vec<usize> = media.tx.lock().table.flows().map(|(s, _)| s).collect();
    let [tx_slot] = tx_slots[..] else {
        panic!("one TX flow: {tx_slots:?}");
    };
    let rx_slot = flow.slot;

    // One simulated second, the passes stepped as the threads step them:
    // the TX pass at each time it asks to wake at.
    let mut tx = Transmitter::new(Arc::clone(&media.tx));
    let mut rx = Receiver::new(Arc::clone(&media.rx)).unwrap();
    let deadline = Instant::now() + 4 * BOUND;
    while clock.now_ns().unwrap() < START + SECOND {
        let now = clock.now_ns();
        tx.step(now, clock.epoch());
        let sent = media.tx.lane(tx_slot).packets;
        while media.rx.lane(rx_slot).packets < sent {
            assert!(
                Instant::now() < deadline,
                "sent {sent}, received {:?}",
                media.rx.lane(rx_slot)
            );
            rx.wait(Duration::from_millis(10)).unwrap();
            rx.pass(clock.now_ns(), media.rx.now_ns());
        }
        media.step_checks();
        clock.sleep_until(tx.wake_ns(now));
    }

    // 96 000 samples a second at 32 a packet: about 3 000 packets, each
    // received whole and in time.
    let sent = media.tx.lane(tx_slot);
    let got = media.rx.lane(rx_slot);
    assert!((2_990..=3_000).contains(&sent.packets), "{sent:?}");
    assert_eq!(got.packets, sent.packets);
    assert_eq!((sent.send_errors, sent.rebootstraps, sent.holes), (0, 0, 0));
    assert_eq!(
        (
            got.late,
            got.out_of_order,
            got.refused,
            got.stray,
            got.unplaced
        ),
        (0, 0, 0, 0, 0),
        "{got:?}"
    );
    let views = media.checks.lock().unwrap().views();
    let CheckStats::Counter(s) = views[0].stats else {
        panic!("{views:?}");
    };
    assert_eq!(
        (s.mismatch, s.dropout, s.slips, s.lag, s.no_return),
        (0, 0, 0, Some(0), false),
        "{s:?}"
    );
    assert!(s.checked > 95_000, "{s:?}");
    assert_eq!(views[0].skipped, 0);

    // The stop: our server gets our stop and releases the flow.
    tokio::time::timeout(BOUND, driver.stop())
        .await
        .expect("the stop within the bound");
    assert!(media.tx.lock().table.flow(tx_slot).is_none());
    assert!(media.manager.lock().unwrap().flow(rx_slot).is_none());
    let counts = media.requests.counts();
    assert_eq!(
        counts.sent,
        [
            Sent {
                peer: 1,
                op: op::REQUEST,
                n: 1
            },
            Sent {
                peer: 1,
                op: op::STOP,
                n: 1
            }
        ]
    );
    assert_eq!(
        counts.received,
        [
            Received {
                port: PORT,
                peer: 1,
                op: op::REQUEST,
                n: 1
            },
            Received {
                port: PORT,
                peer: 1,
                op: op::STOP,
                n: 1
            }
        ]
    );
    serving.abort();
}
