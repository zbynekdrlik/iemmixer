//! Unit tests of the listen probe (S7 design note §6, #10): while a HIL
//! signal with `listen` runs, each listened slot's probe tap carries the
//! spare outputs' own samples, its listen tap stays exactly silent and every
//! mix's TX zero (the owner's rule, #9 2026-09-28).

use super::hil::{AT_ONCE, SPARE, TEST_FLAG};
use super::*;
use iem_engine_proto::ErrCode;

/// 100 ms: every ring (200 ms) is drained after each, so none overruns.
const CHUNK: usize = 9_600;
/// The signal's span from sample 0: its 0.2 s TTL, then the 50 ms fade-out.
const END: usize = 19_200 + 4_800;
/// The signal's 50 ms fade-in.
const FADE: usize = 4_800;
/// A run past the signal's end.
const RUN: usize = 28_800;
/// Both listen slots: the engineer (slot 0) and one member (slot 1).
const BOTH: [&str; 2] = ["engineer", "member1"];

/// The HIL signal on both spare outputs at −20 dBFS (the test cap) for 0.2 s.
fn signal(listen: bool) -> Cmd {
    Cmd::HilTestSignal {
        input: input("mic1"),
        hz: 1000.0,
        dbfs: -20.0,
        ttl_s: 0.2,
        card_tx: SPARE.to_vec(),
        listen,
    }
}

/// The test site with mic1 open at 0 dB in the engineer's mix and member1's,
/// so both mixes carry the sine (their TX and taps would, without the HIL
/// gate), the listen slots `slots` and `start` at sample 0. The slots are
/// transient: they reach the processor as commands, not through the state.
fn probe_rig(opts: Options, slots: &[&str], start: &Cmd) -> Rig {
    let site = crate::test_support::test_site_text();
    let open = BOTH.map(|m| level(m, src_in("mic1"), 0.0));
    let mut r = rig_hil(&site, &open, TEST_FLAG, opts, SPARE.to_vec());
    for m in slots {
        r.at(0, &Cmd::StartListen { mix: mix(m) });
    }
    r.at(0, start);
    r
}

/// What a run left: every output; what both listen taps and both probe taps
/// carried; the loudest engineer mix in the meter frames read after each
/// chunk.
struct Heard {
    out: Vec<Vec<f64>>,
    taps: [Vec<f32>; 2],
    probes: [Vec<f32>; 2],
    loudest: f64,
}

/// Moves everything `c` holds to the end of `into`.
fn take(c: &mut Consumer<f32>, into: &mut Vec<f32>) {
    let mut got = vec![0.0f32; c.slots()];
    c.pop_entire_slice(&mut got).unwrap();
    into.extend(got);
}

/// Runs `samples` of silent inputs at B = 32, `CHUNK` at a time, draining
/// both taps and both probes after each chunk.
fn hear(r: &mut Rig, samples: usize) -> Heard {
    let rxn = r.p.topo.rx.len();
    let engineer = r.p.topo.engineer;
    let mut h = Heard {
        out: vec![Vec::new(); r.p.outputs()],
        taps: Default::default(),
        probes: Default::default(),
        loudest: 0.0,
    };
    let mut done = 0;
    while done < samples {
        let n = CHUNK.min(samples - done);
        let out = r.run(&Planar::new(rxn, n), 32);
        for (ch, all) in h.out.iter_mut().enumerate() {
            all.extend_from_slice(out.channel(ch));
        }
        for k in 0..2 {
            take(&mut r.h.taps[k], &mut h.taps[k]);
            take(&mut r.h.probes[k], &mut h.probes[k]);
        }
        h.loudest = h.loudest.max(r.h.meters.read().mixes[engineer][0]);
        done += n;
    }
    h
}

/// The left channel of an interleaved tap; asserts both channels are equal.
fn left(tap: &[f32], what: &str) -> Vec<f32> {
    let (pairs, rest) = tap.as_chunks::<2>();
    assert!(rest.is_empty(), "{what}: whole stereo frames");
    assert!(pairs.iter().all(|[a, b]| a == b), "{what}: l == r");
    pairs.iter().map(|[a, _]| *a).collect()
}

#[test]
fn a_listen_probe_carries_the_spare_outputs_sine_on_both_probe_taps() {
    let mut r = probe_rig(AT_ONCE, &BOTH, &signal(true));
    let tx = r.p.topo.tx.len();
    let heard = hear(&mut r, RUN);
    // Spare output 94 (HIL slot 0) as f32: what each probe's left carries.
    let spare: Vec<f32> = heard.out[tx][..END].iter().map(|y| *y as f32).collect();
    assert!(heard.out[tx][END..].iter().all(|y| *y == 0.0));
    for (k, probe) in heard.probes.iter().enumerate() {
        let what = format!("probe {k}");
        assert_eq!(probe.len(), 2 * END, "{what}: one frame a sample");
        let l = left(probe, &what);
        assert!(l == spare, "{what}: the spare output's own samples");
        let peak = l[FADE..].iter().fold(0.0f32, |m, y| m.max(y.abs()));
        assert!((f64::from(peak) - 0.1).abs() < 1e-6, "{what}: {peak}");
    }
    assert_eq!(r.h.status.tap_overruns.load(Ordering::Relaxed), 0);
    // It follows the output's fade like the spare outputs (here the 500 ms
    // fade-in from the start): silent while the fade is, never above it.
    let mut r = probe_rig(Options::default(), &BOTH, &signal(true));
    let heard = hear(&mut r, CHUNK);
    let spare: Vec<f32> = heard.out[tx].iter().map(|y| *y as f32).collect();
    assert_eq!(spare[0], 0.0);
    assert!(spare.iter().any(|y| y.abs() > 1e-3));
    for (k, probe) in heard.probes.iter().enumerate() {
        let l = left(probe, &format!("faded probe {k}"));
        assert!(
            l == spare,
            "faded probe {k}: the spare output's own samples"
        );
    }
}

#[test]
fn the_listen_taps_stay_exactly_silent_during_a_probe() {
    let mut r = probe_rig(AT_ONCE, &BOTH, &signal(true));
    let heard = hear(&mut r, RUN);
    assert!(heard.loudest > 0.05, "the engineer's mix carries the sine");
    for k in 0..2 {
        let tap = &heard.taps[k];
        assert_eq!(tap.len(), 2 * RUN, "tap {k}: its cadence stays");
        let during = &tap[..2 * END];
        assert!(
            during.iter().all(|x| *x == 0.0),
            "tap {k} carried the mix during the probe"
        );
        assert_eq!(
            heard.probes[k].len(),
            during.len(),
            "probe {k}: the tap's cadence"
        );
    }
}

#[test]
fn every_mix_tx_is_zero_during_a_probe() {
    let mut r = probe_rig(AT_ONCE, &BOTH, &signal(true));
    let tx = r.p.topo.tx.len();
    let heard = hear(&mut r, RUN);
    assert!(heard.loudest > 0.05, "the engineer's mix carries the sine");
    assert!(heard.probes.iter().all(|p| p.len() == 2 * END));
    for (ch, y) in heard.out[..tx].iter().enumerate() {
        assert!(
            y[..END].iter().all(|v| *v == 0.0),
            "TX slot {ch} sounded during the probe"
        );
    }
}

#[test]
fn no_probe_without_listen_or_without_a_listened_slot() {
    let mut r = probe_rig(AT_ONCE, &BOTH, &signal(false));
    let heard = hear(&mut r, RUN);
    assert!(heard.probes.iter().all(Vec::is_empty), "listen: false");
    for (k, tap) in heard.taps.iter().enumerate() {
        assert_eq!(tap.len(), 2 * RUN, "tap {k}");
        assert!(tap[..2 * END].iter().all(|x| *x == 0.0), "tap {k}");
    }
    let mut r = probe_rig(AT_ONCE, &BOTH[..1], &signal(true));
    let heard = hear(&mut r, RUN);
    assert_eq!(heard.probes[0].len(), 2 * END, "slot 0 is listened");
    assert!(heard.probes[1].is_empty(), "slot 1 is not");
    let mut r = probe_rig(AT_ONCE, &[], &signal(true));
    let heard = hear(&mut r, RUN);
    assert!(heard.probes.iter().all(Vec::is_empty), "no slot listened");
    assert!(heard.taps.iter().all(Vec::is_empty));
}

#[test]
fn a_plain_test_signal_never_feeds_the_probe() {
    let plain = Cmd::StartTestSignal {
        input: input("mic1"),
        hz: 1000.0,
        dbfs: -20.0,
        ttl_s: 0.2,
    };
    let mut r = probe_rig(AT_ONCE, &BOTH, &plain);
    let tx = r.p.topo.tx.len();
    let heard = hear(&mut r, RUN);
    assert!(heard.probes.iter().all(Vec::is_empty));
    for (k, tap) in heard.taps.iter().enumerate() {
        assert!(
            tap[..2 * END].iter().any(|x| x.abs() > 0.01),
            "tap {k} carries the capped mix"
        );
    }
    assert!(heard.out[tx].iter().all(|y| *y == 0.0), "no spare output");
}

#[test]
fn the_probe_taps_go_quiet_when_the_signal_ends() {
    let mut r = probe_rig(AT_ONCE, &BOTH, &signal(true));
    let heard = hear(&mut r, END);
    for (k, probe) in heard.probes.iter().enumerate() {
        let l = left(probe, &format!("probe {k}"));
        assert_eq!(l.len(), END);
        // The fade-out's last quarter period: at most 24/4 800 of 0.1.
        let tail = l[END - 24..].iter().fold(0.0f32, |m, y| m.max(y.abs()));
        assert!(tail < 1e-3, "probe {k} fades out with the signal: {tail}");
    }
    let after = hear(&mut r, CHUNK);
    assert!(
        after.probes.iter().all(Vec::is_empty),
        "nothing after the end"
    );
    assert!(
        after.taps.iter().all(|t| t.len() == 2 * CHUNK),
        "the taps go on"
    );
    // A stop ends it with its 50 ms fade-out, and the probe with it.
    let mut r = probe_rig(AT_ONCE, &BOTH, &signal(true));
    r.at(1_000, &Cmd::StopTestSignal);
    let heard = hear(&mut r, CHUNK);
    for (k, probe) in heard.probes.iter().enumerate() {
        assert_eq!(probe.len(), 2 * (1_000 + 4_800), "probe {k} after a stop");
    }
}

/// The probe shares the taps' overrun counter: a ring nobody drains keeps
/// its first 200 ms, and every segment that found it full counts once.
#[test]
fn a_full_probe_ring_counts_its_overruns() {
    let mut r = probe_rig(AT_ONCE, &BOTH[..1], &signal(true));
    let rxn = r.p.topo.rx.len();
    let mut sink = Vec::new();
    for _ in 0..RUN / CHUNK {
        r.run(&Planar::new(rxn, CHUNK), 32);
        take(&mut r.h.taps[0], &mut sink);
    }
    assert_eq!(r.h.probes[0].slots(), TAP_RING);
    // 750 segments of 32 samples, 600 of them fill the ring.
    assert_eq!(r.h.status.tap_overruns.load(Ordering::Relaxed), 150);
}

#[test]
fn the_listen_flag_travels_from_the_command_to_the_rt_op() {
    let site = crate::test_support::test_site_text();
    let mut r = rig_hil(&site, &[], TEST_FLAG, AT_ONCE, SPARE.to_vec());
    for listen in [true, false] {
        let out = r.core.apply(&signal(listen)).unwrap();
        assert_eq!(out.rt.len(), 1, "{:?}", out.rt);
        let RtOp::HilTestSignal { listen: got, .. } = out.rt[0] else {
            panic!("{:?}", out.rt)
        };
        assert_eq!(got, listen);
    }
    let mut off = rig_hil(&site, &[], Flags::default(), AT_ONCE, SPARE.to_vec());
    let e = off.core.apply(&signal(true)).unwrap_err();
    assert_eq!(e.code, ErrCode::Forbidden);
}
