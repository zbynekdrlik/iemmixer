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

#[test]
fn the_bound_is_500_ms() {
    assert_eq!(STALL_WITHIN, Duration::from_millis(500));
}

#[test]
fn blocks_frozen_while_the_clock_is_ready_stall_at_the_bound_and_not_before() {
    let t0 = Instant::now();
    let mut w = StallWatch::new(STALL_WITHIN);
    // Running for 1 s (100 looks, 30 blocks a look), then frozen at the
    // count of look 100 from there on.
    let blocks = |i: u32| u64::from(i.min(100)) * 30 + 7;
    let (at, stall) =
        first_stall(&mut w, t0, 400, blocks, |_| true).expect("a frozen pump is a stall");
    // The count stopped moving at look 100 (t0 + 1 s): the stall is seen at
    // the first look 500 ms later, look 150, never earlier.
    assert_eq!(at, 150);
    assert_eq!(
        stall,
        Stall {
            blocks: 3007,
            for_: STALL_WITHIN
        }
    );
    // Seen again at every later look while it stays frozen.
    assert_eq!(
        w.look(t0 + 10 * MS * 151, 3007, true),
        Some(Stall {
            blocks: 3007,
            for_: STALL_WITHIN + 10 * MS
        })
    );
}

#[test]
fn one_look_short_of_the_bound_is_no_stall() {
    let t0 = Instant::now();
    let mut w = StallWatch::new(STALL_WITHIN);
    assert_eq!(w.look(t0, 5, true), None);
    assert_eq!(w.look(t0 + STALL_WITHIN - MS, 5, true), None);
    assert!(w.look(t0 + STALL_WITHIN, 5, true).is_some());
}

#[test]
fn blocks_frozen_while_the_clock_is_not_ready_never_stall() {
    let t0 = Instant::now();
    let mut w = StallWatch::new(STALL_WITHIN);
    // 10 s frozen at 42 in a holdover.
    assert_eq!(first_stall(&mut w, t0, 1000, |_| 42, |_| false), None);
    // The clock comes back: the bound counts from its first ready look, not
    // from when the count stopped.
    let back = t0 + Duration::from_secs(10);
    assert_eq!(w.look(back, 42, true), None);
    assert_eq!(w.look(back + STALL_WITHIN - MS, 42, true), None);
    assert!(w.look(back + STALL_WITHIN, 42, true).is_some());
}

#[test]
fn a_holdover_in_the_middle_starts_the_bound_again() {
    let t0 = Instant::now();
    let mut w = StallWatch::new(STALL_WITHIN);
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
    let mut w = StallWatch::new(STALL_WITHIN);
    // 10 s of looks, one block a look (a slow but moving pump).
    assert_eq!(
        first_stall(&mut w, t0, 1000, |i| u64::from(i) + 1, |_| true),
        None
    );
    // One block every 400 ms is still moving.
    let mut w = StallWatch::new(STALL_WITHIN);
    assert_eq!(
        first_stall(&mut w, t0, 1000, |i| u64::from(i / 40) + 1, |_| true),
        None
    );
}

#[test]
fn no_block_yet_never_stalls() {
    let t0 = Instant::now();
    let mut w = StallWatch::new(STALL_WITHIN);
    // The start: the clock ready, the pump's first block not yet run.
    assert_eq!(first_stall(&mut w, t0, 1000, |_| 0, |_| true), None);
    // Its first block arms the watch from then.
    let first = t0 + Duration::from_secs(10);
    assert_eq!(w.look(first, 1, true), None);
    assert!(w.look(first + STALL_WITHIN, 1, true).is_some());
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
