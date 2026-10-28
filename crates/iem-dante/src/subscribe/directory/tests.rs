use super::*;

use std::cell::RefCell;
use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::mpsc;

use mdns_sd::ServiceInfo;

use crate::announce::CHAN_SERVICE;
use crate::arc::client::TxChannel;
use crate::arc::{ArcOutcome, Live, handle};
use crate::ctl::Sent;
use crate::device::{DeviceConfig, DeviceState, Subscription};
use crate::flows::control::FlowRequest;
use crate::media::tx::TxTable;
use crate::subscribe::resolve::Own;
use crate::subscribe::{Action, MAX_FLOWS, Manager};

const TEST_DEVICE: &str = include_str!("../../../../../config/test-dante-device.toml");
const S: u64 = 1_000_000_000;
const MS: u64 = 1_000_000;
const OUR_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 10);
const PEER_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 11);
const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;
/// Every wait in these tests ends.
const BOUND: Duration = Duration::from_secs(5);

fn cfg() -> DeviceConfig {
    DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-dante-device.toml")
}

/// A record of `service` named `device` at `ip`:`port`, as the browse
/// hands it over.
fn record(service: &str, device: &str, ip: &str, port: u16) -> ServiceEvent {
    let info = ServiceInfo::new(service, device, "peer-device.local.", ip, port, None).unwrap();
    ServiceEvent::ServiceResolved(Box::new(info.as_resolved_service()))
}

fn gone(service: &str, device: &str) -> ServiceEvent {
    ServiceEvent::ServiceRemoved(service.to_string(), format!("{device}.{service}"))
}

/// A book that knows peer-device's records since 0.
fn peer_book() -> Book {
    peer_book_with(Timing::default())
}

/// [`peer_book`] on `timing`.
fn peer_book_with(timing: Timing) -> Book {
    let mut book = Book::new(timing);
    book.apply(&record(ARC_SERVICE, "peer-device", "10.0.0.11", 4440), 0);
    book.apply(&record(DBC_SERVICE, "peer-device", "10.0.0.11", 4455), 0);
    book
}

/// [`peer_book_with`] `timing`, the card's directory read at [`SETTLE_NS`].
fn read_peer_book(timing: Timing) -> Book {
    let mut book = peer_book_with(timing);
    assert_eq!(book.lookup("peer-device", "Spare", SETTLE_NS), None);
    let job = book.next(SETTLE_NS).unwrap();
    book.finished(&job, done(card(), 6), SETTLE_NS);
    book
}

/// A card's directory: 16 channels at 96 kHz, 24 bits, default names `01`
/// to `16`, channel 3 named `Spare`.
fn card() -> Directory {
    let listed = (1..=16)
        .map(|id| TxChannel {
            id,
            rate: 96_000,
            bits: 24,
            default: format!("{id:02}"),
            name: None,
        })
        .collect();
    Directory::new(listed, &[(3, "Spare".to_string())])
}

fn done(dir: Directory, requests: usize) -> Outcome {
    Outcome {
        result: Ok(dir),
        requests,
    }
}

fn no_reply() -> Outcome {
    Outcome {
        result: Err(ReadError::Timeout { op: 0x1000 }),
        requests: 1,
    }
}

const DBC: SocketAddrV4 = SocketAddrV4::new(PEER_IP, 4455);

#[test]
fn an_arc_resolved_channel_offers_the_dbc_port_4_slots_fpp_16_and_our_latency() {
    let dir = card();
    let want = Advertised {
        control: DBC,
        tx_channel_id: 3,
        nchan: 4,
        bits: 24,
        rate: 96_000,
        dbcp1: 0x1102,
        fpp_min: 16,
        fpp_max: 16,
        latency_ns: 0,
    };
    assert_eq!(offer(dir.find("spare").unwrap(), DBC, 16), want);
    // A book on the default timing (a config without arc_rx_fpp) offers it.
    let mut book = read_peer_book(Timing::default());
    assert_eq!(book.lookup("peer-device", "Spare", SETTLE_NS), Some(want));
}

#[test]
fn an_arc_resolved_channel_offers_the_fpp_its_timing_asks() {
    // #72 phase F: the device config's arc_rx_fpp, both ends of it.
    for fpp in [4, 2, 32] {
        let mut book = read_peer_book(Timing {
            fpp,
            ..Timing::default()
        });
        let a = book.lookup("peer-device", "Spare", SETTLE_NS).unwrap();
        assert_eq!((a.fpp_min, a.fpp_max), (fpp, fpp));
        assert_eq!(a, offer(card().find("Spare").unwrap(), DBC, fpp));
    }
}

#[test]
fn a_device_config_s_timing_asks_its_arc_rx_fpp() {
    assert_eq!(Timing::default().fpp, FPP);
    assert_eq!(Timing::of(&cfg()), Timing::default(), "16 when left out");
    let cfg = DeviceConfig {
        arc_rx_fpp: 4,
        ..cfg()
    };
    assert_eq!(
        Timing::of(&cfg),
        Timing {
            fpp: 4,
            ..Timing::default()
        }
    );
}

#[test]
fn many_lookups_of_one_device_make_one_read() {
    let mut book = peer_book();
    let t = SETTLE_NS;
    for _ in 0..4 {
        for ch in ["Spare", "01", "02", "16"] {
            assert_eq!(book.lookup("peer-device", ch, t), None);
        }
    }
    assert_eq!(book.wants(), 1);
    let job = book.next(t).unwrap();
    assert_eq!(
        job,
        Job {
            device: "peer-device".into(),
            arc: SocketAddrV4::new(PEER_IP, 4440),
            want: Want::Missing,
        }
    );
    // While it runs: no second read, no want.
    assert_eq!(book.lookup("PEER-device", "01", t + MS), None);
    assert_eq!(book.next(t + 20 * S), None);
    assert_eq!(book.next_due(), None);
    assert_eq!(book.wants(), 1);
    book.finished(&job, done(card(), 6), t + 100 * MS);
    let at = t + 200 * MS;
    assert_eq!(
        book.lookup("peer-device", "SPARE", at)
            .unwrap()
            .tx_channel_id,
        3
    );
    assert_eq!(book.lookup("peer-device", "16", at).unwrap().control, DBC);
    assert_eq!(book.next(at), None);
    assert_eq!(
        book.counts(),
        Counts {
            reads: 1,
            requests: 6,
            ..Counts::default()
        }
    );
}

/// `book`'s reads over `minutes` of lookups of `channels` every 250 ms, each
/// read taking `takes` and ending with `outcome`: the reads' start times.
fn reads_over(
    book: &mut Book,
    minutes: u64,
    channels: &[&str],
    takes: u64,
    outcome: impl Fn() -> Outcome,
) -> Vec<u64> {
    let mut starts = Vec::new();
    let mut running: Option<(Job, u64)> = None;
    for tick in 0..=minutes * 60 * 4 {
        let t = tick * 250 * MS;
        if let Some((job, end)) = running.take() {
            if t >= end {
                book.finished(&job, outcome(), end);
            } else {
                running = Some((job, end));
            }
        }
        for ch in channels {
            book.lookup("peer-device", ch, t);
        }
        if let Some(job) = book.next(t) {
            assert!(running.is_none(), "two reads of one device at {t}");
            starts.push(t);
            running = Some((job, t + takes));
        }
    }
    starts
}

#[test]
fn a_dead_device_gets_at_most_12_reads_of_one_request_in_10_minutes() {
    let mut book = peer_book();
    let channels: Vec<String> = (1..=18).map(|n| format!("{n:02}")).collect();
    let names: Vec<&str> = channels.iter().map(String::as_str).collect();
    // Each read is one request that waits its 1 s.
    let starts = reads_over(&mut book, 10, &names, S, no_reply);
    assert_eq!(
        starts,
        [5, 15, 34, 71, 144, 217, 290, 363, 436, 509, 582].map(|s| s * S)
    );
    for pair in starts.windows(2) {
        assert!(pair[1] - pair[0] >= 9 * S + S);
    }
    let c = book.counts();
    assert_eq!((c.reads, c.no_reply, c.requests), (0, 11, 11));
    assert_eq!(c.failed(), 11);
}

#[test]
fn a_name_not_listed_is_read_again_within_the_same_bounds() {
    let mut book = peer_book();
    let starts = reads_over(&mut book, 10, &["Spare", "Nope"], 0, || done(card(), 6));
    assert_eq!(
        starts,
        [5, 14, 23, 41, 77, 149, 221, 293, 365, 437, 509, 581].map(|s| s * S)
    );
    let c = book.counts();
    assert_eq!((c.reads, c.requests), (12, 72));
    assert!(c.not_listed > 0);
    // The listed channel answers from the last read.
    assert_eq!(
        book.lookup("peer-device", "Spare", 581 * S)
            .unwrap()
            .tx_channel_id,
        3
    );
}

#[test]
fn an_old_directory_is_read_again_once_and_a_failure_clears_on_success() {
    let mut book = peer_book();
    assert_eq!(book.lookup("peer-device", "01", 5 * S), None);
    let job = book.next(5 * S).unwrap();
    book.finished(&job, no_reply(), 6 * S);
    // The wait after one failure: 9 s from the end.
    book.lookup("peer-device", "01", 6 * S);
    assert_eq!(book.next_due(), Some(15 * S));
    assert_eq!(book.next(15 * S - 1), None);
    let job = book.next(15 * S).unwrap();
    assert_eq!(job.want, Want::Missing);
    book.finished(&job, done(card(), 6), 15 * S);
    // Fresh for a minute, then old: one read, which clears the failures.
    assert!(book.lookup("peer-device", "01", 75 * S - 1).is_some());
    assert_eq!(book.lookup("peer-device", "01", 75 * S), None);
    let job = book.next(75 * S).unwrap();
    assert_eq!(job.want, Want::Old);
    assert_eq!(book.next(75 * S), None);
    book.finished(&job, done(card(), 6), 75 * S);
    assert!(book.lookup("peer-device", "01", 75 * S).is_some());
    // A name missing now waits the 9 s of no failure.
    assert_eq!(book.lookup("peer-device", "nope", 76 * S), None);
    assert_eq!(book.next_due(), Some(84 * S));
    let job = book.next(84 * S).unwrap();
    assert_eq!(job.want, Want::NotListed);
    assert_eq!(book.counts().reads, 2);
}

#[test]
fn no_read_before_its_records_settle_or_without_flow_control() {
    let mut book = Book::new(Timing::default());
    book.apply(
        &record(ARC_SERVICE, "peer-device", "10.0.0.11", 4440),
        2 * S,
    );
    // No `_netaudio-dbc` record: nothing to ask, so nothing read.
    assert_eq!(book.lookup("peer-device", "01", 60 * S), None);
    assert_eq!(book.next(60 * S), None);
    assert_eq!(book.counts().unknown, 1);
    book.apply(
        &record(DBC_SERVICE, "peer-device", "10.0.0.11", 4455),
        60 * S,
    );
    // Settled 5 s after the ARC record came, not after the DBC record.
    let mut book2 = Book::new(Timing::default());
    book2.apply(&record(DBC_SERVICE, "peer-device", "10.0.0.11", 4455), 0);
    book2.apply(
        &record(ARC_SERVICE, "peer-device", "10.0.0.11", 4440),
        2 * S,
    );
    assert_eq!(book2.lookup("peer-device", "01", 7 * S - 1), None);
    assert_eq!(book2.next(7 * S - 1), None);
    assert_eq!(book2.wants(), 0);
    assert_eq!(book2.lookup("peer-device", "01", 7 * S), None);
    assert_eq!(book2.wants(), 1);
    assert!(book2.next(7 * S).is_some());
    // A device the browse never showed.
    assert_eq!(book2.lookup("other-device", "01", 60 * S), None);
    assert_eq!(book2.counts().unknown, 1);
    // An ARC record again (after its goodbye) settles again.
    let job = book.next(60 * S);
    assert!(job.is_none());
    book.lookup("peer-device", "01", 60 * S);
    book.apply(&gone(ARC_SERVICE, "peer-device"), 61 * S);
    // Its want is dropped with the record.
    assert_eq!(book.next(61 * S), None);
    assert_eq!(book.next_due(), None);
    book.apply(
        &record(ARC_SERVICE, "peer-device", "10.0.0.11", 4440),
        62 * S,
    );
    assert_eq!(book.lookup("peer-device", "01", 66 * S), None);
    assert_eq!(book.next(66 * S), None);
    book.lookup("peer-device", "01", 67 * S);
    assert!(book.next(67 * S).is_some());
}

#[test]
fn the_browse_s_records_are_kept_case_aside_and_bounded() {
    let mut book = Book::new(Timing {
        settle_ns: 0,
        ..Timing::default()
    });
    assert_eq!(
        book.apply(
            &record(ARC_SERVICE, "Peer-Device", "10.0.0.12,10.0.0.11", 4440),
            0
        ),
        Applied::Resolved
    );
    assert_eq!(
        book.apply(&record(DBC_SERVICE, "PEER-device", "10.0.0.11", 4455), 0),
        Applied::Resolved
    );
    book.lookup("peer-device", "01", 0);
    let job = book.next(0).unwrap();
    // The lowest address.
    assert_eq!(job.arc, SocketAddrV4::new(PEER_IP, 4440));
    book.finished(&job, done(card(), 6), 0);
    assert_eq!(book.lookup("peer-DEVICE", "01", 0).unwrap().control, DBC);
    // A newer flow-control record moves the port.
    book.apply(&record(DBC_SERVICE, "peer-device", "10.0.0.11", 4456), 0);
    assert_eq!(
        book.lookup("peer-device", "01", 0).unwrap().control,
        SocketAddrV4::new(PEER_IP, 4456)
    );
    // Its goodbye: no flow control, nothing resolved.
    assert_eq!(
        book.apply(&gone(DBC_SERVICE, "Peer-Device"), 0),
        Applied::Removed
    );
    assert_eq!(book.lookup("peer-device", "01", 0), None);
    assert_eq!(
        book.apply(&gone(DBC_SERVICE, "peer-device"), 0),
        Applied::Ignored
    );
    // Both gone: the device is forgotten.
    assert_eq!(
        book.apply(&gone(ARC_SERVICE, "peer-device"), 0),
        Applied::Removed
    );
    assert_eq!(
        book.apply(&gone(ARC_SERVICE, "peer-device"), 0),
        Applied::Ignored
    );
    // Other services, records without an IPv4 address, names that are not
    // the service's, and other events change nothing.
    let other = ServiceInfo::new(
        CHAN_SERVICE,
        "01@peer-device",
        "peer-device.local.",
        "10.0.0.11",
        4455,
        None,
    )
    .unwrap();
    let no_ip = ServiceInfo::new(ARC_SERVICE, "x-device", "x.local.", "", 4440, None).unwrap();
    for ev in [
        ServiceEvent::ServiceResolved(Box::new(other.as_resolved_service())),
        ServiceEvent::ServiceResolved(Box::new(no_ip.as_resolved_service())),
        ServiceEvent::ServiceRemoved(
            CHAN_SERVICE.to_string(),
            format!("01@peer-device.{CHAN_SERVICE}"),
        ),
        ServiceEvent::ServiceRemoved(
            ARC_SERVICE.to_string(),
            format!("peer-device.{DBC_SERVICE}"),
        ),
        ServiceEvent::ServiceRemoved(ARC_SERVICE.to_string(), ARC_SERVICE.to_string()),
        ServiceEvent::ServiceRemoved(ARC_SERVICE.to_string(), format!(".{ARC_SERVICE}")),
        ServiceEvent::SearchStarted(ARC_SERVICE.to_string()),
    ] {
        assert_eq!(book.apply(&ev, 0), Applied::Ignored, "{ev:?}");
    }
    assert!(book.records.is_empty());
    // At most MAX_RECORDS devices; a known one is still updated.
    for n in 0..MAX_RECORDS {
        let ev = record(ARC_SERVICE, &format!("d{n}"), "10.0.0.11", 4440);
        assert_eq!(book.apply(&ev, 0), Applied::Resolved);
    }
    assert_eq!(
        book.apply(&record(ARC_SERVICE, "one-more", "10.0.0.11", 4440), 0),
        Applied::Ignored
    );
    assert_eq!(
        book.apply(&record(DBC_SERVICE, "d0", "10.0.0.11", 4455), 0),
        Applied::Resolved
    );
    assert_eq!(book.records.len(), MAX_RECORDS);
}

#[test]
fn at_most_max_devices_are_read() {
    let mut book = Book::new(Timing {
        settle_ns: 0,
        ..Timing::default()
    });
    for n in 0..=MAX_DEVICES {
        let device = format!("d{n}");
        book.apply(&record(ARC_SERVICE, &device, "10.0.0.11", 4440), 0);
        book.apply(&record(DBC_SERVICE, &device, "10.0.0.11", 4455), 0);
        book.lookup(&device, "01", 0);
    }
    assert_eq!(book.wants(), MAX_DEVICES as u64);
    assert_eq!(book.counts().unknown, 1);
    // A known one still asks.
    book.lookup("d0", "02", 0);
    assert_eq!(book.counts().unknown, 1);
    let mut jobs = 0;
    while book.next(0).is_some() {
        jobs += 1;
    }
    assert_eq!(jobs, MAX_DEVICES);
}

#[test]
fn a_failed_read_counts_why() {
    let mut book = peer_book();
    let errors = [
        ReadError::Io {
            op: 0x1000,
            kind: std::io::ErrorKind::ConnectionRefused,
        },
        ReadError::Timeout { op: 0x2000 },
        ReadError::Refused {
            op: 0x2010,
            status: 0xffff,
        },
        ReadError::Malformed { op: 0x2000 },
        ReadError::Stopped,
    ];
    let mut t = SETTLE_NS;
    for e in errors {
        book.lookup("peer-device", "01", t);
        let job = book.next(t).unwrap();
        book.finished(
            &job,
            Outcome {
                result: Err(e),
                requests: 2,
            },
            t,
        );
        t += 100 * S;
    }
    assert_eq!(
        book.counts(),
        Counts {
            no_reply: 2,
            refused: 1,
            malformed: 1,
            socket: 1,
            requests: 10,
            ..Counts::default()
        }
    );
    // A read ended for a device forgotten meanwhile only counts its requests.
    let job = Job {
        device: "gone".into(),
        arc: DBC,
        want: Want::Missing,
    };
    book.finished(&job, done(card(), 3), t);
    assert_eq!(book.counts().requests, 13);
    assert_eq!(book.counts().reads, 0);
}

#[test]
fn the_report_is_at_most_once_a_minute_and_only_of_what_happened() {
    let mut r = Report::new(0);
    let one = Counts {
        reads: 1,
        requests: 6,
        ..Counts::default()
    };
    assert_eq!(r.due(REPORT_NS - 1, one), None);
    assert_eq!(r.due(REPORT_NS, one), Some(one));
    // Nothing new: no line; the next minute's counts are the difference.
    assert_eq!(r.due(2 * REPORT_NS, one), None);
    let more = Counts {
        reads: 3,
        requests: 18,
        not_listed: 4,
        ..Counts::default()
    };
    assert_eq!(r.due(3 * REPORT_NS - 1, more), None);
    assert_eq!(
        r.due(3 * REPORT_NS, more),
        Some(Counts {
            reads: 2,
            requests: 12,
            not_listed: 4,
            ..Counts::default()
        })
    );
    let c = Counts {
        reads: 6,
        no_reply: 1,
        refused: 2,
        malformed: 3,
        socket: 4,
        requests: 7,
        not_listed: 8,
        unknown: 5,
    };
    assert_eq!(c.failed(), 10);
    assert_eq!(c.since(&Counts::default()), c);
    assert_eq!(c.since(&c), Counts::default());
}

/// A log sink for the report's line.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// What `f` logs at INFO or above on this thread, as text.
fn logged(f: impl FnOnce()) -> String {
    let sink = Sink::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let bytes = sink.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn the_minute_s_report_is_one_line_of_numbers() {
    let mut r = Report::new(0);
    let c = Counts {
        reads: 2,
        no_reply: 1,
        requests: 13,
        not_listed: 40,
        ..Counts::default()
    };
    let early = logged(|| r.look(REPORT_NS - 1, c));
    assert_eq!(early, "");
    let line = logged(|| r.look(REPORT_NS, c));
    assert_eq!(line.lines().count(), 1, "{line}");
    for part in [
        "ARC directory reads in a minute",
        "reads=2",
        "failed=1",
        "no_reply=1",
        "requests=13",
        "not_listed=40",
        "unknown_device=0",
    ] {
        assert!(line.contains(part), "{part}: {line}");
    }
    assert_eq!(logged(|| r.look(2 * REPORT_NS, c)), "");
}

/// A resolver of a book at a fixed time.
struct At(RefCell<Book>, u64);

impl Resolver for At {
    fn lookup(&self, tx_device: &str, tx_channel: &str) -> Option<Advertised> {
        self.0.borrow_mut().lookup(tx_device, tx_channel, self.1)
    }
}

fn sub(channel: &str, device: &str) -> Subscription {
    Subscription {
        tx_channel: channel.to_string(),
        tx_device: device.to_string(),
    }
}

/// The manager of the device `cfg`, RX 1 and 2 subscribed to the card's
/// Spare and 05, ticked once on a book on `timing` that read the card: its
/// one request, and the manager.
fn manager_asks_the_card(
    cfg: &DeviceConfig,
    timing: Timing,
) -> (SocketAddrV4, FlowRequest, Manager) {
    let ports: [u16; MAX_FLOWS] = std::array::from_fn(|s| 50_000 + s as u16);
    let mut m = Manager::new(cfg, "test-device", OUR_IP, ports);
    m.set(0, Some(sub("Spare", "peer-device")));
    m.set(1, Some(sub("05", "peer-device")));
    let at = At(RefCell::new(read_peer_book(timing)), SETTLE_NS);
    let actions = m.tick(SETTLE_NS, &[0; MAX_FLOWS], &at);
    let [Action::Request { to, req, .. }] = actions.as_slice() else {
        panic!("one request: {actions:?}");
    };
    (*to, req.clone(), m)
}

/// The flow request RX 1 and 2 make of the card at `fpp`.
fn card_request(fpp: u16) -> FlowRequest {
    FlowRequest {
        rx_host: "test-device".into(),
        rx_flow_name: "1_0".into(),
        sample_rate: 96_000,
        bits: 24,
        channels: vec![Some(3), Some(5), None, None],
        fpp,
        rx: SocketAddrV4::new(OUR_IP, 50_000),
    }
}

#[test]
fn the_manager_asks_an_arc_resolved_transmitter_for_4_slots_at_fpp_16() {
    let cfg = cfg();
    let (to, req, m) = manager_asks_the_card(&cfg, Timing::default());
    assert_eq!(to, DBC);
    assert_eq!(req, card_request(16));
    let flow = m.flow(0).unwrap();
    // Played out at our own latency.
    assert_eq!(flow.latency_ns, cfg.latency_ns);
}

#[test]
fn the_manager_asks_an_arc_resolved_transmitter_for_the_config_s_arc_rx_fpp() {
    // #72 phase F: fewer frames a packet than the card's guessed 16, so its
    // packets leave it sooner.
    let cfg = DeviceConfig {
        arc_rx_fpp: 4,
        ..cfg()
    };
    let (to, req, m) = manager_asks_the_card(&cfg, Timing::of(&cfg));
    assert_eq!(to, DBC);
    assert_eq!(req, card_request(4));
    assert_eq!(m.flow(0).unwrap().fpp, 4);
}

/// The mDNS daemon's browses, as a test sees them: each service's events
/// from a channel; each browse counted.
struct FakeDaemon {
    events: RefCell<HashMap<String, mpsc::Receiver<ServiceEvent>>>,
    browses: RefCell<Vec<String>>,
}

impl FakeDaemon {
    /// The daemon and the senders of `services`' events.
    fn new(services: &[&str]) -> (Self, Vec<mpsc::Sender<ServiceEvent>>) {
        let mut events = HashMap::new();
        let mut senders = Vec::new();
        for s in services {
            let (send, recv) = mpsc::channel();
            events.insert(s.to_string(), recv);
            senders.push(send);
        }
        (
            FakeDaemon {
                events: RefCell::new(events),
                browses: RefCell::default(),
            },
            senders,
        )
    }
}

impl Browse for FakeDaemon {
    type Events = mpsc::IntoIter<ServiceEvent>;
    fn browse(&self, service: &str) -> Result<Self::Events, mdns_sd::Error> {
        self.browses.borrow_mut().push(service.to_string());
        self.events
            .borrow_mut()
            .remove(service)
            .map(mpsc::Receiver::into_iter)
            .ok_or_else(|| mdns_sd::Error::Msg(format!("no browse of {service}")))
    }
}

/// Our own ARC server on loopback (the test device's channels) standing in
/// for a card: it answers until `BOUND` passes without a request and counts
/// what it got.
fn our_server() -> (SocketAddrV4, Arc<Mutex<Vec<u16>>>) {
    let sock = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    sock.set_read_timeout(Some(BOUND)).unwrap();
    let std::net::SocketAddr::V4(at) = sock.local_addr().unwrap() else {
        panic!("IPv4")
    };
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = Arc::clone(&got);
    std::thread::spawn(move || {
        let cfg = cfg();
        let mut state = DeviceState::from_config(&cfg);
        state.tx[2] = "Spare".to_string();
        let tx = TxTable::new(0);
        let rx = Manager::new(&cfg, &cfg.name, LOOPBACK, [0; MAX_FLOWS]);
        let mut buf = [0u8; 2048];
        while let Ok((n, from)) = sock.recv_from(&mut buf) {
            g.lock().unwrap().push(u16::from_be_bytes([buf[6], buf[7]]));
            let live = Live { tx: &tx, rx: &rx };
            if let ArcOutcome::Reply(d) = handle(&mut state, &cfg, &live, &buf[..n]) {
                sock.send_to(&d, from).unwrap();
            }
        }
    });
    (at, got)
}

/// Waits until `f` is true, at most `BOUND`.
fn eventually(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + BOUND;
    while !f() {
        assert!(Instant::now() < deadline, "{what} never happened");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_network_resolves_channel_records_first_and_reads_a_device_without_them() {
    let (at, got) = our_server();
    let (daemon, senders) = FakeDaemon::new(&[CHAN_SERVICE, ARC_SERVICE, DBC_SERVICE]);
    let mut network = Network {
        chan: Mdns::new(),
        // A settle the reader's clock must pass; a reader that is only
        // woken by the want (its idle wait is longer than the test).
        arc: ArcDirectory::new(Timing {
            settle_ns: 50 * MS,
            max_age_ns: MAX_AGE_NS,
            reply: Duration::from_secs(1),
            idle: Duration::from_secs(30),
            fpp: FPP,
        }),
    };
    assert!(network.chan.start(&daemon).unwrap());
    // A rack publishes channel records (and an ARC record).
    let rack = ServiceInfo::new(
        CHAN_SERVICE,
        "TX 3@rack",
        "rack.local.",
        "10.0.0.12",
        4455,
        [
            ("id", "3"),
            ("rate", "96000"),
            ("enc", "24"),
            ("dbcp1", "0x1102"),
            ("fpp", "32,2"),
            ("latency_ns", "1000000"),
            ("nchan", "8"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<HashMap<String, String>>(),
    )
    .unwrap();
    senders[0]
        .send(ServiceEvent::ServiceResolved(Box::new(
            rack.as_resolved_service(),
        )))
        .unwrap();
    eventually("the rack's record", || network.chan.names_device("rack"));
    let a = network.lookup("rack", "TX 3").unwrap();
    assert_eq!((a.nchan, a.fpp_max), (8, 32));
    // Another channel of the rack never reaches the ARC directory.
    assert_eq!(network.lookup("rack", "TX 9"), None);
    assert!(!network.arc.asked());
    // A card without channel records does.
    assert_eq!(network.lookup("card", "Spare"), None);
    assert!(network.arc.asked());
    let requests = Arc::new(Requests::default());
    assert!(
        network
            .arc
            .start(&daemon, LOOPBACK, Arc::clone(&requests))
            .unwrap()
    );
    // Once for the directory's life.
    assert!(
        !network
            .arc
            .start(&daemon, LOOPBACK, Arc::clone(&requests))
            .unwrap()
    );
    assert_eq!(
        *daemon.browses.borrow(),
        [CHAN_SERVICE, ARC_SERVICE, DBC_SERVICE]
    );
    let rack_arc = record(ARC_SERVICE, "rack", "10.0.0.12", 4440);
    senders[1].send(rack_arc).unwrap();
    senders[1]
        .send(record(ARC_SERVICE, "card", "127.0.0.1", at.port()))
        .unwrap();
    senders[2]
        .send(record(DBC_SERVICE, "card", "127.0.0.1", 4455))
        .unwrap();
    // The lookup answers once the read is done; it never blocks.
    eventually("the card's channel", || {
        let t = Instant::now();
        let found = network.lookup("card", "spare").is_some();
        assert!(t.elapsed() < Duration::from_millis(100));
        found
    });
    let a = network.lookup("card", "Spare").unwrap();
    assert_eq!(
        a,
        offer(
            &TxChannel {
                id: 3,
                rate: 96_000,
                bits: 24,
                default: "TX 3".into(),
                name: None,
            },
            SocketAddrV4::new(LOOPBACK, 4455),
            FPP
        )
    );
    assert_eq!(network.lookup("card", "TX 16").unwrap().tx_channel_id, 16);
    // One read: the counts, one page of channels, one of names.
    assert_eq!(*got.lock().unwrap(), [0x1000, 0x2000, 0x2010]);
    assert_eq!(
        requests.counts().sent,
        [
            Sent {
                peer: 1,
                op: 0x1000,
                n: 1
            },
            Sent {
                peer: 1,
                op: 0x2000,
                n: 1
            },
            Sent {
                peer: 1,
                op: 0x2010,
                n: 1
            },
        ]
    );
    let c = network.arc.counts();
    assert_eq!((c.reads, c.requests), (1, 3));
    // A name the card does not list asks again, after the wait (9 s).
    assert_eq!(network.lookup("card", "TX 17"), None);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(got.lock().unwrap().len(), 3);
    drop(senders);
    network.arc.join_within(BOUND);
}

#[test]
fn own_records_then_channel_records_then_arc() {
    let cfg = cfg();
    let state = DeviceState::from_config(&cfg);
    let own = Own::new(&state, &cfg, OUR_IP);
    let network = Network::default();
    let both = (own, network);
    assert_eq!(
        both.lookup("test-device", "TX 1").unwrap().control.ip(),
        &OUR_IP
    );
    assert!(!both.1.arc.asked());
    assert_eq!(both.lookup("peer-device", "01"), None);
    assert!(both.1.arc.asked());
    assert!(!both.1.arc.started());
}

#[test]
fn a_dead_device_read_by_the_reader_stays_unresolved_and_is_counted() {
    // A port that never answers: the read waits its reply time once.
    let silent = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    let std::net::SocketAddr::V4(at) = silent.local_addr().unwrap() else {
        panic!("IPv4")
    };
    let (daemon, senders) = FakeDaemon::new(&[ARC_SERVICE, DBC_SERVICE]);
    // The reader's idle wait is longer than the test: the want wakes it.
    let mut arc = ArcDirectory::new(Timing {
        settle_ns: 0,
        max_age_ns: MAX_AGE_NS,
        reply: Duration::from_millis(100),
        idle: Duration::from_secs(30),
        fpp: FPP,
    });
    let requests = Arc::new(Requests::default());
    assert!(arc.start(&daemon, LOOPBACK, Arc::clone(&requests)).unwrap());
    senders[0]
        .send(record(ARC_SERVICE, "card", "127.0.0.1", at.port()))
        .unwrap();
    senders[1]
        .send(record(DBC_SERVICE, "card", "127.0.0.1", 4455))
        .unwrap();
    eventually("the card's records", || {
        lock(&arc.shared.book)
            .records
            .get("card")
            .is_some_and(|r| r.dbc.is_some())
    });
    // One lookup and no other: its want alone wakes the reader.
    assert_eq!(arc.lookup("card", "01"), None);
    let mut buf = [0u8; 64];
    silent.set_read_timeout(Some(BOUND)).unwrap();
    let n = silent.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], crate::arc::client::page_request(1, 0x1000, 1));
    eventually("the read's failure", || arc.counts().no_reply == 1);
    // The next read waits 9 s from the failure: none meanwhile.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(arc.lookup("card", "01"), None);
    assert_eq!(arc.counts().no_reply, 1);
    assert_eq!(
        requests.counts().sent,
        [Sent {
            peer: 1,
            op: 0x1000,
            n: 1
        }]
    );
    drop(senders);
    arc.join_within(BOUND);
}

#[test]
fn dropping_the_directory_ends_its_reader_at_once() {
    let (daemon, senders) = FakeDaemon::new(&[ARC_SERVICE, DBC_SERVICE]);
    let mut arc = ArcDirectory::new(Timing {
        idle: Duration::from_secs(30),
        ..Timing::default()
    });
    assert!(
        arc.start(&daemon, LOOPBACK, Arc::new(Requests::default()))
            .unwrap()
    );
    // The browse threads end with the daemon's events.
    drop(senders);
    let shared = Arc::downgrade(&arc.shared);
    drop(arc);
    // The reader was asked to stop and woken: nothing holds the directory.
    eventually("the reader's end", || shared.upgrade().is_none());
}

/// The stop is set under the book's lock, which the reader holds from its
/// look at the stop to its wait, so a stop never lands in between and is
/// lost (a lost one leaves the reader asleep for its whole wait: a full run
/// of the crate's tests once waited one out, #72). While the test holds
/// the lock, the stop is not set.
#[test]
fn the_stop_is_set_under_the_book_s_lock() {
    let arc = ArcDirectory::new(Timing::default());
    let shared = Arc::clone(&arc.shared);
    let book = lock(&shared.book);
    let asking = {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || ask_stop(&shared))
    };
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !shared.stop.load(Ordering::Relaxed),
        "the stop was set while the book was locked"
    );
    drop(book);
    asking.join().unwrap();
    assert!(shared.stop.load(Ordering::Relaxed));
}
