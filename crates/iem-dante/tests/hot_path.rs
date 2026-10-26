//! The media hot path allocates nothing (Gen 3 phase B, plan Task 3): a
//! packet written and read, a ring written and read, every TX source and
//! both checkers, under `assert_no_alloc` (warn mode: the violation count
//! must stay zero). The first test proves the detector sees an allocation.
//! Its own test binary, because it installs the global allocator; every
//! buffer, ring and checker is built before the detector runs.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use iem_dante::media::check::{CounterCheck, SineCheck};
use iem_dante::media::packet::{self, MEDIA_HEADER};
use iem_dante::media::ring::Ring;
use iem_dante::media::source::Source;

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
