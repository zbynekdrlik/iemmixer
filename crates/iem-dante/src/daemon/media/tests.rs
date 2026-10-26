use super::*;

use std::collections::HashMap;
use std::net::{SocketAddr, SocketAddrV4};
use std::path::Path;

use tokio::net::UdpSocket;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

use crate::clock::ManualClock;
use crate::ctl::{CheckKind, CheckStats, Sent};
use crate::device::{DeviceConfig, Subscription};
use crate::flows::control::{Handle, status};
use crate::media::packet;
use crate::media::rx::{MAX_FLOWS, Receiver};
use crate::media::source::Source;
use crate::subscribe::SubStatus;
use crate::subscribe::resolve::Advertised;
use crate::wire::{Header, reply};

pub(crate) const TEST_DEVICE: &str = include_str!("../../../../../config/test-dante-device.toml");
/// Every wait's bound, so a mutant that hangs fails fast.
pub(crate) const BOUND: Duration = Duration::from_secs(5);
/// A PTP time (1000 s): 96 000 000 samples at 96 kHz, a whole second.
pub(crate) const T: u64 = 1_000_000_000_000;
const OTHER_HOST: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);

/// A media clock that is never ready; its sleeps nap a millisecond (the
/// media threads' passes).
pub(crate) struct Unready;

impl PtpClock for Unready {
    fn now_ns(&self) -> Option<u64> {
        None
    }

    fn sleep_until(&self, _ptp_ns: u64) {
        std::thread::sleep(Duration::from_millis(1));
    }

    fn epoch(&self) -> u32 {
        0
    }
}

/// The synthetic device (config/test-dante-device.toml) with its state
/// file in `dir`.
pub(crate) fn device_at(dir: &Path) -> Device {
    let cfg = DeviceConfig {
        state_file: dir.join("state.toml"),
        ..DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-dante-device.toml")
    };
    Device::load(cfg).unwrap()
}

/// `device`'s audio side on loopback, on the clock `ptp` (dantesync's
/// frequency adjustment −750 ppb).
pub(crate) fn media_on(device: &Device, ptp: Arc<dyn PtpClock>) -> Arc<Media> {
    media_with_engine(device, ptp, &[])
}

/// [`media_on`] with an engine ring on each TX channel index in `engine`.
pub(crate) fn media_with_engine(
    device: &Device,
    ptp: Arc<dyn PtpClock>,
    engine: &[usize],
) -> Arc<Media> {
    let clock = Clock {
        ptp,
        freq: Arc::new(AtomicI32::new(-750)),
        stats: Arc::default(),
    };
    Arc::new(Media::new(device, Ipv4Addr::LOCALHOST, clock, 7, engine).unwrap())
}

pub(crate) fn sub(channel: &str, device: &str) -> Subscription {
    Subscription {
        tx_channel: channel.into(),
        tx_device: device.into(),
    }
}

/// A transmitter's channel record: 2 slots a flow, 24 bits at 96 kHz,
/// 16–32 frames a packet, a 1 ms latency.
pub(crate) fn advert(control: SocketAddrV4, id: u16) -> Advertised {
    Advertised {
        control,
        tx_channel_id: id,
        nchan: 2,
        bits: 24,
        rate: 96_000,
        dbcp1: 0x1102,
        fpp_min: 16,
        fpp_max: 32,
        latency_ns: 1_000_000,
    }
}

/// The channel records a test's network holds.
#[derive(Default)]
pub(crate) struct Records(HashMap<(String, String), Advertised>);

impl Records {
    pub(crate) fn with(mut self, device: &str, channel: &str, a: Advertised) -> Self {
        self.0.insert((device.into(), channel.into()), a);
        self
    }
}

impl Resolver for Records {
    fn lookup(&self, tx_device: &str, tx_channel: &str) -> Option<Advertised> {
        self.0
            .get(&(tx_device.to_string(), tx_channel.to_string()))
            .copied()
    }
}

fn v4(addr: SocketAddr) -> SocketAddrV4 {
    match addr {
        SocketAddr::V4(v4) => v4,
        SocketAddr::V6(_) => panic!("an IPv4 socket"),
    }
}

/// What a stand-in transmitter answers a flow request.
#[derive(Debug, Clone, Copy)]
enum Grant {
    Handle(Handle),
    Refuse(u16),
}

/// A transmitter's flow control on loopback: it answers requests as
/// `grant` says, never answers a stop, and hands every stop's handle over.
struct StandIn {
    addr: SocketAddrV4,
    stops: UnboundedReceiver<Handle>,
    task: tokio::task::JoinHandle<()>,
}

async fn stand_in(ip: Ipv4Addr, grant: Grant) -> StandIn {
    let sock = UdpSocket::bind((ip, 0)).await.unwrap();
    let addr = v4(sock.local_addr().unwrap());
    let (tx, stops) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            let Ok((n, from)) = sock.recv_from(&mut buf).await else {
                return;
            };
            let Ok((h, body)) = Header::parse(&buf[..n]) else {
                continue;
            };
            match (h.op1, grant) {
                (op::REQUEST, Grant::Handle(handle)) => {
                    sock.send_to(&reply(&h, status::OK, &handle), from)
                        .await
                        .unwrap();
                }
                (op::REQUEST, Grant::Refuse(code)) => {
                    sock.send_to(&reply(&h, code, &[]), from).await.unwrap();
                }
                (op::STOP, _) => {
                    let _ = tx.send(body.try_into().expect("a stop is the 6-byte handle"));
                }
                _ => {}
            }
        }
    });
    StandIn { addr, stops, task }
}

fn driver_of(media: &Arc<Media>) -> (Driver, Answers, Arc<Changes>) {
    let changes = Arc::new(Changes::default());
    let (driver, answers) = Driver::new(
        Arc::clone(media),
        Client::new(Ipv4Addr::LOCALHOST),
        Arc::clone(&changes),
    );
    (driver, answers, changes)
}

/// The next answer, bounded, handed to the driver; returns it.
async fn answer_next(driver: &mut Driver, answers: &mut Answers) -> Answer {
    let (flow, answer) = tokio::time::timeout(BOUND, answers.recv())
        .await
        .expect("an answer within the bound")
        .expect("the driver holds a sender");
    driver.answered(flow, answer);
    answer
}

/// The flow slots the manager holds a flow in.
fn flow_slots(media: &Media) -> Vec<usize> {
    let m = lock(&media.manager);
    (0..MAX_FLOWS).filter(|&s| m.flow(s).is_some()).collect()
}

fn sent(media: &Media) -> Vec<(usize, u16, u64)> {
    media
        .requests
        .counts()
        .sent
        .iter()
        .map(|&Sent { peer, op, n }| (peer, op, n))
        .collect()
}

#[test]
fn the_media_side_starts_from_the_state_file_s_subscriptions_and_silence() {
    let dir = tempfile::tempdir().unwrap();
    let mut device = device_at(dir.path());
    let cfg = device.cfg.clone();
    assert!(
        device
            .state
            .set_subscription(&cfg, 2, Some(sub("TX 4", "peer-device")))
    );
    let media = media_on(&device, Arc::new(Unready));
    assert_eq!(media.ip, Ipv4Addr::LOCALHOST);
    assert_eq!(
        media.limits(),
        Limits {
            tx: 16,
            rx: 32,
            rate: 96_000
        }
    );
    assert_eq!(media.rx.rings().len(), 32);
    assert!(
        Arc::ptr_eq(media.rx.rings(), media.tx.rings()),
        "one ring set"
    );
    {
        let mut m = lock(&media.manager);
        assert_eq!(m.subscription(2), Some(&sub("TX 4", "peer-device")));
        assert_eq!(m.status(2), Some(SubStatus::Unresolved));
        assert_eq!(m.status(0), None);
        // The start's statuses are the baseline: nothing moved yet.
        assert!(m.changed().is_empty());
        assert_eq!(m.ip(), Ipv4Addr::LOCALHOST);
    }
    for tx in 0..16 {
        assert_eq!(media.tx.source(tx), Some(Source::Silence), "TX {}", tx + 1);
    }
    let ports = media.rx.ports().unwrap();
    assert!(ports.iter().all(|&p| p != 0));
    let flow = media.manager.lock().unwrap().flow(0);
    assert_eq!(flow, None);
    assert!(lock(&media.checks).views().is_empty());
    // The standalone device: no engine ring on any TX channel, no engine.
    for tx in 0..16 {
        assert!(media.tx.engine_ring(tx).is_none(), "TX {}", tx + 1);
    }
    assert_eq!(media.engine(), None);
}

#[test]
fn engine_channels_get_one_engine_ring_each_and_the_rest_none() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_with_engine(&device, Arc::new(Unready), &[0, 5, 15]);
    for tx in 0..16 {
        assert_eq!(
            media.tx.engine_ring(tx).is_some(),
            [0, 5, 15].contains(&tx),
            "TX {}",
            tx + 1
        );
        // Every channel still starts silent: the engine sets its sources.
        assert_eq!(media.tx.source(tx), Some(Source::Silence), "TX {}", tx + 1);
    }
    assert!(media.tx.set_source(5, Source::Engine));
    assert!(!media.tx.set_source(6, Source::Engine));
    // A channel the device lacks: refused, nothing bound.
    let clock = Clock {
        ptp: Arc::new(Unready),
        freq: Arc::default(),
        stats: Arc::default(),
    };
    let e = Media::new(&device, Ipv4Addr::LOCALHOST, clock, 7, &[16])
        .err()
        .expect("TX 17 is past the device's 16");
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn an_attached_engine_s_numbers_are_its_pump_s() {
    use crate::stream::{PumpStats, PumpView, Timing};
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_with_engine(&device, Arc::new(Unready), &[0]);
    assert_eq!(media.engine(), None, "none before it is attached");
    let timing = Timing {
        block: 32,
        playout: 96,
        margin: 96,
    };
    media.attach_engine(Arc::new(PumpStats::new(timing, 96_000)));
    assert_eq!(
        media.engine(),
        Some(PumpView {
            offset: 224,
            playout: 96,
            ..PumpView::default()
        })
    );
    // A later attach replaces it.
    let at_2_ms = Timing {
        playout: 192,
        ..timing
    };
    media.attach_engine(Arc::new(PumpStats::new(at_2_ms, 96_000)));
    assert_eq!(
        media.engine().map(|v| (v.offset, v.playout)),
        Some((320, 192))
    );
}

#[test]
fn the_heartbeat_and_arc_read_the_media_side_itself() {
    let dir = tempfile::tempdir().unwrap();
    let mut device = device_at(dir.path());
    let cfg = device.cfg.clone();
    device
        .state
        .set_subscription(&cfg, 0, Some(sub("TX 1", "peer-device")));
    let clock = Arc::new(ManualClock::new(T));
    let media = media_on(&device, clock.clone());
    use crate::arc::Flows;
    use crate::info::Beat;
    let r = media
        .beat()
        .records()
        .expect("records once the clock is ready");
    assert_eq!((r.freq_ppb, r.rate), (-750, 96_000));
    assert_eq!((r.tx_peaks.len(), r.rx_peaks.len()), (16, 32));
    clock.set_ready(false);
    assert!(media.beat().records().is_none());
    let flows = media.flows();
    assert!(Arc::ptr_eq(&flows.tx, &media.tx) && Arc::ptr_eq(&flows.rx, &media.manager));
    let seen = flows.view(|live| live.rx.subscription(0).cloned());
    assert_eq!(seen, Some(sub("TX 1", "peer-device")));
}

#[tokio::test]
async fn the_driver_asks_hands_the_grant_to_the_rx_thread_and_notifies() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(ManualClock::new(T)));
    let handle: Handle = [0, 0, 0, 3, 0xab, 0xcd];
    let t = stand_in(Ipv4Addr::LOCALHOST, Grant::Handle(handle)).await;
    let records = Records::default().with("peer-device", "TX 2", advert(t.addr, 2));
    lock(&media.manager).set(4, Some(sub("TX 2", "peer-device")));
    let (mut driver, mut answers, changes) = driver_of(&media);
    driver.tick(&records);
    // One request, counted; RX 5 moved to InProgress and was notified.
    assert_eq!(sent(&media), [(1, op::REQUEST, 1)]);
    assert_eq!(changes.take(), [4]);
    assert_eq!(
        answer_next(&mut driver, &mut answers).await,
        Answer::Granted(handle)
    );
    let slot = flow_slots(&media)[0];
    {
        let m = lock(&media.manager);
        assert_eq!(m.flow(slot).unwrap().handle, Some(handle));
        assert_eq!(m.status(4), Some(SubStatus::InProgress));
    }
    // The plan reached the RX thread: the transmitter's audio to the flow's
    // port is a packet of the flow (an unpublished plan counts it stray).
    let mut receiver = Receiver::new(Arc::clone(&media.rx)).unwrap();
    let audio = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut buf = [0u8; 1472];
    let n = packet::write(&mut buf, 1000, 0, 2, 3, 32, |f, _| f as i32 * 256);
    audio
        .send_to(&buf[..n], ("127.0.0.1", media.rx.ports().unwrap()[slot]))
        .unwrap();
    let deadline = Instant::now() + BOUND;
    while media.rx.lane(slot).packets == 0 {
        assert!(Instant::now() < deadline, "{:?}", media.rx.lane(slot));
        receiver.wait(Duration::from_millis(10)).unwrap();
        receiver.pass(Some(T), media.rx.now_ns());
    }
    assert_eq!(media.rx.lane(slot).stray, 0);
    // Heard at the next round: Receiving, notified.
    driver.tick(&records);
    assert_eq!(lock(&media.manager).status(4), Some(SubStatus::Receiving));
    assert_eq!(changes.take(), [4]);
    t.task.abort();
}

#[tokio::test]
async fn a_refused_request_reads_its_failure_and_is_notified() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    let t = stand_in(
        Ipv4Addr::LOCALHOST,
        Grant::Refuse(status::TOO_MANY_TX_FLOWS),
    )
    .await;
    let records = Records::default().with("peer-device", "TX 1", advert(t.addr, 1));
    lock(&media.manager).set(0, Some(sub("TX 1", "peer-device")));
    let (mut driver, mut answers, changes) = driver_of(&media);
    driver.tick(&records);
    assert_eq!(changes.take(), [0]);
    assert_eq!(
        answer_next(&mut driver, &mut answers).await,
        Answer::Refused(status::TOO_MANY_TX_FLOWS)
    );
    assert_eq!(
        lock(&media.manager).status(0),
        Some(SubStatus::TooManyTxFlows)
    );
    assert_eq!(changes.take(), [0]);
    assert!(flow_slots(&media).is_empty());
    // Asked once: the next round waits the transmitter's 9 s.
    driver.tick(&records);
    assert_eq!(sent(&media), [(1, op::REQUEST, 1)]);
    t.task.abort();
}

#[tokio::test]
async fn a_stop_with_a_silent_transmitter_ends_within_1_s() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    // Two transmitters that grant and then never answer a stop: the stops
    // go out together, so the stop ends within one wait, not two.
    let (ha, hb): (Handle, Handle) = ([0, 0, 0, 1, 0, 0xa], [0, 0, 0, 2, 0, 0xb]);
    let mut a = stand_in(Ipv4Addr::LOCALHOST, Grant::Handle(ha)).await;
    let mut b = stand_in(OTHER_HOST, Grant::Handle(hb)).await;
    let records = Records::default()
        .with("tx-a", "TX 1", advert(a.addr, 1))
        .with("tx-b", "TX 1", advert(b.addr, 1));
    {
        let mut m = lock(&media.manager);
        m.set(0, Some(sub("TX 1", "tx-a")));
        m.set(1, Some(sub("TX 1", "tx-b")));
    }
    let (mut driver, mut answers, changes) = driver_of(&media);
    driver.tick(&records);
    for _ in 0..2 {
        assert!(matches!(
            answer_next(&mut driver, &mut answers).await,
            Answer::Granted(_)
        ));
    }
    assert_eq!(flow_slots(&media).len(), 2);
    changes.take();
    let started = Instant::now();
    tokio::time::timeout(BOUND, driver.stop())
        .await
        .expect("the stop ends within the bound");
    let took = started.elapsed();
    assert!(
        took <= STOP_WAIT + Duration::from_millis(300),
        "the stop took {took:?}"
    );
    // Each transmitter got its stop, once.
    for (t, handle) in [(&mut a, ha), (&mut b, hb)] {
        let got = tokio::time::timeout(BOUND, t.stops.recv()).await.unwrap();
        assert_eq!(got, Some(handle));
        assert!(t.stops.try_recv().is_err(), "one stop a flow");
    }
    assert_eq!(
        sent(&media),
        [
            (1, op::REQUEST, 1),
            (1, op::STOP, 1),
            (2, op::REQUEST, 1),
            (2, op::STOP, 1)
        ]
    );
    // Nothing left to receive: no flow, every plan withdrawn, both
    // channels' statuses moved (back to Unresolved) and notified.
    assert!(flow_slots(&media).is_empty());
    {
        let m = lock(&media.manager);
        assert!((0..MAX_FLOWS).all(|s| m.rx_plan(s).src.is_none()));
        assert_eq!(m.status(0), Some(SubStatus::Unresolved));
    }
    assert_eq!(changes.take(), [0, 1]);
    a.task.abort();
    b.task.abort();
}

#[tokio::test]
async fn a_stop_drops_a_request_in_flight_and_ends_at_once_without_flows() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    // A transmitter that never answers at all.
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let records = Records::default().with(
        "peer-device",
        "TX 1",
        advert(v4(silent.local_addr().unwrap()), 1),
    );
    lock(&media.manager).set(0, Some(sub("TX 1", "peer-device")));
    let (mut driver, mut answers, _) = driver_of(&media);
    driver.tick(&records);
    let mut buf = [0u8; 2048];
    tokio::time::timeout(BOUND, silent.recv_from(&mut buf))
        .await
        .expect("the request went out")
        .unwrap();
    let started = Instant::now();
    driver.stop().await;
    assert!(started.elapsed() < Duration::from_millis(500));
    // No stop for a flow never granted, and the request's task is gone.
    assert_eq!(sent(&media), [(1, op::REQUEST, 1)]);
    assert!(flow_slots(&media).is_empty());
    let late = tokio::time::timeout(Duration::from_millis(100), answers.recv()).await;
    assert!(!matches!(late, Ok(Some(_))), "no answer after the stop");
}

#[tokio::test]
async fn the_media_threads_start_and_end_within_the_wait() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    let threads = media.spawn_threads().unwrap();
    assert!(!media.tx.stopping() && !media.rx.stopping());
    let started = Instant::now();
    assert!(threads.stop(&media, THREADS_WAIT).await);
    assert!(started.elapsed() < THREADS_WAIT);
    assert!(media.tx.stopping() && media.rx.stopping());
}

#[tokio::test]
async fn the_checks_read_a_granted_flow_s_ring_on_a_ready_clock_only() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let clock = Arc::new(ManualClock::new(T));
    let media = media_on(&device, clock.clone());
    let peer = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4455);
    let records = Records::default()
        .with("peer-device", "TX 1", advert(peer, 1))
        .with("peer-device", "TX 2", advert(peer, 2))
        .with(
            "other-device",
            "TX 1",
            advert(SocketAddrV4::new(OTHER_HOST, 4455), 1),
        );
    {
        let mut c = lock(&media.checks);
        assert!(c.set(0, Some(CheckKind::Counter)));
        assert!(c.set(1, Some(CheckKind::Counter)));
        assert!(c.set(2, Some(CheckKind::Sine { hz: 1000.0 })));
    }
    // RX 3 holds a steady 100 000 (left-justified) where the checks read:
    // above a 24-bit flow's floor, under the floor of any narrower width.
    let at = to_samples(T, 96_000) - 96;
    media.rx.rings()[2].write_at(at, &vec![100_000; 2_000]);
    let stats = |rx: usize| lock(&media.checks).views()[rx].stats;
    // No subscription: nothing read.
    media.step_checks();
    assert_eq!(stats(0), CheckStats::Waiting);
    // RX 1 and RX 3 granted (one flow of the peer's TX 1 and TX 2), RX 2
    // only asked for.
    {
        let mut m = lock(&media.manager);
        m.set(0, Some(sub("TX 1", "peer-device")));
        m.set(1, Some(sub("TX 1", "other-device")));
        m.set(2, Some(sub("TX 2", "peer-device")));
        let now = media.rx.now_ns();
        let actions = m.tick(now, &media.rx.packets(), &records);
        let granted = actions
            .iter()
            .find_map(|a| match a {
                Action::Request { flow, to, .. } if *to == peer => Some(*flow),
                _ => None,
            })
            .unwrap();
        m.answered(granted, Answer::Granted([0, 0, 0, 1, 0, 1]), now);
        media.publish(&m);
    }
    media.step_checks();
    clock.advance(10_000_000);
    media.step_checks();
    // 10 ms at 96 kHz of a ring nothing wrote: blind, no lock.
    let CheckStats::Counter(s) = stats(0) else {
        panic!("{:?}", stats(0));
    };
    assert_eq!((s.blind, s.checked, s.lag), (960, 0, None));
    assert_eq!(stats(1), CheckStats::Waiting);
    // The sine check judged with its flow's 24 bits: nothing blind.
    let CheckStats::Sine(s) = stats(2) else {
        panic!("{:?}", stats(2));
    };
    assert_eq!((s.checked, s.blind, s.discontinuities), (956, 0, 0));
    // A clock that is not ready reads nothing.
    clock.set_ready(false);
    clock.advance(10_000_000);
    media.step_checks();
    let CheckStats::Counter(s) = stats(0) else {
        panic!("{:?}", stats(0));
    };
    assert_eq!(s.blind, 960);
}
