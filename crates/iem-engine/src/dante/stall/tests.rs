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

/// The device's rate on the box: the bound the engine runs with.
fn bound() -> Duration {
    within(96_000)
}

#[test]
fn the_bound_is_dstacks_longest_wait_plus_500_ms() {
    // dstack's Blocks::next waits through a clock step back of up to
    // DISCONTINUITY samples (2 s at 96 kHz) before it re-bootstraps.
    assert_eq!(DISCONTINUITY, 192_000);
    assert_eq!(SLACK, Duration::from_millis(500));
    assert_eq!(within(96_000), Duration::from_millis(2500));
    assert_eq!(within(48_000), Duration::from_millis(4500));
    assert_eq!(within(192_000), Duration::from_millis(1500));
    // 192 000 / 44 100 s = 4.353741496 s (truncated to the ns).
    assert_eq!(within(44_100), Duration::from_nanos(4_353_741_496) + SLACK);
    // A rate of 0 never divides by zero.
    assert_eq!(within(0), Duration::from_secs(192_000) + SLACK);
}

#[test]
fn a_clock_step_back_dstack_still_waits_through_never_stalls() {
    // The pump waits out a step back of DISCONTINUITY samples with no
    // block: 2 s at 96 kHz, then runs again. The watch stays quiet.
    let t0 = Instant::now();
    let mut w = StallWatch::new(bound());
    assert_eq!(w.look(t0, 77, true), None);
    assert_eq!(w.look(t0 + Duration::from_secs(2), 77, true), None);
    assert_eq!(
        w.look(t0 + Duration::from_secs(2) + 10 * MS, 78, true),
        None
    );
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
    // the first look 2.5 s later, look 350, never earlier.
    assert_eq!(at, 350);
    assert_eq!(
        stall,
        Stall {
            blocks: 3007,
            for_: bound()
        }
    );
    // Seen again at every later look while it stays frozen.
    assert_eq!(
        w.look(t0 + 10 * MS * 351, 3007, true),
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
    // 1 ms short of 2.5 s: none; at it: a stall.
    assert_eq!(w.look(t0 + Duration::from_millis(2499), 5, true), None);
    assert_eq!(
        w.look(t0 + Duration::from_millis(2500), 5, true),
        Some(Stall {
            blocks: 5,
            for_: Duration::from_millis(2500)
        })
    );
}

#[test]
fn blocks_frozen_while_the_clock_is_not_ready_never_stall() {
    let t0 = Instant::now();
    let mut w = StallWatch::new(bound());
    // 10 s frozen at 42 in a holdover.
    assert_eq!(first_stall(&mut w, t0, 1000, |_| 42, |_| false), None);
    // The clock comes back: the bound counts from its first ready look, not
    // from when the count stopped.
    let back = t0 + Duration::from_secs(10);
    assert_eq!(w.look(back, 42, true), None);
    assert_eq!(w.look(back + bound() - MS, 42, true), None);
    assert!(w.look(back + bound(), 42, true).is_some());
}

#[test]
fn a_holdover_in_the_middle_starts_the_bound_again() {
    let t0 = Instant::now();
    let mut w = StallWatch::new(Duration::from_millis(500));
    assert_eq!(w.look(t0, 9, true), None);
    assert_eq!(w.look(t0 + 400 * MS, 9, false), None);
    assert_eq!(w.look(t0 + 450 * MS, 9, true), None);
    // 600 ms after the first look, but only 150 ms of it ready again.
    assert_eq!(w.look(t0 + 600 * MS, 9, true), None);
    assert!(w.look(t0 + 950 * MS, 9, true).is_some());
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
    // One block every 2.4 s is still moving.
    let mut w = StallWatch::new(bound());
    assert_eq!(
        first_stall(&mut w, t0, 2000, |i| u64::from(i / 240) + 1, |_| true),
        None
    );
    // One block every 400 ms against a 500 ms bound too.
    let mut w = StallWatch::new(Duration::from_millis(500));
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
    assert_eq!(at, 250, "2.5 s after the first ready look, never earlier");
    assert_eq!(
        stall,
        Stall {
            blocks: 0,
            for_: bound()
        }
    );
    // Armed from the first ready look, not from the start.
    let mut w = StallWatch::new(bound());
    assert_eq!(
        first_stall(&mut w, t0, 600, |_| 0, |i| i >= 100),
        Some((
            350,
            Stall {
                blocks: 0,
                for_: bound()
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
