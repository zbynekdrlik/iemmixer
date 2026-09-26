//! I7: `process()` neither allocates nor frees, whatever the commands, taps,
//! talkback, test signal, meters or sanitiser trips (design note §3.8).
//! `assert_no_alloc` runs in warn mode (the violation count must stay zero);
//! the first test proves the detector sees an allocation. Its own test binary,
//! because it installs the global allocator.

mod common;

use std::sync::atomic::Ordering;

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn the_detector_sees_an_allocation() {
    reset_violation_count();
    let v = assert_no_alloc(|| vec![1.0f64; 4]);
    assert!(violation_count() > 0);
    assert_eq!(v.len(), 4);
}

#[test]
fn process_does_not_allocate() {
    let mut s = common::scenario();
    let mut b = common::buffers(&s.graph);
    assert!(s.groups.len() >= 18, "{}", s.groups.len());
    assert!(
        s.groups.iter().any(|g| g.len() > 300),
        "the import group is in the mix"
    );
    reset_violation_count();
    assert_no_alloc(|| common::drive(&mut s, &mut b, 6_000));
    assert_eq!(violation_count(), 0, "the process path allocated");
    assert!(s.handles.status.trips.load(Ordering::Relaxed) >= 5);
    assert_eq!(s.processor.time(), 6_000 * common::BLOCK as u64);
    assert!(b.output.iter().all(|y| y.is_finite() && y.abs() <= 1.0));
    assert!(s.handles.meters.read().seq > 0);
}
