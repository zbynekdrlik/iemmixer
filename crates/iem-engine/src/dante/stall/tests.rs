//! The stall watch's decisions on explicit instants (#130).

use super::*;

const MS: Duration = Duration::from_millis(1);

/// Feeds `watch` one look every 10 ms (the control tick) from `t0` for
/// `looks` looks, the blocks from `blocks(i)` and the clock's readiness
/// from `ready(i)`: the first look that saw a stall, by its index.
fn first_stall(
    watch: &mut StallWatch,
    t0: Instant,
    looks: u32,
    blocks: impl Fn(u32) -> u64,
    ready: impl Fn(u32) -> bool,
) -> Option<(u32, Stall)> {
    (0..looks).find_map(|i| {
        watch
            .look(t0 + 10 * MS * i, blocks(i), ready(i))
            .map(|s| (i, s))
    })
}

/// The bound the engine runs with.
fn bound() -> Duration {
    STALL_WITHIN
}

#[test]
fn the_bound_is_500_ms() {
    assert_eq!(STALL_WITHIN, Duration::from_millis(500));
}

#[test]
fn a_43_ms_gap_never_stalls() {
    // Since dstack v0.6.193 the pump waits at most W + B samples (4 128:
    // 43 ms at 96 kHz) with no block, then runs again. The watch stays quiet.
    let t0 = Instant::now();
    let mut w = StallWatch::new(bound());
    assert_eq!(w.look(t0, 77, true), None);
    assert_eq!(w.look(t0 + 43 * MS, 77, true), None);
    assert_eq!(w.look(t0 + 53 * MS, 78, true), None);
}

#[test]
fn blocks_frozen_while_the_clock_is_ready_stall_at_the_bound_and_not_before() {
    let t0 = Instant::now();
    let mut w = StallWatch::new(bound());
    // Running for 1 s (100 looks, 30 blocks a look), then frozen at the
    // count of look 100 from there on.
    let blocks = |i: u32| u64::from(i.min(100)) * 30 + 7;
    let (at, stall) =
        first_stall(&mut w, t0, 600, blocks, |_| true).expect("a frozen pump is a stall");
    // The count stopped moving at look 100 (t0 + 1 s): the stall is seen at
    // the first look 500 ms later, look 150, never earlier.
    assert_eq!(at, 150);
    assert_eq!(
        stall,
        Stall {
            blocks: 3007,
            for_: bound()
        }
    );
    // Seen again at every later look while it stays frozen.
    assert_eq!(
        w.look(t0 + 10 * MS * 151, 3007, true),
        Some(Stall {
            blocks: 3007,
            for_: bound() + 10 * MS
        })
    );
}

#[test]
fn one_look_short_of_the_bound_is_no_stall() {
    let t0 = Instant::now();
    let mut w = StallWatch::new(bound());
    assert_eq!(w.look(t0, 5, true), None);
    // 1 ms short of 500 ms: none; at it: a stall.
    assert_eq!(w.look(t0 + Duration::from_millis(499), 5, true), None);
    assert_eq!(
        w.look(t0 + Duration::from_millis(500), 5, true),
        Some(Stall {
            blocks: 5,
            for_: Duration::from_millis(500)
        })
    );
}

#[test]
fn blocks_frozen_while_the_clock_is_not_ready_are_reported_at_its_first_ready_look() {
    // The pump counts its free blocks too (dstack's PumpStats::record at
    // every block), so a count frozen through a holdover is a stuck pump.
    let t0 = Instant::now();
    let mut w = StallWatch::new(bound());
    // 10 s frozen at 42 with the clock not ready: never reported then.
    assert_eq!(first_stall(&mut w, t0, 1000, |_| 42, |_| false), None);
    // The clock comes back: reported at once, frozen since the first look.
    let back = t0 + Duration::from_secs(10);
    assert_eq!(
        w.look(back, 42, true),
        Some(Stall {
            blocks: 42,
            for_: Duration::from_secs(10)
        })
    );
}

#[test]
fn a_clock_not_ready_never_reports_and_never_hides_a_frozen_pump() {
    let t0 = Instant::now();
    let mut w = StallWatch::new(Duration::from_millis(500));
    assert_eq!(w.look(t0, 9, true), None);
    assert_eq!(w.look(t0 + 400 * MS, 9, false), None);
    assert_eq!(w.look(t0 + 450 * MS, 9, true), None);
    // Frozen since t0: not ready at 600 ms reports nothing, the next ready
    // look does, the not-ready stretch counted in.
    assert_eq!(w.look(t0 + 600 * MS, 9, false), None);
    assert_eq!(
        w.look(t0 + 610 * MS, 9, true),
        Some(Stall {
            blocks: 9,
            for_: 610 * MS
        })
    );
    // A clock that flaps under the bound never hides it.
    let mut w = StallWatch::new(Duration::from_millis(500));
    let ready = |i: u32| i % 20 < 10;
    assert_eq!(
        first_stall(&mut w, t0, 100, |_| 4, ready),
        Some((
            60,
            Stall {
                blocks: 4,
                for_: 600 * MS
            }
        ))
    );
    // A pump whose count moves while the clock is not ready (its free
    // blocks) never stalls.
    let mut w = StallWatch::new(Duration::from_millis(500));
    assert_eq!(first_stall(&mut w, t0, 300, u64::from, |_| false), None);
}

#[test]
fn advancing_blocks_never_stall() {
    let t0 = Instant::now();
    let mut w = StallWatch::new(bound());
    // 10 s of looks, one block a look (a slow but moving pump).
    assert_eq!(
        first_stall(&mut w, t0, 1000, |i| u64::from(i) + 1, |_| true),
        None
    );
    // One block every 490 ms is still moving.
    let mut w = StallWatch::new(bound());
    assert_eq!(
        first_stall(&mut w, t0, 1000, |i| u64::from(i / 49) + 1, |_| true),
        None
    );
    // One block every 400 ms too.
    let mut w = StallWatch::new(bound());
    assert_eq!(
        first_stall(&mut w, t0, 1000, |i| u64::from(i / 40) + 1, |_| true),
        None
    );
}

#[test]
fn a_pump_that_never_runs_its_first_block_while_the_clock_is_ready_stalls_at_the_bound() {
    // Right after a stall restart with the clock still in the bad state:
    // the pump bootstraps and waits, its count stays 0.
    let t0 = Instant::now();
    let mut w = StallWatch::new(bound());
    let (at, stall) =
        first_stall(&mut w, t0, 600, |_| 0, |_| true).expect("no block at all is a stall");
    assert_eq!(at, 50, "500 ms after the first ready look, never earlier");
    assert_eq!(
        stall,
        Stall {
            blocks: 0,
            for_: bound()
        }
    );
    // A clock ready only from look 100: reported at that look, the count
    // frozen since the first.
    let mut w = StallWatch::new(bound());
    assert_eq!(
        first_stall(&mut w, t0, 600, |_| 0, |i| i >= 100),
        Some((
            100,
            Stall {
                blocks: 0,
                for_: Duration::from_secs(1)
            }
        ))
    );
}

#[test]
fn a_first_block_before_the_bound_is_no_stall() {
    let t0 = Instant::now();
    let mut w = StallWatch::new(bound());
    assert_eq!(w.look(t0, 0, true), None);
    // The first block 1 ms short of the bound: the count moved.
    let first = t0 + bound() - MS;
    assert_eq!(w.look(first, 1, true), None);
    assert_eq!(w.look(t0 + bound(), 1, true), None);
    // The bound counts again from that block.
    assert_eq!(w.look(first + bound() - MS, 1, true), None);
    assert!(w.look(first + bound(), 1, true).is_some());
}

#[test]
fn the_reason_names_the_frozen_count_and_the_time() {
    let s = Stall {
        blocks: 490_392_062,
        for_: Duration::from_millis(510),
    };
    assert_eq!(
        s.reason(),
        "the engine pump stalled: blocks frozen at 490392062 for 510 ms while the media clock is ready"
    );
}
