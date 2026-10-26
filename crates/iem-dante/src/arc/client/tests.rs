use super::*;

use std::collections::BTreeMap;
use std::net::UdpSocket;
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::arc::{ArcOutcome, Live, handle};
use crate::ctl::Sent;
use crate::device::{DeviceConfig, DeviceState};
use crate::media::tx::TxTable;
use crate::subscribe::Manager;
use crate::wire::{Body, Header, STATUS_ERROR};

const TEST_DEVICE: &str = include_str!("../../../../../config/test-dante-device.toml");
/// The IEM card's replies, names and named ids replaced (the file's header).
const CARD_PAGES: &str = include_str!("card-pages.txt");
const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;
/// Every wait in these tests ends.
const BOUND: Duration = Duration::from_secs(5);

/// The synthetic ids the fixture's 0x2010 page names, in its order, and the
/// byte length of each name (kept from the card).
const NAMED_IDS: [u16; 24] = [
    41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 101, 102, 103, 7, 8,
    100,
];
const NAME_LENGTHS: [usize; 24] = [
    16, 16, 16, 16, 16, 16, 15, 15, 15, 15, 16, 16, 15, 15, 15, 15, 15, 15, 19, 19, 19, 12, 10, 10,
];

/// The fixture's synthetic name of its `k`-th named channel (from 0).
fn card_name(k: usize) -> String {
    let mut name = format!("Name {:02}", k + 1);
    name.extend(std::iter::repeat_n('_', NAME_LENGTHS[k] - name.len()));
    name
}

/// The card's replies by (op1, first item).
fn card_replies() -> BTreeMap<(u16, u16), Vec<u8>> {
    CARD_PAGES
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let mut parts = l.split(' ');
            let op1 =
                u16::from_str_radix(parts.next().unwrap().trim_start_matches("0x"), 16).unwrap();
            let start: u16 = parts.next().unwrap().parse().unwrap();
            let hex = parts.next().unwrap();
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            ((op1, start), bytes)
        })
        .collect()
}

/// An exchange that answers from recorded replies (whole datagrams, their
/// headers checked) and keeps what it was asked.
struct Replay {
    replies: BTreeMap<(u16, u16), Vec<u8>>,
    asked: Vec<(u16, u16)>,
}

impl Exchange for Replay {
    fn ask(&mut self, op1: u16, start: u16) -> Result<(u16, Vec<u8>), ReadError> {
        self.asked.push((op1, start));
        let d = self
            .replies
            .get(&(op1, start))
            .ok_or(ReadError::Timeout { op: op1 })?;
        let (h, body) = Header::parse(d).expect("a whole reply");
        assert_eq!(h.op1, op1);
        Ok((h.op2, body.to_vec()))
    }
}

#[test]
fn the_card_s_pages_read_as_its_128_channels_in_6_requests() {
    let mut ex = Replay {
        replies: card_replies(),
        asked: Vec::new(),
    };
    let dir = read_directory(&mut ex).unwrap();
    assert_eq!(
        ex.asked,
        [
            (op::COUNTS, 1),
            (op::TX_CHANNELS, 1),
            (op::TX_CHANNELS, 33),
            (op::TX_CHANNELS, 65),
            (op::TX_CHANNELS, 97),
            (op::TX_NAMES, 1),
        ]
    );
    let channels = dir.channels();
    assert_eq!(channels.len(), 128);
    for (i, ch) in channels.iter().enumerate() {
        let id = u16::try_from(i + 1).unwrap();
        let named = NAMED_IDS.iter().position(|&n| n == id).map(card_name);
        assert_eq!(
            ch,
            &TxChannel {
                id,
                rate: 96_000,
                bits: 24,
                default: format!("{id:02}"),
                name: named,
            }
        );
    }
    assert_eq!(dir.named(), 24);
    // By its own name, case aside; by its default name too.
    assert_eq!(dir.find(&card_name(0)).unwrap().id, 41);
    assert_eq!(dir.find(&card_name(23).to_uppercase()).unwrap().id, 100);
    assert_eq!(dir.find("41").unwrap().id, 41);
    assert_eq!(dir.find("05").unwrap().id, 5);
    assert_eq!(dir.find("128").unwrap().id, 128);
    assert_eq!(dir.find("129"), None);
    assert_eq!(dir.find("5"), None);
}

#[test]
fn the_card_s_first_page_says_more_and_counts_32_of_its_33_strings() {
    let replies = card_replies();
    let (h, body) = Header::parse(&replies[&(op::TX_CHANNELS, 1)]).unwrap();
    assert_eq!(h.op2, STATUS_MORE);
    let page = tx_channels(h.op2, body).unwrap();
    assert!(page.more);
    assert_eq!((page.slots, page.count, page.items.len()), (32, 32, 32));
    assert_eq!(page.items.last().unwrap().default, "32");
    let (h, body) = Header::parse(&replies[&(op::TX_CHANNELS, 97)]).unwrap();
    assert_eq!(h.op2, STATUS_OK);
    assert!(!tx_channels(h.op2, body).unwrap().more);
    // 0x2010 past the named channels: an empty page that says done.
    let (h, body) = Header::parse(&replies[&(op::TX_NAMES, 33)]).unwrap();
    let page = tx_names(h.op2, body).unwrap();
    assert_eq!((page.more, page.slots, page.count), (false, 32, 0));
}

/// A device of `tx` TX channels with 31-byte names, renamed every third.
fn long_device(tx: usize) -> (DeviceConfig, DeviceState) {
    let cfg = DeviceConfig {
        tx: (1..=tx)
            .map(|n| format!("T{n:02}{}", "x".repeat(28)))
            .collect(),
        ..DeviceConfig::from_toml(TEST_DEVICE).unwrap()
    };
    let mut state = DeviceState::from_config(&cfg);
    for (i, name) in state.tx.iter_mut().enumerate().step_by(3) {
        *name = format!("N{:02}{}", i + 1, "y".repeat(28));
    }
    (cfg, state)
}

/// Our own ARC server as the device read.
struct Ours {
    cfg: DeviceConfig,
    state: DeviceState,
    seq: u16,
    asked: Vec<(u16, u16)>,
}

impl Exchange for Ours {
    fn ask(&mut self, op1: u16, start: u16) -> Result<(u16, Vec<u8>), ReadError> {
        self.asked.push((op1, start));
        self.seq += 1;
        let tx = TxTable::new(0);
        let rx = Manager::new(
            &self.cfg,
            &self.cfg.name,
            LOOPBACK,
            [0; crate::subscribe::MAX_FLOWS],
        );
        let live = Live { tx: &tx, rx: &rx };
        let req = page_request(self.seq, op1, start);
        let ArcOutcome::Reply(d) = handle(&mut self.state, &self.cfg, &live, &req) else {
            panic!("our server answers {op1:#06x}");
        };
        let (status, body) = reply_to(&d, self.seq, op1).expect("its reply");
        Ok((status, body.to_vec()))
    }
}

#[test]
fn our_own_server_s_pages_read_back_as_its_channels() {
    let (cfg, state) = long_device(70);
    let mut ours = Ours {
        cfg: cfg.clone(),
        state: state.clone(),
        seq: 0,
        asked: Vec::new(),
    };
    let dir = read_directory(&mut ours).unwrap();
    let want: Vec<TxChannel> = (0..70)
        .map(|i| TxChannel {
            id: u16::try_from(i + 1).unwrap(),
            rate: cfg.sample_rate,
            bits: u32::from(cfg.encoding),
            default: cfg.tx[i].clone(),
            name: Some(state.tx[i].clone()),
        })
        .collect();
    assert_eq!(dir.channels(), want);
    // 31-byte names end pages early (0x8112 at the soft limit): more pages
    // than 70 / 32, each from the item after the last one counted.
    let pages = |op1| ours.asked.iter().filter(|(o, _)| *o == op1).count();
    assert!(pages(op::TX_CHANNELS) > 3, "{:?}", ours.asked);
    assert!(pages(op::TX_NAMES) > 3, "{:?}", ours.asked);
    assert_eq!(dir.find(&state.tx[3]).unwrap().id, 4);
    assert_eq!(dir.find(&cfg.tx[3]).unwrap().id, 4);
}

/// A 0x2000 page body of `slots` slots listing `items` (id, rate, bits,
/// name), each with a format descriptor of its own.
fn channels_body(slots: usize, items: &[(u16, u32, u16, &str)]) -> Vec<u8> {
    let mut out = Body::new();
    out.u8(slots as u8);
    out.u8(items.len() as u8);
    let table = out.offset();
    out.zeros(slots * CHANNEL_ITEM);
    for (i, &(id, rate, bits, name)) in items.iter().enumerate() {
        let format = out.offset();
        out.u32(rate);
        out.u8(1);
        out.u8(1);
        out.u16(bits);
        out.zeros(8);
        let at = out.cstr(name);
        let d: Vec<u8> = [id, 7, format, at]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        out.set_bytes(table + (i * CHANNEL_ITEM) as u16, &d);
    }
    out.into_vec()
}

/// A 0x2010 page body of `slots` slots naming `items` (id, name).
fn names_body(slots: usize, items: &[(u16, &str)]) -> Vec<u8> {
    let mut out = Body::new();
    out.u8(slots as u8);
    out.u8(items.len() as u8);
    let table = out.offset();
    out.zeros(slots * NAME_ITEM);
    for (i, &(id, name)) in items.iter().enumerate() {
        let at = out.cstr(name);
        let d: Vec<u8> = [i as u16 + 1, id, at]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        out.set_bytes(table + (i * NAME_ITEM) as u16, &d);
    }
    out.into_vec()
}

/// A counts body saying `tx` TX channels.
fn counts_body(tx: u16) -> Vec<u8> {
    let mut out = Body::new();
    out.u16(0x0010);
    out.u16(tx);
    out.u16(2);
    out.into_vec()
}

/// A scripted page: (op1, first item) → (status, body).
type Scripted = ((u16, u16), (u16, Vec<u8>));

/// An exchange answering from a script: (status, body) by (op1, first
/// item); anything else is no reply.
struct Script {
    pages: BTreeMap<(u16, u16), (u16, Vec<u8>)>,
    asked: Vec<(u16, u16)>,
}

impl Script {
    fn new(pages: impl IntoIterator<Item = Scripted>) -> Self {
        Script {
            pages: pages.into_iter().collect(),
            asked: Vec::new(),
        }
    }
}

impl Exchange for Script {
    fn ask(&mut self, op1: u16, start: u16) -> Result<(u16, Vec<u8>), ReadError> {
        self.asked.push((op1, start));
        self.pages
            .get(&(op1, start))
            .cloned()
            .ok_or(ReadError::Timeout { op: op1 })
    }
}

fn ch(id: u16) -> (u16, u32, u16, &'static str) {
    (
        id,
        48_000,
        24,
        ["a", "b", "c", "d", "e", "f", "g", "h", "i"][usize::from(id) % 9],
    )
}

#[test]
fn a_full_page_that_says_done_asks_for_the_next_while_channels_are_missing() {
    let first: Vec<_> = (1..=4).map(ch).collect();
    let mut s = Script::new([
        ((op::COUNTS, 1), (STATUS_OK, counts_body(6))),
        ((op::TX_CHANNELS, 1), (STATUS_OK, channels_body(4, &first))),
        (
            (op::TX_CHANNELS, 5),
            (STATUS_OK, channels_body(4, &[ch(5), ch(6)])),
        ),
        (
            (op::TX_NAMES, 1),
            (STATUS_OK, names_body(4, &[(2, "two"), (6, "six")])),
        ),
    ]);
    let dir = read_directory(&mut s).unwrap();
    assert_eq!(
        s.asked,
        [
            (op::COUNTS, 1),
            (op::TX_CHANNELS, 1),
            (op::TX_CHANNELS, 5),
            (op::TX_NAMES, 1)
        ]
    );
    let ids: Vec<u16> = dir.channels().iter().map(|c| c.id).collect();
    assert_eq!(ids, [1, 2, 3, 4, 5, 6]);
    assert_eq!(dir.find("six").unwrap().id, 6);
    assert_eq!(dir.find("TWO").unwrap().rate, 48_000);
    assert_eq!(dir.named(), 2);
    // A full page once every channel is counted ends the list.
    let mut s = Script::new([
        ((op::COUNTS, 1), (STATUS_OK, counts_body(4))),
        ((op::TX_CHANNELS, 1), (STATUS_OK, channels_body(4, &first))),
        ((op::TX_NAMES, 1), (STATUS_OK, names_body(4, &[]))),
    ]);
    assert_eq!(read_directory(&mut s).unwrap().channels().len(), 4);
    assert_eq!(s.asked.len(), 3);
}

#[test]
fn more_follow_from_the_item_after_the_page_s_count_until_a_page_says_done() {
    // Pages of 4 slots that end early (as ours at the soft limit).
    let mut s = Script::new([
        ((op::COUNTS, 1), (STATUS_OK, counts_body(5))),
        (
            (op::TX_CHANNELS, 1),
            (STATUS_MORE, channels_body(4, &[ch(1), ch(2)])),
        ),
        (
            (op::TX_CHANNELS, 3),
            (STATUS_MORE, channels_body(4, &[ch(3)])),
        ),
        (
            (op::TX_CHANNELS, 4),
            (STATUS_OK, channels_body(4, &[ch(4), ch(5)])),
        ),
        (
            (op::TX_NAMES, 1),
            (STATUS_MORE, names_body(4, &[(1, "one")])),
        ),
        (
            (op::TX_NAMES, 2),
            (STATUS_OK, names_body(4, &[(5, "five")])),
        ),
    ]);
    let dir = read_directory(&mut s).unwrap();
    assert_eq!(dir.channels().len(), 5);
    assert_eq!(dir.find("five").unwrap().id, 5);
    assert_eq!(dir.find("one").unwrap().id, 1);
    assert_eq!(s.asked.len(), 6);
}

#[test]
fn a_page_of_none_or_a_list_past_max_pages_ends_it() {
    // "More" with nothing counted ends the list (no loop on the same page).
    let mut s = Script::new([
        ((op::COUNTS, 1), (STATUS_OK, counts_body(8))),
        (
            (op::TX_CHANNELS, 1),
            (STATUS_MORE, channels_body(4, &[ch(1)])),
        ),
        ((op::TX_CHANNELS, 2), (STATUS_MORE, channels_body(4, &[]))),
        ((op::TX_NAMES, 1), (STATUS_MORE, names_body(4, &[]))),
    ]);
    assert_eq!(read_directory(&mut s).unwrap().channels().len(), 1);
    assert_eq!(s.asked.len(), 4);
    // A device that always says more, one item a page: MAX_PAGES a list.
    let mut pages = vec![((op::COUNTS, 1), (STATUS_OK, counts_body(u16::MAX)))];
    for start in 1..=100u16 {
        pages.push((
            (op::TX_CHANNELS, start),
            (STATUS_MORE, channels_body(1, &[ch(start)])),
        ));
        pages.push((
            (op::TX_NAMES, start),
            (STATUS_MORE, names_body(1, &[(start, "n")])),
        ));
    }
    let mut s = Script::new(pages);
    let dir = read_directory(&mut s).unwrap();
    assert_eq!(dir.channels().len(), MAX_PAGES);
    assert_eq!(s.asked.len(), 1 + 2 * MAX_PAGES);
}

#[test]
fn a_device_without_tx_channels_is_read_in_one_request() {
    let mut s = Script::new([((op::COUNTS, 1), (STATUS_OK, counts_body(0)))]);
    assert_eq!(read_directory(&mut s).unwrap(), Directory::default());
    assert_eq!(s.asked, [(op::COUNTS, 1)]);
    // Channels whose descriptors do not read: the names are not asked.
    let mut s = Script::new([
        ((op::COUNTS, 1), (STATUS_OK, counts_body(1))),
        (
            (op::TX_CHANNELS, 1),
            (STATUS_OK, channels_body(1, &[(1, 0, 24, "a")])),
        ),
    ]);
    assert_eq!(read_directory(&mut s).unwrap(), Directory::default());
    assert_eq!(s.asked.len(), 2);
}

#[test]
fn a_refusal_no_reply_or_a_page_that_does_not_read_ends_the_read() {
    let ok = |body: Vec<u8>| (STATUS_OK, body);
    let cases: Vec<(Vec<Scripted>, ReadError)> = vec![
        (
            vec![((op::COUNTS, 1), (STATUS_ERROR, Vec::new()))],
            ReadError::Refused {
                op: op::COUNTS,
                status: STATUS_ERROR,
            },
        ),
        // Counts that say more are no counts.
        (
            vec![((op::COUNTS, 1), (STATUS_MORE, counts_body(4)))],
            ReadError::Refused {
                op: op::COUNTS,
                status: STATUS_MORE,
            },
        ),
        (
            vec![((op::COUNTS, 1), ok(vec![0, 0, 0]))],
            ReadError::Malformed { op: op::COUNTS },
        ),
        (
            vec![((op::COUNTS, 1), ok(counts_body(4)))],
            ReadError::Timeout {
                op: op::TX_CHANNELS,
            },
        ),
        (
            vec![
                ((op::COUNTS, 1), ok(counts_body(4))),
                ((op::TX_CHANNELS, 1), (0x0030, channels_body(4, &[ch(1)]))),
            ],
            ReadError::Refused {
                op: op::TX_CHANNELS,
                status: 0x0030,
            },
        ),
        // A count past the slots; a table past the body.
        (
            vec![
                ((op::COUNTS, 1), ok(counts_body(4))),
                ((op::TX_CHANNELS, 1), ok(vec![1, 2, 0, 0, 0, 0, 0, 0, 0, 0])),
            ],
            ReadError::Malformed {
                op: op::TX_CHANNELS,
            },
        ),
        (
            vec![
                ((op::COUNTS, 1), ok(counts_body(4))),
                (
                    (op::TX_CHANNELS, 1),
                    ok(vec![2, 1, 0, 1, 0, 7, 0, 30, 0, 40]),
                ),
            ],
            ReadError::Malformed {
                op: op::TX_CHANNELS,
            },
        ),
        // 4 slots of 8 bytes need 34: 20 bytes do not hold the table.
        (
            vec![
                ((op::COUNTS, 1), ok(counts_body(4))),
                ((op::TX_CHANNELS, 1), ok([vec![4, 1], vec![0; 18]].concat())),
            ],
            ReadError::Malformed {
                op: op::TX_CHANNELS,
            },
        ),
        (
            vec![
                ((op::COUNTS, 1), ok(counts_body(4))),
                ((op::TX_CHANNELS, 1), ok(channels_body(4, &[ch(1)]))),
                ((op::TX_NAMES, 1), ok(vec![4])),
            ],
            ReadError::Malformed { op: op::TX_NAMES },
        ),
        (
            vec![
                ((op::COUNTS, 1), ok(counts_body(4))),
                ((op::TX_CHANNELS, 1), ok(channels_body(4, &[ch(1)]))),
                ((op::TX_NAMES, 1), (STATUS_ERROR, Vec::new())),
            ],
            ReadError::Refused {
                op: op::TX_NAMES,
                status: STATUS_ERROR,
            },
        ),
    ];
    for (pages, want) in cases {
        let mut s = Script::new(pages);
        assert_eq!(read_directory(&mut s), Err(want), "{want}");
    }
}

#[test]
fn a_channel_whose_descriptor_does_not_read_is_counted_not_listed() {
    let items = [
        (0, 48_000, 24, "id 0"),
        (2, 0, 24, "rate 0"),
        (3, 48_000, 20, "20 bits"),
        (4, 48_000, 24, ""),
        (5, 48_000, 16, "ok 16"),
        (6, 44_100, 32, "ok 32"),
    ];
    let mut body = channels_body(8, &items);
    // A seventh item whose format lies in the header, an eighth whose name
    // lies past the body.
    body[1] = 8;
    let at = 2 + 6 * CHANNEL_ITEM;
    body[at..at + 8].copy_from_slice(&[0, 7, 0, 7, 0, 4, 0, 30]);
    let past = u16::try_from(HEADER_LEN + body.len()).unwrap();
    let format = u16::from_be_bytes([body[2 + 4], body[2 + 5]]);
    let at = 2 + 7 * CHANNEL_ITEM;
    body[at..at + 8]
        .copy_from_slice(&[[0, 8], [0, 7], format.to_be_bytes(), past.to_be_bytes()].concat());
    let page = tx_channels(STATUS_OK, &body).unwrap();
    assert_eq!(page.count, 8);
    let listed: Vec<(u16, u32, u32, &str)> = page
        .items
        .iter()
        .map(|c| (c.id, c.rate, c.bits, c.default.as_str()))
        .collect();
    assert_eq!(listed, [(5, 48_000, 16, "ok 16"), (6, 44_100, 32, "ok 32")]);
    // 0x2010: id 0 and an empty name are not listed.
    let page = tx_names(
        STATUS_MORE,
        &names_body(4, &[(0, "zero"), (2, ""), (3, "three")]),
    )
    .unwrap();
    assert!(page.more);
    assert_eq!(page.count, 3);
    assert_eq!(page.items, [(3, "three".to_string())]);
}

#[test]
fn a_directory_keeps_the_first_of_an_id_and_its_first_name() {
    let c = |id: u16, default: &str| TxChannel {
        id,
        rate: 48_000,
        bits: 24,
        default: default.to_string(),
        name: None,
    };
    let dir = Directory::new(
        vec![c(1, "a"), c(2, "b"), c(1, "again")],
        &[(2, "B two".into()), (2, "later".into()), (9, "none".into())],
    );
    assert_eq!(dir.channels().len(), 2);
    assert_eq!(dir.channels()[0].default, "a");
    assert_eq!(dir.channels()[1].name.as_deref(), Some("B two"));
    assert_eq!(dir.find("again"), None);
    assert_eq!(dir.find("later"), None);
    // A channel's own name wins over another's default name.
    let dir = Directory::new(vec![c(1, "x"), c(2, "y")], &[(2, "X".into())]);
    assert_eq!(dir.find("x").unwrap().id, 2);
    assert_eq!(dir.find("y").unwrap().id, 2);
}

#[test]
fn the_request_is_netaudio_s_page_read_and_only_its_reply_is_taken() {
    assert_eq!(
        page_request(0x5105, op::TX_CHANNELS, 33),
        [
            0x27, 0xff, 0, 16, 0x51, 0x05, 0x20, 0x00, 0, 0, 0, 1, 0, 33, 0, 0
        ]
    );
    let reply = |seq: u16, op1: u16, status: u16| request(START, seq, op1, status, &[9, 8]);
    assert_eq!(
        reply_to(&reply(7, 0x2000, STATUS_MORE), 7, 0x2000),
        Some((STATUS_MORE, &[9u8, 8][..]))
    );
    assert_eq!(reply_to(&reply(8, 0x2000, STATUS_OK), 7, 0x2000), None);
    assert_eq!(reply_to(&reply(7, 0x2010, STATUS_OK), 7, 0x2000), None);
    // A request (status 0) and a datagram shorter than the header.
    assert_eq!(reply_to(&reply(7, 0x2000, 0), 7, 0x2000), None);
    assert_eq!(reply_to(&reply(7, 0x2000, 1)[..9], 7, 0x2000), None);
    assert_eq!(
        ReadError::Refused {
            op: 0x2000,
            status: 0xffff
        }
        .to_string(),
        "0x2000: refused with status 0xffff"
    );
    let labels: Vec<&str> = [
        ReadError::Io {
            op: 1,
            kind: io::ErrorKind::Other,
        },
        ReadError::Timeout { op: 1 },
        ReadError::Refused { op: 1, status: 2 },
        ReadError::Malformed { op: 1 },
        ReadError::Stopped,
    ]
    .iter()
    .map(ReadError::label)
    .collect();
    assert_eq!(
        labels,
        ["socket", "no_reply", "refused", "malformed", "stopped"]
    );
}

/// The requests a stand-in device got: (op1, first item).
type Got = Arc<std::sync::Mutex<Vec<(u16, u16)>>>;

/// A stand-in device's ARC on loopback: each request (its header and first
/// item) is answered by `answer` (any datagrams, none at all); it ends
/// after `BOUND` without one. Returns its address and the requests it got.
fn device(
    answer: impl Fn(&Header, u16) -> Vec<Vec<u8>> + Send + 'static,
) -> (SocketAddrV4, Got, JoinHandle<()>) {
    let sock = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    sock.set_read_timeout(Some(BOUND)).unwrap();
    let SocketAddr::V4(at) = sock.local_addr().unwrap() else {
        panic!("IPv4")
    };
    let got = Arc::new(std::sync::Mutex::new(Vec::new()));
    let g = Arc::clone(&got);
    let thread = std::thread::spawn(move || {
        let mut buf = [0u8; 2048];
        while let Ok((n, from)) = sock.recv_from(&mut buf) {
            let (h, body) = Header::parse(&buf[..n]).unwrap();
            let start = read_u16(body, 2).unwrap();
            g.lock().unwrap().push((h.op1, start));
            for d in answer(&h, start) {
                sock.send_to(&d, from).unwrap();
            }
        }
    });
    (at, got, thread)
}

/// The card's reply to (op1, first item), as an answer to `h`.
fn card_answer(replies: &BTreeMap<(u16, u16), Vec<u8>>, h: &Header, start: u16) -> Vec<u8> {
    let mut d = replies[&(h.op1, start)].clone();
    d[4..6].copy_from_slice(&h.seq.to_be_bytes());
    d
}

#[test]
fn a_read_over_udp_takes_only_its_replies_and_counts_every_request() {
    let replies = card_replies();
    let (at, got, _thread) = device(move |h, start| {
        // Another sequence and another opcode first: skipped.
        let mut other_seq = card_answer(&replies, h, start);
        other_seq[4..6].copy_from_slice(&h.seq.wrapping_add(9).to_be_bytes());
        let mut other_op = card_answer(&replies, h, start);
        other_op[6..8].copy_from_slice(&0x3000u16.to_be_bytes());
        vec![other_seq, other_op, card_answer(&replies, h, start)]
    });
    let seq = AtomicU16::new(1);
    let requests = Requests::default();
    let stop = AtomicBool::new(false);
    let mut ex = Udp::open(LOOPBACK, at, &seq, BOUND, &requests, &stop).unwrap();
    let dir = read_directory(&mut ex).unwrap();
    assert_eq!(dir.channels().len(), 128);
    assert_eq!(dir.named(), 24);
    assert_eq!((ex.sent, ex.peer), (6, 1));
    assert_eq!(seq.load(Ordering::Relaxed), 7);
    assert_eq!(
        *got.lock().unwrap(),
        [
            (op::COUNTS, 1),
            (op::TX_CHANNELS, 1),
            (op::TX_CHANNELS, 33),
            (op::TX_CHANNELS, 65),
            (op::TX_CHANNELS, 97),
            (op::TX_NAMES, 1)
        ]
    );
    assert_eq!(
        requests.counts().sent,
        [
            Sent {
                peer: 1,
                op: op::COUNTS,
                n: 1
            },
            Sent {
                peer: 1,
                op: op::TX_CHANNELS,
                n: 4
            },
            Sent {
                peer: 1,
                op: op::TX_NAMES,
                n: 1
            },
        ]
    );
}

#[test]
fn a_silent_device_ends_the_request_at_its_wait_and_a_stopped_read_sends_nothing() {
    let (at, got, _thread) = device(|_, _| Vec::new());
    let seq = AtomicU16::new(1);
    let requests = Requests::default();
    let stop = AtomicBool::new(false);
    let wait = Duration::from_millis(150);
    let mut ex = Udp::open(LOOPBACK, at, &seq, wait, &requests, &stop).unwrap();
    let t = Instant::now();
    assert_eq!(
        read_directory(&mut ex),
        Err(ReadError::Timeout { op: op::COUNTS })
    );
    let took = t.elapsed();
    assert!(took >= wait && took < BOUND, "{took:?}");
    assert_eq!(ex.sent, 1);
    stop.store(true, Ordering::Relaxed);
    assert_eq!(ex.ask(op::COUNTS, 1), Err(ReadError::Stopped));
    assert_eq!(ex.sent, 1);
    let deadline = Instant::now() + BOUND;
    while got.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "the request never came");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(got.lock().unwrap().len(), 1);
    assert_eq!(requests.counts().sent.len(), 1);
}

#[test]
fn a_closed_port_is_a_socket_error_at_once() {
    let closed = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    let SocketAddr::V4(at) = closed.local_addr().unwrap() else {
        panic!("IPv4")
    };
    drop(closed);
    let seq = AtomicU16::new(1);
    let requests = Requests::default();
    let stop = AtomicBool::new(false);
    let mut ex = Udp::open(LOOPBACK, at, &seq, BOUND, &requests, &stop).unwrap();
    let t = Instant::now();
    let err = read_directory(&mut ex).unwrap_err();
    assert_eq!(
        err,
        ReadError::Io {
            op: op::COUNTS,
            kind: io::ErrorKind::ConnectionRefused
        }
    );
    assert!(t.elapsed() < BOUND);
}
