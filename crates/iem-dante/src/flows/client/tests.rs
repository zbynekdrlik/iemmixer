use super::*;

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use tokio::net::UdpSocket;
use tokio::sync::Mutex;

use crate::device::DeviceConfig;
use crate::flows::control::{FlowRequest, split};
use crate::flows::server::{FlowOutcome, handle};
use crate::media::tx::TxTable;
use crate::wire::{self, Header};

const TEST_DEVICE: &str = include_str!("../../../../../config/test-dante-device.toml");
const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;
/// Every wait in these tests ends: a hung exchange fails, never hangs.
const BOUND: Duration = Duration::from_secs(5);

fn cfg() -> DeviceConfig {
    DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-dante-device.toml")
}

fn req(rx: SocketAddrV4, sample_rate: u32) -> FlowRequest {
    FlowRequest {
        rx_host: "test-device".to_string(),
        rx_flow_name: "1_0".to_string(),
        sample_rate,
        bits: 24,
        channels: vec![Some(1), None],
        fpp: 32,
        rx,
    }
}

fn v4(addr: SocketAddr) -> SocketAddrV4 {
    match addr {
        SocketAddr::V4(a) => a,
        SocketAddr::V6(_) => panic!("an IPv4 socket"),
    }
}

/// A stand-in transmitter on loopback: our own 4455 answers (`handle`) over
/// a table, every datagram it got kept.
struct Transmitter {
    at: SocketAddrV4,
    table: Arc<Mutex<TxTable>>,
    got: Arc<Mutex<Vec<Vec<u8>>>>,
}

async fn transmitter(cfg: DeviceConfig) -> Transmitter {
    let sock = UdpSocket::bind((LOOPBACK, 0)).await.unwrap();
    let at = v4(sock.local_addr().unwrap());
    let table = Arc::new(Mutex::new(TxTable::new(7)));
    let got = Arc::new(Mutex::new(Vec::new()));
    let (t, g) = (Arc::clone(&table), Arc::clone(&got));
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        while let Ok((n, from)) = sock.recv_from(&mut buf).await {
            g.lock().await.push(buf[..n].to_vec());
            let outcome = handle(&mut *t.lock().await, &cfg, v4(from), &buf[..n], 0);
            if let FlowOutcome::Reply(bytes) = outcome {
                sock.send_to(&bytes, from).await.unwrap();
            }
        }
    });
    Transmitter { at, table, got }
}

/// A stand-in transmitter that answers each datagram with what `answer`
/// gives (any number of datagrams, none at all), counting what it got.
async fn scripted(
    answer: impl Fn(&Header) -> Vec<Vec<u8>> + Send + 'static,
) -> (SocketAddrV4, Arc<AtomicUsize>) {
    let sock = UdpSocket::bind((LOOPBACK, 0)).await.unwrap();
    let at = v4(sock.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        while let Ok((n, from)) = sock.recv_from(&mut buf).await {
            c.fetch_add(1, Ordering::SeqCst);
            let (h, _) = Header::parse(&buf[..n]).unwrap();
            for d in answer(&h) {
                sock.send_to(&d, from).await.unwrap();
            }
        }
    });
    (at, count)
}

async fn bounded<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(BOUND, f).await.expect("bounded")
}

#[tokio::test]
async fn our_request_gets_the_transmitter_s_handle_and_our_stop_ends_the_flow() {
    let t = transmitter(cfg()).await;
    let client = Client::new(LOOPBACK);
    let rx = SocketAddrV4::new(LOOPBACK, 40_000);
    let h = bounded(client.request_flow(t.at, &req(rx, 96_000)))
        .await
        .unwrap();
    {
        let table = t.table.lock().await;
        let (slot, flow) = table.get(&h).expect("the table holds our flow");
        assert_eq!(split(&h).0 as usize, slot);
        assert_eq!(flow.dest, rx);
        assert_eq!(flow.channels, [Some(1), None]);
    }
    bounded(client.stop_flow(t.at, &h)).await.unwrap();
    assert_eq!(t.table.lock().await.flows().count(), 0);
    // One datagram each, byte for byte ours, sequence numbers from 1.
    let got = t.got.lock().await.clone();
    assert_eq!(got, [build_request(1, &req(rx, 96_000)), build_stop(2, &h)]);
}

#[tokio::test]
async fn a_refusal_is_the_transmitter_s_status() {
    // Another sample rate: 0x0301.
    let t = transmitter(cfg()).await;
    let client = Client::new(LOOPBACK);
    let rx = SocketAddrV4::new(LOOPBACK, 40_000);
    let err = bounded(client.request_flow(t.at, &req(rx, 48_000)))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClientError::Refused(status::SAMPLE_RATE_MISMATCH)),
        "{err:?}"
    );
    // An unknown handle: 0x0103.
    let err = bounded(client.stop_flow(t.at, &[0, 0, 0, 9, 0, 1]))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClientError::Refused(status::FLOW_NOT_FOUND)),
        "{err:?}"
    );
    assert_eq!(t.table.lock().await.flows().count(), 0);
    assert_eq!(err.to_string(), "refused with status 0x0103");
}

#[tokio::test]
async fn replies_to_another_sequence_or_opcode_and_requests_are_not_the_reply() {
    let (at, count) = scripted(|h| {
        let reply = |seq, op1, op2, body: &[u8]| wire::request(h.start, seq, op1, op2, body);
        vec![
            reply(h.seq.wrapping_add(1), h.op1, status::OK, &[0; 6]),
            reply(h.seq, op::STOP, status::OK, &[0; 6]),
            reply(h.seq, h.op1, 0, &[0; 6]),
            vec![0x11, 0x02, 0x00],
            reply(h.seq, h.op1, status::OK, &[0, 0, 0, 3, 0xab, 0xcd, 0xee]),
        ]
    })
    .await;
    let client = Client::new(LOOPBACK);
    let h = bounded(client.request_flow(at, &req(SocketAddrV4::new(LOOPBACK, 1), 96_000)))
        .await
        .unwrap();
    // The handle is the first 6 bytes of the matching OK's body.
    assert_eq!(h, [0, 0, 0, 3, 0xab, 0xcd]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_ok_without_a_handle_is_no_handle() {
    let (at, _) = scripted(|h| vec![wire::reply(h, status::OK, &[1, 2, 3, 4, 5])]).await;
    let client = Client::new(LOOPBACK);
    let err = bounded(client.request_flow(at, &req(SocketAddrV4::new(LOOPBACK, 1), 96_000)))
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::NoHandle), "{err:?}");
}

#[tokio::test]
async fn a_silent_transmitter_times_out_after_one_datagram() {
    let (at, count) = scripted(|_| Vec::new()).await;
    let client = Client::with_timeouts(
        LOOPBACK,
        Duration::from_millis(300),
        Duration::from_millis(100),
    );
    let started = std::time::Instant::now();
    let err = bounded(client.request_flow(at, &req(SocketAddrV4::new(LOOPBACK, 1), 96_000)))
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Timeout), "{err:?}");
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(300), "{took:?}");
    assert!(took < Duration::from_millis(2_000), "{took:?}");
    let started = std::time::Instant::now();
    let err = bounded(client.stop_flow(at, &[0; 6])).await.unwrap_err();
    assert!(matches!(err, ClientError::Timeout), "{err:?}");
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(100), "{took:?}");
    assert!(took < Duration::from_millis(1_000), "{took:?}");
    // Never asked again: one datagram per exchange.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_stop_to_a_silent_transmitter_ends_within_1_s() {
    let (at, count) = scripted(|_| Vec::new()).await;
    let client = Client::new(LOOPBACK);
    let started = std::time::Instant::now();
    let err = bounded(client.stop_flow(at, &[0, 0, 0, 1, 0, 2]))
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Timeout), "{err:?}");
    assert!(started.elapsed() < Duration::from_millis(1_200));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(STOP_TIMEOUT, Duration::from_secs(1));
    assert_eq!(REPLY_TIMEOUT, Duration::from_secs(3));
}

#[tokio::test]
async fn a_closed_port_is_an_error_at_once_not_a_wait() {
    // A port nobody holds: loopback answers with port unreachable.
    let closed = {
        let s = std::net::UdpSocket::bind((LOOPBACK, 0)).unwrap();
        v4(s.local_addr().unwrap())
    };
    let client = Client::new(LOOPBACK);
    let started = std::time::Instant::now();
    let err = bounded(client.request_flow(closed, &req(SocketAddrV4::new(LOOPBACK, 1), 96_000)))
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Io(_)), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn the_sequence_starts_at_1_and_wraps() {
    let client = Client::new(LOOPBACK);
    assert_eq!(client.next_seq(), 1);
    assert_eq!(client.next_seq(), 2);
    client.seq.store(u16::MAX, Ordering::SeqCst);
    assert_eq!(client.next_seq(), u16::MAX);
    assert_eq!(client.next_seq(), 0);
}

#[test]
fn a_reply_matches_by_sequence_and_opcode_only() {
    let ok = wire::request(0x1102, 5, op::REQUEST, status::OK, &[9; 6]);
    assert_eq!(reply_of(&ok, 5, op::REQUEST), Some(Ok(&[9u8; 6][..])));
    assert_eq!(reply_of(&ok, 6, op::REQUEST), None);
    assert_eq!(reply_of(&ok, 5, op::STOP), None);
    // Any start code, any length field (inferno reads neither).
    let mut odd = wire::request(0x2222, 5, op::REQUEST, status::OK, &[]);
    odd[3] = 99;
    assert_eq!(reply_of(&odd, 5, op::REQUEST), Some(Ok(&[][..])));
    let refused = wire::request(0x1102, 5, op::STOP, status::FLOW_NOT_FOUND, &[]);
    assert_eq!(
        reply_of(&refused, 5, op::STOP),
        Some(Err(status::FLOW_NOT_FOUND))
    );
    // A request (op2 0) is no reply; nor is anything under a header.
    let request = wire::request(0x1102, 5, op::STOP, 0, &[0; 6]);
    assert_eq!(reply_of(&request, 5, op::STOP), None);
    for n in 0..wire::HEADER_LEN {
        assert_eq!(reply_of(&ok[..n], 5, op::REQUEST), None);
    }
}
