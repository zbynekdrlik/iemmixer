//! I7 for the RT panic path (S6 design note §3): marking the callback thread
//! real-time and recording a panic neither allocate nor free; nor does
//! counting a driver message, which may come on the callback's thread (#9
//! 2026-09-28). Its own binary, so only these tests run on the allocation
//! detector. `assert_no_alloc` runs in warn mode (the per-thread violation
//! count must stay zero); the first test proves the detector sees an
//! allocation.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use iem_audio_io::messages::{Messages, Topic};
use iem_audio_io::rtpanic::{is_rt_thread, latest, mark_rt_thread, record};
use iem_audio_io::telemetry::{Telemetry, selector};

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

/// The driver calls `asioMessage` and `sampleRateDidChange` on a thread of
/// its choosing, the audio callback's too: the host's handler counts in
/// atomics only (the process-wide log and the stream's telemetry).
#[test]
fn counting_a_driver_message_does_not_allocate() {
    static LOG: Messages = Messages::new();
    let (violations, reopen) = std::thread::spawn(|| {
        let telemetry = Telemetry::new(32, 96_000.0);
        reset_violation_count();
        assert_no_alloc(|| {
            LOG.message(selector::RESET_REQUEST, 0, 1);
            LOG.rate_change(96_000.0, 2);
            telemetry.driver_message(selector::RESET_REQUEST, 0);
            telemetry.on_rate_change();
        });
        (violation_count(), telemetry.take_reopen())
    })
    .join()
    .unwrap();
    assert_eq!(
        (violations, reopen),
        (0, true),
        "counting a driver message allocated"
    );
    assert_eq!(
        (LOG.count(Topic::ResetRequest), LOG.count(Topic::RateChange)),
        (1, 1)
    );
}
