// Part of iem-limiter-mga (GPL-3.0-or-later); see ../src/lib.rs.

//! I7: the limiter's process path neither allocates nor frees (assert_no_alloc,
//! warn mode, violation count zero); the first test proves the detector works.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use iem_limiter_mga::{Limiter, Mga, Sliders};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn the_detector_sees_an_allocation() {
    reset_violation_count();
    let v = assert_no_alloc(|| vec![0u8; 16]);
    assert!(violation_count() > 0);
    assert_eq!(v.len(), 16);
}

#[test]
fn processing_and_controls_do_not_allocate() {
    let mut lim = Limiter::new(96_000.0, -6.0);
    let mut core = Mga::new(
        48_000.0,
        Sliders {
            threshold_db: -3.0,
            release_ms: 50.0,
            link_pct: 75.0,
            ceiling_db: -3.0,
        },
    );
    let mut l: Vec<f64> = (0..4096)
        .map(|i| 3.0 * (f64::from(i) * 0.01).sin())
        .collect();
    let mut r = l.clone();
    reset_violation_count();
    assert_no_alloc(|| {
        lim.process(&mut l, &mut r);
        lim.set_limit_db(-3.0);
        lim.set_enabled(false);
        lim.process(&mut l, &mut r);
        lim.set_enabled(true);
        assert!(lim.gr_db() <= 0.0);
        assert!(lim.active_seconds() > 0.0);
        lim.reset_active();
        lim.reset();
        let (a, b) = core.tick(2.0, -2.0);
        core.set_sliders(
            48_000.0,
            Sliders {
                threshold_db: 0.0,
                release_ms: 100.0,
                link_pct: 0.0,
                ceiling_db: 0.0,
            },
        );
        core.reset();
        assert!(a.is_finite() && b.is_finite() && core.gr_meter() == 1.0);
    });
    assert_eq!(violation_count(), 0, "the limiter allocated");
}
