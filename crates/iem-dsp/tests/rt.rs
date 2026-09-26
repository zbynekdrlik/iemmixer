//! I7: nothing on a process path allocates or frees. `assert_no_alloc` runs in
//! warn mode (the per-thread violation count must stay zero); the first test
//! proves the detector sees an allocation.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use iem_dsp::eq::{Band, BandKind, EqParams, Equalizer, response_db};
use iem_dsp::meter::{PeakMeter, seconds, to_db};
use iem_dsp::pan::{StereoGain, gains, mono_downmix, send_gains};
use iem_dsp::ramp::Ramp;
use iem_dsp::sanitize::{Trips, sanitize};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

fn allocation_free<F: FnOnce()>(f: F) {
    reset_violation_count();
    assert_no_alloc(f);
    assert_eq!(violation_count(), 0, "the process path allocated");
}

#[test]
fn the_detector_sees_an_allocation() {
    reset_violation_count();
    let v = assert_no_alloc(|| vec![1.0f64; 4]);
    assert!(violation_count() > 0);
    assert_eq!(v.len(), 4);
}

fn peak(freq_hz: f64, gain_lin: f64) -> Band {
    Band {
        kind: BandKind::Peak,
        enabled: true,
        freq_hz,
        gain_lin,
        bw_oct: 1.0,
    }
}

#[test]
fn eq_process_set_reset_and_response_do_not_allocate() {
    let mut p = EqParams::standard_flat();
    p.bands[2] = peak(1000.0, 2.0);
    let mut q = p;
    q.bands[2] = peak(3000.0, 0.5);
    q.bands[0].enabled = true;
    q.global_gain = 0.7;
    let mut eq = Equalizer::<2>::new(&p, 96_000.0);
    let (mut l, mut r) = (vec![0.1; 4096], vec![-0.2; 4096]);
    allocation_free(|| {
        eq.set(&q);
        assert!(!eq.is_identity());
        eq.process([l.as_mut_slice(), r.as_mut_slice()]);
        eq.reset();
        eq.process([l.as_mut_slice(), r.as_mut_slice()]);
        assert!(response_db(&q, 96_000.0, 1000.0).is_finite());
        assert_eq!(eq.params(), q);
    });
}

#[test]
fn gains_meters_ramps_and_the_sanitiser_do_not_allocate() {
    let mut g = StereoGain::new(96_000.0, 1.0, false, 0.0);
    let mut meter = PeakMeter::<2>::new();
    let mut trips = Trips::default();
    let mut ramp = Ramp::new(0.0, 960);
    let (mut l, mut r) = (vec![0.25; 512], vec![f64::NAN; 512]);
    allocation_free(|| {
        g.set(0.5, true, -0.3);
        assert!(g.steady().is_none());
        let mut acc = 0.0;
        for _ in 0..2000 {
            let (a, b) = g.tick();
            acc += a + b;
        }
        let (gl, gr) = send_gains(0.8, false, 0.25);
        acc += mono_downmix(0.1, 0.2, gl, gr) + gains(0.7).0;
        ramp.set(1.0);
        acc += ramp.tick();
        meter.observe([l.as_slice(), r.as_slice()]);
        acc += to_db(meter.take()[0]) + seconds(96_000, 96_000.0);
        assert!(trips.check([l.as_mut_slice(), r.as_mut_slice()]));
        assert!(!sanitize([l.as_mut_slice(), r.as_mut_slice()]));
        assert!(acc.is_finite());
    });
}
