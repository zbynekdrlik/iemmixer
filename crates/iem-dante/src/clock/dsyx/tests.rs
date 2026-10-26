use super::*;
use crate::wire::tests::Rng;

/// dantesync's test values (`src/time_server/tests.rs`, `master_status`
/// and the pending-step and slew tests).
const D: i64 = 1_790_000_000_000_000_000;
const WALL: i64 = 1_790_000_100_000_000_000;
const STEP_AT: i64 = 12_350_000_000_000;

/// A reply laid out as dantesync's `build_response` and `encode_extension`
/// write it (offsets from `src/time_server.rs` and
/// `src/date_offset/wire.rs`).
struct Reply {
    id: u32,
    wall: i64,
    locked: bool,
    mode: u8,
    adj_ppb: i32,
    settled: bool,
    ext: Option<Ext>,
}

struct Ext {
    version: u8,
    announced: i64,
    effective: i64,
    now_ptp: i64,
    /// A slew: its starting `D` and its rate.
    slew: Option<(i64, u16)>,
    /// The extension's length (dantesync writes 48).
    len: usize,
}

impl Reply {
    fn locked(id: u32) -> Self {
        Reply {
            id,
            wall: WALL,
            locked: true,
            mode: 3,
            adj_ppb: -750,
            settled: true,
            ext: Some(Ext::step(D - 51_000_000, STEP_AT, WALL - D)),
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut b = vec![0u8; 64];
        // [0-3] "DSYR", [4-7] the request id.
        b[0..4].copy_from_slice(b"DSYR");
        b[4..8].copy_from_slice(&self.id.to_be_bytes());
        // [8-15] the wall clock, u64 ns since 1970.
        b[8..16].copy_from_slice(&(self.wall as u64).to_be_bytes());
        // [16-23] the monotonic counter, [24-31] the PTP offset (-12345 in
        // dantesync's test), [32-35] the drift rate (1.5 ppm × 1000).
        b[16..24].copy_from_slice(&123_456_789u64.to_be_bytes());
        b[24..32].copy_from_slice(&(-12_345i64).to_be_bytes());
        b[32..36].copy_from_slice(&1_500i32.to_be_bytes());
        // [36-39] the frequency adjustment, ppm × 1000.
        b[36..40].copy_from_slice(&self.adj_ppb.to_be_bytes());
        // [40] the mode, [41] locked, [42-47] the grandmaster.
        b[40] = self.mode;
        b[41] = u8::from(self.locked);
        b[42..48].copy_from_slice(&[0x00, 0x1d, 0xc1, 0x0a, 0x0b, 0x0c]);
        // [48-55] the counter's frequency, [56-59] the NTP offset (µs).
        b[48..56].copy_from_slice(&1_000_000_000u64.to_be_bytes());
        b[56..60].copy_from_slice(&(-1_234i32).to_be_bytes());
        // [62] flags: bit 0 NTP failed, bit 1 settled.
        b[62] = if self.settled { 0x02 } else { 0x01 };
        if let Some(ext) = &self.ext {
            b.extend_from_slice(&ext.bytes());
        }
        b
    }
}

impl Ext {
    fn step(announced: i64, effective: i64, now_ptp: i64) -> Self {
        Ext {
            version: 3,
            announced,
            effective,
            now_ptp,
            slew: None,
            len: 48,
        }
    }

    fn slew(from: i64, to: i64, ppm: u16, start: i64, now_ptp: i64) -> Self {
        Ext {
            slew: Some((from, ppm)),
            ..Ext::step(to, start, now_ptp)
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut e = vec![0u8; 48];
        // [0] version, [1] flags (bit 0 the authority).
        e[0] = self.version;
        e[1] = 0x01;
        e[4..12].copy_from_slice(&self.announced.to_be_bytes());
        e[12..20].copy_from_slice(&self.effective.to_be_bytes());
        // [20-23] seq, [24-29] the anchor's grandmaster.
        e[20..24].copy_from_slice(&4u32.to_be_bytes());
        e[24..30].copy_from_slice(&[0x00, 0x1d, 0xc1, 0x0a, 0x0b, 0x0c]);
        e[32..40].copy_from_slice(&self.now_ptp.to_be_bytes());
        if let Some((from, ppm)) = self.slew {
            // v2: flags bit 1, [2-3] the rate, [40-47] the starting D.
            e[1] |= 0x02;
            e[2..4].copy_from_slice(&ppm.to_be_bytes());
            e[40..48].copy_from_slice(&from.to_be_bytes());
        }
        e.truncate(self.len);
        e
    }
}

/// A locked dantesync's reply to `id` with `wall`, `d` in effect, its last
/// step at PTP `step_at` and `adj_ppb` (the clock's poller tests).
pub(crate) fn locked_reply(id: u32, wall: i64, d: i64, step_at: i64, adj_ppb: i32) -> Vec<u8> {
    Reply {
        wall,
        adj_ppb,
        ext: Some(Ext::step(d, step_at, wall - d)),
        ..Reply::locked(id)
    }
    .bytes()
}

fn date(r: &Reply) -> Option<DateState> {
    parse(&r.bytes(), r.id).unwrap().date
}

#[test]
fn the_dsyx_reply_reads_as_dantesync_writes_it() {
    // The request: "DSYX", the id, zero padding to the base's size.
    let req = request(0xdead_beef);
    assert_eq!(req.len(), 64);
    assert_eq!(&req[0..4], b"DSYX");
    assert_eq!(&req[4..8], &0xdead_beefu32.to_be_bytes());
    assert!(req[8..].iter().all(|&b| b == 0));

    // A locked master with a pending step of -51 ms (dantesync's
    // `a_pending_step_is_published_as_the_offset_it_will_take_88`).
    let reply = Reply::locked(42);
    let bytes = reply.bytes();
    assert_eq!(bytes.len(), 112, "dantesync's extended reply");
    assert_eq!(
        parse(&bytes, 42),
        Ok(Reading {
            wall_ns: WALL,
            locked: true,
            settled: true,
            mode: 3,
            freq_ppb: -750,
            date: Some(DateState {
                d_ns: D,
                step_at_ptp_ns: Some(STEP_AT),
                slewing: false,
            }),
        })
    );

    // A slewing master, D in effect part-way down (dantesync's
    // `a_slew_is_published_as_the_slew_never_as_a_backward_step_119`):
    // 51 ms at 100 ppm run 510 s from the start.
    let in_effect = D - 10_000_000;
    let now_ptp = STEP_AT + 100_000_000_000;
    let r = Reply {
        wall: in_effect + now_ptp,
        ext: Some(Ext::slew(D, D - 51_000_000, 100, STEP_AT, now_ptp)),
        ..Reply::locked(43)
    };
    let got = parse(&r.bytes(), 43).unwrap();
    assert_eq!(
        got.date,
        Some(DateState {
            d_ns: in_effect,
            step_at_ptp_ns: None,
            slewing: true,
        })
    );

    // Unlocked, not settled, another mode.
    let r = Reply {
        locked: false,
        settled: false,
        mode: 1,
        adj_ppb: 12_345,
        ..Reply::locked(44)
    };
    let got = parse(&r.bytes(), 44).unwrap();
    assert!(!got.locked);
    assert!(!got.settled);
    assert_eq!((got.mode, got.freq_ppb), (1, 12_345));
}

#[test]
fn a_64_byte_reply_has_no_date_state() {
    let base = Reply {
        ext: None,
        ..Reply::locked(1)
    };
    let bytes = base.bytes();
    assert_eq!(bytes.len(), 64);
    let got = parse(&bytes, 1).unwrap();
    assert_eq!(got.date, None);
    assert!(got.locked);
    // An extension under 40 bytes, or of version 0, is none either.
    let mut short = Reply::locked(1);
    short.ext.as_mut().unwrap().len = 39;
    assert_eq!(date(&short), None);
    let mut zero = Reply::locked(1);
    zero.ext.as_mut().unwrap().version = 0;
    assert_eq!(date(&zero), None);
    // A version 1 extension (40 bytes) is read.
    let mut v1 = Reply::locked(1);
    let ext = v1.ext.as_mut().unwrap();
    ext.version = 1;
    ext.len = 40;
    assert_eq!(
        date(&v1),
        Some(DateState {
            d_ns: D,
            step_at_ptp_ns: Some(STEP_AT),
            slewing: false,
        })
    );
}

#[test]
fn a_wrong_magic_or_request_id_is_refused() {
    let good = Reply::locked(7).bytes();
    assert!(parse(&good, 7).is_ok());
    assert_eq!(parse(&good, 8), Err(DsyxError::RequestId { got: 7 }));
    let mut magic = good.clone();
    magic[3] = b'N';
    assert_eq!(parse(&magic, 7), Err(DsyxError::Magic));
    assert_eq!(parse(&good[..63], 7), Err(DsyxError::Short { len: 63 }));
    assert_eq!(parse(&[], 7), Err(DsyxError::Short { len: 0 }));
    // Our own request is not a reply.
    assert_eq!(parse(&request(7), 7), Err(DsyxError::Magic));
}

#[test]
fn a_slew_runs_from_two_seconds_before_its_start_to_two_seconds_after_its_end() {
    // 51 ms at 100 ppm: 510 s.
    let end = STEP_AT + 510_000_000_000;
    let at = |now_ptp: i64, ppm: u16, to: i64| {
        let r = Reply {
            ext: Some(Ext::slew(D, to, ppm, STEP_AT, now_ptp)),
            ..Reply::locked(1)
        };
        date(&r).unwrap()
    };
    let to = D - 51_000_000;
    assert!(!at(STEP_AT - STEP_GUARD_NS - 1, 100, to).slewing);
    assert!(at(STEP_AT - STEP_GUARD_NS, 100, to).slewing);
    assert!(at(end + STEP_GUARD_NS - 1, 100, to).slewing);
    assert!(!at(end + STEP_GUARD_NS, 100, to).slewing);
    assert_eq!(at(end, 100, to).step_at_ptp_ns, None);
    // dantesync's rate rules: 0 is its default 100 ppm, the rest clamped to
    // 10..=500, the duration rounded up to the ns.
    assert!(at(end + STEP_GUARD_NS - 1, 0, to).slewing);
    assert!(!at(end + STEP_GUARD_NS, 0, to).slewing);
    // 1 000 ppm reads as 500: 102 s.
    let fast_end = STEP_AT + 102_000_000_000;
    assert!(at(fast_end + STEP_GUARD_NS - 1, 1_000, to).slewing);
    assert!(!at(fast_end + STEP_GUARD_NS, 1_000, to).slewing);
    // 5 ppm reads as 10: 5 100 s.
    let slow_end = STEP_AT + 5_100_000_000_000;
    assert!(at(slow_end + STEP_GUARD_NS - 1, 5, to).slewing);
    assert!(!at(slow_end + STEP_GUARD_NS, 5, to).slewing);
    // 1 ns at 300 ppm: 3 333.3 ns, rounded up to 3 334.
    let tiny = D - 1;
    assert!(at(STEP_AT + 3_334 + STEP_GUARD_NS - 1, 300, tiny).slewing);
    assert!(!at(STEP_AT + 3_334 + STEP_GUARD_NS, 300, tiny).slewing);
}

#[test]
fn only_a_backward_slew_of_version_2_on_is_a_slew() {
    let now_ptp = STEP_AT + 1_000_000_000;
    let slew = |from: i64, to: i64| Ext::slew(from, to, 100, STEP_AT, now_ptp);
    let step = Some(DateState {
        d_ns: WALL - now_ptp,
        step_at_ptp_ns: Some(STEP_AT),
        slewing: false,
    });
    let with = |ext: Ext| {
        date(&Reply {
            ext: Some(ext),
            ..Reply::locked(1)
        })
    };
    assert!(with(slew(D, D - 1)).unwrap().slewing);
    // Forward, or no change at all: a step at its instant (dantesync's
    // `as_slew`).
    assert_eq!(with(slew(D, D + 51_000_000)), step);
    assert_eq!(with(slew(D, D)), step);
    // Version 1 never carries a slew; a version 2 without its 48 bytes
    // neither.
    let mut v1 = slew(D, D - 1);
    v1.version = 1;
    assert_eq!(with(v1), step);
    let mut cut = slew(D, D - 1);
    cut.len = 47;
    assert_eq!(with(cut), step);
    let mut v2 = slew(D, D - 1);
    v2.version = 2;
    assert!(with(v2).unwrap().slewing);
    // Only the slew flag makes one: bytes 40-47 alone do not.
    let unflagged = Reply {
        ext: Some(slew(D, D - 1)),
        ..Reply::locked(1)
    };
    let mut bytes = unflagged.bytes();
    bytes[65] &= !0x02;
    assert_eq!(parse(&bytes, 1).unwrap().date, step);
}

#[test]
fn random_replies_never_panic() {
    let mut rng = Rng::new(0x5eed_d5e5);
    for _ in 0..10_000 {
        let len = rng.below(200);
        let mut buf: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let id = rng.next() as u32;
        if len >= 8 && rng.below(2) == 0 {
            // Often a reply to us, so the extension is read.
            buf[0..4].copy_from_slice(b"DSYR");
            buf[4..8].copy_from_slice(&id.to_be_bytes());
        }
        if let Ok(r) = parse(&buf, id) {
            let _ = crate::clock::sample(&r, rng.next() as i64, rng.next() as i64);
        }
    }
}
