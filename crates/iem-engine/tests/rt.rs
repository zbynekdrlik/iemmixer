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
    let mut b = common::buffers(&s.topo);
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
    // The D5(b) loopback probe ran under the detector: one block of loopback.
    assert_eq!(
        s.measured,
        common::BLOCK as u64,
        "the loopback probe measured"
    );
    // The listen probe ran under the detector (S7), into both probe rings.
    assert!(
        s.handles.probes.iter().all(|c| c.slots() > 0),
        "the probe path ran"
    );
    // The backends' histogram records ran under the detector too (S7).
    let h = s.hists.snapshot();
    let total = |v: &[(u32, u64)]| v.iter().map(|e| e.1).sum::<u64>();
    assert_eq!(
        (total(&h.interval), total(&h.process)),
        (6_000, 6_000),
        "the histograms recorded every block"
    );
    assert!(
        h.interval.iter().any(|&(b, _)| b == h.top_us),
        "the overflow path ran"
    );
}

/// Gen 3 phase C (Linux): the Dante backend's pump block (the RX rings
/// read, the real processor on the worst-case scenario, the outputs
/// quantized into the engine rings, the stats and both histograms) does
/// not allocate either.
#[cfg(target_os = "linux")]
#[test]
fn the_dante_pump_block_does_not_allocate() {
    const BLOCKS: usize = 300;
    let mut r = common::dante::pump_rig(BLOCKS);
    reset_violation_count();
    assert_no_alloc(|| common::dante::pump_drive(&mut r, BLOCKS));
    assert_eq!(violation_count(), 0, "the pump's blocks allocated");
    let v = r.stats.view();
    assert_eq!((v.blocks, v.late, v.rebootstraps), (BLOCKS as u64, 0, 0));
    assert!(!r.stats.stream_stats().faulted, "the processor ran clean");
    assert_eq!(
        r.measured,
        common::BLOCK as u64,
        "the loopback probe measured"
    );
    assert!(
        r.handles.probes.iter().all(|c| c.slots() > 0),
        "the probe path ran"
    );
    assert!(r.handles.meters.read().seq > 0);
    assert!(
        common::dante::loudest_output(&mut r, BLOCKS) > 0,
        "the mixes reached the engine rings"
    );
    let h = r.stats.histograms();
    assert_eq!(h.process.iter().map(|e| e.1).sum::<u64>(), BLOCKS as u64);
}
