//! Unit tests of the RT processor for S6 (split from `rt_tests.rs`, #32):
//! the held output, the reopen fade-in, HIL's spare outputs and the D5(b)
//! loopback return.

use super::*;
use iem_engine_proto::ErrCode;

/// `rig_hil` that also opens the D5(b) loopback return (S6 test 5): one
/// return input per spare output, after the topology's rx.
fn rig_loopback(site: &str, cmds: &[Cmd], flags: Flags, opts: Options, hil: Vec<u16>) -> Rig {
    let topo = Arc::new(compile(&parse(site).unwrap()).unwrap());
    let spare = hil.len();
    let mut core = Core::new(Arc::clone(&topo), &MixState::default(), 0, flags).with_hil(hil);
    for c in cmds {
        core.apply(c).unwrap();
    }
    let (p, h) = Processor::with_hil(topo, &core.state(), &[], opts, spare, spare);
    Rig { core, p, h }
}

/// `--hold` (S6 design note §4).
const HELD: Options = Options {
    fade_in_ms: 500.0,
    hold: true,
};

#[test]
fn hold_keeps_outputs_silent_until_arm() {
    let mut r = rig_with(
        SITE,
        &[level("m1", src_in("mono"), 0.0)],
        Flags::default(),
        HELD,
    );
    let full = 0.25 * g0() * g0();
    let out = r.run(&rx(&[0.25], 9_600), 32);
    for ch in 0..out.channels() {
        assert!(out.channel(ch).iter().all(|y| *y == 0.0), "held: TX {ch}");
    }
    // Arm: the 500 ms fade-in from silence.
    assert!(push_group(&mut r.h.cmds, 0, &[RtOp::Arm]));
    let out = r.run(&rx(&[0.25], 50_000), 32);
    let y = out.channel(M1_L);
    assert!(y[0] > 0.0 && y[0] < full * 1e-3, "{}", y[0]);
    assert!(close(y[24_000], full * 0.5, 1e-3), "{}", y[24_000]);
    assert!(close(y[49_999], full, 1e-15));
    // A second Arm leaves the level alone.
    assert!(push_group(&mut r.h.cmds, 0, &[RtOp::Arm]));
    let out = r.run(&rx(&[0.25], 512), 32);
    assert!(out.channel(M1_L).iter().all(|y| close(*y, full, 1e-15)));
    // Faded out while held: an Arm brings nothing back.
    let mut r = rig_with(
        SITE,
        &[level("m1", src_in("mono"), 0.0)],
        Flags::default(),
        HELD,
    );
    assert!(push_group(&mut r.h.cmds, 0, &[RtOp::FadeOut, RtOp::Arm]));
    let out = r.run(&rx(&[0.25], 50_000), 32);
    assert!(out.channel(M1_L).iter().all(|y| *y == 0.0));
    assert!(r.h.status.faded_out.load(Ordering::Acquire));
}

#[test]
fn discontinuity_restarts_the_fade_in() {
    let open = [level("m1", src_in("mono"), 0.0)];
    let mut r = rig_with(SITE, &open, Flags::default(), Options::default());
    let full = 0.25 * g0() * g0();
    let out = r.run(&rx(&[0.25], 50_000), 32);
    assert!(close(out.channel(M1_L)[49_999], full, 1e-15));
    // After a driver reopen the output fades in again over 500 ms.
    r.p.discontinuity();
    let out = r.run(&rx(&[0.25], 50_000), 32);
    let y = out.channel(M1_L);
    assert!(y[0] > 0.0 && y[0] < full * 1e-3, "{}", y[0]);
    assert!(close(y[24_000], full * 0.5, 1e-3), "{}", y[24_000]);
    assert!(close(y[49_999], full, 1e-15));
    // Held: a reopen does not arm it.
    let mut r = rig_with(SITE, &open, Flags::default(), HELD);
    r.p.discontinuity();
    let out = r.run(&rx(&[0.25], 9_600), 32);
    assert!(out.channel(M1_L).iter().all(|y| *y == 0.0));
    // Faded out (a shutdown): a reopen does not bring the sound back.
    let mut r = rig(&open);
    assert!(push_group(&mut r.h.cmds, 0, &[RtOp::FadeOut]));
    r.run(&rx(&[0.25], 9_600), 32);
    r.p.discontinuity();
    let out = r.run(&rx(&[0.25], 9_600), 32);
    assert!(out.channel(M1_L).iter().all(|y| *y == 0.0));
}

/// HIL's spare outputs of the test site (`[guard] hil_tx`), as `run` opens
/// them under the test-signal flag.
const SPARE: [u16; 2] = [94, 95];
/// The D5(b) loopback round-trip (S6 test 5): the HIL sine leaves on the
/// spare outputs; a synthetic loopback returns it on the spare inputs one
/// block later; the probe measures exactly that delay, and it clears when a
/// new signal starts.
#[test]
fn the_loopback_round_trip_is_measured() {
    use iem_audio_io::{Block, Process};
    let site = crate::test_support::test_site_text();
    let mut r = rig_loopback(&site, &[], TEST_FLAG, AT_ONCE, SPARE.to_vec());
    let topo = Arc::clone(&r.p.topo);
    let tx = topo.tx.len();
    let rxn = topo.rx.len();
    // Return inputs sit after the topology's rx; outputs are tx + the 2 spares.
    let ins = rxn + 2;
    let outs = r.p.outputs();
    assert_eq!(outs, tx + 2);
    // A HIL signal on both spares, long enough to travel and return.
    r.at(
        0,
        &Cmd::HilTestSignal {
            input: input("mic1"),
            hz: 1000.0,
            dbfs: -30.0,
            ttl_s: 0.1,
            card_tx: SPARE.to_vec(),
        },
    );
    const N: usize = 256;
    const DELAY: u64 = N as u64; // one block of loopback delay
    let mut prev_hil = vec![0.0f64; 2 * N]; // last block's spare outputs
    let mut ibuf = vec![0.0f64; ins * N];
    let mut obuf = vec![0.0f64; outs * N];
    for _ in 0..8 {
        ibuf.iter_mut().for_each(|x| *x = 0.0);
        // Asymmetric loopback: only return 0 carries the previous block's
        // spare-0 output; return 1 stays silent. So the arrival can only be
        // read from the correct return slot (rxn + 0), which pins the mapping
        // (iemmixer#9 review: a wrong return slot would read silence and miss
        // the echo).
        ibuf[rxn * N..(rxn + 1) * N].copy_from_slice(&prev_hil[..N]);
        obuf.iter_mut().for_each(|x| *x = 0.0);
        let mut b = Block::new(N, &ibuf, &mut obuf);
        r.p.process(&mut b);
        // Keep this block's spare outputs for the next block's return.
        for j in 0..2 {
            let src = &obuf[(tx + j) * N..(tx + j + 1) * N];
            prev_hil[j * N..(j + 1) * N].copy_from_slice(src);
        }
    }
    let samples =
        r.h.status
            .loopback_samples
            .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(samples, DELAY, "round-trip should be one block of delay");
    // A new signal clears the measurement.
    r.at(
        DELAY,
        &Cmd::HilTestSignal {
            input: input("mic1"),
            hz: 1000.0,
            dbfs: -30.0,
            ttl_s: 0.1,
            card_tx: SPARE.to_vec(),
        },
    );
    let silent = vec![0.0; ins * N];
    let mut b = Block::new(N, &silent, &mut obuf);
    r.p.process(&mut b);
    assert_eq!(
        r.h.status
            .loopback_samples
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "a new signal restarts the measurement"
    );
}

/// S6 test 5 (#32 D3): a return that already carries a signal when the HIL
/// signal starts cannot tell its echo apart; the probe reports no
/// round-trip, never the 16-sample minimum.
#[test]
fn a_busy_loopback_return_gives_no_round_trip() {
    use iem_audio_io::{Block, Process};
    let site = crate::test_support::test_site_text();
    let mut r = rig_loopback(&site, &[], TEST_FLAG, AT_ONCE, SPARE.to_vec());
    let rxn = r.p.topo.rx.len();
    let outs = r.p.outputs();
    const N: usize = 256;
    let mut ibuf = vec![0.0f64; (rxn + SPARE.len()) * N];
    // An unrelated signal on return 0, before and during the HIL signal.
    ibuf[rxn * N..(rxn + 1) * N].fill(0.5);
    let mut obuf = vec![0.0f64; outs * N];
    r.p.process(&mut Block::new(N, &ibuf, &mut obuf));
    r.at(
        N as u64,
        &Cmd::HilTestSignal {
            input: input("mic1"),
            hz: 1000.0,
            dbfs: -30.0,
            ttl_s: 0.1,
            card_tx: SPARE.to_vec(),
        },
    );
    for _ in 0..8 {
        r.p.process(&mut Block::new(N, &ibuf, &mut obuf));
    }
    // The signal did sound on the spare outputs.
    let tx = r.p.topo.tx.len();
    assert!(
        obuf[tx * N..(tx + 1) * N]
            .iter()
            .any(|y| y.abs() >= crate::latency::ONSET)
    );
    assert_eq!(
        r.h.status
            .loopback_samples
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[test]
fn input_channels_are_the_rx_then_the_loopback_returns() {
    // The driver opens `topo.rx.len() + hil_rx` inputs. Pinned like `outputs`
    // (topo.tx + spares) so the sizing engine::run hands the driver cannot slip.
    let site = crate::test_support::test_site_text();
    let r = rig_loopback(&site, &[], TEST_FLAG, AT_ONCE, SPARE.to_vec());
    let rxn = r.p.topo.rx.len();
    assert_eq!(r.p.input_channels(), rxn + SPARE.len());
    // Without a loopback return the inputs are just the topology's rx.
    let r = rig_hil(&site, &[], TEST_FLAG, AT_ONCE, SPARE.to_vec());
    assert_eq!(r.p.input_channels(), rxn);
}

#[test]
fn the_loopback_probe_reads_each_spare_by_its_offset() {
    // probe_latency reads the emit from output `topo.tx.len() + k` and the
    // arrival from input `topo.rx.len() + j`. Put the ONLY emit onset on the
    // LAST spare output and the ONLY arrival onset on the LAST loopback return,
    // so any wrong index (+ becoming - or *) reads a silent channel and
    // measures nothing, while the correct offset measures the delay.
    use iem_audio_io::Block;
    let site = crate::test_support::test_site_text();
    let mut r = rig_loopback(&site, &[], TEST_FLAG, AT_ONCE, SPARE.to_vec());
    let tx = r.p.topo.tx.len();
    let rxn = r.p.topo.rx.len();
    let outs = r.p.outputs();
    let ins = rxn + SPARE.len();
    const N: usize = 256;
    const EMIT_AT: usize = 8;
    const ARRIVE_AT: usize = 100; // ≥ MIN_ROUND_TRIP past the emit
    let mut ibuf = vec![0.0f64; ins * N];
    let mut obuf = vec![0.0f64; outs * N];
    // Emit onset ONLY on the last spare output (index tx + 1).
    obuf[(tx + 1) * N + EMIT_AT] = 0.5;
    // Arrival onset ONLY on the last loopback return (index rxn + 1).
    ibuf[(rxn + 1) * N + ARRIVE_AT] = 0.5;
    let mut b = Block::new(N, &ibuf, &mut obuf);
    r.p.probe_latency(&mut b, 0, N);
    assert_eq!(
        r.h.status
            .loopback_samples
            .load(std::sync::atomic::Ordering::Relaxed),
        (ARRIVE_AT - EMIT_AT) as u64,
        "the probe must read the emit and arrival from the correct channel offset"
    );
}

const TEST_FLAG: Flags = Flags {
    test_signal: true,
    fault_injection: false,
};

/// Unity fade-in: the output at full level from the first sample.
const AT_ONCE: Options = Options {
    fade_in_ms: 0.0,
    hold: false,
};

/// The HIL signal (S6; the owner's decision on #9 of 2026-09-28) sounds
/// only on the engine's spare outputs after the topology's TX: on the
/// masked ones its sine itself, capped at the test-signal level, while it
/// runs (TTL and fade-out); zero on the others. Every mix renders and
/// meters as usual, but no mix's TX carries anything meanwhile: the signal
/// never reaches a band member. Afterwards the mixes are back and the spare
/// outputs zero again (A1). Their peaks reach the meter frame.
#[test]
fn hil_test_signal_reaches_only_the_masked_spare_outputs() {
    let site = crate::test_support::test_site_text();
    let mut r = rig_hil(
        &site,
        &[
            level("member1", src_in("mic1"), 0.0),
            level("member2", src_in("mic1"), 0.0),
            level("member3", src_in("mic2"), 0.0),
        ],
        TEST_FLAG,
        AT_ONCE,
        SPARE.to_vec(),
    );
    let topo = Arc::clone(&r.p.topo);
    let tx = topo.tx.len();
    assert_eq!(r.p.outputs(), tx + 2);
    let slot = |ch: u16| topo.tx.iter().position(|c| *c == ch).unwrap();
    let mixn = |m: &str| topo.mix_index(&mix(m)).unwrap();
    let hil = |dbfs: f64, card_tx: Vec<u16>| Cmd::HilTestSignal {
        input: input("mic1"),
        hz: 1000.0,
        dbfs,
        ttl_s: 0.1,
        card_tx,
    };
    // Above the test-signal cap (−20 dBFS), or naming a mix's TX: refused.
    assert_eq!(
        r.core.apply(&hil(-19.0, vec![95])).unwrap_err().code,
        ErrCode::BadValue
    );
    assert_eq!(
        r.core.apply(&hil(-30.0, vec![95, 72])).unwrap_err().code,
        ErrCode::Forbidden
    );
    r.at(0, &hil(-30.0, vec![95]));
    // mic1 (RX index 0) is silent on the card: only the sine replaces it.
    // mic2 (RX index 1) carries 0.3 into member3.
    let mut first = Planar::new(topo.rx.len(), 6_400);
    first.channel_mut(1).fill(0.3);
    let out1 = r.run(&first, 256);
    let amp = 10f64.powf(-1.5);
    // Inside the engine every mix that hears mic1 carries the sine.
    let f = r.h.meters.read().clone();
    for m in ["member1", "member2"] {
        assert!(
            close(f.mixes[mixn(m)][0], amp * g0() * g0(), 1e-4),
            "{m}: {:?}",
            f.mixes[mixn(m)]
        );
    }
    assert!(close(f.mixes[mixn("member3")][0], 0.3 * g0() * g0(), 1e-12));
    assert_eq!(f.mixes[mixn("member4")], [0.0, 0.0]);
    // The spare outputs' peaks: the masked one at the signal's level.
    assert_eq!(f.hil.len(), 2);
    assert!(close(f.hil[1], amp, 1e-4), "{:?}", f.hil);
    assert_eq!(f.hil[0], 0.0);
    // The masked output carries the sine itself: 96 samples a period at
    // 1 kHz from phase 0, faded in over 50 ms (4 800 samples).
    let y = out1.channel(tx + 1);
    let sine = |k: usize| (std::f64::consts::TAU * k as f64 / 96.0).sin();
    assert_eq!(y[0], 0.0);
    assert!(
        close(y[2_424], amp * 2_425.0 / 4_800.0, 1e-9),
        "{}",
        y[2_424]
    );
    for k in [4_824, 5_000, 6_000, 6_399] {
        assert!(close(y[k], amp * sine(k), 1e-9), "{k}: {}", y[k]);
    }
    let mut rest = Planar::new(topo.rx.len(), 23_600);
    rest.channel_mut(1).fill(0.3);
    let out2 = r.run(&rest, 256);
    // TTL 9 600 samples, then the 50 ms fade-out: 14 400 samples.
    let end = 9_600 + 4_800;
    let during = |ch: usize| -> Vec<f64> {
        out1.channel(ch)
            .iter()
            .chain(&out2.channel(ch)[..end - 6_400])
            .copied()
            .collect()
    };
    for ch in 0..tx {
        assert!(
            during(ch).iter().all(|v| *v == 0.0),
            "TX slot {ch} sounded during the HIL signal"
        );
    }
    assert!(
        during(tx).iter().all(|v| *v == 0.0),
        "the unmasked spare output"
    );
    let peak = during(tx + 1).iter().fold(0.0f64, |m, v| m.max(v.abs()));
    assert!(close(peak, amp, 1e-4), "{peak} vs {amp}");
    // Then the outputs are the mixes again and the spare outputs zero.
    let after = end - 6_400 + 1_000;
    let normal = 0.3 * g0() * g0();
    for ch in [slot(75), slot(76)] {
        assert!(
            close(out2.channel(ch)[after], normal, 1e-12),
            "{}",
            out2.channel(ch)[after]
        );
    }
    assert_eq!(out2.channel(slot(72))[after], 0.0, "mic1 is silent again");
    for ch in [tx, tx + 1] {
        assert!(
            out2.channel(ch)[end - 6_400..].iter().all(|v| *v == 0.0),
            "spare output {ch} after the end"
        );
    }
    assert_eq!(r.h.meters.read().hil, [0.0, 0.0]);
}

/// Without a HIL signal the spare outputs are zero (A1): none yet, or a
/// plain test signal, which reaches the mixes only. And they follow the
/// output's own fade like every TX: a held engine (`--hold`, before Arm)
/// keeps them silent while a HIL signal runs.
#[test]
fn the_spare_outputs_are_silent_without_a_hil_signal_and_while_held() {
    let site = crate::test_support::test_site_text();
    let open = [level("member1", src_in("mic1"), 0.0)];
    let mut r = rig_hil(&site, &open, TEST_FLAG, AT_ONCE, SPARE.to_vec());
    let (tx, rxn) = (r.p.topo.tx.len(), r.p.topo.rx.len());
    let m1 = r.p.topo.tx.iter().position(|c| *c == 71).unwrap();
    r.at(
        0,
        &Cmd::StartTestSignal {
            input: input("mic1"),
            hz: 1000.0,
            dbfs: -30.0,
            ttl_s: 0.05,
        },
    );
    let out = r.run(&Planar::new(rxn, 9_600), 97);
    assert!(
        out.channel(m1).iter().any(|v| v.abs() > 0.01),
        "member1 hears the plain signal"
    );
    for ch in [tx, tx + 1] {
        assert!(
            out.channel(ch).iter().all(|v| *v == 0.0),
            "spare output {ch}"
        );
    }
    assert_eq!(r.h.meters.read().hil, [0.0, 0.0]);
    // Held: the HIL signal runs, and every output stays silent until Arm.
    let held = Options {
        fade_in_ms: 0.0,
        hold: true,
    };
    let mut r = rig_hil(&site, &open, TEST_FLAG, held, SPARE.to_vec());
    r.at(
        0,
        &Cmd::HilTestSignal {
            input: input("mic1"),
            hz: 1000.0,
            dbfs: -30.0,
            ttl_s: 0.05,
            card_tx: SPARE.to_vec(),
        },
    );
    let out = r.run(&Planar::new(rxn, 6_400), 97);
    for ch in 0..tx + 2 {
        assert!(out.channel(ch).iter().all(|v| *v == 0.0), "held: {ch}");
    }
    // Armed while it runs: the spare outputs carry it, both at once.
    assert!(push_group(&mut r.h.cmds, 0, &[RtOp::Arm]));
    let out = r.run(&Planar::new(rxn, 1_000), 97);
    for ch in [tx, tx + 1] {
        assert!(
            out.channel(ch).iter().any(|v| v.abs() > 1e-3),
            "armed: {ch}"
        );
    }
    assert_eq!(out.channel(tx), out.channel(tx + 1));
    // An engine without spare outputs renders the topology's TX only.
    let r = rig_with(&site, &open, TEST_FLAG, AT_ONCE);
    assert_eq!(r.p.outputs(), tx);
}

/// While a HIL signal runs the listen taps carry silence (the owner's
/// decision on #9 of 2026-09-28: the signal never reaches a channel a band
/// member hears). The sine replaces an input, so every mix that hears it
/// carries it, and the taps (X3: slot 0 the engineer, slot 1 one other mix)
/// go on to the server's media stream and a web listener. The taps keep
/// their cadence, one stereo frame a sample, and once the signal ended they
/// carry the mixes again, as on an engine that never ran it.
#[test]
fn a_hil_signal_leaves_the_listen_taps_silent() {
    let site = crate::test_support::test_site_text();
    let open = [
        level("engineer", src_in("mic1"), 0.0),
        level("engineer", src_in("mic2"), 0.0),
        level("member1", src_in("mic1"), 0.0),
        level("member1", src_in("mic2"), 0.0),
    ];
    let listened = || {
        let mut r = rig_hil(&site, &open, TEST_FLAG, AT_ONCE, SPARE.to_vec());
        r.at(
            0,
            &Cmd::StartListen {
                mix: mix("engineer"),
            },
        );
        r.at(
            0,
            &Cmd::StartListen {
                mix: mix("member1"),
            },
        );
        r
    };
    let mut hil = listened();
    let mut plain = listened();
    hil.at(
        0,
        &Cmd::HilTestSignal {
            input: input("mic1"),
            hz: 1000.0,
            dbfs: -30.0,
            ttl_s: 0.05,
            card_tx: SPARE.to_vec(),
        },
    );
    // mic2 carries 0.3 into both mixes; the sine replaces mic1 for the TTL
    // (4 800 samples) and the 50 ms fade-out: 9 600 samples.
    let (tx, rxn) = (hil.p.topo.tx.len(), hil.p.topo.rx.len());
    let mut first = Planar::new(rxn, 9_600);
    first.channel_mut(1).fill(0.3);
    let out = hil.run(&first, 97);
    assert!(
        out.channel(tx).iter().any(|v| v.abs() > 1e-3),
        "the HIL signal ran on its spare output"
    );
    plain.run(&first, 97);
    for slot in 0..2 {
        let mut during = vec![1.0f32; 2 * 9_600];
        hil.h.taps[slot].pop_entire_slice(&mut during).unwrap();
        assert_eq!(
            hil.h.taps[slot].slots(),
            0,
            "tap {slot}: one frame a sample"
        );
        assert!(
            during.iter().all(|x| *x == 0.0),
            "tap {slot} carried the mix during the HIL signal"
        );
        let mut heard = vec![0.0f32; 2 * 9_600];
        plain.h.taps[slot].pop_entire_slice(&mut heard).unwrap();
        assert!(
            heard.iter().all(|x| x.abs() > 0.1),
            "tap {slot} of an engine without the HIL signal"
        );
    }
    let mut rest = Planar::new(rxn, 1_000);
    rest.channel_mut(1).fill(0.3);
    hil.run(&rest, 97);
    plain.run(&rest, 97);
    for slot in 0..2 {
        let mut after = vec![0.0f32; 2_000];
        let mut want = vec![0.0f32; 2_000];
        hil.h.taps[slot].pop_entire_slice(&mut after).unwrap();
        plain.h.taps[slot].pop_entire_slice(&mut want).unwrap();
        for (k, (a, w)) in after.iter().zip(&want).enumerate() {
            assert!(
                (a - w).abs() < 1e-6 && w.abs() > 0.1,
                "tap {slot} after the HIL signal, {k}: {a} vs {w}"
            );
        }
    }
    assert_eq!(hil.h.status.tap_overruns.load(Ordering::Relaxed), 0);
}
