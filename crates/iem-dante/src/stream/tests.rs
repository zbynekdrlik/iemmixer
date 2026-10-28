use super::*;

use std::net::Ipv4Addr;
use std::sync::mpsc;

use crate::device::DeviceConfig;
use crate::media::rx::{RxPlan, RxRoute};
use crate::wire::tests::Rng;

const TEST_DEVICE: &str = include_str!("../../../../config/test-dante-device.toml");
const RATE: u32 = 96_000;
const B: u64 = BLOCK;
/// The test device's latency, 1 ms: P.
const P: u64 = 96;
const TIMING: Timing = Timing {
    block: B,
    playout: P,
    margin: WORK_MARGIN,
};
/// D = B + P + W.
const D: u64 = 224;
/// A device config's `block = 16` (#72) with W at its least, one block.
const B16: u64 = 16;
const W16: u64 = 16;
const TIMING_16: Timing = Timing {
    block: B16,
    playout: P,
    margin: W16,
};
/// D = 16 + 96 + 16.
const D16: u64 = 128;
/// Ten years of a grandmaster's uptime at 96 kHz, a multiple of the block.
const T: u64 = 10 * 365 * 86_400 * 96_000;
/// Every wait in these tests ends.
const BOUND: Duration = Duration::from_secs(5);

// ---- samples ----

#[test]
fn quantize_rounds_at_the_bits_and_saturates_at_full_scale() {
    assert_eq!(quantize(0.5, 24), 0x4000_0000);
    assert_eq!(quantize(-1.0, 24), i32::MIN, "−1.0 is exact");
    for x in [1.0, 4.0, f64::INFINITY, 1e300] {
        assert_eq!(quantize(x, 24), 0x7fff_ff00, "{x} saturates at the top");
    }
    for x in [-1.5, f64::NEG_INFINITY, -1e300] {
        assert_eq!(quantize(x, 24), i32::MIN, "{x} saturates at the bottom");
    }
    for bits in [16, 24, 32] {
        assert_eq!(quantize(f64::NAN, bits), 0, "NaN at {bits}");
        assert_eq!(quantize(0.0, bits), 0);
        assert_eq!(quantize(-0.0, bits), 0);
    }
    // Rounded at the flow's bits, never truncated: a quarter of a 24-bit
    // step is lost, three quarters round up (and down for a negative).
    let step24 = 1.0 / 8_388_608.0;
    assert_eq!(quantize(step24 * 0.25, 24), 0);
    assert_eq!(quantize(step24 * 0.75, 24), 0x100);
    assert_eq!(quantize(step24 * -0.75, 24), -0x100);
    assert_eq!(quantize(step24 * 2.75, 24), 0x300);
    // 16 bits: the low 16 bits clear.
    assert_eq!(quantize(0.5, 16), 0x4000_0000);
    assert_eq!(quantize(1.0, 16), 0x7fff_0000);
    let step16 = 1.0 / 32_768.0;
    assert_eq!(quantize(step16 * 3.0, 16), 0x3_0000);
    assert_eq!(quantize(step16 * 0.4, 16), 0);
    // Half a step rounds away from zero.
    assert_eq!(quantize(step16 * 0.5, 16), 0x1_0000);
    assert_eq!(quantize(step16 * -0.5, 16), -0x1_0000);
    // 32 bits: every step of 2⁻³¹.
    assert_eq!(quantize(1.0, 32), i32::MAX);
    assert_eq!(quantize(-1.0, 32), i32::MIN);
    assert_eq!(quantize(3.0 / FULL_SCALE, 32), 3);
    assert_eq!(quantize(-3.4 / FULL_SCALE, 32), -3);
    // Bits out of range read as the nearest, never a panic.
    assert_eq!(quantize(0.5, 0), quantize(0.5, 1));
    assert_eq!(quantize(-1.0, 1), i32::MIN);
    assert_eq!(quantize(0.3, 40), quantize(0.3, 32));
}

#[test]
fn to_f64_is_exact_and_quantize_takes_it_back_on_the_24_bit_grid() {
    assert_eq!(to_f64(0x4000_0000), 0.5);
    assert_eq!(to_f64(i32::MIN), -1.0);
    assert_eq!(to_f64(0x100), 1.0 / 8_388_608.0);
    assert_eq!(to_f64(-3), -3.0 / FULL_SCALE);
    for s in [0, 0x100, -0x100, 0x7fff_ff00, i32::MIN, 0x12_3400] {
        assert_eq!(quantize(to_f64(s), 24), s, "{s:#x}");
    }
    // A seeded sweep of the 24-bit grid.
    let mut rng = Rng::new(0x5eed_0072);
    for _ in 0..100_000 {
        let s = ((rng.next() as u32) & 0xffff_ff00) as i32;
        assert_eq!(quantize(to_f64(s), 24), s, "{s:#x}");
        let s32 = rng.next() as u32 as i32;
        assert_eq!(quantize(to_f64(s32), 32), s32, "{s32:#x}");
    }
}

// ---- the schedule ----

#[test]
fn the_offset_is_block_plus_playout_plus_margin() {
    assert_eq!(TIMING.offset(), D);
    let at_2_ms = Timing {
        block: 32,
        playout: 192,
        margin: 96,
    };
    assert_eq!(at_2_ms.offset(), 320);
}

#[test]
fn bootstrapping_never_starts_behind() {
    let mut nows: Vec<u64> = (0..300).collect();
    nows.extend((0..200).map(|i| T + i));
    nows.extend([T - 1, T + P, T + P + B - 1, T + P + B, T + P + B + 1]);
    for now in nows {
        let b = Blocks::bootstrap(now, TIMING);
        assert_eq!(b.start() % B, 0, "{now}");
        assert_eq!(b.due(), b.start() + B + P);
        assert!(b.due() >= now, "{now}: due {} behind", b.due());
        // The smallest such start: its block is due within one block.
        if now >= P + B {
            assert!(b.due() < now + B, "{now}: due {}", b.due());
        } else {
            assert_eq!(b.start(), 0, "{now}");
        }
        let mut next = b;
        let want = if b.due() == now {
            Due::Run {
                start: b.start(),
                late: false,
            }
        } else {
            Due::Wait { until: b.due() }
        };
        assert_eq!(next.next(now), want, "{now}");
    }
    // Exactly at a block's playout point, that block; one sample later,
    // the next one.
    assert_eq!(Blocks::bootstrap(T + B + P, TIMING).start(), T);
    assert_eq!(Blocks::bootstrap(T + B + P + 1, TIMING).start(), T + B);
    assert_eq!(Blocks::bootstrap(T + P + 1, TIMING).start(), T);
    assert_eq!(Blocks::bootstrap(T + P, TIMING).start(), T - B);
}

#[test]
fn a_block_is_due_exactly_at_start_plus_block_plus_playout() {
    let mut b = Blocks::bootstrap(T + 5, TIMING);
    let start = b.start();
    let due = start + B + P;
    assert_eq!(b.due(), due);
    assert_eq!(b.next(due - 1), Due::Wait { until: due });
    assert_eq!(b.next(due), Due::Run { start, late: false });
    // The next one a block later.
    assert_eq!(b.next(due), Due::Wait { until: due + B });
    assert_eq!(b.next(due + B - 1), Due::Wait { until: due + B });
    assert_eq!(
        b.next(due + B),
        Due::Run {
            start: start + B,
            late: false
        }
    );
    assert_eq!(b.start(), start + 2 * B);
}

#[test]
fn late_is_one_block_behind() {
    let fresh = Blocks::bootstrap(T + 5, TIMING);
    let (start, due) = (fresh.start(), fresh.due());
    let mut b = fresh;
    assert_eq!(b.next(due + B - 1), Due::Run { start, late: false });
    let mut b2 = fresh;
    assert_eq!(b2.next(due + B), Due::Run { start, late: true });
    // The block after it is then one block less behind.
    assert_eq!(
        b2.next(due + B),
        Due::Run {
            start: start + B,
            late: false
        }
    );
    assert_eq!(b2.next(due + B), Due::Wait { until: due + 2 * B });
    // Behind by exactly the margin: still run, late.
    let mut b3 = fresh;
    assert_eq!(b3.next(due + WORK_MARGIN), Due::Run { start, late: true });
}

#[test]
fn a_pump_behind_more_than_its_margin_rebootstraps_once_and_counts_the_skipped_blocks() {
    let fresh = Blocks::bootstrap(T + 5, TIMING);
    let (start, due) = (fresh.start(), fresh.due());
    // One sample past the margin: the blocks at start … start + 3B are
    // skipped (the last of them due a sample ago: no catch-up), the next
    // start is the first due at or after now.
    let mut b = fresh;
    let now = due + WORK_MARGIN + 1;
    assert_eq!(b.next(now), Due::Rebootstrap { skipped: 4 });
    assert_eq!(b.start(), start + 4 * B);
    assert_eq!(b.next(now), Due::Wait { until: due + 4 * B }, "once");
    assert_eq!(
        b.next(due + 4 * B),
        Due::Run {
            start: start + 4 * B,
            late: false
        }
    );
    // Far behind: 10 000 samples skip ⌈10 000 / 32⌉ blocks.
    let mut b = fresh;
    assert_eq!(b.next(due + 10_000), Due::Rebootstrap { skipped: 313 });
    assert_eq!(b.start(), start + 313 * B);
    assert!(matches!(b.next(due + 10_000), Due::Wait { .. }));
    // Another start: the skipped count is the distance in blocks.
    let mut b = Blocks::bootstrap(T + 1_000 * B, TIMING);
    let s = b.start();
    assert_eq!(b.rebootstrap(s + B + P + 7 * B), 7);
    assert_eq!(b.start(), s + 7 * B);
}

#[test]
fn a_schedule_far_ahead_rebootstraps() {
    let fresh = Blocks::bootstrap(T + 5, TIMING);
    let due = fresh.due();
    // Exactly the discontinuity ahead (the clock went back): a wait.
    let mut b = fresh;
    assert_eq!(b.next(due - DISCONTINUITY), Due::Wait { until: due });
    // One sample more: the schedule starts again at now, nothing skipped.
    let now = due - DISCONTINUITY - 1;
    let mut b = fresh;
    assert_eq!(b.next(now), Due::Rebootstrap { skipped: 0 });
    assert_eq!(b, Blocks::bootstrap(now, TIMING));
    assert!(b.due() >= now && b.due() < now + B);
    assert!(matches!(b.next(now), Due::Wait { .. }));
}

#[test]
fn at_block_16_a_block_is_due_at_start_plus_16_plus_playout_and_late_from_16_behind() {
    // A device config's `block = 16` (#72): the schedule a block of 16
    // apart, D = 16 + P + W.
    assert_eq!(TIMING_16.offset(), D16);
    let with_1_ms = Timing {
        margin: WORK_MARGIN,
        ..TIMING_16
    };
    assert_eq!(with_1_ms.offset(), 208);
    // Bootstrapped on a multiple of 16, due within one block of 16.
    for now in [T + 5, T + 16 + 5, T + 1_000_003] {
        let b = Blocks::bootstrap(now, TIMING_16);
        assert_eq!(b.start() % B16, 0, "{now}");
        assert_eq!(b.due(), b.start() + B16 + P);
        assert!(
            b.due() >= now && b.due() < now + B16,
            "{now}: due {}",
            b.due()
        );
    }
    // An odd multiple of 16: never rounded to a block of 32.
    assert_eq!(Blocks::bootstrap(T + 16 + 5, TIMING_16).start(), T - 80);
    assert_eq!(Blocks::bootstrap(T + B16 + P, TIMING_16).start(), T);
    assert_eq!(
        Blocks::bootstrap(T + B16 + P + 1, TIMING_16).start(),
        T + B16
    );
    assert_eq!(Blocks::bootstrap(T + P + 1, TIMING_16).start(), T);
    assert_eq!(Blocks::bootstrap(T + P, TIMING_16).start(), T - B16);
    // Due at kB + B + P, the next one 16 later.
    let fresh = Blocks::bootstrap(T + 5, TIMING_16);
    let (start, due) = (fresh.start(), fresh.due());
    assert_eq!((start, due), (T - 96, T + 16));
    let mut b = fresh;
    assert_eq!(b.next(due - 1), Due::Wait { until: due });
    assert_eq!(b.next(due), Due::Run { start, late: false });
    assert_eq!(b.next(due), Due::Wait { until: due + B16 });
    assert_eq!(b.next(due + B16 - 1), Due::Wait { until: due + B16 });
    assert_eq!(
        b.next(due + B16),
        Due::Run {
            start: start + B16,
            late: false
        }
    );
    assert_eq!(b.start(), start + 2 * B16);
    // Late from one block of 16 behind (W, 16, still runs).
    let mut b = fresh;
    assert_eq!(b.next(due + B16 - 1), Due::Run { start, late: false });
    let mut b = fresh;
    assert_eq!(b.next(due + B16), Due::Run { start, late: true });
    assert_eq!(
        b.next(due + B16),
        Due::Run {
            start: start + B16,
            late: false
        }
    );
    assert_eq!(
        b.next(due + B16),
        Due::Wait {
            until: due + 2 * B16
        }
    );
    // One sample past W: re-bootstrapped once, two blocks (17 / 16, up)
    // skipped.
    let mut b = fresh;
    let now = due + W16 + 1;
    assert_eq!(b.next(now), Due::Rebootstrap { skipped: 2 });
    assert_eq!(b.start(), start + 2 * B16);
    assert_eq!(
        b.next(now),
        Due::Wait {
            until: due + 2 * B16
        },
        "once"
    );
    // Far behind: 10 000 samples skip 10 000 / 16 blocks.
    let mut b = fresh;
    assert_eq!(b.next(due + 10_000), Due::Rebootstrap { skipped: 625 });
    assert_eq!(b.start(), start + 625 * B16);
}

#[test]
fn free_pacing_keeps_its_grid_and_resynchronises_past_eight_blocks() {
    let t0 = Instant::now();
    let p = Duration::from_micros(333);
    let ns = Duration::from_nanos(1);
    // On time or behind: a period after the last deadline.
    assert_eq!(free_deadline(t0, t0, p), t0 + p);
    assert_eq!(free_deadline(t0, t0 + 5 * p, p), t0 + p);
    assert_eq!(free_deadline(t0, t0 + 9 * p, p), t0 + p);
    // More than eight periods behind the next one: now.
    let far = t0 + 9 * p + ns;
    assert_eq!(free_deadline(t0, far, p), far);
    let farther = t0 + 50 * p;
    assert_eq!(free_deadline(t0, farther, p), farther);
}

// ---- the pump ----

/// Copies input i to output i; logs `d` for each discontinuity and `p`
/// for each block; panics on the block `panic_at` (from 1) after a 2 ms
/// sleep, its outputs already written.
#[derive(Default)]
struct Copy {
    log: String,
    calls: u64,
    panic_at: Option<u64>,
    /// The largest input magnitude seen.
    loudest: f64,
}

impl Process for Copy {
    fn process(&mut self, block: &mut Block<'_>) {
        self.calls += 1;
        self.log.push('p');
        for ch in 0..block.inputs().min(block.outputs()) {
            let input = block.input(ch).to_vec();
            self.loudest = input.iter().fold(self.loudest, |m, x| m.max(x.abs()));
            block.output(ch).copy_from_slice(&input);
        }
        if Some(self.calls) == self.panic_at {
            std::thread::sleep(Duration::from_millis(2));
            panic!("boom in block {}", self.calls);
        }
    }

    fn discontinuity(&mut self) {
        self.log.push('d');
    }
}

/// A pump over `map` with RX rings for the test device's 32 RX channels
/// (`routed` routed to a flow), engine rings on `engine`.
struct Rig {
    pump: Pump<Copy>,
    rx: Arc<RxShared>,
    tx: Arc<TxShared>,
    stats: Arc<PumpStats>,
}

fn rig(map: Map, routed: &[usize], engine: &[usize]) -> Rig {
    rig_at(map, routed, engine, TIMING)
}

/// [`rig`] with the pump's `timing`.
fn rig_at(map: Map, routed: &[usize], engine: &[usize], timing: Timing) -> Rig {
    let cfg = DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-dante-device.toml");
    let rings: Arc<[Ring]> = (0..cfg.rx.len()).map(|_| Ring::new()).collect();
    let rx = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::clone(&rings)).unwrap());
    rx.publish(
        |_| RxPlan::default(),
        |c| routed.contains(&c).then_some(RxRoute { flow: 0, slot: 0 }),
    );
    let tx = Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, rings, 1, engine).unwrap());
    let stats = Arc::new(PumpStats::new(timing, RATE));
    let pump = Pump::new(
        Copy::default(),
        map,
        timing,
        24,
        Arc::clone(&rx),
        Arc::clone(&tx),
        Arc::clone(&stats),
    );
    Rig {
        pump,
        rx,
        tx,
        stats,
    }
}

/// The counter of position `p`, on the 24-bit grid.
fn counter(p: u64) -> i32 {
    ((p % 65_536) << 8) as i32
}

/// RX channel `rx`'s ring holding `value(p)` at every position from
/// `from` for `n`.
fn fill(r: &Rig, rx: usize, from: u64, n: u64, value: impl Fn(u64) -> i32) {
    let v: Vec<i32> = (from..from + n).map(value).collect();
    assert_eq!(r.rx.rings()[rx].write_at(from, &v).written, v.len());
}

/// TX channel `tx`'s engine ring at `from` for `n`, and its holes.
fn engine_at(r: &Rig, tx: usize, from: u64, n: usize) -> (Vec<i32>, usize) {
    let mut out = vec![0x5555_5500; n];
    let holes = r.tx.engine_ring(tx).unwrap().read_at(from, &mut out).holes;
    (out, holes)
}

/// Steps the pump on the media clock from `from` to `to`, waking where it
/// asks; returns the media steps taken.
fn run_to(r: &mut Rig, from: u64, to: u64, epoch: u32) -> u64 {
    let mut now = from;
    let mut steps = 0;
    while now < to {
        let wake = r.pump.media(now, epoch);
        assert!(wake > now, "a wake ahead of now: {wake} at {now}");
        now = wake;
        steps += 1;
    }
    steps
}

#[test]
fn the_output_stamped_t_is_the_input_stamped_t_minus_the_offset() {
    let mut r = rig(
        Map {
            rx: vec![0, 3],
            tx: vec![1, 4],
        },
        &[0, 3],
        &[1, 4],
    );
    fill(&r, 0, T - 1_000, 21_000, counter);
    fill(&r, 3, T - 1_000, 21_000, |p| -counter(p) - 256);
    let first = Blocks::bootstrap(T, TIMING).start();
    let steps = run_to(&mut r, T, T + 19_200, 0);
    let blocks = r.stats.view().blocks;
    // Waking where it asks, one block a step from the first one's due time.
    assert_eq!(blocks, steps, "one block a step");
    assert!((598..=601).contains(&blocks), "{blocks}");
    let n = (blocks * B) as usize;
    let (out, holes) = engine_at(&r, 1, first + D, n);
    assert_eq!(holes, 0);
    for (i, &v) in out.iter().enumerate() {
        let t = first + D + i as u64;
        assert_eq!(v, counter(t - D), "TX 2 at {t} is RX 1 at T − D");
    }
    let (out, holes) = engine_at(&r, 4, first + D, n);
    assert_eq!(holes, 0);
    assert!(
        out.iter()
            .enumerate()
            .all(|(i, &v)| v == -counter(first + i as u64) - 256),
        "TX 5 is RX 4 D earlier"
    );
    // Nothing before the first block's outputs, nothing after the last.
    assert_eq!(engine_at(&r, 1, first + D - 1, 1).1, 1);
    assert_eq!(engine_at(&r, 1, first + D + n as u64, 1).1, 1);
    // Each output written once, every input held.
    assert_eq!(r.tx.engine_ring(1).unwrap().counters().written, blocks * B);
    assert_eq!(r.rx.rings()[0].counters().holes, 0);
    let v = r.stats.view();
    assert_eq!(
        (
            v.late,
            v.missed,
            v.rebootstraps,
            v.offset,
            v.playout,
            v.media
        ),
        (0, 0, 0, D, P, true),
        "{v:?}"
    );
    assert_eq!(r.pump.processor.log.matches('d').count(), 1);
    assert!(r.pump.processor.log.starts_with("dp"));
}

#[test]
fn an_unrouted_rx_channel_is_silence_and_counts_no_hole() {
    // RX 2 holds samples but is not routed; RX 3 is routed and empty.
    let mut r = rig(
        Map {
            rx: vec![0, 1, 2],
            tx: vec![0, 1, 2],
        },
        &[0, 2],
        &[0, 1, 2],
    );
    fill(&r, 0, T - 1_000, 4_000, counter);
    fill(&r, 1, T - 1_000, 4_000, counter);
    let first = Blocks::bootstrap(T, TIMING).start();
    run_to(&mut r, T, T + 960, 0);
    let n = (r.stats.view().blocks * B) as usize;
    assert!(n >= 900, "{n}");
    let (out, holes) = engine_at(&r, 0, first + D, n);
    assert_eq!(holes, 0);
    assert!(
        out.iter()
            .enumerate()
            .all(|(i, &v)| v == counter(first + i as u64))
    );
    // Silence, written (no TX hole), and RX 2's ring untouched.
    assert_eq!(engine_at(&r, 1, first + D, n), (vec![0; n], 0));
    assert_eq!(r.rx.rings()[1].counters().holes, 0, "RX 2 not read");
    assert_eq!(
        r.rx.rings()[1].write_at(first, &[1 << 8]).late,
        0,
        "RX 2's reader never moved"
    );
    // A routed channel with no audio is silence too, its holes counted.
    assert_eq!(engine_at(&r, 2, first + D, n), (vec![0; n], 0));
    assert_eq!(r.rx.rings()[2].counters().holes, n as u64);
    assert_eq!(r.rx.rings()[0].counters().holes, 0);
}

#[test]
fn a_late_media_step_runs_its_blocks_and_counts_the_late_ones() {
    let mut r = rig(
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        &[0],
        &[0],
    );
    fill(&r, 0, T - 1_000, 4_000, counter);
    let w = r.pump.media(T, 0);
    let before = r.stats.view().blocks;
    // 40 behind the block due at w: it runs late, the next one on time.
    assert_eq!(r.pump.media(w + 40, 0), w + 2 * B);
    let v = r.stats.view();
    assert_eq!((v.blocks - before, v.late), (2, 1), "{v:?}");
    // Exactly the margin behind: four blocks, three of them late.
    let w = w + 2 * B;
    assert_eq!(r.pump.media(w + WORK_MARGIN, 0), w + 4 * B);
    let v = r.stats.view();
    assert_eq!(
        (v.blocks - before, v.late, v.rebootstraps),
        (6, 4, 0),
        "{v:?}"
    );
    // Every output was written, in time or late.
    let first = Blocks::bootstrap(T, TIMING).start();
    let n = (v.blocks * B) as usize;
    let (out, holes) = engine_at(&r, 0, first + D, n);
    assert_eq!(holes, 0);
    assert!(
        out.iter()
            .enumerate()
            .all(|(i, &v)| v == counter(first + i as u64))
    );
    assert_eq!(r.pump.processor.log.matches('d').count(), 1);
}

#[test]
fn a_media_step_past_the_margin_rebootstraps_once_and_the_skipped_stamps_stay_holes() {
    let mut r = rig(
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        &[0],
        &[0],
    );
    fill(&r, 0, T - 1_000, 4_000, counter);
    let w = r.pump.media(T, 0);
    let ran = r.stats.view().blocks;
    let next_start = w - B - P;
    // One sample past the margin: four blocks skipped, nothing runs now.
    assert_eq!(r.pump.media(w + WORK_MARGIN + 1, 0), w + 4 * B);
    let v = r.stats.view();
    assert_eq!(
        (v.blocks, v.rebootstraps, v.missed, v.late),
        (ran, 1, 4, 0),
        "{v:?}"
    );
    // The next block calls discontinuity first, then runs.
    assert_eq!(r.pump.media(w + 4 * B, 0), w + 5 * B);
    assert!(
        r.pump.processor.log.ends_with("dp"),
        "{}",
        r.pump.processor.log
    );
    assert_eq!(r.pump.processor.log.matches('d').count(), 2);
    // The skipped blocks' stamps were never written: TX holes.
    let (_, holes) = engine_at(&r, 0, next_start + D, 4 * B as usize);
    assert_eq!(holes, 4 * B as usize);
    let (out, holes) = engine_at(&r, 0, next_start + 4 * B + D, B as usize);
    assert_eq!(holes, 0);
    assert_eq!(out[0], counter(next_start + 4 * B));
}

#[test]
fn at_block_16_the_output_stamped_t_is_the_input_stamped_t_minus_16_plus_playout_plus_margin() {
    // A device config's `block = 16` (#72): D = 16 + 96 + 16 = 128.
    let mut r = rig_at(
        Map {
            rx: vec![0, 3],
            tx: vec![1, 4],
        },
        &[0, 3],
        &[1, 4],
        TIMING_16,
    );
    fill(&r, 0, T - 1_000, 21_000, counter);
    fill(&r, 3, T - 1_000, 21_000, |p| -counter(p) - 256);
    let first = Blocks::bootstrap(T, TIMING_16).start();
    let steps = run_to(&mut r, T, T + 19_200, 0);
    let blocks = r.stats.view().blocks;
    // One block of 16 a step: twice the blocks of 32 in the same time.
    assert_eq!(blocks, steps, "one block a step");
    assert!((1_198..=1_201).contains(&blocks), "{blocks}");
    let n = (blocks * B16) as usize;
    let (out, holes) = engine_at(&r, 1, first + D16, n);
    assert_eq!(holes, 0);
    for (i, &v) in out.iter().enumerate() {
        let t = first + D16 + i as u64;
        assert_eq!(v, counter(t - D16), "TX 2 at {t} is RX 1 at T − D");
    }
    let (out, holes) = engine_at(&r, 4, first + D16, n);
    assert_eq!(holes, 0);
    assert!(
        out.iter()
            .enumerate()
            .all(|(i, &v)| v == -counter(first + i as u64) - 256),
        "TX 5 is RX 4 D earlier"
    );
    // Nothing before the first block's outputs, nothing after the last.
    assert_eq!(engine_at(&r, 1, first + D16 - 1, 1).1, 1);
    assert_eq!(engine_at(&r, 1, first + D16 + n as u64, 1).1, 1);
    assert_eq!(
        r.tx.engine_ring(1).unwrap().counters().written,
        blocks * B16
    );
    let v = r.stats.view();
    assert_eq!(
        (
            v.late,
            v.missed,
            v.rebootstraps,
            v.offset,
            v.playout,
            v.media
        ),
        (0, 0, 0, D16, P, true),
        "{v:?}"
    );
    assert_eq!(r.stats.stream_stats().frames, 16);
}

#[test]
fn at_block_16_late_is_16_behind_and_one_past_the_margin_skips_its_blocks() {
    let mut r = rig_at(
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        &[0],
        &[0],
        TIMING_16,
    );
    fill(&r, 0, T - 1_000, 4_000, counter);
    let first = Blocks::bootstrap(T, TIMING_16).start();
    let w = r.pump.media(T, 0);
    assert_eq!(
        w,
        T + B16,
        "the block due at T ran, the next is due 16 later"
    );
    let before = r.stats.view().blocks;
    // Exactly the margin (one block of 16) behind: it runs late, the next
    // one on time.
    assert_eq!(r.pump.media(w + W16, 0), w + 2 * B16);
    let v = r.stats.view();
    assert_eq!(
        (v.blocks - before, v.late, v.rebootstraps),
        (2, 1, 0),
        "{v:?}"
    );
    // One sample past the margin: two blocks skipped, nothing runs now.
    let w = w + 2 * B16;
    let next_start = w - B16 - P;
    let ran = v.blocks;
    assert_eq!(r.pump.media(w + W16 + 1, 0), w + 2 * B16);
    let v = r.stats.view();
    assert_eq!(
        (v.blocks, v.rebootstraps, v.missed, v.late),
        (ran, 1, 2, 1),
        "{v:?}"
    );
    // The next block calls discontinuity first, then runs.
    assert_eq!(r.pump.media(w + 2 * B16, 0), w + 3 * B16);
    assert!(
        r.pump.processor.log.ends_with("dp"),
        "{}",
        r.pump.processor.log
    );
    assert_eq!(r.pump.processor.log.matches('d').count(), 2);
    // Every block that ran wrote the input D earlier; the skipped blocks'
    // stamps were never written: TX holes.
    let (out, holes) = engine_at(&r, 0, first + D16, (next_start - first) as usize);
    assert_eq!(holes, 0);
    assert!(
        out.iter()
            .enumerate()
            .all(|(i, &v)| v == counter(first + i as u64))
    );
    let (_, holes) = engine_at(&r, 0, next_start + D16, 2 * B16 as usize);
    assert_eq!(holes, 2 * B16 as usize);
    let (out, holes) = engine_at(&r, 0, next_start + 2 * B16 + D16, B16 as usize);
    assert_eq!(holes, 0);
    assert_eq!(out[0], counter(next_start + 2 * B16));
}

#[test]
fn a_new_epoch_rebootstraps_and_calls_discontinuity_once() {
    let mut r = rig(
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        &[0],
        &[0],
    );
    fill(&r, 0, T - 1_000, 30_000, counter);
    let mut now = T;
    for _ in 0..5 {
        now = r.pump.media(now, 7);
    }
    assert_eq!(r.pump.processor.log, "dppppp");
    assert_eq!(
        r.stats.view().rebootstraps,
        0,
        "the first epoch is no change"
    );
    // A new epoch at the same media time: re-bootstrapped, nothing skipped.
    let wake = r.pump.media(now, 8);
    assert_eq!(wake, now + B);
    assert_eq!(r.pump.processor.log, "dpppppdp");
    let v = r.stats.view();
    assert_eq!((v.rebootstraps, v.missed), (1, 0), "{v:?}");
    // The same epoch again: no change.
    now = r.pump.media(wake, 8);
    assert_eq!(r.pump.processor.log, "dpppppdpp");
    // A re-anchor 10 000 samples on: the blocks in between are skipped
    // (⌈10 000 / 32⌉), the first block after it calls discontinuity.
    let jumped = now + 10_000;
    let wake = r.pump.media(jumped, 9);
    let v = r.stats.view();
    assert_eq!((v.rebootstraps, v.missed), (2, 313), "{v:?}");
    assert!(wake > jumped && wake <= jumped + B, "{wake}");
    r.pump.media(wake, 9);
    assert!(
        r.pump.processor.log.ends_with("dp"),
        "{}",
        r.pump.processor.log
    );
    assert_eq!(r.pump.processor.log.matches('d').count(), 3);
    // A re-anchor 5 000 samples back: re-aligned, nothing skipped.
    let back = jumped - 5_000;
    let wake = r.pump.media(back, 10);
    assert!(wake >= back && wake <= back + B, "{wake}");
    let v = r.stats.view();
    assert_eq!((v.rebootstraps, v.missed), (3, 313), "{v:?}");
    r.pump.media(wake, 10);
    assert!(
        r.pump.processor.log.ends_with("dp"),
        "{}",
        r.pump.processor.log
    );
    assert_eq!(r.pump.processor.log.matches('d').count(), 4);
}

#[test]
fn the_first_media_block_after_free_pacing_calls_discontinuity() {
    let mut r = rig(
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        &[0],
        &[0],
    );
    fill(&r, 0, T - 1_000, 30_000, counter);
    for _ in 0..3 {
        r.pump.free();
    }
    assert_eq!(r.pump.processor.log, "ppp", "free blocks: no discontinuity");
    assert!(!r.stats.view().media);
    let now = r.pump.media(T, 0);
    assert!(r.stats.view().media);
    assert_eq!(r.pump.processor.log, "pppdp");
    let now = r.pump.media(now, 0);
    assert_eq!(r.pump.processor.log, "pppdpp");
    // The clock lost and back (the same epoch): bootstrapped again.
    r.pump.free();
    assert!(!r.stats.view().media);
    r.pump.media(now + 3 * B, 0);
    assert!(r.pump.processor.log.starts_with("pppdppp"));
    assert_eq!(&r.pump.processor.log[7..9], "dp");
    // A bootstrap is not a re-bootstrap.
    let v = r.stats.view();
    assert_eq!((v.rebootstraps, v.missed), (0, 0), "{v:?}");
}

#[test]
fn free_blocks_write_nothing_to_the_engine_rings() {
    let mut r = rig(
        Map {
            rx: vec![0, 1],
            tx: vec![0, 1],
        },
        &[0, 1],
        &[0, 1],
    );
    fill(&r, 0, 0, 4_000, |_| 1 << 30);
    fill(&r, 1, 0, 4_000, |_| -(1 << 30));
    for _ in 0..5 {
        r.pump.free();
    }
    assert_eq!(r.pump.processor.calls, 5);
    assert_eq!(r.pump.processor.loudest, 0.0, "the inputs are silent");
    for tx in [0, 1] {
        assert_eq!(r.tx.engine_ring(tx).unwrap().counters().written, 0);
    }
    for rx in [0, 1] {
        let c = r.rx.rings()[rx].counters();
        assert_eq!((c.holes, c.written), (0, 4_000), "RX {} not read", rx + 1);
        assert_eq!(r.rx.rings()[rx].write_at(0, &[0]).late, 0);
    }
    let v = r.stats.view();
    assert_eq!((v.blocks, v.media), (5, false), "{v:?}");
    let h = r.stats.histograms();
    let total = |v: &[(u32, u64)]| v.iter().map(|e| e.1).sum::<u64>();
    assert_eq!((total(&h.process), total(&h.interval)), (5, 4), "{h:?}");
}

#[test]
fn a_panic_zeroes_its_block_and_the_processor_is_never_called_again() {
    let mut r = rig(
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        &[0],
        &[0],
    );
    r.pump.processor.panic_at = Some(3);
    fill(&r, 0, T - 1_000, 4_000, |p| counter(p) | 0x100);
    let first = Blocks::bootstrap(T, TIMING).start();
    let mut now = T;
    for _ in 0..6 {
        now = r.pump.media(now, 0);
    }
    assert_eq!(r.pump.processor.calls, 3, "never called after the panic");
    let s = r.stats.stream_stats();
    assert!(s.faulted, "{s:?}");
    assert_eq!(s.callbacks, 6, "the pump goes on");
    assert_eq!(s.fault.as_deref(), Some("boom in block 3"));
    // The faulting block slept 2 ms inside its own time.
    assert!(
        (2_000_000..=s.max_process_ns).contains(&s.fault_callback_ns),
        "{s:?}"
    );
    assert!(s.overruns >= 1, "{s:?}");
    // Blocks 1 and 2 carry the input; 3 (it had copied it) and after are
    // written as silence, never a hole.
    let (out, holes) = engine_at(&r, 0, first + D, 6 * B as usize);
    assert_eq!(holes, 0);
    for (i, &v) in out.iter().enumerate() {
        let want = if i < 2 * B as usize {
            counter(first + i as u64) | 0x100
        } else {
            0
        };
        assert_eq!(v, want, "sample {i}");
    }
}

#[test]
fn stream_stats_map_every_count() {
    let s = PumpStats::new(TIMING, RATE);
    assert_eq!(
        s.stream_stats(),
        StreamStats {
            frames: 32,
            ..StreamStats::default()
        }
    );
    assert_eq!(
        s.view(),
        PumpView {
            offset: D,
            playout: P,
            ..PumpView::default()
        }
    );
    // One period at 32 samples is 333 333 ns: longer is an overrun.
    s.record(333_333);
    s.record(333_334);
    s.record(5_000_999);
    s.rebootstrapped(4);
    s.rebootstrapped(0);
    s.late.fetch_add(7, Ordering::Relaxed);
    s.media.store(true, Ordering::Relaxed);
    s.running.store(true, Ordering::Relaxed);
    assert_eq!(
        s.stream_stats(),
        StreamStats {
            frames: 32,
            callbacks: 3,
            late: 7,
            missed: 4,
            overruns: 2,
            resets: 2,
            parked: false,
            faulted: false,
            running: true,
            max_process_ns: 5_000_999,
            fault: None,
            last_reopen_us: 0,
            fault_callback_ns: 0,
        }
    );
    assert_eq!(
        s.view(),
        PumpView {
            media: true,
            offset: D,
            playout: P,
            blocks: 3,
            late: 7,
            missed: 4,
            overruns: 2,
            rebootstraps: 2,
            max_process_us: 5_000,
        }
    );
    s.fault("boom".to_string(), 0);
    let st = s.stream_stats();
    assert_eq!(
        (st.faulted, st.fault.as_deref(), st.fault_callback_ns),
        (true, Some("boom"), 1),
        "a fault's time is never 0"
    );
    s.fault("again".to_string(), 812);
    assert_eq!(s.stream_stats().fault_callback_ns, 812);
    // The histograms: 1 µs buckets below two periods, then the overflow.
    let h = s.histograms();
    assert_eq!(h.top_us, 667);
    assert_eq!(h.process, vec![(333, 2), (667, 1)]);
    assert!(h.interval.is_empty());
}

#[test]
fn at_block_16_a_period_is_166_667_ns_and_the_histograms_span_two_of_them() {
    let s = PumpStats::new(TIMING_16, RATE);
    assert_eq!(
        s.stream_stats(),
        StreamStats {
            frames: 16,
            ..StreamStats::default()
        }
    );
    assert_eq!(
        s.view(),
        PumpView {
            offset: D16,
            playout: P,
            ..PumpView::default()
        }
    );
    // One period at 16 samples is 166 667 ns (rounded): longer is an
    // overrun.
    s.record(166_667);
    s.record(166_668);
    s.record(5_000_999);
    let st = s.stream_stats();
    assert_eq!((st.frames, st.callbacks, st.overruns), (16, 3, 2), "{st:?}");
    // 1 µs buckets below two periods (333 334 ns), then the overflow.
    let h = s.histograms();
    assert_eq!(h.top_us, 334);
    assert_eq!(h.process, vec![(166, 2), (334, 1)]);
}

#[test]
fn a_media_step_returns_once_its_due_blocks_ran() {
    // Bounded (the nextest `mutants` priority list): a schedule that never
    // moves on would loop here for ever.
    let mut r = rig(
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        &[0],
        &[0],
    );
    let (done, wait) = mpsc::channel();
    std::thread::spawn(move || {
        let w = r.pump.media(T, 0);
        let after = r.pump.media(w + WORK_MARGIN, 0);
        let far = r.pump.media(w + 1_000_005, 0);
        let _ = done.send((w, after, far, r.stats.view()));
    });
    let (w, after, far, v) = wait.recv_timeout(BOUND).expect("media returned");
    assert_eq!(after, w + 4 * B);
    let jumped = w + 1_000_005;
    assert!(far > jumped && far <= jumped + B, "{far}");
    assert_eq!((v.blocks, v.rebootstraps), (5, 1), "{v:?}");
}

// ---- the thread ----

/// A clock whose time moves only to the times the thread sleeps to, up to
/// a limit the test sets; at the limit the thread parks (1 ms of real time
/// a sleep). Not ready, it never parks: the free pacing sleeps on its own.
#[derive(Debug, Default)]
struct StepClock {
    now: AtomicU64,
    limit: AtomicU64,
    ready: AtomicBool,
    parked: AtomicU64,
}

impl PtpClock for StepClock {
    fn now_ns(&self) -> Option<u64> {
        self.ready
            .load(Ordering::SeqCst)
            .then(|| self.now.load(Ordering::SeqCst))
    }

    fn sleep_until(&self, ptp_ns: u64) {
        let limit = self.limit.load(Ordering::SeqCst);
        if ptp_ns <= limit {
            self.now.fetch_max(ptp_ns, Ordering::SeqCst);
            return;
        }
        self.now.fetch_max(limit, Ordering::SeqCst);
        self.parked.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(1));
    }

    fn epoch(&self) -> u32 {
        0
    }
}

impl StepClock {
    /// Lets time run to `t` and waits (at most 5 s) until the thread made
    /// a whole pass there.
    fn run_to(&self, t: u64) {
        let parked = self.parked.load(Ordering::SeqCst);
        self.limit.store(t, Ordering::SeqCst);
        let deadline = Instant::now() + BOUND;
        // The third park: one may have read the old limit, the second
        // comes before the pass at the new one.
        while self.parked.load(Ordering::SeqCst) < parked + 3 {
            assert!(Instant::now() < deadline, "the pump thread made no pass");
            std::thread::sleep(Duration::from_micros(200));
        }
    }
}

/// Waits (at most 5 s) until `stats` say `done`.
fn wait_for(stats: &PumpStats, done: impl Fn(&PumpView) -> bool) -> PumpView {
    let deadline = Instant::now() + BOUND;
    loop {
        let v = stats.view();
        if done(&v) {
            return v;
        }
        assert!(Instant::now() < deadline, "{v:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn the_pump_thread_follows_the_media_clock_paces_itself_while_it_is_not_ready_and_ends_on_stop() {
    let r = rig(
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        &[0],
        &[0],
    );
    fill(&r, 0, T - 1_000, 30_000, counter);
    let Rig {
        pump, tx, stats, ..
    } = r;
    let t0 = ns_of(T, RATE);
    let clock = Arc::new(StepClock::default());
    clock.now.store(t0, Ordering::SeqCst);
    clock.limit.store(t0, Ordering::SeqCst);
    clock.ready.store(true, Ordering::SeqCst);
    let stop = Arc::new(AtomicBool::new(false));
    let thread = spawn(
        Arc::clone(&clock) as Arc<dyn PtpClock>,
        pump,
        Arc::clone(&stop),
    )
    .unwrap();
    assert!(stats.stream_stats().running);
    // 10 ms of media time: the blocks due at T, T + 32, …, T + 960.
    clock.run_to(t0 + 10_000_000);
    let v = stats.view();
    assert!(v.media, "{v:?}");
    assert_eq!((v.blocks, v.late, v.rebootstraps), (31, 0, 0), "{v:?}");
    let first = Blocks::bootstrap(T, TIMING).start();
    let mut out = vec![0; 31 * B as usize];
    let ring = tx.engine_ring(0).unwrap();
    assert_eq!(ring.read_at(first + D, &mut out).holes, 0);
    assert!(
        out.iter()
            .enumerate()
            .all(|(i, &v)| v == counter(first + i as u64))
    );
    // The clock not ready: free blocks on the thread's own pace, nothing
    // written.
    clock.ready.store(false, Ordering::SeqCst);
    let free = wait_for(&stats, |v| !v.media && v.blocks >= 31 + 20);
    assert_eq!(ring.counters().written, 31 * B);
    // Ready again: on the media clock from where it stands.
    clock.ready.store(true, Ordering::SeqCst);
    clock.run_to(t0 + 20_000_000);
    let v = stats.view();
    assert!(v.media && v.blocks > free.blocks, "{v:?}");
    assert!(ring.counters().written > 31 * B);
    stop.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + BOUND;
    while !thread.is_finished() {
        assert!(Instant::now() < deadline, "the pump thread did not end");
        std::thread::sleep(Duration::from_millis(1));
    }
    thread.join().unwrap();
    assert!(!stats.stream_stats().running);
}
