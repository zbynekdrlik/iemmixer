use super::*;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::Notify;

use std::net::{Ipv4Addr, SocketAddrV4};

use crate::device::{Device, DeviceConfig, DeviceState, Subscription};
use crate::flows::control::{FlowRequest, handle as flow_handle};
use crate::info::Changes;
use crate::media::tx::TxTable;
use crate::subscribe::resolve::{Advertised, Resolver};
use crate::subscribe::{Answer, Manager};
use crate::wire::tests::Rng;
use crate::wire::{HEADER_LEN, Header, MAX_DATAGRAM, read_u16, request, string_at};

const TEST_DEVICE: &str = include_str!("../../../../config/test-dante-device.toml");

/// The tests' flows: no TX flow, and a manager that keeps the
/// subscriptions and resolves none, so every subscribed channel reads
/// Unresolved (the daemon's are `Running`, `crate::daemon::media`).
#[derive(Debug)]
struct NoFlows {
    tx: TxTable,
    rx: Mutex<Manager>,
}

impl NoFlows {
    /// No flows for `device`, its saved subscriptions known.
    fn new(device: &Device) -> Self {
        let mut rx = Manager::new(
            &device.cfg,
            &device.state.name,
            Ipv4Addr::UNSPECIFIED,
            [0; crate::subscribe::MAX_FLOWS],
        );
        rx.load(&device.state.subscriptions);
        rx.changed();
        NoFlows {
            tx: TxTable::new(0),
            rx: Mutex::new(rx),
        }
    }
}

impl Flows for NoFlows {
    fn view<R>(&self, f: impl FnOnce(&Live<'_>) -> R) -> R {
        let rx = self.rx.lock().unwrap_or_else(PoisonError::into_inner);
        f(&Live {
            tx: &self.tx,
            rx: &rx,
        })
    }

    fn subscribe(&self, subs: &[Option<Subscription>]) -> Vec<usize> {
        let mut rx = self.rx.lock().unwrap_or_else(PoisonError::into_inner);
        rx.load(subs);
        rx.changed()
    }
}

/// netaudio's start code; Dante Controller's is 0x2729.
const NETAUDIO: u16 = 0x27ff;
const CONTROLLER: u16 = 0x2729;

fn config() -> DeviceConfig {
    DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-dante-device.toml")
}

/// The test device with `rx` and `tx` channels named `R<n>` and `T<n>`.
fn config_sized(rx: usize, tx: usize) -> DeviceConfig {
    DeviceConfig {
        rx: (1..=rx).map(|n| format!("R{n}")).collect(),
        tx: (1..=tx).map(|n| format!("T{n}")).collect(),
        ..config()
    }
}

fn config_in(dir: &Path) -> DeviceConfig {
    DeviceConfig {
        state_file: dir.join("state.toml"),
        ..config()
    }
}

/// Synthetic addresses (P6): ours, a peer's.
const OUR_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 10);
const PEER_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 11);

/// Our flow slots' ports in the tests.
fn ports() -> [u16; crate::subscribe::MAX_FLOWS] {
    std::array::from_fn(|slot| 50_000 + slot as u16)
}

/// A manager of `cfg` with no subscription.
fn manager_of(cfg: &DeviceConfig) -> Manager {
    Manager::new(cfg, &cfg.name, OUR_IP, ports())
}

/// The device as `handle` sees it: its config and state, and the running
/// flows (none until a test adds some).
struct Dev {
    cfg: DeviceConfig,
    state: DeviceState,
    tx: TxTable,
    rx: Manager,
}

/// A reply, decoded: its header, its body, whether the state changed.
struct Reply {
    header: Header,
    body: Vec<u8>,
    changed: bool,
}

impl Dev {
    fn new(cfg: DeviceConfig) -> Self {
        let state = DeviceState::from_config(&cfg);
        let rx = manager_of(&cfg);
        Self {
            cfg,
            state,
            tx: TxTable::new(7),
            rx,
        }
    }

    fn send(&mut self, packet: &[u8]) -> ArcOutcome {
        let live = Live {
            tx: &self.tx,
            rx: &self.rx,
        };
        handle(&mut self.state, &self.cfg, &live, packet)
    }

    /// netaudio's request `op1` with `body`, answered.
    fn ask(&mut self, seq: u16, op1: u16, body: &[u8]) -> Reply {
        decode(self.send(&request(NETAUDIO, seq, op1, 0, body)))
    }
}

fn decode(outcome: ArcOutcome) -> Reply {
    let (bytes, changed) = match outcome {
        ArcOutcome::Reply(bytes) => (bytes, false),
        ArcOutcome::Changed(bytes, _) => (bytes, true),
        ArcOutcome::Ignored | ArcOutcome::Misframed { .. } | ArcOutcome::Unanswered { .. } => {
            panic!("a well-formed request was not answered: {outcome:?}")
        }
    };
    let (header, body) = Header::parse(&bytes).expect("a reply parses");
    Reply {
        header,
        body: body.to_vec(),
        changed,
    }
}

/// [`respond`] for a device without flows.
fn respond_idle(device: &mut Device, req: &[u8]) -> ArcOutcome {
    let tx = TxTable::new(7);
    let rx = manager_of(&device.cfg);
    respond(device, &Live { tx: &tx, rx: &rx }, req)
}

/// netaudio's page request: `0001 <first channel> 0000`.
fn page_request(start: u16) -> Vec<u8> {
    let [hi, lo] = start.to_be_bytes();
    vec![0, 1, hi, lo, 0, 0]
}

/// netaudio's TX rename: `0201 0000 00<ch> 0018 000000000000 <name> 00`.
fn tx_rename(channel: u8, name: &[u8]) -> Vec<u8> {
    let mut body = vec![0x02, 0x01, 0, 0, 0, channel, 0x00, 0x18, 0, 0, 0, 0, 0, 0];
    body.extend_from_slice(name);
    body.push(0);
    body
}

/// netaudio's RX rename: `0201 00<ch> 0014 00000000 <name> 00`.
fn rx_rename(channel: u8, name: &[u8]) -> Vec<u8> {
    let mut body = vec![0x02, 0x01, 0, channel, 0x00, 0x14, 0, 0, 0, 0];
    body.extend_from_slice(name);
    body.push(0);
    body
}

/// Several TX renames in one request: the items, then their names.
fn tx_renames(items: &[(u16, &str)]) -> Vec<u8> {
    let table = 2 + items.len() * 6;
    let mut body = vec![0x02, items.len() as u8];
    let mut names = Vec::new();
    for (id, name) in items {
        let offset = (HEADER_LEN + table + names.len()) as u16;
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&id.to_be_bytes());
        body.extend_from_slice(&offset.to_be_bytes());
        names.extend_from_slice(name.as_bytes());
        names.push(0);
    }
    body.extend(names);
    body
}

/// The body of Dante Controller's 0x2200 and 0x2204, captured (#72,
/// 2026-10-26): a page request from item 1.
const DC_FIRST_PAGE: [u8; 4] = [0x00, 0x01, 0x00, 0x01];

/// Dante Controller's 0x1100, the body inferno's source quotes.
const DC_1100: [u8; 40] = [
    0x00, 0x13, 0x02, 0x01, 0x82, 0x04, 0x82, 0x05, 0x02, 0x10, 0x02, 0x11, 0x82, 0x18, 0x82, 0x19,
    0x83, 0x01, 0x83, 0x02, 0x83, 0x06, 0x03, 0x10, 0x03, 0x11, 0x03, 0x03, 0x80, 0x21, 0x00, 0xf0,
    0x80, 0x60, 0x00, 0x22, 0x00, 0x63, 0x00, 0x64,
];

/// netaudio's add_subscription: `0201 00<rx> 0034 00<device offset>`, 34
/// zero bytes, then the TX channel's and the TX device's names.
fn subscribe(rx: u8, tx_channel: &str, tx_device: &str) -> Vec<u8> {
    let device_at = 52 + tx_channel.len() as u8 + 1;
    let mut body = vec![0x02, 0x01, 0, rx, 0, 52, 0, device_at];
    body.extend([0; 34]);
    for name in [tx_channel, tx_device] {
        body.extend_from_slice(name.as_bytes());
        body.push(0);
    }
    body
}

/// netaudio's remove_subscription: `0001 0000 00<rx>`.
fn unsubscribe(rx: u8) -> Vec<u8> {
    vec![0, 1, 0, 0, 0, rx]
}

/// A 0x3010 of several items, as inferno reads it: `02 <n>`, the items
/// (RX channel, TX channel name offset, TX device name offset; `None`
/// names are offsets 0, Dante Controller's unsubscribe), then each name
/// once.
fn subscriptions(items: &[(u16, Option<(&str, &str)>)]) -> Vec<u8> {
    let mut body = vec![0x02, items.len() as u8];
    let table = HEADER_LEN + 2 + 6 * items.len();
    let mut strings: Vec<u8> = Vec::new();
    let mut at = |name: &str| -> u16 {
        let offset = (table + strings.len()) as u16;
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
        offset
    };
    for (rx, names) in items {
        let (c, d) = match names {
            Some((channel, device)) => (at(channel), at(device)),
            None => (0, 0),
        };
        for field in [*rx, c, d] {
            body.extend(field.to_be_bytes());
        }
    }
    body.extend(strings);
    body
}

fn sub(tx_channel: &str, tx_device: &str) -> Subscription {
    Subscription {
        tx_channel: tx_channel.to_string(),
        tx_device: tx_device.to_string(),
    }
}

/// A subscription's change.
fn subs_changed(rx: &[usize]) -> Change {
    Change {
        renamed_rx: Vec::new(),
        subscriptions: rx.to_vec(),
    }
}

/// The whole reply datagram, whether the state changed or not.
fn sent_bytes(outcome: ArcOutcome) -> Vec<u8> {
    match outcome {
        ArcOutcome::Reply(bytes) | ArcOutcome::Changed(bytes, _) => bytes,
        other => panic!("not answered: {other:?}"),
    }
}

/// The channel records a test's network holds: (device, channel) → record.
struct Net(Vec<(String, String, Advertised)>);

impl Resolver for Net {
    fn lookup(&self, tx_device: &str, tx_channel: &str) -> Option<Advertised> {
        self.0
            .iter()
            .find(|(d, c, _)| d == tx_device && c == tx_channel)
            .map(|(_, _, a)| *a)
    }
}

/// A transmitter's channel record at `ip`: channel `id`, 4 slots a flow,
/// 24 bits at 96 kHz, 4–16 frames, a 2 ms latency.
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

/// `m` with `subs` set, every flow it asks for granted, and audio heard on
/// every slot: the channels read Receiving.
fn receiving(m: &mut Manager, subs: &[(usize, Subscription)], net: &Net) {
    for (rx, s) in subs {
        assert!(m.set(*rx, Some(s.clone())));
    }
    // A request a transmitter per 9 s: four rounds, each granted at once,
    // every slot hearing packets at every round.
    let mut now = 0;
    for k in 0..4u64 {
        now = k * 9_000_000_000;
        for action in m.tick(now, &[k; 32], net) {
            if let crate::subscribe::Action::Request { flow, .. } = action {
                let granted = flow_handle(flow.slot as u32, 0x100 + flow.slot as u16);
                m.answered(flow, Answer::Granted(granted), now);
            }
        }
    }
    m.tick(now + 100_000_000, &[99; 32], net);
}

/// A receiver's flow request for our TX channels `channels`, to `rx`.
fn tx_request(
    rx_host: &str,
    rx_flow_name: &str,
    channels: &[Option<u16>],
    rx: SocketAddrV4,
) -> FlowRequest {
    FlowRequest {
        rx_host: rx_host.to_string(),
        rx_flow_name: rx_flow_name.to_string(),
        sample_rate: 96_000,
        bits: 24,
        channels: channels.to_vec(),
        fpp: 16,
        rx,
    }
}

/// A flow list page: its slots, its flows' descriptor offsets, its status.
fn flow_page(answer: &Reply) -> (usize, Vec<u16>, u16) {
    let body = &answer.body;
    let slots = usize::from(body[0]);
    let count = usize::from(body[1]);
    let items = (0..count)
        .map(|k| read_u16(body, 2 + 2 * k).unwrap())
        .collect();
    assert!(HEADER_LEN + body.len() <= MAX_DATAGRAM);
    (slots, items, answer.header.op2)
}

/// A multicast TX flow to create, as inferno parses it: `[?, 1, descriptor
/// offset]`, the descriptor (flow id, type 2, 10 bytes, channel count), the
/// channels, the offset of the mostly-zero block, the block.
fn multicast_create(flow: u16, channels: &[u16]) -> Vec<u8> {
    let mut body = vec![0x00, 0x01];
    body.extend(((HEADER_LEN + 4) as u16).to_be_bytes());
    body.extend(flow.to_be_bytes());
    body.extend(2u16.to_be_bytes());
    body.extend([0; 10]);
    body.extend((channels.len() as u16).to_be_bytes());
    for channel in channels {
        body.extend(channel.to_be_bytes());
    }
    body.extend(((HEADER_LEN + body.len() + 2) as u16).to_be_bytes());
    body.extend(0x0a00u16.to_be_bytes());
    body.extend([0; 14]);
    body.extend([0, 1, 0, 0]);
    body
}

/// A multicast TX flow to delete, as inferno reads it: `[count, ?, flow
/// ids]`.
fn multicast_delete(flow: u16) -> Vec<u8> {
    let mut body = vec![0, 1, 0, 0];
    body.extend(flow.to_be_bytes());
    body
}

/// The whole reply datagram to `packet`.
fn reply_bytes(dev: &mut Dev, packet: &[u8]) -> Vec<u8> {
    match dev.send(packet) {
        ArcOutcome::Reply(bytes) => bytes,
        other => panic!("not a plain reply: {other:?}"),
    }
}

/// One decoded page of a channel list: its slots, its channels (number and
/// name), for an RX page their subscriptions (TX channel, TX device,
/// status code), and its status.
struct Page {
    slots: usize,
    channels: Vec<(u16, String)>,
    subs: Vec<Option<(String, String, u32)>>,
    status: u16,
}

/// The 16-byte descriptor every channel of a page points at.
fn check_common(cfg: &DeviceConfig, body: &[u8], offset: u16) {
    let at = usize::from(offset) - HEADER_LEN;
    let bits = u16::from(cfg.encoding).to_be_bytes();
    let mut want = cfg.sample_rate.to_be_bytes().to_vec();
    want.extend_from_slice(&[1, 1, bits[0], bits[1], 0x04, 0x00]);
    want.extend_from_slice(&[bits[0], bits[1], bits[0], bits[1], 0x00, 0x0e]);
    assert_eq!(body[at..at + 16], want);
}

/// Decodes a 0x2000, 0x2010 or 0x3000 page, checking every fixed field.
fn decode_page(cfg: &DeviceConfig, op1: u16, answer: &Reply) -> Page {
    let body = &answer.body;
    let item = match op1 {
        op::TX_CHANNELS => 8,
        op::TX_NAMES => 6,
        op::RX_CHANNELS => 20,
        _ => unreachable!(),
    };
    let slots = usize::from(body[0]);
    let count = usize::from(body[1]);
    assert!(count <= slots);
    // A page the soft limit ended keeps the descriptor of the item that
    // reached it in the next slot, not counted (inferno).
    let cut = answer.header.op2 == STATUS_MORE
        && count < slots
        && body[2 + count * item..2 + (count + 1) * item]
            .iter()
            .any(|&b| b != 0);
    if cut {
        assert!(HEADER_LEN + body.len() >= SOFT_LIMIT);
    }
    let mut channels = Vec::new();
    let mut subs = Vec::new();
    let mut common = None;
    for k in 0..count + usize::from(cut) {
        let at = 2 + k * item;
        let id = read_u16(body, at).unwrap();
        let name_offset = match op1 {
            op::TX_CHANNELS => {
                assert_eq!(read_u16(body, at + 2), Some(7));
                let c = read_u16(body, at + 4).unwrap();
                assert_eq!(*common.get_or_insert(c), c, "one descriptor per page");
                read_u16(body, at + 6).unwrap()
            }
            op::TX_NAMES => {
                assert_eq!(read_u16(body, at + 2), Some(id));
                read_u16(body, at + 4).unwrap()
            }
            _ => {
                assert_eq!(read_u16(body, at + 2), Some(6));
                let c = read_u16(body, at + 4).unwrap();
                assert_eq!(*common.get_or_insert(c), c, "one descriptor per page");
                let tx_channel = read_u16(body, at + 6).unwrap();
                let tx_device = read_u16(body, at + 8).unwrap();
                let status = u32::from_be_bytes(body[at + 12..at + 16].try_into().unwrap());
                assert_eq!(body[at + 16..at + 20], [0; 4]);
                if k < count {
                    subs.push(if tx_channel == 0 {
                        // No subscription: no TX channel, no TX device, status 0.
                        assert_eq!((tx_device, status), (0, 0));
                        None
                    } else {
                        Some((
                            string_at(body, tx_channel).unwrap().to_string(),
                            string_at(body, tx_device).unwrap().to_string(),
                            status,
                        ))
                    });
                }
                read_u16(body, at + 10).unwrap()
            }
        };
        let name = string_at(body, name_offset).expect("a name at its offset");
        if k < count {
            channels.push((id, name.to_string()));
        } else {
            // The uncounted item is the next channel, whole.
            let last = channels.last().map_or(0, |(id, _)| *id);
            assert_eq!(id, last + 1);
        }
    }
    // Unused slots are zero.
    let used = count + usize::from(cut);
    assert!(
        body[2 + used * item..2 + slots * item]
            .iter()
            .all(|&b| b == 0)
    );
    if let Some(c) = common {
        check_common(cfg, body, c);
    }
    if op1 == op::TX_NAMES && count > 0 {
        // A zero u32 opens the strings, as inferno writes it.
        let strings = 2 + slots * item;
        assert_eq!(body[strings..strings + 4], [0, 0, 0, 0]);
    }
    assert!(HEADER_LEN + body.len() <= MAX_DATAGRAM);
    Page {
        slots,
        channels,
        subs,
        status: answer.header.op2,
    }
}

/// Reads a whole list as Dante Controller does: from channel 1, each next
/// page from the channel after the last one listed, while the status says
/// more follow. Every page, decoded.
fn read_pages(dev: &mut Dev, op1: u16) -> Vec<Page> {
    let mut start = 1u16;
    let mut out = Vec::new();
    for _ in 0..100 {
        let answer = dev.ask(start, op1, &page_request(start));
        let page = decode_page(&dev.cfg, op1, &answer);
        let n = page.channels.len() as u16;
        let status = page.status;
        out.push(page);
        if status == STATUS_OK {
            return out;
        }
        assert_eq!(status, STATUS_MORE);
        assert!(n > 0, "a page that says more must hold a channel");
        start += n;
    }
    panic!("the list never ended");
}

/// A whole list's channels (number and name), read as Dante Controller
/// reads it.
fn read_list(dev: &mut Dev, op1: u16) -> Vec<(u16, String)> {
    read_pages(dev, op1)
        .into_iter()
        .flat_map(|p| p.channels)
        .collect()
}

/// The whole RX list's subscriptions, read as Dante Controller reads it.
fn read_subs(dev: &mut Dev) -> Vec<Option<(String, String, u32)>> {
    read_pages(dev, op::RX_CHANNELS)
        .into_iter()
        .flat_map(|p| p.subs)
        .collect()
}

fn numbered(names: &[String]) -> Vec<(u16, String)> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| (i as u16 + 1, n.clone()))
        .collect()
}

#[test]
fn counts_name_32_rx_and_16_tx() {
    let mut dev = Dev::new(config());
    let answer = dev.ask(5, op::COUNTS, &[]);
    assert_eq!(answer.header.op2, STATUS_OK);
    assert!(!answer.changed);
    #[rustfmt::skip]
    let want = [
        0x00, 0x10,             // flags: TX channels can be renamed; no multicast (inferno: 0x30)
        0x00, 0x10, 0x00, 0x20, // 16 TX, 32 RX
        0x00, 0x04, 0x00, 0x08, // 4 (inferno), at most 8 channels in a flow
        0x00, 0x08,             // 8 (inferno)
        0x00, 0x20, 0x00, 0x20, // inferno's 32 TX and 32 RX flows
        0x00, 0x30,             // 48 channels in all
        0x00, 0x01, 0x00, 0x01, // 1, 1 (inferno)
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    assert_eq!(answer.body, want);
    // netaudio reads the counts' low bytes at packet offsets 13 (TX) and 15 (RX).
    let packet = request(NETAUDIO, 5, op::COUNTS, STATUS_OK, &answer.body);
    assert_eq!((packet[13], packet[15]), (16, 32));
    // Fewer TX channels than a flow holds: the flow size follows them.
    let mut small = Dev::new(config_sized(2, 3));
    let body = small.ask(1, op::COUNTS, &[]).body;
    assert_eq!(body[2..10], [0, 3, 0, 2, 0, 4, 0, 3]);
    assert_eq!(body[16..18], [0, 5]);
}

#[test]
fn the_device_names_are_the_states_and_the_configs() {
    let mut dev = Dev::new(config());
    dev.state.name = "renamed-device".into();
    let answer = dev.ask(1, op::DEVICE_NAME, &[]);
    assert_eq!(answer.header.op2, STATUS_OK);
    assert_eq!(answer.body, b"renamed-device\0");

    let answer = dev.ask(2, op::DEVICE_NAMES, &[]);
    assert_eq!(answer.header.op2, STATUS_OK);
    let body = &answer.body;
    let field = |i: usize| read_u16(body, 2 * i).unwrap();
    let text = |i: usize| string_at(body, field(i)).unwrap();
    assert_eq!(text(6), "renamed-device", "the name now");
    assert_eq!(text(8), "renamed-device", "the name now, again");
    assert_eq!(text(7), "test-device", "the factory name");
    assert_eq!(text(3), "iem-dante", "the board name");
    assert_eq!(text(4), ":705", "the revision");
    // The strings follow the 38-byte header: name, factory name, board, revision.
    assert_eq!(field(6), 48);
    assert_eq!(
        body[38..],
        *b"renamed-device\0test-device\0iem-dante\0:705\0"
    );
    for (i, want) in [
        (0, 0),
        (1, 0),
        (2, 0),
        (5, 0),
        (15, 0x2729),
        (16, 0),
        (17, 0x1102),
        (18, 0),
    ] {
        assert_eq!(field(i), want, "field {i}");
    }
    for i in 9..15 {
        assert_eq!(field(i), 0, "field {i}");
    }
}

#[test]
fn the_tx_list_pages_and_names_every_channel_once() {
    // The test device's 16 TX channels fit one page of 32.
    let mut dev = Dev::new(config());
    let answer = dev.ask(1, op::TX_CHANNELS, &page_request(1));
    let page = decode_page(&dev.cfg, op::TX_CHANNELS, &answer);
    assert_eq!((page.slots, page.status), (16, STATUS_OK));
    assert_eq!(page.channels, numbered(&dev.cfg.tx));

    // 70 TX channels: pages of 32, 32 and 6, each channel once and in order,
    // read by count (Dante Controller) or at netaudio's fixed starts 1, 33, 65.
    let mut dev = Dev::new(config_sized(32, 70));
    for op1 in [op::TX_CHANNELS, op::TX_NAMES] {
        assert_eq!(read_list(&mut dev, op1), numbered(&dev.cfg.tx));
        let mut seen = Vec::new();
        for (start, count, status) in [
            (1, 32, STATUS_MORE),
            (33, 32, STATUS_MORE),
            (65, 6, STATUS_OK),
        ] {
            let answer = dev.ask(start, op1, &page_request(start));
            let page = decode_page(&dev.cfg, op1, &answer);
            assert_eq!(
                (page.slots, page.channels.len(), page.status),
                (32, count, status)
            );
            seen.extend(page.channels);
        }
        assert_eq!(seen, numbered(&dev.cfg.tx));
        // A start past the list holds nothing and ends it.
        let answer = dev.ask(9, op1, &page_request(71));
        let page = decode_page(&dev.cfg, op1, &answer);
        assert_eq!((page.channels.len(), page.status), (0, STATUS_OK));
        // So does a page that ends exactly at the last channel.
        let answer = dev.ask(9, op1, &page_request(39));
        let page = decode_page(&dev.cfg, op1, &answer);
        assert_eq!((page.channels.len(), page.status), (32, STATUS_OK));
    }

    // The friendly names are the state's; the channel list keeps the defaults.
    let mut dev = Dev::new(config());
    dev.state.tx[2] = "Mix 3".into();
    let names = read_list(&mut dev, op::TX_NAMES);
    assert_eq!(names[2], (3, "Mix 3".to_string()));
    assert_eq!(names[3], (4, "TX 4".to_string()));
    assert_eq!(
        read_list(&mut dev, op::TX_CHANNELS)[2],
        (3, "TX 3".to_string())
    );
}

#[test]
fn the_rx_list_pages_by_16_and_names_every_channel_once() {
    let mut dev = Dev::new(config());
    dev.state.rx[20] = "Return 21".into();
    let mut want = numbered(&dev.cfg.rx);
    want[20].1 = "Return 21".into();
    assert_eq!(read_list(&mut dev, op::RX_CHANNELS), want);
    // netaudio's read: pages 0 and 1 (starts 1 and 17), 16 channels each, at
    // the fixed packet offsets 12 + 20 k, without looking at the count.
    let mut seen = Vec::new();
    for (start, status) in [(1u16, STATUS_MORE), (17, STATUS_OK)] {
        let answer = dev.ask(start, op::RX_CHANNELS, &page_request(start));
        assert_eq!(answer.header.op2, status);
        assert_eq!(answer.body[..2], [16, 16]);
        let packet = request(NETAUDIO, 0, op::RX_CHANNELS, status, &answer.body);
        for k in 0..16 {
            let at = 12 + k * 20;
            let id = u16::from_be_bytes([packet[at], packet[at + 1]]);
            let offset = u16::from_be_bytes([packet[at + 10], packet[at + 11]]);
            seen.push((id, string_at(&answer.body, offset).unwrap().to_string()));
        }
    }
    assert_eq!(seen, want);
}

#[test]
fn netaudio_reads_the_test_devices_tx_list_in_one_page() {
    let mut dev = Dev::new(config());
    dev.state.tx[0] = "Mix 1".into();
    // netaudio asks 0x2010 then 0x2000 at page 0 only (16 / 16 = 1) and
    // reads min(16, 32) channels at packet offsets 12 + 6 k and 12 + 8 k.
    let friendly = dev.ask(1, op::TX_NAMES, &page_request(1));
    let channels = dev.ask(2, op::TX_CHANNELS, &page_request(1));
    for k in 0..16 {
        let at = 2 + k * 6;
        assert_eq!(read_u16(&friendly.body, at + 2), Some(k as u16 + 1));
        let name = string_at(&friendly.body, read_u16(&friendly.body, at + 4).unwrap());
        assert_eq!(name, Some(dev.state.tx[k].as_str()));
        let at = 2 + k * 8;
        assert_eq!(read_u16(&channels.body, at), Some(k as u16 + 1));
        let name = string_at(&channels.body, read_u16(&channels.body, at + 6).unwrap());
        assert_eq!(name, Some(dev.cfg.tx[k].as_str()));
    }
}

#[test]
fn the_longest_names_still_fit_one_datagram_per_page() {
    let long = |prefix: char, n: usize| format!("{prefix}{n:02}{}", "x".repeat(28));
    let mut dev = Dev::new(DeviceConfig {
        rx: (1..=40).map(|n| long('R', n)).collect(),
        tx: (1..=70).map(|n| long('T', n)).collect(),
        ..config()
    });
    assert_eq!(dev.cfg.tx[0].len(), 31);
    for op1 in [op::TX_CHANNELS, op::TX_NAMES, op::RX_CHANNELS] {
        let names = read_list(&mut dev, op1);
        let want = if op1 == op::RX_CHANNELS {
            &dev.cfg.rx
        } else {
            &dev.cfg.tx
        };
        assert_eq!(names, numbered(want));
    }
}

#[test]
fn a_page_request_without_a_first_channel_is_refused() {
    let mut dev = Dev::new(config());
    for op1 in [op::TX_CHANNELS, op::TX_NAMES, op::RX_CHANNELS] {
        for body in [&[][..], &[0, 1, 0], &[0, 1, 0, 0, 0, 0]] {
            let answer = dev.ask(3, op1, body);
            assert_eq!(answer.header.op2, STATUS_ERROR, "{op1:#06x} {body:?}");
            assert!(answer.body.is_empty());
        }
        // Four bytes are enough (the RX list's first page says more follow).
        let answer = dev.ask(3, op1, &[0, 1, 0, 1]);
        let want = if op1 == op::RX_CHANNELS {
            STATUS_MORE
        } else {
            STATUS_OK
        };
        assert_eq!(answer.header.op2, want, "{op1:#06x}");
        // 16 channels: all 16 TX, or the first page of the 32 RX.
        assert_eq!(answer.body[1], 16, "{op1:#06x}");
    }
}

#[test]
fn a_tx_rename_changes_the_state_and_says_so() {
    let mut dev = Dev::new(config());
    let answer = dev.ask(42, op::TX_RENAME, &tx_rename(3, b"Mix 3"));
    assert!(answer.changed);
    assert_eq!(answer.header.seq, 42);
    assert_eq!(answer.header.op2, STATUS_OK);
    assert_eq!(answer.body, [0, 0]);
    assert_eq!(dev.state.tx[2], "Mix 3");
    assert_eq!(dev.state.tx[1], "TX 2");
    assert_eq!(read_list(&mut dev, op::TX_NAMES)[2].1, "Mix 3");

    // The same name again changes nothing, and says OK.
    let answer = dev.ask(43, op::TX_RENAME, &tx_rename(3, b"Mix 3"));
    assert!(!answer.changed);
    assert_eq!(
        (answer.header.op2, answer.body.as_slice()),
        (STATUS_OK, &[0u8, 0][..])
    );

    // An empty name is Dante's reset: the channel's default name again.
    let answer = dev.ask(44, op::TX_RENAME, &tx_rename(3, b""));
    assert!(answer.changed);
    assert_eq!(answer.header.op2, STATUS_OK);
    assert_eq!(dev.state.tx[2], "TX 3");
    // netaudio's reset_channel_name: an empty name and one more zero byte.
    dev.state.tx[2] = "Mix 3".into();
    let mut reset = tx_rename(3, b"");
    reset.push(0);
    assert!(dev.ask(45, op::TX_RENAME, &reset).changed);
    assert_eq!(dev.state.tx[2], "TX 3");

    // Several channels at once; a channel may take its own default back.
    let answer = dev.ask(
        46,
        op::TX_RENAME,
        &tx_renames(&[(1, "Mix 1"), (16, "Mix 16")]),
    );
    assert!(answer.changed);
    assert_eq!(
        (dev.state.tx[0].as_str(), dev.state.tx[15].as_str()),
        ("Mix 1", "Mix 16")
    );
    assert!(
        dev.ask(47, op::TX_RENAME, &tx_renames(&[(1, "TX 1")]))
            .changed
    );
    assert_eq!(dev.state.tx[0], "TX 1");
    // Two channels may swap names in one request.
    let answer = dev.ask(
        48,
        op::TX_RENAME,
        &tx_renames(&[(5, "Swap B"), (6, "Swap A")]),
    );
    assert!(answer.changed);
    let answer = dev.ask(
        49,
        op::TX_RENAME,
        &tx_renames(&[(5, "Swap A"), (6, "Swap B")]),
    );
    assert!(answer.changed);
    assert_eq!(
        (dev.state.tx[4].as_str(), dev.state.tx[5].as_str()),
        ("Swap A", "Swap B")
    );
}

#[test]
fn an_rx_rename_changes_the_state_and_says_so() {
    let mut dev = Dev::new(config());
    let answer = dev.ask(7, op::RX_RENAME, &rx_rename(32, b"Return 32"));
    assert!(answer.changed);
    assert_eq!(answer.header.op2, STATUS_OK);
    assert!(answer.body.is_empty());
    assert_eq!(dev.state.rx[31], "Return 32");
    assert_eq!(dev.state.tx, dev.cfg.tx);
    assert_eq!(read_list(&mut dev, op::RX_CHANNELS)[31].1, "Return 32");
    // An RX name may equal a TX name: they never share an mDNS record.
    assert!(dev.ask(8, op::RX_RENAME, &rx_rename(1, b"TX 1")).changed);
    // netaudio's reset: the default again.
    assert!(dev.ask(9, op::RX_RENAME, &rx_rename(32, b"")).changed);
    assert_eq!(dev.state.rx[31], "RX 32");
    let answer = dev.ask(10, op::RX_RENAME, &rx_rename(32, b""));
    assert!(!answer.changed);
    assert_eq!(answer.header.op2, STATUS_OK);
    // RX channels have no mDNS record, so a name that is another channel's
    // default (but not its name now) is free.
    assert!(dev.ask(11, op::RX_RENAME, &rx_rename(2, b"In 2")).changed);
    assert!(dev.ask(12, op::RX_RENAME, &rx_rename(1, b"RX 2")).changed);
    assert_eq!(dev.state.rx[0], "RX 2");
}

#[test]
fn a_tx_name_may_not_be_another_channels_default() {
    let mut dev = Dev::new(config());
    // Channel 4's default name stays its mDNS name after a rename, so
    // channel 3 cannot take it.
    assert!(dev.ask(1, op::TX_RENAME, &tx_rename(4, b"Mix 4")).changed);
    let answer = dev.ask(2, op::TX_RENAME, &tx_rename(3, b"TX 4"));
    assert!(!answer.changed);
    assert_eq!(answer.header.op2, STATUS_ERROR);
    assert_eq!(dev.state.tx[2], "TX 3");
    // Nor channel 4's name now.
    let answer = dev.ask(3, op::TX_RENAME, &tx_rename(3, b"mix 4"));
    assert_eq!(answer.header.op2, STATUS_ERROR);
    // A rename whose empty name lies in its own item table resets the
    // channel: all the request needs is its items.
    dev.state.tx[2] = "Mix 3".into();
    let answer = dev.ask(
        4,
        op::TX_RENAME,
        &[0x02, 0x01, 0x00, 0x00, 0x00, 0x03, 0x00, 0x0c],
    );
    assert!(answer.changed);
    assert_eq!(dev.state.tx[2], "TX 3");
}

#[test]
fn an_invalid_rename_keeps_the_name() {
    let thirty_two = "x".repeat(32);
    let mut bodies: Vec<(&str, Vec<u8>)> = vec![
        ("too long", tx_rename(3, thirty_two.as_bytes())),
        ("an @", tx_rename(3, b"Mix@3")),
        ("an =", tx_rename(3, b"Mix=3")),
        ("a .", tx_rename(3, b"Mix.3")),
        ("a tab", tx_rename(3, b"Mix\t3")),
        ("not ASCII", tx_rename(3, "Mix \u{e9}".as_bytes())),
        ("not UTF-8", tx_rename(3, b"Mix \xff")),
        ("another channel's name", tx_rename(3, b"TX 4")),
        ("another channel's name, case aside", tx_rename(3, b"tx 4")),
        ("channel 0", tx_rename(0, b"Mix 0")),
        ("channel 17 of 16", tx_rename(17, b"Mix 17")),
        (
            "one bad item spoils the request",
            tx_renames(&[(1, "Fine 1"), (2, "Bad.2")]),
        ),
        (
            "two channels, one name",
            tx_renames(&[(1, "Same"), (2, "Same")]),
        ),
        ("no item", vec![0x02, 0x00]),
        ("no count", vec![0x02]),
        ("an empty body", vec![]),
        (
            "a cut item table",
            vec![0x02, 0x02, 0, 0, 0, 1, 0, 0x18, 0, 0],
        ),
    ];
    // A name offset inside the header, and one past the datagram.
    let mut low = tx_rename(3, b"Mix 3");
    low[6..8].copy_from_slice(&5u16.to_be_bytes());
    bodies.push(("an offset in the header", low));
    let mut high = tx_rename(3, b"Mix 3");
    high[6..8].copy_from_slice(&200u16.to_be_bytes());
    bodies.push(("an offset past the datagram", high));
    for (why, body) in &bodies {
        let mut dev = Dev::new(config());
        dev.state.tx[2] = "Old 3".into();
        let before = dev.state.clone();
        let answer = dev.ask(9, op::TX_RENAME, body);
        assert!(!answer.changed, "{why}");
        assert_eq!(answer.header.op2, STATUS_ERROR, "{why}");
        assert!(answer.body.is_empty(), "{why}");
        assert_eq!(dev.state, before, "{why}");
    }
    let rx_bodies: [(&str, Vec<u8>); 4] = [
        ("too long", rx_rename(1, thirty_two.as_bytes())),
        ("an @", rx_rename(1, b"In@1")),
        ("another channel's name", rx_rename(1, b"RX 2")),
        ("channel 33 of 32", rx_rename(33, b"In 33")),
    ];
    for (why, body) in &rx_bodies {
        let mut dev = Dev::new(config());
        let answer = dev.ask(9, op::RX_RENAME, body);
        assert!(!answer.changed, "{why}");
        assert_eq!(answer.header.op2, STATUS_ERROR, "{why}");
        assert_eq!(dev.state, DeviceState::from_config(&dev.cfg), "{why}");
    }
}

/// A log sink for [`warnings_of`].
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

/// What `f` logs at WARN or above on this thread, as text.
fn warnings_of(f: impl FnOnce()) -> String {
    let sink = Sink::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let bytes = sink.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn a_refused_rename_is_counted_never_a_warning_of_its_own() {
    // Dante Controller asks again at once what it gets refused: a warning
    // per request would be a flood. Every refusal is a gap the server's
    // once-a-minute warning counts (peer, opcode, refused), and no more.
    let tx: [Vec<u8>; 4] = [
        tx_rename(0, b"Mix 0"),
        tx_rename(3, b"Mix.3"),
        tx_rename(3, b"TX 4"),
        {
            let mut high = tx_rename(3, b"Mix 3");
            high[6..8].copy_from_slice(&200u16.to_be_bytes());
            high
        },
    ];
    let rx: [Vec<u8>; 3] = [
        rx_rename(33, b"In 33"),
        rx_rename(1, b"In@1"),
        rx_rename(1, b"RX 2"),
    ];
    let mut dev = Dev::new(config());
    let mut gaps = Vec::new();
    let logged = warnings_of(|| {
        for k in 0..1_000u16 {
            for body in &tx {
                gaps.push(
                    dev.send(&request(CONTROLLER, k, op::TX_RENAME, 0, body))
                        .gap(),
                );
            }
            for body in &rx {
                gaps.push(
                    dev.send(&request(CONTROLLER, k, op::RX_RENAME, 0, body))
                        .gap(),
                );
            }
        }
    });
    assert_eq!(
        logged.lines().count(),
        0,
        "refused renames logged warnings of their own, the first: {}",
        logged.lines().next().unwrap_or_default()
    );
    assert_eq!(gaps.len(), 7_000);
    for (i, gap) in gaps.iter().enumerate() {
        let op1 = if i % 7 < 4 {
            op::TX_RENAME
        } else {
            op::RX_RENAME
        };
        assert_eq!(*gap, Some((op1, Gap::Refused)), "{i}");
    }
    assert_eq!(dev.state, DeviceState::from_config(&dev.cfg));
    // The capture works: a warning of the crate's own does show.
    assert!(warnings_of(|| tracing::warn!("a test warning")).contains("a test warning"));
}

#[test]
fn an_invalid_rename_leaves_the_state_file_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let mut device = Device::load(config_in(dir.path())).unwrap();
    // No file yet: a refused rename writes none.
    let refused = request(CONTROLLER, 1, op::TX_RENAME, 0, &tx_rename(3, b"Mix.3"));
    let answer = decode(respond_idle(&mut device, &refused));
    assert_eq!(answer.header.op2, STATUS_ERROR);
    assert!(!device.cfg.state_file.exists());
    // With a file: it keeps its bytes.
    let good = request(CONTROLLER, 2, op::TX_RENAME, 0, &tx_rename(3, b"Mix 3"));
    assert!(decode(respond_idle(&mut device, &good)).changed);
    let saved = std::fs::read(&device.cfg.state_file).unwrap();
    let too_long = "y".repeat(32);
    let refused = request(
        CONTROLLER,
        3,
        op::TX_RENAME,
        0,
        &tx_rename(3, too_long.as_bytes()),
    );
    assert_eq!(
        decode(respond_idle(&mut device, &refused)).header.op2,
        STATUS_ERROR
    );
    assert_eq!(std::fs::read(&device.cfg.state_file).unwrap(), saved);
    assert_eq!(device.state.tx[2], "Mix 3");
}

#[test]
fn a_restart_comes_back_with_the_renamed_names() {
    let dir = tempfile::tempdir().unwrap();
    let mut device = Device::load(config_in(dir.path())).unwrap();
    for (seq, op1, body) in [
        (1, op::TX_RENAME, tx_rename(3, b"Mix 3")),
        (2, op::RX_RENAME, rx_rename(5, b"Input 5")),
    ] {
        let answer = decode(respond_idle(
            &mut device,
            &request(CONTROLLER, seq, op1, 0, &body),
        ));
        assert!(answer.changed);
    }
    // The box restarts: the same config, the state read from its file.
    let mut back = Device::load(config_in(dir.path())).unwrap();
    assert_eq!(back.state, device.state);
    assert_eq!(back.state.tx[2], "Mix 3");
    assert_eq!(back.state.rx[4], "Input 5");
    // And it lists them so.
    let answer = decode(respond_idle(
        &mut back,
        &request(CONTROLLER, 3, op::TX_NAMES, 0, &page_request(1)),
    ));
    let page = decode_page(&back.cfg, op::TX_NAMES, &answer);
    assert_eq!(page.channels[2].1, "Mix 3");
}

#[test]
fn a_rename_that_cannot_be_saved_is_refused_and_undone() {
    let dir = tempfile::tempdir().unwrap();
    let mut device = Device::load(DeviceConfig {
        state_file: dir.path().join("missing").join("state.toml"),
        ..config()
    })
    .unwrap();
    let before = device.state.clone();
    let rename = request(CONTROLLER, 4, op::TX_RENAME, 0, &tx_rename(3, b"Mix 3"));
    let answer = decode(respond_idle(&mut device, &rename));
    assert!(!answer.changed);
    assert_eq!(answer.header.op2, STATUS_ERROR);
    assert_eq!(answer.header.seq, 4);
    assert!(answer.body.is_empty());
    assert_eq!(device.state, before);
}

#[test]
fn an_opcode_inferno_does_not_answer_gets_no_reply() {
    // inferno's catch-all logs the opcode and answers nothing; Dante
    // Controller then asks again after its timeout. Refusing it instead
    // (0xffff) made Dante Controller ask 0x2200 and 0x2204 about 2 250
    // times a second (#72, 2026-10-26).
    let mut dev = Dev::new(config());
    dev.state.tx[0] = "Mix 1".into();
    let before = dev.state.clone();
    let set_name = [&[0u8, 0][..], b"renamed\0"].concat();
    for (op1, body) in [
        // Dante Controller's 0x2204 with its captured body.
        (0x2204, DC_FIRST_PAGE.to_vec()),
        // netaudio's set_name and set_latency: not in inferno either.
        (0x1001, set_name),
        (0x1101, vec![0, 0, 0, 5]),
        (0x2203, DC_FIRST_PAGE.to_vec()),
        (0x3201, DC_FIRST_PAGE.to_vec()),
        (0x0000, Vec::new()),
        (0xffff, tx_rename(1, b"Mix X")),
    ] {
        for start in [CONTROLLER, NETAUDIO] {
            let outcome = dev.send(&request(start, 11, op1, 0, &body));
            assert_eq!(outcome, ArcOutcome::Unanswered { op1 }, "{op1:#06x}");
            assert_eq!(dev.state, before);
        }
    }
    // A request whose op2 is not 0 gets none either, even a known opcode
    // (inferno: "unknown opcode2").
    for (op1, body) in [
        (op::COUNTS, Vec::new()),
        (op::TX_RENAME, tx_rename(1, b"Mix Y")),
        (op::TX_FLOWS, DC_FIRST_PAGE.to_vec()),
        (op::RX_SUBSCRIBE, subscribe(5, "TX 1", "other-device")),
    ] {
        for op2 in [STATUS_OK, STATUS_MORE, STATUS_ERROR] {
            let outcome = dev.send(&request(NETAUDIO, 12, op1, op2, &body));
            assert_eq!(outcome, ArcOutcome::Unanswered { op1 }, "{op1:#06x}");
        }
    }
    assert_eq!(dev.state, before);
    // A datagram that is no request gets no answer at all: shorter than the
    // header it is ignored; one whose length field disagrees is misframed.
    let mut cut = request(NETAUDIO, 13, op::COUNTS, 0, &[]);
    cut.pop();
    assert_eq!(dev.send(&cut), ArcOutcome::Ignored);
    let mut long = request(NETAUDIO, 13, op::COUNTS, 0, &[]);
    long.push(0);
    assert_eq!(dev.send(&long), ArcOutcome::Misframed { op1: op::COUNTS });
    assert_eq!(dev.send(&[]), ArcOutcome::Ignored);
    assert_eq!(dev.state, before);
}

#[test]
fn a_datagram_whose_length_field_disagrees_is_counted_never_answered() {
    // The review of PR #78: such a datagram was dropped and never counted,
    // so a sender whose datagrams we misread (padding, a field we read
    // wrong) would never show. It is a gap of its own: per peer and the
    // opcode its header names.
    let mut dev = Dev::new(config());
    let before = dev.state.clone();
    for (op1, body) in [
        (op::COUNTS, Vec::new()),
        (op::TX_FLOWS, DC_FIRST_PAGE.to_vec()),
        (op::TX_RENAME, tx_rename(3, b"Mix 3")),
        (0x2204, DC_FIRST_PAGE.to_vec()),
    ] {
        let good = request(CONTROLLER, 21, op1, 0, &body);
        let mut padded = good.clone();
        padded.extend_from_slice(&[0, 0]);
        let mut cut = good.clone();
        cut.pop();
        for datagram in [padded, cut] {
            if datagram.len() < HEADER_LEN {
                continue;
            }
            let outcome = dev.send(&datagram);
            assert_eq!(outcome, ArcOutcome::Misframed { op1 }, "{op1:#06x}");
            assert_eq!(outcome.gap(), Some((op1, Gap::Misframed)), "{op1:#06x}");
        }
        // A length field that is neither: larger than any datagram.
        let mut wrong = good.clone();
        wrong[2..4].copy_from_slice(&0xffffu16.to_be_bytes());
        assert_eq!(dev.send(&wrong).gap(), Some((op1, Gap::Misframed)));
    }
    // Nothing renamed, and shorter than the header stays uncounted.
    assert_eq!(dev.state, before);
    assert_eq!(dev.send(&[0x27, 0x29, 0, 99]).gap(), None);
}

#[test]
fn a_misframed_gap_reads_as_misframed() {
    let report = gaps::Report {
        entries: vec![gaps::Entry {
            peer: 1,
            op1: 0x1000,
            gap: Gap::Misframed,
            count: 4,
        }],
        uncounted: 0,
    };
    assert_eq!(report.to_string(), "peer 1 0x1000 misframed 4");
}

#[test]
fn two_interleaved_requests_get_their_own_sequence() {
    let mut dev = Dev::new(config());
    // Dante Controller and a reader, their sequences interleaved.
    let requests = [
        (CONTROLLER, 0x0100, op::COUNTS),
        (NETAUDIO, 0xfff0, op::DEVICE_NAME),
        (CONTROLLER, 0x0101, op::TX_NAMES),
        (NETAUDIO, 0xfff1, op::RX_CHANNELS),
        (CONTROLLER, 0x0102, op::CLOCK),
        (NETAUDIO, 0xfff2, op::TX_RENAME),
        (CONTROLLER, 0x0103, op::OP_2320),
    ];
    for (start, seq, op1) in requests {
        let body = match op1 {
            op::TX_NAMES | op::RX_CHANNELS => page_request(1),
            op::TX_RENAME => tx_rename(2, b"Mix 2"),
            _ => Vec::new(),
        };
        let answer = decode(dev.send(&request(start, seq, op1, 0, &body)));
        assert_eq!(
            (answer.header.start, answer.header.seq, answer.header.op1),
            (start, seq, op1)
        );
        assert_eq!(
            usize::from(answer.header.len),
            HEADER_LEN + answer.body.len()
        );
    }
}

#[test]
fn the_clock_reply_is_the_fixed_one() {
    let mut dev = Dev::new(config());
    let answer = dev.ask(1, op::CLOCK, &[]);
    assert_eq!(answer.header.op2, STATUS_OK);
    assert_eq!(
        answer.body,
        [0x38, 0x00, 0x38, 0xfd, 0x38, 0xfe, 0x38, 0xff]
    );
    // The two Dante Controller asks inferno answers with zeros.
    let answer = dev.ask(2, op::ZEROS_1100, &[]);
    assert_eq!((answer.header.op2, answer.body.len()), (STATUS_OK, 110));
    assert!(answer.body.iter().all(|&b| b == 0));
    let answer = dev.ask(3, op::ZEROS_1102, &[]);
    assert_eq!((answer.header.op2, answer.body.len()), (STATUS_OK, 94));
    assert!(answer.body.iter().all(|&b| b == 0));
}

#[test]
fn the_flow_lists_are_inferno_s_empty_page() {
    // No flows: 16 slots (inferno's 32 flows, at most 16 a page), none used,
    // status OK. Dante Controller's captured request first, byte for byte.
    let mut dev = Dev::new(config());
    let mut want = vec![
        0x27, 0x29, 0x00, 0x2c, 0x12, 0x34, 0x22, 0x00, 0x00, 0x01, 0x10, 0x00,
    ];
    want.extend([0; 32]);
    let packet = request(CONTROLLER, 0x1234, op::TX_FLOWS, 0, &DC_FIRST_PAGE);
    assert_eq!(reply_bytes(&mut dev, &packet), want);
    want[6] = 0x32;
    let packet = request(CONTROLLER, 0x1234, op::RX_FLOWS, 0, &DC_FIRST_PAGE);
    assert_eq!(reply_bytes(&mut dev, &packet), want);
    // Any first flow, netaudio's page request too: the same empty page.
    for op1 in [op::TX_FLOWS, op::RX_FLOWS] {
        for body in [
            page_request(1),
            page_request(17),
            page_request(0xffff),
            vec![0xaa, 0xbb, 0, 3],
        ] {
            let answer = dev.ask(7, op1, &body);
            assert_eq!(answer.header.op2, STATUS_OK, "{op1:#06x} {body:?}");
            assert_eq!(answer.body, want[HEADER_LEN..], "{op1:#06x} {body:?}");
            assert!(!answer.changed);
        }
        // Without its first flow it is refused, as inferno refuses a page
        // request it cannot read.
        for body in [&[][..], &[0, 1, 0], &[0, 1, 0, 0], &[0, 1, 0, 0, 0, 1]] {
            let answer = dev.ask(8, op1, body);
            assert_eq!(answer.header.op2, STATUS_ERROR, "{op1:#06x} {body:?}");
            assert!(answer.body.is_empty());
        }
    }
    assert_eq!(dev.state, DeviceState::from_config(&dev.cfg));
}

#[test]
fn a_multicast_flow_is_neither_created_nor_deleted() {
    // Phase A has no flows: inferno, with no transmitter, creates and
    // deletes nothing and refuses, no body.
    let mut dev = Dev::new(config());
    let before = dev.state.clone();
    for (op1, body, want) in [
        (
            op::TX_MULTICAST_CREATE,
            multicast_create(1, &[1, 2]),
            [0x27, 0x29, 0x00, 0x0a, 0x00, 0x05, 0x22, 0x01, 0xff, 0xff],
        ),
        (
            op::TX_MULTICAST_DELETE,
            multicast_delete(1),
            [0x27, 0x29, 0x00, 0x0a, 0x00, 0x05, 0x22, 0x02, 0xff, 0xff],
        ),
    ] {
        let packet = request(CONTROLLER, 5, op1, 0, &body);
        assert_eq!(reply_bytes(&mut dev, &packet), want, "{op1:#06x}");
        assert_eq!(dev.state, before);
    }
    // Whatever they name, as long as inferno can read it.
    for (op1, body) in [
        (op::TX_MULTICAST_CREATE, multicast_create(32, &[16])),
        (op::TX_MULTICAST_CREATE, vec![0, 0]),
        (op::TX_MULTICAST_DELETE, multicast_delete(0)),
        (op::TX_MULTICAST_DELETE, vec![0, 0, 0, 0]),
    ] {
        let answer = dev.ask(6, op1, &body);
        assert_eq!(answer.header.op2, STATUS_ERROR, "{op1:#06x} {body:?}");
        assert!(answer.body.is_empty());
    }
}

#[test]
fn op_2320_is_answered_with_status_0x30() {
    let mut dev = Dev::new(config());
    for body in [&[][..], &DC_FIRST_PAGE, &[1, 2, 3]] {
        let packet = request(CONTROLLER, 0x0102, op::OP_2320, 0, body);
        assert_eq!(
            reply_bytes(&mut dev, &packet),
            [0x27, 0x29, 0x00, 0x0a, 0x01, 0x02, 0x23, 0x20, 0x00, 0x30]
        );
    }
}

#[test]
fn a_subscription_is_made_and_answered_as_inferno_answers_it() {
    // inferno answers OK, no body, to every subscription request it can
    // read; the channel is subscribed (our own RX channel only).
    let mut dev = Dev::new(config());
    let packet = request(
        CONTROLLER,
        9,
        op::RX_SUBSCRIBE,
        0,
        &subscribe(5, "TX 1", "other-device"),
    );
    let ok = vec![0x27, 0x29, 0x00, 0x0a, 0x00, 0x09, 0x30, 0x10, 0x00, 0x01];
    assert_eq!(
        dev.send(&packet),
        ArcOutcome::Changed(ok.clone(), subs_changed(&[4]))
    );
    assert_eq!(
        dev.state.subscription(4),
        Some(&sub("TX 1", "other-device"))
    );
    assert_eq!(dev.state.subscriptions.len(), 32);
    // Listed at once, Unresolved: the manager has not been told yet.
    let subs = read_subs(&mut dev);
    assert_eq!(subs[4], Some(("TX 1".into(), "other-device".into(), 1)));
    assert_eq!(subs.iter().flatten().count(), 1);
    // The same subscription again: OK, nothing changed.
    assert_eq!(dev.send(&packet), ArcOutcome::Reply(ok));
    // netaudio's unsubscribe.
    let packet = request(NETAUDIO, 10, op::RX_UNSUBSCRIBE, 0, &unsubscribe(5));
    let ok = vec![0x27, 0xff, 0x00, 0x0a, 0x00, 0x0a, 0x30, 0x14, 0x00, 0x01];
    assert_eq!(
        dev.send(&packet),
        ArcOutcome::Changed(ok.clone(), subs_changed(&[4]))
    );
    assert_eq!(dev.state.subscription(4), None);
    assert_eq!(read_subs(&mut dev).iter().flatten().count(), 0);
    // Not subscribed: OK, nothing changed.
    assert_eq!(dev.send(&packet), ArcOutcome::Reply(ok));
    // Dante Controller's unsubscribe: a 0x3010 item with no TX names, or
    // with either name's offset 0 (inferno subscribes only with both).
    for item in [
        [0x00, 0x05, 0, 0, 0, 0],
        [0x00, 0x05, 0, 0x34, 0, 0],
        [0x00, 0x05, 0, 0, 0, 0x34],
    ] {
        let again = dev.ask(11, op::RX_SUBSCRIBE, &subscribe(5, "TX 2", "peer-device"));
        assert!(again.changed);
        let body = [&[0x02, 0x01][..], &item[..]].concat();
        let answer = dev.ask(12, op::RX_SUBSCRIBE, &body);
        assert_eq!((answer.header.op2, answer.body.len()), (STATUS_OK, 0));
        assert!(answer.changed, "{item:?}");
        assert_eq!(dev.state.subscription(4), None);
    }
}

#[test]
fn a_subscription_inferno_passes_over_changes_nothing() {
    let mut dev = Dev::new(config());
    dev.state
        .set_subscription(&dev.cfg, 1, Some(sub("TX 9", "peer-device")));
    let before = dev.state.clone();
    let long = "c".repeat(32);
    let mut bodies: Vec<(&str, Vec<u8>)> = vec![
        (
            "a channel we do not have",
            subscribe(33, "TX 1", "peer-device"),
        ),
        ("channel 0", subscribe(0, "TX 1", "peer-device")),
        ("no item", vec![0x02, 0x00]),
        (
            "a channel name with a .",
            subscribe(2, "TX.1", "peer-device"),
        ),
        (
            "a channel name with an @",
            subscribe(2, "TX@1", "peer-device"),
        ),
        ("a channel name of 32", subscribe(2, &long, "peer-device")),
        (
            "a device name with a space",
            subscribe(2, "TX 1", "peer device"),
        ),
        ("a device name ending in -", subscribe(2, "TX 1", "peer-")),
        ("an empty channel name", subscribe(2, "", "peer-device")),
        ("an empty device name", subscribe(2, "TX 1", "")),
    ];
    // Names inferno cannot read: an offset in the header, past the
    // datagram, bytes that are not UTF-8.
    let mut low = subscribe(2, "TX 1", "peer-device");
    low[4..6].copy_from_slice(&5u16.to_be_bytes());
    bodies.push(("a name offset in the header", low));
    let mut high = subscribe(2, "TX 1", "peer-device");
    high[6..8].copy_from_slice(&300u16.to_be_bytes());
    bodies.push(("a name offset past the datagram", high));
    let mut bad = subscribe(2, "TX 1", "peer-device");
    let at = bad.len() - 3;
    bad[at] = 0xff;
    bodies.push(("bytes that are not UTF-8", bad));
    for (why, body) in &bodies {
        let answer = dev.ask(3, op::RX_SUBSCRIBE, body);
        assert_eq!(answer.header.op2, STATUS_OK, "{why}");
        assert!(answer.body.is_empty(), "{why}");
        assert!(!answer.changed, "{why}");
        assert_eq!(dev.state, before, "{why}");
    }
    for channel in [0, 33] {
        let answer = dev.ask(4, op::RX_UNSUBSCRIBE, &unsubscribe(channel));
        assert_eq!((answer.header.op2, answer.changed), (STATUS_OK, false));
    }
    assert_eq!(dev.state, before);
    // A device that never had a subscription keeps phase A's state: an
    // unsubscribe writes no list.
    let mut fresh = Dev::new(config());
    let answer = fresh.ask(5, op::RX_SUBSCRIBE, &subscriptions(&[(3, None)]));
    assert!(!answer.changed);
    assert!(fresh.state.subscriptions.is_empty());
}

#[test]
fn several_items_subscribe_several_channels_in_one_request() {
    let mut dev = Dev::new(config());
    dev.state
        .set_subscription(&dev.cfg, 2, Some(sub("TX 9", "peer-device")));
    let body = subscriptions(&[
        (1, Some(("TX 1", "peer-device"))),
        (2, Some(("TX 1", "peer-device"))),
        (3, None),
        (1, Some(("TX 2", "peer-device"))),
        (40, Some(("TX 3", "peer-device"))),
    ]);
    let outcome = dev.send(&request(CONTROLLER, 6, op::RX_SUBSCRIBE, 0, &body));
    let ArcOutcome::Changed(bytes, change) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(bytes, request(CONTROLLER, 6, op::RX_SUBSCRIBE, 1, &[]));
    // Each channel once, in the order the request first changed it.
    assert_eq!(change, subs_changed(&[0, 1, 2]));
    // The later item for channel 1 wins, as inferno's loop.
    assert_eq!(dev.state.subscription(0), Some(&sub("TX 2", "peer-device")));
    assert_eq!(dev.state.subscription(1), Some(&sub("TX 1", "peer-device")));
    assert_eq!(dev.state.subscription(2), None);
    assert_eq!(dev.state.problems(&dev.cfg), Vec::<String>::new());
}

/// RX channel 5's listed subscription, read through [`respond`] with `tx`
/// and `rx` as the running flows.
fn listed_rx5(device: &mut Device, tx: &TxTable, rx: &Manager) -> Option<(String, String, u32)> {
    let req = request(CONTROLLER, 2, op::RX_CHANNELS, 0, &page_request(1));
    let answer = decode(respond(device, &Live { tx, rx }, &req));
    decode_page(&device.cfg, op::RX_CHANNELS, &answer).subs[4].clone()
}

#[test]
fn a_subscribe_is_unresolved_at_once_and_saved_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut device = Device::load(config_in(dir.path())).unwrap();
    // RX 5 receives TX 3 of a peer, its flow live.
    let net = Net(vec![
        ("peer-device".into(), "TX 3".into(), adv(PEER_IP, 3)),
        ("peer-device".into(), "TX 4".into(), adv(PEER_IP, 4)),
    ]);
    let tx = TxTable::new(7);
    let mut rx = manager_of(&device.cfg);
    device
        .state
        .set_subscription(&device.cfg, 4, Some(sub("TX 3", "peer-device")));
    receiving(&mut rx, &[(4, sub("TX 3", "peer-device"))], &net);
    assert_eq!(
        listed_rx5(&mut device, &tx, &rx),
        Some(("TX 3".into(), "peer-device".into(), 0x0101_0009))
    );
    // Dante Controller moves RX 5 to TX 4: saved before the OK.
    let req = request(
        CONTROLLER,
        3,
        op::RX_SUBSCRIBE,
        0,
        &subscribe(5, "TX 4", "peer-device"),
    );
    let outcome = respond(&mut device, &Live { tx: &tx, rx: &rx }, &req);
    assert!(
        matches!(&outcome, ArcOutcome::Changed(_, c) if *c == subs_changed(&[4])),
        "{outcome:?}"
    );
    let saved = std::fs::read_to_string(&device.cfg.state_file).unwrap();
    assert!(saved.contains("TX 4"), "{saved}");
    // It reads Unresolved at once, the manager not told yet...
    assert_eq!(
        listed_rx5(&mut device, &tx, &rx),
        Some(("TX 4".into(), "peer-device".into(), 1))
    );
    // ...and once told, until a flow brings it.
    rx.load(&device.state.subscriptions);
    assert_eq!(rx.changed(), [4]);
    assert_eq!(
        listed_rx5(&mut device, &tx, &rx),
        Some(("TX 4".into(), "peer-device".into(), 1))
    );
    // A restart comes back with it.
    let back = Device::load(config_in(dir.path())).unwrap();
    assert_eq!(
        back.state.subscription(4),
        Some(&sub("TX 4", "peer-device"))
    );
    assert_eq!(back.state, device.state);
}

#[test]
fn a_failed_save_refuses_and_undoes() {
    let dir = tempfile::tempdir().unwrap();
    let mut device = Device::load(DeviceConfig {
        state_file: dir.path().join("missing").join("state.toml"),
        ..config()
    })
    .unwrap();
    device
        .state
        .set_subscription(&device.cfg, 0, Some(sub("TX 1", "peer-device")));
    let before = device.state.clone();
    for (seq, op1, body) in [
        (4, op::RX_SUBSCRIBE, subscribe(2, "TX 2", "peer-device")),
        (5, op::RX_SUBSCRIBE, subscriptions(&[(1, None)])),
        (6, op::RX_UNSUBSCRIBE, unsubscribe(1)),
    ] {
        let outcome = respond_idle(&mut device, &request(CONTROLLER, seq, op1, 0, &body));
        assert_eq!(
            outcome,
            ArcOutcome::Reply(request(CONTROLLER, seq, op1, STATUS_ERROR, &[])),
            "{op1:#06x}"
        );
        assert_eq!(device.state, before);
    }
    assert!(!device.cfg.state_file.exists());
}

#[test]
fn the_tx_flow_list_is_inferno_s_query_tx_flows() {
    // One flow a receiver asked for: TX 1 and TX 2 to 10.0.0.11:50000.
    let mut dev = Dev::new(config());
    let rx_at = SocketAddrV4::new(PEER_IP, 50_000);
    let req = tx_request("peer-device", "dc-flow", &[Some(1), Some(2)], rx_at);
    assert_eq!(dev.tx.add(&req, 0).unwrap().0, 0);
    let packet = request(CONTROLLER, 0x1234, op::TX_FLOWS, 0, &DC_FIRST_PAGE);
    #[rustfmt::skip]
    let mut want = vec![
        0x27, 0x29, 0x00, 0x78, 0x12, 0x34, 0x22, 0x00, 0x00, 0x01,
        0x10, 0x01,                         // 16 slots, 1 flow
        0x00, 0x60,                         // flow 1's descriptor at 96
    ];
    want.extend([0; 30]); // the other 15 slots
    #[rustfmt::skip]
    want.extend([
        b'1', b'_', b'0', 0,                // 44: our flow name, <flow>_<process>
        b'p', b'e', b'e', b'r', b'-', b'd', b'e', b'v', b'i', b'c', b'e', 0, // 48
        b'd', b'c', b'-', b'f', b'l', b'o', b'w', 0, // 60: the receiver's flow name
        // 68 (a multiple of 4): the socket descriptor, the receiver's address.
        0x80, 0x02, 0xc3, 0x50, 10, 0, 0, 11,
        // 76: the names descriptor.
        0x0a, 0x00, 0x00, 0x01, 0x00, 0x30, 0x00, 0x3c, 0x00, 0x10, 0x00, 0x2c,
        0, 0, 0, 0, 0, 0, 0, 0,
        // 96: the flow: 1, unicast, 96 kHz, 0, 24 bits, 1, 2 slots, socket at 68.
        0x00, 0x01, 0x00, 0x11, 0x00, 0x01, 0x77, 0x00, 0x00, 0x00, 0x00, 0x18,
        0x00, 0x01, 0x00, 0x02, 0x00, 0x44,
        0x00, 0x01, 0x00, 0x02,             // TX 1, TX 2
        0x00, 0x4c,                         // the names descriptor at 76
    ]);
    assert_eq!(reply_bytes(&mut dev, &packet), want);
    // An empty slot is 0, an expired flow is listed (inferno lists it until
    // a new destination frees its slot).
    let req = tx_request("peer-device", "dc-flow", &[None, Some(16)], rx_at);
    dev.tx.add(&req, 0).unwrap();
    assert_eq!(dev.tx.expire(crate::media::tx::KEEPALIVE_TIMEOUT_NS), 1);
    let answer = dev.ask(1, op::TX_FLOWS, &DC_FIRST_PAGE);
    let (_, items, status) = flow_page(&answer);
    assert_eq!((items.len(), status), (1, STATUS_OK));
    let main = usize::from(items[0]) - HEADER_LEN;
    assert_eq!(answer.body[main + 18..main + 22], [0, 0, 0, 16]);
}

#[test]
fn a_tx_flow_s_names_are_cut_to_63_bytes_at_a_character() {
    let mut dev = Dev::new(config());
    let host = format!("{}\u{e9}{}", "h".repeat(62), "x".repeat(900));
    let name = "n".repeat(500);
    let rx_at = SocketAddrV4::new(PEER_IP, 50_000);
    dev.tx
        .add(&tx_request(&host, &name, &[Some(1)], rx_at), 0)
        .unwrap();
    let answer = dev.ask(1, op::TX_FLOWS, &DC_FIRST_PAGE);
    let (_, items, status) = flow_page(&answer);
    assert_eq!((items.len(), status), (1, STATUS_OK));
    let body = &answer.body;
    let main = usize::from(items[0]) - HEADER_LEN;
    let names = usize::from(read_u16(body, main + 20).unwrap()) - HEADER_LEN;
    let host_at = read_u16(body, names + 4).unwrap();
    let name_at = read_u16(body, names + 6).unwrap();
    // The two-byte é would end at byte 64: cut before it.
    assert_eq!(string_at(body, host_at), Some("h".repeat(62).as_str()));
    assert_eq!(string_at(body, name_at), Some("n".repeat(63).as_str()));
    assert_eq!(capped("short"), "short");
    assert_eq!(capped(&"a".repeat(63)).len(), 63);
    assert_eq!(capped(&"a".repeat(64)).len(), 63);
}

#[test]
fn the_rx_flow_list_is_inferno_s_query_rx_flows() {
    // RX 1 and RX 2 receive TX 3 of a peer, RX 3 its TX 4: one flow of the
    // peer's 4 slots, granted, its latency the peer's 2 ms (ours is 1 ms).
    let mut dev = Dev::new(config());
    let net = Net(vec![
        ("peer-device".into(), "TX 3".into(), adv(PEER_IP, 3)),
        ("peer-device".into(), "TX 4".into(), adv(PEER_IP, 4)),
    ]);
    let s3 = sub("TX 3", "peer-device");
    let subs = [(0, s3.clone()), (1, s3), (2, sub("TX 4", "peer-device"))];
    receiving(&mut dev.rx, &subs, &net);
    for (rx, s) in subs {
        dev.state.set_subscription(&dev.cfg, rx, Some(s));
    }
    let packet = request(CONTROLLER, 0x1234, op::RX_FLOWS, 0, &DC_FIRST_PAGE);
    #[rustfmt::skip]
    let mut want = vec![
        0x27, 0x29, 0x00, 0x72, 0x12, 0x34, 0x32, 0x00, 0x00, 0x01,
        0x10, 0x01,                         // 16 slots, 1 flow
        0x00, 0x54,                         // flow 1's descriptor at 84
    ];
    want.extend([0; 30]);
    #[rustfmt::skip]
    want.extend([
        // 44: the socket descriptor, our address and slot 1's port.
        0x80, 0x02, 0xc3, 0x50, 10, 0, 0, 10,
        // 52: descriptor 2, the latency 2 000 000 ns.
        0x00, 0x09, 0x00, 0x01, 0x08, 0x00, 0x00, 0x00,
        0x00, 0x1e, 0x84, 0x80, 0x00, 0x00, 0x00, 0x00,
        // 68: each slot's mask of our 32 RX channels, two words.
        0x00, 0x03, 0x00, 0x00,             // TX 3 feeds RX 1 and RX 2
        0x00, 0x04, 0x00, 0x00,             // TX 4 feeds RX 3
        0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
        // 84: the flow: 1, 1, 96 kHz, 0, 24 bits, 1, 4 slots, 2 words,
        // socket at 44, the masks at 68, 72, 76, 80, descriptor 2 at 52.
        0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x77, 0x00, 0x00, 0x00, 0x00, 0x18,
        0x00, 0x01, 0x00, 0x04, 0x00, 0x02, 0x00, 0x2c,
        0x00, 0x44, 0x00, 0x48, 0x00, 0x4c, 0x00, 0x50,
        0x00, 0x34,
    ]);
    assert_eq!(reply_bytes(&mut dev, &packet), want);
    // The channels read Receiving with their TX names.
    let subs = read_subs(&mut dev);
    assert_eq!(
        subs[..3],
        [
            Some(("TX 3".into(), "peer-device".into(), 0x0101_0009)),
            Some(("TX 3".into(), "peer-device".into(), 0x0101_0009)),
            Some(("TX 4".into(), "peer-device".into(), 0x0101_0009)),
        ]
    );
}

#[test]
fn an_rx_flow_is_listed_from_its_grant() {
    let mut dev = Dev::new(config());
    let net = Net(vec![("peer-device".into(), "TX 3".into(), adv(PEER_IP, 3))]);
    dev.rx.set(0, Some(sub("TX 3", "peer-device")));
    let asked = dev.rx.tick(0, &[0; 32], &net);
    assert_eq!(asked.len(), 1);
    assert!(dev.rx.flow(0).is_some());
    // Asked for, not granted: inferno lists it only once granted.
    let answer = dev.ask(1, op::RX_FLOWS, &DC_FIRST_PAGE);
    assert_eq!(flow_page(&answer), (16, vec![], STATUS_OK));
    assert_eq!(answer.body.len(), 34);
    let crate::subscribe::Action::Request { flow, .. } = asked[0].clone() else {
        panic!()
    };
    dev.rx.answered(flow, Answer::Granted(flow_handle(0, 9)), 1);
    let answer = dev.ask(2, op::RX_FLOWS, &DC_FIRST_PAGE);
    assert_eq!(flow_page(&answer).1.len(), 1);
    assert_eq!(stated_latency_ns(2_000_000, 96_000), 2_000_000);
    // inferno states the latency in whole samples: 1 ms at 44.1 kHz is 44.
    assert_eq!(stated_latency_ns(1_000_000, 44_100), 997_732);
    assert_eq!(stated_latency_ns(u64::MAX, 96_000), u32::MAX);
}

#[test]
fn a_flow_page_passes_over_empty_slots_and_counts_only_flows() {
    let mut dev = Dev::new(config());
    // Flows in slots 0 to 16, then slot 3 freed.
    for port in 0..17u16 {
        let rx_at = SocketAddrV4::new(PEER_IP, 40_000 + port);
        dev.tx
            .add(&tx_request("p", "f", &[Some(1)], rx_at), 0)
            .unwrap();
    }
    let gone = flow_handle(3, dev.tx.flow(3).unwrap().cookie);
    dev.tx.stop(&gone).unwrap();
    let ids = |answer: &Reply| -> Vec<u16> {
        flow_page(answer)
            .1
            .iter()
            .map(|&at| read_u16(&answer.body, usize::from(at) - HEADER_LEN).unwrap())
            .collect()
    };
    // From slot 1: slot 4 is passed over, not counted; the soft limit ends
    // the page (a TX flow takes about 60 bytes: 13 do not fit 800).
    let answer = dev.ask(1, op::TX_FLOWS, &page_request(1));
    let listed = ids(&answer);
    assert_eq!(answer.header.op2, STATUS_MORE);
    let want: Vec<u16> = (1..=17).filter(|&id| id != 4).take(listed.len()).collect();
    assert_eq!(listed, want);
    assert!((8..16).contains(&listed.len()), "{}", listed.len());
    // A page by slot: the last flow, then the end.
    let answer = dev.ask(2, op::TX_FLOWS, &page_request(17));
    assert_eq!((ids(&answer), answer.header.op2), (vec![17], STATUS_OK));
    let answer = dev.ask(3, op::TX_FLOWS, &page_request(18));
    assert_eq!((ids(&answer), answer.header.op2), (vec![], STATUS_OK));
}

#[test]
fn a_page_says_more_while_an_item_is_left_after_its_slots_fill() {
    // inferno's serialize_items: an item not listed is passed over and not
    // counted; once the slots are full, any item left, listed or not, says
    // more follow.
    let h = Header {
        start: CONTROLLER,
        len: 16,
        seq: 1,
        op1: op::TX_FLOWS,
        op2: 0,
    };
    // Items 1, 5, 9 … are not listed.
    let listed = |i: usize| (i % 4 != 1).then(|| vec![0, i as u8]);
    let run = |items: std::ops::Range<usize>, first: u16| -> (u16, Vec<u8>) {
        let outcome = page(&h, &page_request(first), 4, 2, items, |i, _| listed(i));
        let (head, body) = Header::parse(&sent_bytes(outcome))
            .map(|(h, b)| (h, b.to_vec()))
            .unwrap();
        (head.op2, body)
    };
    // Items 0, 2, 3, 4 fill the 4 slots; item 5 is left, not listed: more
    // follow all the same.
    let full = vec![4, 4, 0, 0, 0, 2, 0, 3, 0, 4];
    assert_eq!(run(0..6, 1), (STATUS_MORE, full.clone()));
    assert_eq!(run(0..7, 1), (STATUS_MORE, full.clone()));
    // Nothing left: done.
    assert_eq!(run(0..5, 1), (STATUS_OK, full));
    // From the second item: 2, 3, 4 listed, 5 passed over.
    assert_eq!(
        run(0..6, 2),
        (STATUS_OK, vec![4, 3, 0, 2, 0, 3, 0, 4, 0, 0])
    );
    // A start past every item: an empty page, done.
    assert_eq!(
        run(0..6, 9),
        (STATUS_OK, vec![4, 0, 0, 0, 0, 0, 0, 0, 0, 0])
    );
    // No slots and no items: inferno's `[0, 0]`, done.
    let outcome = page(&h, &page_request(1), 0, 2, 0..0, |i, _| listed(i));
    assert_eq!(sent_bytes(outcome)[8..], [0, 1, 0, 0]);
}

#[test]
fn an_rx_flow_s_bitmask_is_inferno_s_bit_vector() {
    assert_eq!(bitmask(&[], 2), [0, 0]);
    assert_eq!(bitmask(&[0, 1], 2), [0x0003, 0]);
    assert_eq!(bitmask(&[15, 16, 31], 2), [0x8000, 0x8001]);
    // A channel named twice is one bit.
    assert_eq!(bitmask(&[4, 4, 4], 1), [0x0010]);
    // Past the words: left out.
    assert_eq!(bitmask(&[16, 2], 1), [0x0004]);
    assert_eq!(bitmask(&[511], 32)[31], 0x8000);
}

#[test]
fn a_page_ends_after_the_item_that_reaches_800_bytes_as_inferno() {
    // 32 TX channels of 31-byte names: the table ends at 268, the shared
    // descriptor and each name bring 32 more: the 17th channel reaches 828.
    let long = |n: usize| format!("T{n:02}{}", "x".repeat(28));
    let mut dev = Dev::new(DeviceConfig {
        tx: (1..=32).map(long).collect(),
        ..config()
    });
    let answer = dev.ask(1, op::TX_CHANNELS, &page_request(1));
    let body = &answer.body;
    assert_eq!((body[0], body[1]), (32, 16));
    assert_eq!(answer.header.op2, STATUS_MORE);
    assert_eq!(HEADER_LEN + body.len(), 828);
    // The 17th channel's descriptor is in its slot, not counted.
    assert_eq!(read_u16(body, 2 + 16 * 8), Some(17));
    let name = read_u16(body, 2 + 16 * 8 + 6).unwrap();
    assert_eq!(string_at(body, name), Some(long(17).as_str()));
    assert_eq!(body[2 + 17 * 8..2 + 32 * 8], [0; 120]);
    // Dante Controller asks from the 17th: the rest fits one page.
    let answer = dev.ask(2, op::TX_CHANNELS, &page_request(17));
    assert_eq!((answer.body[0], answer.body[1]), (32, 16));
    assert_eq!(answer.header.op2, STATUS_OK);
    assert_eq!(HEADER_LEN + answer.body.len(), 796);
}

/// A device whose every name is 31 bytes: 40 RX channels, each subscribed
/// to its own transmitter's 31-byte channel, 70 TX channels, 32 TX flows to
/// receivers with names of a kilobyte, and 32 granted RX flows.
fn longest() -> Dev {
    let long =
        |prefix: &str, n: usize| format!("{prefix}{n:02}{}", "x".repeat(31 - prefix.len() - 2));
    let mut dev = Dev::new(DeviceConfig {
        rx: (1..=40).map(|n| long("R", n)).collect(),
        tx: (1..=70).map(|n| long("T", n)).collect(),
        ..config()
    });
    let mut net = Vec::new();
    let mut subs = Vec::new();
    for rx in 0..40 {
        let device = long("d", rx);
        let channel = long("c", rx);
        let ip = Ipv4Addr::new(10, 0, 1, rx as u8);
        net.push((device.clone(), channel.clone(), adv(ip, 1)));
        subs.push((rx, sub(&channel, &device)));
        dev.state
            .set_subscription(&dev.cfg, rx, Some(sub(&channel, &device)));
    }
    receiving(&mut dev.rx, &subs, &Net(net));
    let host = "h".repeat(1_000);
    for port in 0..32u16 {
        let rx_at = SocketAddrV4::new(PEER_IP, 40_000 + port);
        let channels: Vec<Option<u16>> = (1..=8).map(Some).collect();
        dev.tx
            .add(&tx_request(&host, &host, &channels, rx_at), 0)
            .unwrap();
    }
    dev
}

/// Every flow of a flow list, read as Dante Controller reads it (each next
/// page from the slot after the last flow listed; the flows are dense).
fn read_flows(dev: &mut Dev, op1: u16) -> Vec<u16> {
    let mut start = 1u16;
    let mut out = Vec::new();
    for _ in 0..40 {
        let answer = dev.ask(start, op1, &page_request(start));
        let (_, items, status) = flow_page(&answer);
        for at in &items {
            out.push(read_u16(&answer.body, usize::from(*at) - HEADER_LEN).unwrap());
        }
        if status == STATUS_OK {
            return out;
        }
        assert_eq!(status, STATUS_MORE);
        assert!(!items.is_empty());
        start += items.len() as u16;
    }
    panic!("the list never ended");
}

#[test]
fn pages_with_the_longest_names_fit_and_list_everything_once() {
    let mut dev = longest();
    assert_eq!(read_list(&mut dev, op::TX_CHANNELS), numbered(&dev.cfg.tx));
    assert_eq!(read_list(&mut dev, op::TX_NAMES), numbered(&dev.cfg.tx));
    // 16 RX channels with three names each pass the soft limit: pages end
    // early, every channel listed once with its subscription.
    let pages = read_pages(&mut dev, op::RX_CHANNELS);
    assert!(pages.len() > 3, "{}", pages.len());
    let channels: Vec<(u16, String)> = pages.iter().flat_map(|p| p.channels.clone()).collect();
    assert_eq!(channels, numbered(&dev.cfg.rx));
    let subs: Vec<_> = pages.into_iter().flat_map(|p| p.subs).collect();
    for (rx, listed) in subs.iter().enumerate() {
        let (channel, device, status) = listed.clone().expect("subscribed");
        let want = dev.state.subscription(rx).unwrap();
        assert_eq!((&channel, &device), (&want.tx_channel, &want.tx_device));
        // 32 flow slots: the first 32 transmitters receive, the rest wait.
        let code = if rx < 32 { 0x0101_0009 } else { 1 };
        assert_eq!(status, code, "RX {}", rx + 1);
    }
    let all: Vec<u16> = (1..=32).collect();
    assert_eq!(read_flows(&mut dev, op::TX_FLOWS), all);
    assert_eq!(read_flows(&mut dev, op::RX_FLOWS), all);
    // Any page from any first item stays within one datagram.
    for op1 in [
        op::TX_CHANNELS,
        op::TX_NAMES,
        op::RX_CHANNELS,
        op::TX_FLOWS,
        op::RX_FLOWS,
    ] {
        for first in 1..=72 {
            let answer = dev.ask(first, op1, &page_request(first));
            assert!(
                HEADER_LEN + answer.body.len() <= MAX_DATAGRAM,
                "{op1:#06x} {first}"
            );
        }
    }
}

/// The test device with two TX flows (to two receivers) and two RX flows
/// (from two transmitters), granted and receiving.
fn with_two_flows_each_way() -> Dev {
    let mut dev = Dev::new(config());
    for port in [50_000, 50_001] {
        let rx_at = SocketAddrV4::new(PEER_IP, port);
        dev.tx
            .add(&tx_request("peer-device", "f", &[Some(1)], rx_at), 0)
            .unwrap();
    }
    let other = Ipv4Addr::new(10, 0, 0, 12);
    let net = Net(vec![
        ("peer-device".into(), "TX 1".into(), adv(PEER_IP, 1)),
        ("other-device".into(), "TX 1".into(), adv(other, 1)),
    ]);
    receiving(
        &mut dev.rx,
        &[
            (0, sub("TX 1", "peer-device")),
            (1, sub("TX 1", "other-device")),
        ],
        &net,
    );
    for rx in 0..2 {
        let s = dev.rx.subscription(rx).cloned();
        dev.state.set_subscription(&dev.cfg, rx, s);
    }
    dev
}

#[test]
fn every_opcode_is_answered_as_inferno_answers_it_with_two_flows_each_way() {
    let mut dev = with_two_flows_each_way();
    for (k, (op1, body, status)) in inferno_opcodes().iter().enumerate() {
        let seq = 0x5000 + k as u16;
        let outcome = dev.send(&request(CONTROLLER, seq, *op1, 0, body));
        let answer = decode(outcome);
        assert_eq!((answer.header.seq, answer.header.op1), (seq, *op1));
        assert_eq!(answer.header.op2, *status, "{op1:#06x}");
    }
    for op1 in [op::TX_FLOWS, op::RX_FLOWS] {
        let answer = dev.ask(1, op1, &DC_FIRST_PAGE);
        assert_eq!(flow_page(&answer).1.len(), 2, "{op1:#06x}");
    }
}

#[tokio::test]
async fn the_server_hands_a_subscription_to_the_manager_and_names_the_channels() {
    let s = served().await;
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    peer.send_to(
        &request(
            CONTROLLER,
            7,
            op::RX_SUBSCRIBE,
            0,
            &subscribe(3, "TX 1", "peer-device"),
        ),
        s.addr,
    )
    .await
    .unwrap();
    let reply = recv(&peer).await;
    assert_eq!(reply, request(CONTROLLER, 7, op::RX_SUBSCRIBE, 1, &[]));
    tokio::time::timeout(Duration::from_secs(5), s.changed.notified())
        .await
        .expect("the change is signalled");
    // The manager has it (Unresolved: no flows here), the notification
    // names RX 3, the state file holds it.
    {
        let rx = s.flows.rx.lock().unwrap();
        assert_eq!(rx.subscription(2), Some(&sub("TX 1", "peer-device")));
        assert_eq!(rx.status(2), Some(crate::subscribe::SubStatus::Unresolved));
    }
    assert_eq!(s.rx_changes.take(), [2]);
    let saved = std::fs::read_to_string(&s.device.lock().unwrap().cfg.state_file).unwrap();
    assert!(saved.contains("peer-device"), "{saved}");
    // An RX rename names its channel too; a TX rename none.
    peer.send_to(
        &request(CONTROLLER, 8, op::RX_RENAME, 0, &rx_rename(6, b"In 6")),
        s.addr,
    )
    .await
    .unwrap();
    recv(&peer).await;
    peer.send_to(
        &request(CONTROLLER, 9, op::TX_RENAME, 0, &tx_rename(2, b"Mix 2")),
        s.addr,
    )
    .await
    .unwrap();
    recv(&peer).await;
    assert_eq!(s.rx_changes.take(), [5]);
    // An unsubscribe: the manager drops it, the notification names RX 3.
    peer.send_to(
        &request(NETAUDIO, 10, op::RX_UNSUBSCRIBE, 0, &unsubscribe(3)),
        s.addr,
    )
    .await
    .unwrap();
    recv(&peer).await;
    assert_eq!(s.flows.rx.lock().unwrap().subscription(2), None);
    assert_eq!(s.rx_changes.take(), [2]);
    // A read changes nothing.
    peer.send_to(
        &request(CONTROLLER, 11, op::RX_CHANNELS, 0, &page_request(1)),
        s.addr,
    )
    .await
    .unwrap();
    recv(&peer).await;
    assert!(s.rx_changes.is_empty());
    s.task.abort();
}

#[test]
fn no_flows_keeps_the_saved_subscriptions_unresolved() {
    let dir = tempfile::tempdir().unwrap();
    let mut device = Device::load(config_in(dir.path())).unwrap();
    device
        .state
        .set_subscription(&device.cfg, 7, Some(sub("TX 2", "peer-device")));
    let flows = NoFlows::new(&device);
    flows.view(|live| {
        assert_eq!(live.tx.flows().count(), 0);
        assert_eq!(live.rx.subscription(7), Some(&sub("TX 2", "peer-device")));
        assert_eq!(
            live.rx.status(7),
            Some(crate::subscribe::SubStatus::Unresolved)
        );
    });
    // Its status was reported at start: a change from here on moves it.
    device.state.set_subscription(&device.cfg, 7, None);
    assert_eq!(flows.subscribe(&device.state.subscriptions), [7]);
    assert_eq!(
        flows.subscribe(&device.state.subscriptions),
        Vec::<usize>::new()
    );
}

#[test]
fn running_flows_read_the_tx_table_and_the_manager() {
    let cfg = config();
    let rings: Arc<[crate::media::ring::Ring]> = (0..cfg.rx.len())
        .map(|_| crate::media::ring::Ring::new())
        .collect::<Vec<_>>()
        .into();
    let tx = Arc::new(TxShared::new(cfg.clone(), Ipv4Addr::LOCALHOST, rings, 3, &[]).unwrap());
    let rx_at = SocketAddrV4::new(PEER_IP, 50_000);
    tx.lock()
        .table
        .add(&tx_request("peer-device", "f", &[Some(1)], rx_at), 0)
        .unwrap();
    let running = Running {
        tx,
        rx: Arc::new(Mutex::new(manager_of(&cfg))),
    };
    let mut subs = vec![None; cfg.rx.len()];
    subs[4] = Some(sub("TX 1", "peer-device"));
    assert_eq!(running.subscribe(&subs), [4]);
    assert_eq!(running.subscribe(&subs), Vec::<usize>::new());
    running.view(|live| {
        assert_eq!(live.tx.flows().count(), 1);
        assert_eq!(live.rx.subscription(4), Some(&sub("TX 1", "peer-device")));
    });
}

#[test]
fn a_request_inferno_cannot_read_gets_no_reply() {
    // inferno fails reading these bodies before it answers.
    let mut dev = Dev::new(config());
    let item = [0, 5, 0, 52, 0, 57];
    for (op1, body) in [
        (op::TX_MULTICAST_CREATE, vec![]),
        (op::TX_MULTICAST_CREATE, vec![0]),
        (op::TX_MULTICAST_DELETE, vec![]),
        (op::TX_MULTICAST_DELETE, vec![0, 1, 0]),
        (op::RX_SUBSCRIBE, vec![]),
        (op::RX_SUBSCRIBE, vec![0x02]),
        // One item announced: inferno reads 6 bytes after the first 2.
        (op::RX_SUBSCRIBE, [&[0x02, 0x01][..], &item[..4]].concat()),
        (op::RX_SUBSCRIBE, [&[0x02, 0x01][..], &item[..5]].concat()),
        // Two in 13 bytes: two fit 13 / 6, but not after the first 2.
        (
            op::RX_SUBSCRIBE,
            [&[0x02, 0x02][..], &item, &item[..5]].concat(),
        ),
        (op::RX_UNSUBSCRIBE, vec![]),
        (op::RX_UNSUBSCRIBE, vec![0, 1, 0, 0, 0]),
    ] {
        let outcome = dev.send(&request(NETAUDIO, 3, op1, 0, &body));
        assert_eq!(
            outcome,
            ArcOutcome::Unanswered { op1 },
            "{op1:#06x} {body:?}"
        );
    }
    // Just long enough: answered.
    for (op1, body, status) in [
        (op::TX_MULTICAST_CREATE, vec![0, 0], STATUS_ERROR),
        (op::TX_MULTICAST_DELETE, vec![0, 1, 0, 0], STATUS_ERROR),
        (op::RX_SUBSCRIBE, vec![0x02, 0x00], STATUS_OK),
        (
            op::RX_SUBSCRIBE,
            [&[0x02, 0x01][..], &item].concat(),
            STATUS_OK,
        ),
        // Five announced, one there: inferno reads the one.
        (
            op::RX_SUBSCRIBE,
            [&[0x02, 0x05][..], &item].concat(),
            STATUS_OK,
        ),
        (
            op::RX_SUBSCRIBE,
            [&[0x02, 0x02][..], &item, &item].concat(),
            STATUS_OK,
        ),
        (op::RX_UNSUBSCRIBE, unsubscribe(1), STATUS_OK),
    ] {
        let answer = dev.ask(4, op1, &body);
        assert_eq!(answer.header.op2, status, "{op1:#06x} {body:?}");
        assert!(answer.body.is_empty());
    }
}

/// Every opcode inferno's ARC server (`arc_server.rs` at 9767558) matches,
/// with the request Dante Controller or netaudio sends, and inferno's status
/// in phase A's state (no flows, no subscriptions).
fn inferno_opcodes() -> Vec<(u16, Vec<u8>, u16)> {
    vec![
        (op::COUNTS, vec![], STATUS_OK),
        (op::DEVICE_NAME, vec![], STATUS_OK),
        (op::DEVICE_NAMES, vec![], STATUS_OK),
        (op::ZEROS_1100, DC_1100.to_vec(), STATUS_OK),
        (op::ZEROS_1102, vec![], STATUS_OK),
        (op::TX_CHANNELS, page_request(1), STATUS_OK),
        (op::TX_NAMES, page_request(1), STATUS_OK),
        (op::TX_RENAME, tx_rename(3, b"Mix 3"), STATUS_OK),
        (op::TX_FLOWS, DC_FIRST_PAGE.to_vec(), STATUS_OK),
        // inferno refuses these two itself when it has no flows to create
        // or delete; there is nothing else to answer.
        (
            op::TX_MULTICAST_CREATE,
            multicast_create(1, &[1, 2]),
            STATUS_ERROR,
        ),
        (op::TX_MULTICAST_DELETE, multicast_delete(1), STATUS_ERROR),
        (op::OP_2320, vec![], STATUS_2320),
        (op::RX_CHANNELS, page_request(1), STATUS_MORE),
        (op::RX_RENAME, rx_rename(5, b"Input 5"), STATUS_OK),
        (
            op::RX_SUBSCRIBE,
            subscribe(5, "TX 1", "other-device"),
            STATUS_OK,
        ),
        (op::RX_UNSUBSCRIBE, unsubscribe(5), STATUS_OK),
        (op::RX_FLOWS, DC_FIRST_PAGE.to_vec(), STATUS_OK),
        (op::CLOCK, vec![], STATUS_OK),
    ]
}

#[test]
fn every_opcode_inferno_answers_is_answered_as_inferno_answers_it() {
    let table = inferno_opcodes();
    let mut ops: Vec<u16> = table.iter().map(|(op1, _, _)| *op1).collect();
    ops.sort_unstable();
    ops.dedup();
    assert_eq!(ops.len(), 18, "each of inferno's 18 opcodes once");
    let mut refused = Vec::new();
    for start in [CONTROLLER, NETAUDIO] {
        let mut dev = Dev::new(config());
        for (k, (op1, body, status)) in table.iter().enumerate() {
            let seq = 0x4000 + k as u16;
            let outcome = dev.send(&request(start, seq, *op1, 0, body));
            assert!(
                matches!(outcome, ArcOutcome::Reply(_) | ArcOutcome::Changed(..)),
                "{op1:#06x} not answered: {outcome:?}"
            );
            let answer = decode(outcome);
            assert_eq!(
                (answer.header.start, answer.header.seq, answer.header.op1),
                (start, seq, *op1)
            );
            assert_eq!(answer.header.op2, *status, "{op1:#06x}");
            if answer.header.op2 == STATUS_ERROR {
                refused.push(*op1);
            }
        }
    }
    // 0xffff only where inferno answers it too.
    assert_eq!(
        refused,
        [
            op::TX_MULTICAST_CREATE,
            op::TX_MULTICAST_DELETE,
            op::TX_MULTICAST_CREATE,
            op::TX_MULTICAST_DELETE
        ]
    );
}

#[test]
fn an_outcome_is_a_gap_when_refused_or_unanswered() {
    let mut dev = Dev::new(config());
    assert_eq!(
        ArcOutcome::Unanswered { op1: 0x2204 }.gap(),
        Some((0x2204, Gap::Unanswered))
    );
    let refused = dev.send(&request(
        CONTROLLER,
        1,
        op::TX_RENAME,
        0,
        &tx_rename(0, b"Mix 0"),
    ));
    assert_eq!(refused.gap(), Some((op::TX_RENAME, Gap::Refused)));
    let refused = dev.send(&request(
        CONTROLLER,
        2,
        op::TX_MULTICAST_DELETE,
        0,
        &multicast_delete(1),
    ));
    assert_eq!(refused.gap(), Some((op::TX_MULTICAST_DELETE, Gap::Refused)));
    // An answer is none: OK, a page that says more, 0x2320's 0x30.
    for (op1, body) in [
        (op::COUNTS, Vec::new()),
        (op::RX_CHANNELS, page_request(1)),
        (op::OP_2320, Vec::new()),
        (op::TX_FLOWS, DC_FIRST_PAGE.to_vec()),
    ] {
        let outcome = dev.send(&request(CONTROLLER, 3, op1, 0, &body));
        assert!(matches!(outcome, ArcOutcome::Reply(_)), "{op1:#06x}");
        assert_eq!(outcome.gap(), None, "{op1:#06x}");
    }
    // Nor a change, nor a datagram that is no request.
    let changed = dev.send(&request(
        CONTROLLER,
        4,
        op::TX_RENAME,
        0,
        &tx_rename(3, b"Mix 3"),
    ));
    assert!(matches!(changed, ArcOutcome::Changed(..)));
    assert_eq!(changed.gap(), None);
    assert_eq!(ArcOutcome::Ignored.gap(), None);
}

#[test]
fn random_requests_never_panic_and_keep_the_names_valid() {
    let mut rng = Rng::new(0xa4c0_1440);
    let mut dev = Dev::new(config());
    let ops = [
        op::COUNTS,
        op::DEVICE_NAME,
        op::DEVICE_NAMES,
        op::ZEROS_1100,
        op::ZEROS_1102,
        op::TX_CHANNELS,
        op::TX_NAMES,
        op::TX_RENAME,
        op::RX_CHANNELS,
        op::RX_RENAME,
        op::CLOCK,
        op::TX_FLOWS,
        op::TX_MULTICAST_CREATE,
        op::TX_MULTICAST_DELETE,
        op::OP_2320,
        op::RX_SUBSCRIBE,
        op::RX_UNSUBSCRIBE,
        op::RX_FLOWS,
        0x1001,
        0x2204,
    ];
    // Three names in four from letters a name may hold, the rest from any.
    let good: &[u8] = b"ab Z09-_~";
    let any: &[u8] = b"ab Z09-_@=.\t\xff~";
    let mut changes = 0;
    for i in 0..10_000 {
        let op1 = ops[rng.below(ops.len())];
        let seq = rng.next() as u16;
        let body = match (op1, i % 3) {
            // A rename-shaped body: a random channel and a random name.
            (op::TX_RENAME | op::RX_RENAME, 0 | 1) => {
                let channel = rng.below(40) as u8;
                let alphabet = if rng.below(4) == 0 { any } else { good };
                let name: Vec<u8> = (0..rng.below(36))
                    .map(|_| alphabet[rng.below(alphabet.len())])
                    .collect();
                if op1 == op::TX_RENAME {
                    tx_rename(channel, &name)
                } else {
                    rx_rename(channel, &name)
                }
            }
            (
                op::TX_CHANNELS | op::TX_NAMES | op::RX_CHANNELS | op::TX_FLOWS | op::RX_FLOWS,
                0 | 1,
            ) => page_request(rng.below(80) as u16),
            _ => {
                let len = rng.below(80);
                rng.bytes(len)
            }
        };
        let op2 = if i % 50 == 0 { rng.next() as u16 } else { 0 };
        let before = dev.state.clone();
        let outcome = dev.send(&request(NETAUDIO, seq, op1, op2, &body));
        // No reply only where inferno sends none: op2 not 0, an opcode it
        // does not handle, a body it cannot read.
        let silent = op2 != 0 || matches!(op1, 0x1001 | 0x2204);
        let readable = !matches!(
            op1,
            op::TX_MULTICAST_CREATE
                | op::TX_MULTICAST_DELETE
                | op::RX_SUBSCRIBE
                | op::RX_UNSUBSCRIBE
        );
        if let ArcOutcome::Unanswered { op1: unanswered } = outcome {
            assert_eq!(unanswered, op1);
            assert!(silent || !readable, "{op1:#06x} {body:02x?}");
            assert_eq!(dev.state, before);
            continue;
        }
        assert!(!silent, "{op1:#06x} op2 {op2:#06x} answered");
        let answer = decode(outcome);
        assert_eq!((answer.header.seq, answer.header.op1), (seq, op1));
        assert!(HEADER_LEN + answer.body.len() <= MAX_DATAGRAM);
        if answer.changed {
            changes += 1;
            assert!(matches!(
                op1,
                op::TX_RENAME | op::RX_RENAME | op::RX_SUBSCRIBE | op::RX_UNSUBSCRIBE
            ));
            assert_ne!(dev.state, before);
        } else {
            assert_eq!(dev.state, before);
        }
        assert_eq!(dev.state.problems(&dev.cfg), Vec::<String>::new());
    }
    assert!(changes > 100, "only {changes} renames went through");
    // Raw random datagrams: dropped or answered, never a panic.
    for _ in 0..2_000 {
        let len = rng.below(64);
        let mut raw = rng.bytes(len);
        if len >= HEADER_LEN && rng.below(2) == 0 {
            raw[2..4].copy_from_slice(&(len as u16).to_be_bytes());
        }
        let _ = dev.send(&raw);
        assert_eq!(dev.state.problems(&dev.cfg), Vec::<String>::new());
    }
}

/// A bounded receive: a mutant that never answers fails here, not by a hang.
async fn recv(sock: &UdpSocket) -> Vec<u8> {
    let mut buf = vec![0; 2048];
    let (n, _) = tokio::time::timeout(Duration::from_secs(5), sock.recv_from(&mut buf))
        .await
        .expect("an answer within 5 s")
        .unwrap();
    buf.truncate(n);
    buf
}

struct Served {
    addr: std::net::SocketAddr,
    device: Arc<Mutex<Device>>,
    flows: Arc<NoFlows>,
    changed: Arc<Notify>,
    rx_changes: Arc<Changes>,
    requests: Arc<crate::ctl::Requests>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    _dir: tempfile::TempDir,
}

async fn served() -> Served {
    let dir = tempfile::tempdir().unwrap();
    let device = Device::load(config_in(dir.path())).unwrap();
    let flows = Arc::new(NoFlows::new(&device));
    let device = Arc::new(Mutex::new(device));
    let changed = Arc::new(Notify::new());
    let rx_changes = Arc::new(Changes::default());
    let requests = Arc::new(crate::ctl::Requests::default());
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = sock.local_addr().unwrap();
    let task = tokio::spawn(serve(
        sock,
        device.clone(),
        flows.clone(),
        changed.clone(),
        rx_changes.clone(),
        requests.clone(),
    ));
    Served {
        addr,
        device,
        flows,
        changed,
        rx_changes,
        requests,
        task,
        _dir: dir,
    }
}

#[tokio::test]
async fn the_server_answers_two_peers_with_their_own_sequences() {
    let s = served().await;
    let a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    // Both send before either reads: the requests interleave at the server.
    for k in 0..20u16 {
        a.send_to(&request(CONTROLLER, 100 + k, op::COUNTS, 0, &[]), s.addr)
            .await
            .unwrap();
        b.send_to(&request(NETAUDIO, 900 + k, op::DEVICE_NAME, 0, &[]), s.addr)
            .await
            .unwrap();
    }
    let mut seen_a = Vec::new();
    let mut seen_b = Vec::new();
    for _ in 0..20 {
        let reply = recv(&a).await;
        let (h, body) = Header::parse(&reply).unwrap();
        assert_eq!(
            (h.start, h.op1, h.op2, body.len()),
            (CONTROLLER, op::COUNTS, STATUS_OK, 34)
        );
        seen_a.push(h.seq);
        let reply = recv(&b).await;
        let (h, body) = Header::parse(&reply).unwrap();
        assert_eq!(
            (h.start, h.op1, body),
            (NETAUDIO, op::DEVICE_NAME, &b"test-device\0"[..])
        );
        seen_b.push(h.seq);
    }
    seen_a.sort_unstable();
    seen_b.sort_unstable();
    assert_eq!(seen_a, (100..120).collect::<Vec<u16>>());
    assert_eq!(seen_b, (900..920).collect::<Vec<u16>>());
    // Every request counted for the status by port, peer (both on one
    // address: one peer) and opcode.
    let counted: Vec<(u16, usize, u16, u64)> = s
        .requests
        .counts()
        .received
        .iter()
        .map(|r| (r.port, r.peer, r.op, r.n))
        .collect();
    assert_eq!(
        counted,
        [(PORT, 1, op::COUNTS, 20), (PORT, 1, op::DEVICE_NAME, 20)]
    );
    s.task.abort();
}

#[tokio::test]
async fn the_server_saves_a_rename_and_signals_it() {
    let s = served().await;
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    // A datagram that is no request gets no answer; the next one does.
    peer.send_to(&[0x27, 0x29, 0, 99], s.addr).await.unwrap();
    peer.send_to(
        &request(CONTROLLER, 7, op::TX_RENAME, 0, &tx_rename(3, b"Mix 3")),
        s.addr,
    )
    .await
    .unwrap();
    let reply = recv(&peer).await;
    let (h, body) = Header::parse(&reply).unwrap();
    assert_eq!((h.seq, h.op2, body), (7, STATUS_OK, &[0u8, 0][..]));
    tokio::time::timeout(Duration::from_secs(5), s.changed.notified())
        .await
        .expect("the change is signalled");
    let state_file = {
        let device = s.device.lock().unwrap();
        assert_eq!(device.state.tx[2], "Mix 3");
        device.cfg.state_file.clone()
    };
    let saved = std::fs::read_to_string(&state_file).unwrap();
    assert!(saved.contains("Mix 3"), "{saved}");
    // A read changes nothing and signals nothing.
    peer.send_to(&request(CONTROLLER, 8, op::COUNTS, 0, &[]), s.addr)
        .await
        .unwrap();
    recv(&peer).await;
    let signalled = tokio::time::timeout(Duration::from_millis(200), s.changed.notified()).await;
    assert!(signalled.is_err(), "a read signalled a change");
    s.task.abort();
}

#[tokio::test]
async fn the_server_sends_nothing_for_an_opcode_inferno_does_not_answer() {
    let s = served().await;
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    // Dante Controller's 0x2204 and 0x2200 as captured, a request with op2
    // set, then the counts.
    for (seq, op1, op2, body) in [
        (1, 0x2204, 0, &DC_FIRST_PAGE[..]),
        (2, op::TX_FLOWS, 0, &DC_FIRST_PAGE[..]),
        (3, 0x2204, 0, &DC_FIRST_PAGE[..]),
        (4, op::COUNTS, 1, &[][..]),
        (5, op::COUNTS, 0, &[][..]),
    ] {
        peer.send_to(&request(CONTROLLER, seq, op1, op2, body), s.addr)
            .await
            .unwrap();
    }
    // The flow list and the counts are answered, in order, and nothing else.
    let reply = recv(&peer).await;
    let (h, body) = Header::parse(&reply).unwrap();
    assert_eq!(
        (h.seq, h.op1, h.op2, body.len()),
        (2, op::TX_FLOWS, STATUS_OK, 34)
    );
    let reply = recv(&peer).await;
    let (h, _) = Header::parse(&reply).unwrap();
    assert_eq!((h.seq, h.op1, h.op2), (5, op::COUNTS, STATUS_OK));
    let mut buf = [0u8; 64];
    let more = tokio::time::timeout(Duration::from_millis(200), peer.recv_from(&mut buf)).await;
    assert!(more.is_err(), "an unanswered request was answered");
    s.task.abort();
}
