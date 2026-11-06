//! I7 for the callback's bookkeeping: its telemetry and recording the stream
//! histograms neither allocate nor free. Its own binary, so
//! only these tests run on the allocation detector. `assert_no_alloc` runs in
//! warn mode (the per-thread violation count must stay zero); the first test
//! proves the detector sees an allocation.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use iem_audio_io::hist::StreamHists;
use iem_audio_io::telemetry::{GLITCH_CAPACITY, GapScan, Telemetry, period_ns};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn the_detector_sees_an_allocation() {
    reset_violation_count();
    let v = assert_no_alloc(|| vec![1u8; 4]);
    assert!(violation_count() > 0);
    assert_eq!(v.len(), 4);
}

/// I7: the callback records glitches, per-CPU and thread counters
/// without allocating — including the ring-full drop path — and the hwlat
/// scanner observes gaps without allocating after `new`. The glitch ring and
/// the scanner's `largest` buffer are preallocated; nothing here may touch the
/// allocator.
#[test]
fn callback_telemetry_and_the_gap_scan_do_not_allocate() {
    let violations = std::thread::spawn(|| {
        let t = Telemetry::new(32, 96_000.0);
        let mut scan = GapScan::new(10_000);
        reset_violation_count();
        assert_no_alloc(|| {
            let mut at = 1_000;
            // More than the ring holds, so the drop path runs too. Each
            // callback is missed (700 µs > 2 periods), its position jumps by
            // two buffers, and the callback overruns: three glitches each.
            for i in 0..GLITCH_CAPACITY as u64 + 100 {
                t.on_thread((i % 70) as u32, 900 + (i % 2) as u32);
                t.on_callback(at, Some(i as i64 * 64));
                t.on_done(1_000_000);
                at += 700_000;
            }
            // Fills and then only replaces the scanner's largest buffer.
            for i in 0..100_u64 {
                scan.observe(i * 1_000_000, i * 1_000_000 + 20_000 + i);
            }
        });
        violation_count()
    })
    .join()
    .unwrap();
    assert_eq!(violations, 0, "the callback telemetry allocated");
}

/// I7: the backends record every callback's interval and
/// time into the stream histograms; neither allocates nor frees, overflow
/// included. Built outside the detector: the arrays are the stream's.
#[test]
fn recording_the_stream_histograms_does_not_allocate() {
    let (violations, snap) = std::thread::spawn(|| {
        let h = StreamHists::new(period_ns(32, 96_000.0));
        reset_violation_count();
        assert_no_alloc(|| {
            for i in 0..100_000_u64 {
                h.interval.record(333_000 + (i % 400) * 1_000); // spans the overflow bucket
                h.process.record(i % 90_000);
            }
            h.interval.record(u64::MAX);
        });
        (violation_count(), h.snapshot())
    })
    .join()
    .unwrap();
    assert_eq!(violations, 0, "recording a histogram allocated");
    assert_eq!(snap.interval.iter().map(|e| e.1).sum::<u64>(), 100_001);
}
