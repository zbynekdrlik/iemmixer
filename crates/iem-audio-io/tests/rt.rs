//! I7 for the RT panic path (S6 design note §3): marking the callback thread
//! real-time and recording a panic neither allocate nor free. Its own binary,
//! so only these tests run on the allocation detector. `assert_no_alloc` runs
//! in warn mode (the per-thread violation count must stay zero); the first
//! test proves the detector sees an allocation.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use iem_audio_io::rtpanic::{is_rt_thread, latest, mark_rt_thread, record};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn the_detector_sees_an_allocation() {
    reset_violation_count();
    let v = assert_no_alloc(|| vec![1u8; 4]);
    assert!(violation_count() > 0);
    assert_eq!(v.len(), 4);
}

// The only test of this binary that records, so the count is its own.
#[test]
fn marking_and_recording_on_the_rt_thread_do_not_allocate() {
    let before = latest().map_or(0, |p| p.count);
    let (marked, violations) = std::thread::spawn(|| {
        reset_violation_count();
        let marked = assert_no_alloc(|| {
            mark_rt_thread();
            record(file!(), line!(), column!());
            is_rt_thread()
        });
        (marked, violation_count())
    })
    .join()
    .unwrap();
    assert_eq!(
        (marked, violations),
        (true, 0),
        "the RT panic path allocated"
    );
    assert_eq!(latest().map(|p| p.count), Some(before + 1));
}
