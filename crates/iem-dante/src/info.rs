// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from inferno (https://github.com/teodly/inferno at 9767558):
// inferno_aoip/src/device_server/info_mcast_server.rs (the requests it
// answers, the replies' bodies, the heartbeat, the groups and ports) and
// inferno_aoip/src/protocol/mcast.rs (the 32-byte header), Copyright (C)
// Teo and the inferno contributors, GPL-3.0-or-later OR AGPL-3.0-or-later,
// taken under GPL-3.0-or-later. Changes Copyright (C) 2026 iemmixer
// contributors.

//! Dante's info requests and heartbeat (#72 phase A): Dante Controller
//! asks a device it found over mDNS and CMC for its board info on UDP
//! [`REQUEST_PORT`] (8700), and goes on to ARC only once that is answered.
//!
//! The server listens at the device's address only (as inferno: no group
//! joined) and sends everything to multicast groups: the replies to
//! [`INFO_GROUP`]:[`INFO_PORT`] (224.0.0.231:8702), where Dante Controller
//! listens, and a heartbeat every second to
//! [`HEARTBEAT_GROUP`]:[`HEARTBEAT_PORT`] (224.0.0.233:8708). At start it
//! sends the board and the product info unasked, as inferno does.
//!
//! Every packet has a 32-byte big-endian header, then its body:
//!
//! | bytes | field | |
//! |---|---|---|
//! | 0–1 | start code | 0xffff info, 0xfffe heartbeat |
//! | 2–3 | length | the whole datagram, header included |
//! | 4–5 | sequence | the sender's own: ours counts from 1 over every packet it sends |
//! | 6–7 | process id | 0 |
//! | 8–15 | device id | ours: [`crate::cmc::device_id`]; Dante Controller's: its MAC, then 0 0 |
//! | 16–23 | vendor | `Audinate` (inferno's "Audinate Dante-compatible", cut to 8) |
//! | 24–31 | opcode | |
//!
//! | request opcode | reply opcode | reply body |
//! |---|---|---|
//! | `07 xx 00 61 00 00 00 00` | `07 2a 00 60 00 00 00 00` | board info, 200 bytes ([`board_info_body`]) |
//! | `07 xx 00 c1 00 00 00 00` | `07 2a 00 c0 00 00 00 00` | product info, 336 bytes ([`product_info_body`]) |
//! | `07 xx 00 13 00 00 00 xx` | `07 2a 00 11 00 00 00 00` | network info, 64 bytes ([`network_info_body`]) |
//! | `07 xx 00 77 00 00 00 xx` | `07 2a 00 78 00 00 00 00` | `00 00 00 03 00 00 00 00` |
//! | `07 xx 00 21 00 00 00 xx` | none | clock stats: inferno sends them only with a clock's frequency offset, and phase A has no clock |
//!
//! Any other opcode gets no reply and is logged by its opcode; a datagram
//! shorter than the header is dropped. As inferno, neither the start code
//! nor the length field of a request is checked, and the replies carry the
//! device's own sequence, not the request's.
//!
//! Unlike inferno, the replies are bounded ([`limit`]): each goes to a group
//! every Dante device on the network hears, so at most
//! [`limit::PER_SPAN`] replies of a kind go out in any second; the rest are
//! dropped and counted in one warning a minute.
//!
//! **The channel-change notification** (`07 2a 01 02 00 00 00 00`, to the
//! info group, unasked): a 16-bit length, then a bitmask of RX channels
//! (bit `n mod 8` of byte `n / 8` for channel `n` from 0, at least one
//! byte; inferno's `make_channel_change_notification`). Dante Controller
//! reads ARC 0x3000 again for them. It goes out when an RX channel is
//! renamed, or its subscription or its status changes ([`Changes`]);
//! changes are coalesced, so one notification names every channel that
//! changed since the last, and it is bounded as a kind of its own (at most
//! [`limit::PER_SPAN`] a second; a change held back goes with the next).
//! inferno also sends an empty one after an RX rename that renamed
//! nothing; we send none without a channel.
//!
//! **The heartbeat**'s opcode is `00 08 00 01 10 00 00 00`. While the
//! media clock is not ready its body is empty, as inferno's before its
//! clock has an offset. Once it is ready ([`Beat::records`]), the body
//! holds inferno's records ([`heartbeat_body`]), each `length, type, 4,
//! content length, the packet's sequence, 0`, then:
//!
//! - 0x8001: dantesync's frequency adjustment, ppb (`i32`): what our clock
//!   follows, never a lock of our own;
//! - 0x8002: the peaks, one byte a channel (`−40·log10` of the peak's part
//!   of full scale, 0 to 255: [`peak_byte`]), TX channels then RX channels,
//!   padded to 4 bytes;
//! - 0x8003: our rate and each RX flow slot's largest lateness since the
//!   last beat, samples (0 for a slot without a granted flow).
//!
//! Clock stats (`07 xx 00 21`) stay unanswered: inferno sends them only
//! with a grandmaster identity read from its PTP daemon's files, and our
//! device has none of its own (the box's dantesync is the PTP follower).
//!
//! Logs name opcodes, sequences and ports, never an address or a MAC.

use std::collections::BTreeSet;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, warn};

use crate::clock::PtpClock;
use crate::cmc::{PROCESS_ID, device_id};
use crate::ctl::{Requests, info_op};
use crate::device::DeviceConfig;
use crate::media::rx::{MAX_FLOWS, RxShared};
use crate::media::tx::TxShared;
use crate::subscribe::Manager;
use limit::Limit;

pub mod limit;

/// The info request port (UDP), at the device's address.
pub const REQUEST_PORT: u16 = 8700;
/// The group and port the info replies go to (Dante's control and
/// monitoring group).
pub const INFO_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 231);
pub const INFO_PORT: u16 = 8702;
/// The group and port the heartbeat goes to.
pub const HEARTBEAT_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 233);
pub const HEARTBEAT_PORT: u16 = 8708;
/// The heartbeat's period (inferno's).
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(1);
/// The header's size.
pub const HEADER_LEN: usize = 32;
/// The start code of the info packets.
pub const START_INFO: u16 = 0xffff;
/// The start code of the heartbeat.
pub const START_HEARTBEAT: u16 = 0xfffe;
/// The vendor field.
pub const VENDOR: [u8; 8] = *b"Audinate";

/// The opcodes the device sends.
pub mod op {
    pub const BOARD_INFO: [u8; 8] = [0x07, 0x2a, 0x00, 0x60, 0x00, 0x00, 0x00, 0x00];
    pub const PRODUCT_INFO: [u8; 8] = [0x07, 0x2a, 0x00, 0xc0, 0x00, 0x00, 0x00, 0x00];
    pub const NETWORK_INFO: [u8; 8] = [0x07, 0x2a, 0x00, 0x11, 0x00, 0x00, 0x00, 0x00];
    /// The answer to 0x77, whose meaning inferno does not name.
    pub const ANSWER_0077: [u8; 8] = [0x07, 0x2a, 0x00, 0x78, 0x00, 0x00, 0x00, 0x00];
    /// RX channels changed (sent unasked).
    pub const CHANNEL_CHANGE: [u8; 8] = [0x07, 0x2a, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00];
    pub const HEARTBEAT: [u8; 8] = [0x00, 0x08, 0x00, 0x01, 0x10, 0x00, 0x00, 0x00];
}

/// The heartbeat records' types.
pub mod record {
    pub const FREQUENCY: u16 = 0x8001;
    pub const PEAKS: u16 = 0x8002;
    pub const RX_LATENESS: u16 = 0x8003;
}

pub const BOARD_INFO_LEN: usize = 200;
pub const PRODUCT_INFO_LEN: usize = 336;
pub const NETWORK_INFO_LEN: usize = 64;
/// inferno's answer to 0x77.
const ANSWER_0077_BODY: [u8; 8] = [0, 0, 0, 3, 0, 0, 0, 0];
/// The board info's firmware, hardware and boot versions (inferno's).
const FIRMWARE_VERSION: [u8; 4] = [4, 1, 0, 6];
const HARDWARE_VERSION: [u8; 4] = [4, 1, 0, 3];
const BOOT_VERSION: [u8; 4] = [1, 0, 0, 0];
/// The board info's feature flag byte 0x16: it has a manufacturer name.
const FEATURE_MANUFACTURER: u8 = 0x10;
/// The board info's byte 0xbb (inferno: "if 0, device is flooded with info
/// multicast requests around 1 per second").
const BYTE_BB: u8 = 0x1f;
/// A datagram read from the socket (inferno's MTU-sized buffer); only its
/// header is read.
const RECV_BUFFER: usize = 1500;

/// The interface's facts the network info carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetInfo {
    /// The interface's MAC address.
    pub mac: [u8; 6],
    /// The device's address on it.
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    /// The default gateway in the interface's subnet, 0.0.0.0 for none.
    pub gateway: Ipv4Addr,
    /// The link's speed in Mbit/s, 0 when unknown.
    pub link_speed: u16,
}

/// What the info messages say about the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The device id, as CMC and the mDNS `id` entry name it.
    pub device_id: [u8; 8],
    /// The config's `manufacturer`.
    pub manufacturer: String,
    /// The config's `model`: the board name and the model name, as ARC
    /// 0x1003 and mDNS `router_info` name it.
    pub model: String,
    /// The product version: the workspace version's major, minor and
    /// patch (a big-endian u16), as inferno sends its crate's.
    pub version: [u8; 4],
    pub net: NetInfo,
}

impl Identity {
    pub fn new(cfg: &DeviceConfig, net: NetInfo) -> Self {
        Self {
            device_id: device_id(net.ip),
            manufacturer: cfg.manufacturer.clone(),
            model: cfg.model.clone(),
            version: product_version(),
            net,
        }
    }
}

/// The workspace version as the product info carries it.
fn product_version() -> [u8; 4] {
    let major = env!("CARGO_PKG_VERSION_MAJOR").parse().unwrap_or(0);
    let minor = env!("CARGO_PKG_VERSION_MINOR").parse().unwrap_or(0);
    let [hi, lo] = env!("CARGO_PKG_VERSION_PATCH")
        .parse::<u16>()
        .unwrap_or(0)
        .to_be_bytes();
    [major, minor, hi, lo]
}

/// What a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 0x61: answered with the board info.
    BoardInfo,
    /// 0xc1: answered with the product info.
    ProductInfo,
    /// 0x13: answered with the network info.
    NetworkInfo,
    /// 0x77: answered with [`op::ANSWER_0077`].
    Op0077,
    /// 0x21: clock stats, which a device without a PTP daemon of its own
    /// does not send.
    ClockStats,
    /// Anything else: no reply.
    Unknown,
    /// No request: the channel-change notification, sent unasked and
    /// bounded as a kind of its own.
    ChannelChange,
}

/// A request received on [`REQUEST_PORT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    pub seq: u16,
    pub opcode: [u8; 8],
    pub kind: Kind,
}

/// The request in `datagram`, `None` when it is shorter than the header.
pub fn parse_request(datagram: &[u8]) -> Option<Request> {
    let (head, _) = datagram.split_first_chunk::<HEADER_LEN>()?;
    let seq = u16::from_be_bytes([head[4], head[5]]);
    let mut opcode = [0u8; 8];
    opcode.copy_from_slice(&head[24..32]);
    let kind = match opcode {
        [0x07, _, 0x00, 0x61, 0, 0, 0, 0] => Kind::BoardInfo,
        [0x07, _, 0x00, 0xc1, 0, 0, 0, 0] => Kind::ProductInfo,
        [0x07, _, 0x00, 0x21, 0, 0, 0, _] => Kind::ClockStats,
        [0x07, _, 0x00, 0x13, 0, 0, 0, _] => Kind::NetworkInfo,
        [0x07, _, 0x00, 0x77, 0, 0, 0, _] => Kind::Op0077,
        _ => Kind::Unknown,
    };
    Some(Request { seq, opcode, kind })
}

/// inferno's `write_str_to_buffer`: at most `max` bytes of `s` at `at`,
/// no terminator (the buffer is zeros).
fn put_str(buf: &mut [u8], at: usize, max: usize, s: &str) {
    let bytes = &s.as_bytes()[..s.len().min(max)];
    buf[at..at + bytes.len()].copy_from_slice(bytes);
}

/// The board info: firmware, hardware and boot versions, the board name
/// (cut to 8 bytes at 0x0c and to 16 at 0x38) and inferno's flags.
pub fn board_info_body(id: &Identity) -> [u8; BOARD_INFO_LEN] {
    let mut c = [0u8; BOARD_INFO_LEN];
    c[0x00..0x04].copy_from_slice(&FIRMWARE_VERSION);
    c[0x04..0x08].copy_from_slice(&HARDWARE_VERSION);
    put_str(&mut c, 0x0c, 8, &id.model);
    // Feature flags 0x14-0x17: only "has a manufacturer name" (no AES67,
    // no lock, no static addressing, no identify, rate, reboot or reset).
    c[0x16] = FEATURE_MANUFACTURER;
    c[0x23] = 2;
    c[0x27] = 1;
    c[0x28..0x2c].copy_from_slice(&BOOT_VERSION);
    put_str(&mut c, 0x38, 16, &id.model);
    c[0xbb] = BYTE_BB;
    c
}

/// The product info: the manufacturer (8 bytes at 0, 16 at 0x2c), the
/// board name (8 at 8), the version at 0x1c, the model name (16 at 0xac).
pub fn product_info_body(id: &Identity) -> [u8; PRODUCT_INFO_LEN] {
    let mut c = [0u8; PRODUCT_INFO_LEN];
    put_str(&mut c, 0x00, 8, &id.manufacturer);
    put_str(&mut c, 0x08, 8, &id.model);
    c[0x1c..0x20].copy_from_slice(&id.version);
    put_str(&mut c, 0x2c, 16, &id.manufacturer);
    put_str(&mut c, 0xac, 16, &id.model);
    c
}

/// The network info: `00 01 00 00 00 00`, the link speed, 1, the MAC, the
/// address, the netmask, the gateway twice (the second is DNS, which
/// inferno fills with the gateway), then `00 18 00 30` and 28 zeros.
pub fn network_info_body(net: &NetInfo) -> [u8; NETWORK_INFO_LEN] {
    let mut c = [0u8; NETWORK_INFO_LEN];
    c[0x00..0x06].copy_from_slice(&[0x00, 0x01, 0x00, 0x00, 0x00, 0x00]);
    c[0x06..0x08].copy_from_slice(&net.link_speed.to_be_bytes());
    c[0x08..0x0a].copy_from_slice(&1u16.to_be_bytes());
    c[0x0a..0x10].copy_from_slice(&net.mac);
    c[0x10..0x14].copy_from_slice(&net.ip.octets());
    c[0x14..0x18].copy_from_slice(&net.netmask.octets());
    c[0x18..0x1c].copy_from_slice(&net.gateway.octets());
    c[0x1c..0x20].copy_from_slice(&net.gateway.octets());
    c[0x20..0x24].copy_from_slice(&[0x00, 0x18, 0x00, 0x30]);
    c
}

/// The channel-change notification's body for the RX channels `rx` (from
/// 0): the bitmask's length in bytes (at least 1), then the bitmask.
pub fn channel_change_body(rx: &[usize]) -> Vec<u8> {
    let mut c = vec![0u8; 3];
    for &ch in rx {
        let byte = 2 + ch / 8;
        if byte >= c.len() {
            c.resize(byte + 1, 0);
        }
        c[byte] |= 1 << (ch % 8);
    }
    let mask = u16::try_from(c.len() - 2).unwrap_or(u16::MAX);
    c[..2].copy_from_slice(&mask.to_be_bytes());
    c
}

/// A channel's peak as the heartbeat carries it (inferno's
/// `peaks_of_buffers`): `−40·log10(peak / i32::MAX)`, rounded, 0 to 255
/// (silence 255, full scale 0), in `f32` as inferno computes it.
pub fn peak_byte(peak: u32) -> u8 {
    let peak = peak.min(i32::MAX as u32) as i32;
    let lin = peak as f32 / i32::MAX as f32;
    let log = lin.log10() * 40.0;
    (-log).round().clamp(0.0, 255.0) as u8
}

/// What the heartbeat reports once the media clock is ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Records {
    /// dantesync's frequency adjustment, ppb.
    pub freq_ppb: i32,
    /// Each TX channel's peak byte ([`peak_byte`]).
    pub tx_peaks: Vec<u8>,
    /// Each RX channel's peak byte.
    pub rx_peaks: Vec<u8>,
    /// Our sample rate.
    pub rate: u32,
    /// Each RX flow slot's largest lateness since the last beat, samples.
    pub lateness: [u32; MAX_FLOWS],
}

/// A record's head: its length, its type, 4, its content's length, the
/// packet's sequence `seq`, 0.
fn record_head(out: &mut Vec<u8>, len: usize, kind: u16, seq: u16) {
    for field in [
        u16::try_from(len).unwrap_or(u16::MAX),
        kind,
        4,
        u16::try_from(len - 12).unwrap_or(u16::MAX),
        seq,
        0,
    ] {
        out.extend_from_slice(&field.to_be_bytes());
    }
}

/// The heartbeat's body with `records` for the packet of sequence `seq`
/// (inferno's `send_heartbeat`): 0x8001, then 0x8002 when there are peaks,
/// then 0x8003.
pub fn heartbeat_body(seq: u16, r: &Records) -> Vec<u8> {
    let mut out = Vec::new();
    record_head(&mut out, 16, record::FREQUENCY, seq);
    out.extend_from_slice(&r.freq_ppb.to_be_bytes());
    let peaks = r.tx_peaks.len() + r.rx_peaks.len();
    if peaks > 0 {
        let end = out.len() + (24 + peaks).next_multiple_of(4);
        record_head(&mut out, 24 + peaks, record::PEAKS, seq);
        for count in [r.tx_peaks.len(), r.rx_peaks.len(), 24] {
            out.extend_from_slice(&u16::try_from(count).unwrap_or(u16::MAX).to_be_bytes());
            out.extend_from_slice(&[0, 0]);
        }
        out.extend_from_slice(&r.tx_peaks);
        out.extend_from_slice(&r.rx_peaks);
        out.resize(end, 0);
    }
    record_head(&mut out, 24 + 4 * MAX_FLOWS, record::RX_LATENESS, seq);
    for field in [MAX_FLOWS as u16, 0, 24, 0] {
        out.extend_from_slice(&field.to_be_bytes());
    }
    out.extend_from_slice(&r.rate.to_be_bytes());
    for lateness in r.lateness {
        out.extend_from_slice(&lateness.to_be_bytes());
    }
    out
}

/// Where the heartbeat's records come from.
pub trait Beat: Send + Sync {
    /// The records now; `None` while the media clock is not ready (the
    /// heartbeat is the header alone then, as inferno's without a clock).
    fn records(&self) -> Option<Records>;
}

/// The running device's records ([`crate::daemon::media::Media::beat`]):
/// the media clock, dantesync's frequency adjustment as the clock's poller
/// keeps it, the TX side (its channels' peaks), the RX side (its rings'
/// peaks, its slots' lateness) and the manager (which slots hold a granted
/// flow). Each beat takes the peaks and every slot's lateness (the largest
/// in samples and the packets' least in ns), so the next one reports its
/// own second (the control socket's status takes none of them).
/// Before the media clock is ready the heartbeat is the header alone.
#[derive(Clone)]
pub struct Sources {
    pub clock: Arc<dyn PtpClock>,
    pub freq: Arc<AtomicI32>,
    pub tx: Arc<TxShared>,
    pub rx: Arc<RxShared>,
    pub manager: Arc<Mutex<Manager>>,
}

impl Beat for Sources {
    fn records(&self) -> Option<Records> {
        self.clock.now_ns()?;
        let granted: [bool; MAX_FLOWS] = {
            let m = self.manager.lock().unwrap_or_else(PoisonError::into_inner);
            std::array::from_fn(|slot| m.flow(slot).is_some_and(|f| f.handle.is_some()))
        };
        Some(Records {
            freq_ppb: self.freq.load(Ordering::Relaxed),
            tx_peaks: (0..self.tx.cfg.tx.len())
                .map(|tx| peak_byte(self.tx.take_peak(tx)))
                .collect(),
            rx_peaks: self
                .rx
                .rings()
                .iter()
                .map(|ring| peak_byte(ring.take_peak()))
                .collect(),
            rate: self.tx.cfg.sample_rate,
            // Every slot's second is taken, granted or not, so the status's
            // lateness and the packets' judge never show an ended flow's
            // last second; only a granted flow's goes into the record.
            lateness: std::array::from_fn(|slot| {
                let taken = self.rx.take_max_lateness(slot);
                if granted[slot] {
                    u32::try_from(taken.min(i32::MAX as u64)).unwrap_or(0)
                } else {
                    0
                }
            }),
        })
    }
}

/// RX channels whose name, subscription or status changed, waiting for
/// the next channel-change notification: coalesced, so one notification
/// names every channel that changed since the last, and none is lost while
/// the bound holds it back. The ARC server and the manager's loop add to
/// it; the info server takes from it.
#[derive(Debug, Default)]
pub struct Changes {
    pending: Mutex<BTreeSet<usize>>,
    added: Notify,
}

impl Changes {
    /// RX channels `rx` (from 0) changed.
    pub fn add(&self, rx: impl IntoIterator<Item = usize>) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let before = pending.len();
        pending.extend(rx);
        if pending.len() > before {
            self.added.notify_one();
        }
    }

    /// Whether a change waits.
    pub fn is_empty(&self) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }

    /// Every change waiting, in channel order; none waits after.
    pub fn take(&self) -> Vec<usize> {
        std::mem::take(&mut *self.pending.lock().unwrap_or_else(PoisonError::into_inner))
            .into_iter()
            .collect()
    }

    /// Until a change is added (at once when one was added since the last
    /// wait).
    pub async fn added(&self) {
        self.added.notified().await;
    }
}

/// The device's sender of info packets: its identity and the sequence its
/// next packet carries (inferno's `Multicaster`).
#[derive(Debug, Clone)]
pub struct Multicaster {
    id: Identity,
    seq: u16,
}

impl Multicaster {
    /// The first packet carries sequence 1, as inferno's.
    pub fn new(id: Identity) -> Self {
        Self { id, seq: 1 }
    }

    /// The next packet: the header, then `body`; the sequence moves on.
    fn packet(&mut self, start: u16, opcode: [u8; 8], body: &[u8]) -> Vec<u8> {
        let len = u16::try_from(HEADER_LEN + body.len()).expect("an info body under 64 KiB");
        let mut out = Vec::with_capacity(usize::from(len));
        out.extend_from_slice(&start.to_be_bytes());
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&PROCESS_ID.to_be_bytes());
        out.extend_from_slice(&self.id.device_id);
        out.extend_from_slice(&VENDOR);
        out.extend_from_slice(&opcode);
        out.extend_from_slice(body);
        self.seq = self.seq.wrapping_add(1);
        out
    }

    pub fn board_info(&mut self) -> Vec<u8> {
        let body = board_info_body(&self.id);
        self.packet(START_INFO, op::BOARD_INFO, &body)
    }

    pub fn product_info(&mut self) -> Vec<u8> {
        let body = product_info_body(&self.id);
        self.packet(START_INFO, op::PRODUCT_INFO, &body)
    }

    /// The heartbeat: the header alone without `records` (the clock is not
    /// ready), else with them ([`heartbeat_body`], each record carrying
    /// this packet's sequence).
    pub fn heartbeat(&mut self, records: Option<&Records>) -> Vec<u8> {
        let body = records.map_or_else(Vec::new, |r| heartbeat_body(self.seq, r));
        self.packet(START_HEARTBEAT, op::HEARTBEAT, &body)
    }

    /// The channel-change notification for the RX channels `rx` (from 0).
    pub fn channel_change(&mut self, rx: &[usize]) -> Vec<u8> {
        self.packet(START_INFO, op::CHANNEL_CHANGE, &channel_change_body(rx))
    }

    /// The channel-change notification for every change waiting in
    /// `changes`, when one waits and `limit` has room for one now; `None`
    /// (the changes kept for later, no sequence used) otherwise.
    pub fn notify_within(
        &mut self,
        limit: &mut Limit,
        now: Instant,
        changes: &Changes,
    ) -> Option<Vec<u8>> {
        if changes.is_empty() || !limit.room(now, Kind::ChannelChange) {
            return None;
        }
        limit.allow(now, Kind::ChannelChange);
        let rx = changes.take();
        debug!(channels = rx.len(), "RX channels changed: notified");
        Some(self.channel_change(&rx))
    }

    /// The reply to a datagram received on [`REQUEST_PORT`], for the info
    /// group; `None` (and no sequence used) when there is none.
    pub fn answer(&mut self, datagram: &[u8]) -> Option<Vec<u8>> {
        let Some(req) = parse_request(datagram) else {
            debug!(
                len = datagram.len(),
                "info datagram shorter than its header: dropped"
            );
            return None;
        };
        let opcode = format_args!("{:016x}", u64::from_be_bytes(req.opcode));
        match req.kind {
            Kind::BoardInfo => Some(self.board_info()),
            Kind::ProductInfo => Some(self.product_info()),
            Kind::NetworkInfo => {
                let body = network_info_body(&self.id.net);
                Some(self.packet(START_INFO, op::NETWORK_INFO, &body))
            }
            Kind::Op0077 => Some(self.packet(START_INFO, op::ANSWER_0077, &ANSWER_0077_BODY)),
            Kind::ClockStats => {
                debug!(op = %opcode, seq = req.seq, "clock stats asked: no PTP daemon of ours, no reply");
                None
            }
            Kind::Unknown | Kind::ChannelChange => {
                info!(op = %opcode, seq = req.seq, "info request not answered");
                None
            }
        }
        .inspect(|_| debug!(op = %opcode, seq = req.seq, "info request answered"))
    }

    /// [`Multicaster::answer`] within `limit` ([`limit`]): a reply over the
    /// bound is dropped (counted there) and uses no sequence.
    pub fn answer_within(
        &mut self,
        limit: &mut Limit,
        now: Instant,
        datagram: &[u8],
    ) -> Option<Vec<u8>> {
        if let Some(req) = parse_request(datagram)
            && !limit.allow(now, req.kind)
        {
            return None;
        }
        self.answer(datagram)
    }
}

/// Where the info packets go, and how often the heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Multicast {
    /// Where the replies and the start's board and product info go.
    pub info: SocketAddrV4,
    /// Where the heartbeat goes.
    pub heartbeat: SocketAddrV4,
    /// The heartbeat's period; never zero.
    pub every: Duration,
}

impl Multicast {
    /// Dante's groups and inferno's period.
    pub const DANTE: Multicast = Multicast {
        info: SocketAddrV4::new(INFO_GROUP, INFO_PORT),
        heartbeat: SocketAddrV4::new(HEARTBEAT_GROUP, HEARTBEAT_PORT),
        every: HEARTBEAT_EVERY,
    };
}

/// The info server on `sock` (bound to the device's address and
/// [`REQUEST_PORT`]): sends the board and the product info, then answers
/// every request to `to.info` within the bound ([`limit`]: at most
/// [`limit::PER_SPAN`] replies of a kind a second, the drops logged as one
/// warning a minute, looked at every [`limit::CHECK`]), sends the
/// channel-change notification to `to.info` when `changes` holds one
/// (within the bound; one held back goes at the next [`limit::CHECK`]) and
/// beats to `to.heartbeat` every `to.every` with `beat`'s records, the
/// first beat at once. The socket's address picks the interface the
/// multicasts leave by (Linux routes a multicast from a bound source
/// address out of that address's interface). Every datagram received is
/// counted in `requests` (the control socket's status). Ends only with an
/// error receiving; a packet that cannot be sent is logged and the server
/// goes on.
pub async fn serve_info(
    sock: UdpSocket,
    mut mc: Multicaster,
    to: Multicast,
    beat: Arc<dyn Beat>,
    changes: Arc<Changes>,
    requests: Arc<Requests>,
) -> io::Result<()> {
    send(&sock, &mc.board_info(), to.info).await;
    send(&sock, &mc.product_info(), to.info).await;
    let mut every = tokio::time::interval(to.every);
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut check = tokio::time::interval(limit::CHECK);
    check.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut bound = Limit::default();
    let mut buf = vec![0u8; RECV_BUFFER];
    loop {
        tokio::select! {
            got = sock.recv_from(&mut buf) => {
                let (n, peer) = got?;
                requests.received(REQUEST_PORT, peer.ip(), info_op(&buf[..n]));
                if let Some(packet) = mc.answer_within(&mut bound, Instant::now(), &buf[..n]) {
                    send(&sock, &packet, to.info).await;
                }
            }
            _ = every.tick() => {
                let records = beat.records();
                send(&sock, &mc.heartbeat(records.as_ref()), to.heartbeat).await;
            }
            () = changes.added() => {
                if let Some(packet) = mc.notify_within(&mut bound, Instant::now(), &changes) {
                    send(&sock, &packet, to.info).await;
                }
            }
            _ = check.tick() => {
                if let Some(packet) = mc.notify_within(&mut bound, Instant::now(), &changes) {
                    send(&sock, &packet, to.info).await;
                }
                if let Some(report) = bound.flush(Instant::now()) {
                    warn!(
                        dropped = report.total(),
                        counts = %report,
                        "info replies over the bound dropped in a minute (at most 10 a second of each kind)"
                    );
                }
            }
        }
    }
}

async fn send(sock: &UdpSocket, packet: &[u8], to: SocketAddrV4) {
    if let Err(e) = sock.send_to(packet, to).await {
        warn!(error = %e, port = to.port(), "an info packet could not be sent");
    }
}

#[cfg(test)]
mod tests;
