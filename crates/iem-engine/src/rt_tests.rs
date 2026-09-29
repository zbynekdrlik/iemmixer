//! Unit tests of the RT processor on a small site and on `test-site.toml`.

use super::*;
use crate::cmd::push_group;
use crate::core::{Core, Flags};
use crate::site::parse;
use crate::topology::compile;
use iem_audio_io::{Offline, Planar};
use iem_dsp::pan::gains;
use iem_engine_proto::{Cmd, EqTarget, ErrCode, GroupId, InputId, MixId, Source};
use std::time::Duration;

/// RX: mono 0, st 1/2, tb 3, gs 4/5 (in the stems group).
/// TX: m1 0/1, tr 2 (mono), eng 3/4; eng hears m1.
const SITE: &str = r#"
[engine]
channels = 16
engineer = "eng"
[[engine.inputs]]
id = "mono"
rx = [1]
[[engine.inputs]]
id = "st"
rx = [2, 3]
[[engine.inputs]]
id = "tb"
rx = [4]
talkback = true
[[engine.inputs]]
id = "gs"
rx = [5, 6]
[[engine.groups]]
id = "stems"
inputs = ["gs"]
[[engine.mixes]]
id = "m1"
tx = [3, 4]
[[engine.mixes]]
id = "tr"
tx = [5]
[[engine.mixes]]
id = "eng"
tx = [1, 2]
mixes = ["m1"]
"#;

const M1_L: usize = 0;
const M1_R: usize = 1;
const TR: usize = 2;
const ENG_L: usize = 3;
/// RX channels of `SITE`.
const RX: usize = 6;

fn mix(s: &str) -> MixId {
    MixId::new(s)
}

fn input(s: &str) -> InputId {
    InputId::new(s)
}

fn src_in(s: &str) -> Source {
    Source::Input(input(s))
}

fn level(m: &str, source: Source, gain_db: f64) -> Cmd {
    Cmd::SetLevel {
        mix: mix(m),
        source,
        gain_db: Some(gain_db),
        pan: None,
        muted: None,
    }
}

fn set_mix(m: &str, volume_db: Option<f64>, muted: Option<bool>) -> Cmd {
    Cmd::SetMix {
        mix: mix(m),
        volume_db,
        muted,
    }
}

fn set_group(m: &str, gain_db: Option<f64>, muted: Option<bool>) -> Cmd {
    Cmd::SetGroup {
        mix: mix(m),
        group: GroupId::new("stems"),
        gain_db,
        muted,
    }
}

fn set_input(i: &str, f: impl FnOnce(&mut Cmd)) -> Cmd {
    let mut c = Cmd::SetInput {
        input: input(i),
        trim_db: None,
        muted: None,
        processing: None,
    };
    f(&mut c);
    c
}

fn limiter(m: &str, enabled: bool, limit_db: f64) -> Cmd {
    Cmd::SetLimiter {
        mix: mix(m),
        enabled: Some(enabled),
        limit_db: Some(limit_db),
    }
}

struct Rig {
    core: Core,
    p: Processor,
    h: RtHandles,
}

fn rig_with(site: &str, cmds: &[Cmd], flags: Flags, opts: Options) -> Rig {
    rig_hil(site, cmds, flags, opts, Vec::new())
}

/// `rig_with` on an engine that opened the spare card outputs `hil` for
/// HIL (S6): the core knows them, and the processor's outputs after the
/// topology's TX are theirs.
fn rig_hil(site: &str, cmds: &[Cmd], flags: Flags, opts: Options, hil: Vec<u16>) -> Rig {
    let topo = Arc::new(compile(&parse(site).unwrap()).unwrap());
    let spare = hil.len();
    let mut core = Core::new(Arc::clone(&topo), &MixState::default(), 0, flags).with_hil(hil);
    for c in cmds {
        core.apply(c).unwrap();
    }
    let (p, h) = Processor::with_hil(topo, &core.state(), &[], opts, spare, 0);
    Rig { core, p, h }
}

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

fn rig(cmds: &[Cmd]) -> Rig {
    rig_with(
        SITE,
        cmds,
        Flags::default(),
        Options {
            fade_in_ms: 0.0,
            hold: false,
        },
    )
}

impl Rig {
    /// Applies `cmd` in the core and queues its RT group at sample `at`.
    fn at(&mut self, at: u64, cmd: &Cmd) {
        let out = self.core.apply(cmd).unwrap();
        assert!(push_group(&mut self.h.cmds, at, &out.rt) || out.rt.is_empty());
    }

    fn run(&mut self, input: &Planar, block: usize) -> Planar {
        let outs = self.p.outputs();
        let run = Offline { block }.run(&mut self.p, input, outs);
        assert!(run.fault.is_none());
        run.output
    }
}

/// `frames` of `values` on the first RX channels, zeros on the rest.
fn rx(values: &[f64], frames: usize) -> Planar {
    let mut all = [0.0; RX];
    for (d, v) in all.iter_mut().zip(values) {
        *d = *v;
    }
    dc(&all, frames)
}

/// `frames` of constant values per RX channel.
fn dc(values: &[f64], frames: usize) -> Planar {
    let mut p = Planar::new(values.len(), frames);
    for (ch, v) in values.iter().enumerate() {
        p.channel_mut(ch).fill(*v);
    }
    p
}

fn g0() -> f64 {
    gains(0.0).0
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn a_mono_input_feeds_both_channels_at_unity() {
    let mut r = rig(&[level("m1", src_in("mono"), 0.0)]);
    let out = r.run(&rx(&[0.25], 512), 64);
    let want = 0.25 * g0() * g0();
    for ch in [M1_L, M1_R] {
        assert!(
            out.channel(ch).iter().all(|y| close(*y, want, 1e-15)),
            "{ch}"
        );
    }
    // Nothing reaches a mix at a level that is off, and every TX is written.
    assert!(out.channel(ENG_L).iter().all(|y| *y == 0.0));
    assert!(out.channel(TR).iter().all(|y| *y == 0.0));
}

#[test]
fn stereo_keeps_its_channels() {
    let mut r = rig(&[level("m1", src_in("st"), 0.0)]);
    let out = r.run(&rx(&[0.0, 0.1, 0.2], 256), 256);
    assert!(close(out.channel(M1_L)[255], 0.1 * g0() * g0(), 1e-15));
    assert!(close(out.channel(M1_R)[255], 0.2 * g0() * g0(), 1e-15));
}

#[test]
fn trim_and_eq_apply_only_with_processing() {
    let trim = set_input("mono", |c| {
        if let Cmd::SetInput { trim_db, .. } = c {
            *trim_db = Some(-6.0);
        }
    });
    let mut r = rig(&[level("m1", src_in("mono"), 0.0), trim]);
    r.at(
        1000,
        &set_input("mono", |c| {
            if let Cmd::SetInput { processing, .. } = c {
                *processing = Some(false);
            }
        }),
    );
    let out = r.run(&rx(&[0.25], 4000), 97);
    let y = out.channel(M1_L);
    let wet = 0.25 * 10f64.powf(-0.3) * g0() * g0();
    let dry = 0.25 * g0() * g0();
    assert!(close(y[999], wet, 1e-15), "{}", y[999]);
    // 20 ms crossfade: halfway at 960 samples, dry from 1920 on.
    assert!(
        close(y[1000 + 959], 0.5 * (wet + dry), 1e-12),
        "{}",
        y[1959]
    );
    assert!(close(y[1000 + 1919], dry, 1e-15));
    assert!(close(y[3999], dry, 1e-15));
    // A centred mono input: the right channel crossfades exactly like the left.
    assert_eq!(out.channel(M1_R), out.channel(M1_L));
    // With processing on, an enabled band shapes the signal: a +12 dB low
    // shelf lifts DC about four times.
    let mut eq = iem_engine_proto::Eq::default();
    eq.bands[1].enabled = true;
    eq.bands[1].gain_db = 12.0;
    let mut r = rig(&[
        level("m1", src_in("mono"), 0.0),
        Cmd::SetEq {
            target: EqTarget::Input(input("mono")),
            eq,
        },
    ]);
    let boosted = r.run(&rx(&[0.01], 4000), 64);
    assert!(
        boosted.channel(M1_L)[3999] > 0.02,
        "{}",
        boosted.channel(M1_L)[3999]
    );
}

#[test]
fn talkback_is_added_before_the_mute_gate() {
    let mut r = rig(&[level("m1", src_in("tb"), 0.0)]);
    let talk = vec![0.5f32; TALK_RING];
    r.h.talkback.push_entire_slice(&talk).unwrap();
    let out = r.run(&rx(&[], 2048), 32);
    let want = 0.5 * TALKBACK_GAIN * g0() * g0();
    assert!(out.channel(M1_L)[0] < want);
    assert!(
        close(out.channel(M1_L)[2047], want, 1e-9),
        "{}",
        out.channel(M1_L)[2047]
    );
    assert_eq!(
        out.channel(M1_R),
        out.channel(M1_L),
        "talkback on both channels"
    );
    // Muting the talkback input silences the talkback too (A3).
    r.at(
        0,
        &set_input("tb", |c| {
            if let Cmd::SetInput { muted, .. } = c {
                *muted = Some(true);
            }
        }),
    );
    let out = r.run(&rx(&[], 1024), 32);
    assert_eq!(out.channel(M1_L)[1023], 0.0);
    // A partial block of talkback is an underrun.
    let mut short = rig(&[level("m1", src_in("tb"), 0.0)]);
    short.h.talkback.push_entire_slice(&[0.5f32; 20]).unwrap();
    short.run(&rx(&[], 64), 32);
    assert_eq!(short.h.status.talkback_underruns.load(Ordering::Relaxed), 1);
    // Without talkback samples the gate closes and nothing is added.
    let mut quiet = rig(&[level("m1", src_in("tb"), 0.0)]);
    let out = quiet.run(&rx(&[], 256), 32);
    assert!(out.channel(M1_L).iter().all(|y| *y == 0.0));
    assert_eq!(quiet.h.status.talkback_underruns.load(Ordering::Relaxed), 0);
}

#[test]
fn a_group_strip_is_its_inputs_then_eq_fader_and_mute() {
    // gs reaches m1 only through the stems strip: its level, then the strip.
    let mut r = rig(&[
        level("m1", src_in("gs"), 0.0),
        set_group("m1", Some(-6.0), None),
    ]);
    let input = rx(&[0.0, 0.0, 0.0, 0.0, 0.2, 0.3], 256);
    let out = r.run(&input, 64);
    let strip = 10f64.powf(-0.3) * g0();
    let want_l = 0.2 * g0() * strip * g0();
    let want_r = 0.3 * g0() * strip * g0();
    assert!(
        close(out.channel(M1_L)[255], want_l, 1e-15),
        "{} vs {want_l}",
        out.channel(M1_L)[255]
    );
    assert!(close(out.channel(M1_R)[255], want_r, 1e-15));
    // The strip is per mix: eng hears nothing of gs at its own (off) level.
    assert_eq!(out.channel(ENG_L)[255], 0.0);
    // Muting the strip silences the group, and only the group.
    let mut r = rig(&[
        level("m1", src_in("gs"), 0.0),
        level("m1", src_in("mono"), 0.0),
        set_group("m1", None, Some(true)),
    ]);
    let out = r.run(&rx(&[0.25, 0.0, 0.0, 0.0, 0.2, 0.3], 1024), 64);
    assert!(close(out.channel(M1_L)[1023], 0.25 * g0() * g0(), 1e-15));
    assert!(close(out.channel(M1_R)[1023], 0.25 * g0() * g0(), 1e-15));
    // A +12 dB low shelf on the strip lifts the group about four times.
    let mut eq = iem_engine_proto::Eq::default();
    eq.bands[1].enabled = true;
    eq.bands[1].gain_db = 12.0;
    let mut r = rig(&[
        level("m1", src_in("gs"), 0.0),
        Cmd::SetEq {
            target: EqTarget::Group {
                mix: mix("m1"),
                group: GroupId::new("stems"),
            },
            eq,
        },
    ]);
    let out = r.run(&rx(&[0.0, 0.0, 0.0, 0.0, 0.01, 0.01], 4000), 64);
    let y = out.channel(M1_L)[3999];
    assert!(y > 0.03 && y < 0.05, "{y}");
}

#[test]
fn output_chain_is_eq_limiter_volume_mute_safety_clamp() {
    let mut r = rig(&[
        level("m1", src_in("mono"), 0.0),
        limiter("m1", true, -6.0),
        set_mix("m1", Some(12.0), None),
        level("eng", Source::Mix(mix("m1")), 0.0),
        limiter("eng", false, -6.0),
        set_mix("eng", Some(-12.0), None),
    ]);
    let out = r.run(&rx(&[1.0], 4096), 128);
    let m1 = out.channel(M1_L);
    assert!(
        m1.iter().all(|y| y.abs() <= 1.0),
        "the TX never exceeds 1.0"
    );
    assert!(m1[4095] > 0.99, "{}", m1[4095]);
    // A9: the engineer hears m1 after its mute, before its safety stage:
    // 1.0 → limiter 0.501 → +12 dB ≈ 2.0, unclipped, → −12 dB ≈ 0.5.
    let limited = 10f64.powf(-6.0 / 20.0);
    let post = limited * (10f64.powf(0.6) * g0());
    let eng = post * g0() * (10f64.powf(-0.6) * g0());
    assert!(post > 1.9);
    assert!(
        close(out.channel(ENG_L)[4095], eng, 1e-9),
        "{}",
        out.channel(ENG_L)[4095]
    );
    // Muting m1 mutes what the engineer hears of it too.
    r.at(0, &set_mix("m1", None, Some(true)));
    let out = r.run(&rx(&[1.0], 1024), 128);
    assert_eq!(out.channel(M1_L)[1023], 0.0);
    assert_eq!(out.channel(ENG_L)[1023], 0.0);
}

#[test]
fn a_mono_mix_is_the_half_sum_on_one_channel() {
    let mut r = rig(&[level("tr", src_in("mono"), 0.0)]);
    let out = r.run(&rx(&[0.4], 256), 64);
    assert!(close(out.channel(TR)[255], 0.4 * g0() * g0(), 1e-15));
    let mut left = rig(&[Cmd::SetLevel {
        mix: mix("tr"),
        source: src_in("mono"),
        gain_db: Some(0.0),
        pan: Some(-1.0),
        muted: None,
    }]);
    let out = left.run(&rx(&[0.4], 256), 64);
    let (gl, gr) = gains(-1.0);
    assert_eq!(gr, 0.0);
    assert!(
        close(out.channel(TR)[255], (0.4 * gl * g0()) * 0.5, 1e-15),
        "{}",
        out.channel(TR)[255]
    );
    // One TX channel only: the stereo mixes keep theirs.
    assert_eq!(out.channels(), 5);
}

#[test]
fn sanitizer_silences_and_resets_a_tripping_node() {
    let mut r = rig(&[level("m1", src_in("mono"), 0.0)]);
    let mut input = rx(&[0.25], 256);
    input.channel_mut(0)[70] = f64::NAN;
    let out = r.run(&input, 64);
    let y = out.channel(M1_L);
    assert!(y[..64].iter().all(|v| *v > 0.2));
    assert!(
        y[64..128].iter().all(|v| *v == 0.0),
        "the tripping block is silent"
    );
    assert!(y[128..].iter().all(|v| *v > 0.2), "the next block passes");
    assert_eq!(r.h.status.trips.load(Ordering::Relaxed), 1);
    let mut huge = rx(&[2e6], 64);
    huge.channel_mut(0)[0] = 2e6;
    r.run(&huge, 64);
    assert_eq!(r.h.status.trips.load(Ordering::Relaxed), 2);
}

#[test]
fn commands_apply_at_their_sample() {
    for block in [256, 97, 1000] {
        let mut r = rig(&[level("m1", src_in("mono"), 0.0)]);
        r.at(1000, &set_mix("m1", Some(-6.0), None));
        let out = r.run(&rx(&[0.25], 3000), block);
        let y = out.channel(M1_L);
        assert_eq!(y[999], y[998], "{block}");
        assert!(y[1000] < y[999], "{block}: the ramp starts at sample 1000");
        assert!(close(
            y[1000 + 959],
            0.25 * g0() * 10f64.powf(-0.3) * g0(),
            1e-15
        ));
        // Due mid-block with budget left: the block is cut, nothing waits.
        assert_eq!(r.h.status.deferred.load(Ordering::Relaxed), 0, "{block}");
    }
}

#[test]
fn at_most_512_commands_per_block_and_groups_stay_whole() {
    let mut r = rig(&[]);
    for _ in 0..600 {
        assert!(push_group(&mut r.h.cmds, 0, &[RtOp::Nop]));
    }
    r.run(&rx(&[], 32), 32);
    assert_eq!(r.h.cmds.slots(), CMD_RING - 88);
    r.run(&rx(&[], 32), 32);
    assert_eq!(r.h.cmds.slots(), CMD_RING);
    for _ in 0..200 {
        push_group(&mut r.h.cmds, 0, &[RtOp::Nop]);
    }
    assert!(push_group(&mut r.h.cmds, 0, &[RtOp::Nop; 400]));
    r.run(&rx(&[], 32), 32);
    assert_eq!(r.h.cmds.slots(), CMD_RING - 400, "the group waits whole");
    assert_eq!(r.h.status.deferred.load(Ordering::Relaxed), 2);
    r.run(&rx(&[], 32), 32);
    assert_eq!(r.h.cmds.slots(), CMD_RING);
}

#[test]
fn limiter_raise_ramps_lower_is_instant() {
    let mut r = rig(&[level("m1", src_in("mono"), 0.0), limiter("m1", true, -6.0)]);
    r.at(2000, &limiter("m1", true, 0.0));
    r.at(5000, &limiter("m1", true, -6.0));
    let out = r.run(&rx(&[0.9], 6000), 64);
    let y = out.channel(M1_L);
    let low = 0.9f64.min(10f64.powf(-0.3)) * g0() * g0();
    let wide = 0.9 * g0() * g0();
    assert!(close(y[1999], low, 1e-9), "{}", y[1999]);
    // Raising takes 10 ms (960 samples).
    assert!(
        y[2000 + 480] > low + 0.01 && y[2000 + 480] < wide - 0.01,
        "{}",
        y[2480]
    );
    assert!(close(y[4999], wide, 1e-9), "{}", y[4999]);
    // Lowering is instant.
    assert!(close(y[5000], low, 1e-9), "{}", y[5000]);
}

#[test]
fn a_test_signal_caps_every_tx_and_ends_after_its_ttl() {
    let flags = Flags {
        test_signal: true,
        fault_injection: false,
    };
    let mut r = rig_with(
        SITE,
        &[
            level("m1", src_in("st"), 0.0),
            level("m1", src_in("mono"), 0.0),
            level("tr", src_in("mono"), 0.0),
        ],
        flags,
        Options {
            fade_in_ms: 0.0,
            hold: false,
        },
    );
    r.at(
        0,
        &Cmd::StartTestSignal {
            input: input("st"),
            hz: 1000.0,
            dbfs: -3.0,
            ttl_s: 0.01,
        },
    );
    let out = r.run(&rx(&[0.5, 0.5, 0.5], 12_000), 256);
    let m1 = out.channel(M1_L);
    let tr = out.channel(TR);
    let active = 960 + 4800;
    assert!(
        m1[..active].iter().all(|y| y.abs() <= TEST_CAP),
        "TX capped"
    );
    assert!(m1[100..active].contains(&TEST_CAP));
    // Every mix hears every input: the other TX are capped too.
    assert!(tr[..active].iter().all(|y| y.abs() <= TEST_CAP));
    assert_eq!(tr[1000], TEST_CAP);
    assert!(
        m1[active + 1000] > 0.4,
        "caps lifted: {}",
        m1[active + 1000]
    );
    assert!(close(tr[active + 1000], 0.5 * g0() * g0(), 1e-15));
    // The sine replaced the input: its peak is at most −20 dBFS through a 0 dB level.
    let mut probe = rig_with(
        SITE,
        &[level("eng", src_in("st"), 0.0)],
        flags,
        Options {
            fade_in_ms: 0.0,
            hold: false,
        },
    );
    probe.at(
        0,
        &Cmd::StartTestSignal {
            input: input("st"),
            hz: 1000.0,
            dbfs: -30.0,
            ttl_s: 1.0,
        },
    );
    let out = probe.run(&rx(&[0.0, 0.3, 0.3], 9600), 256);
    let peak = out.channel(ENG_L)[5000..]
        .iter()
        .fold(0.0f64, |m, y| m.max(y.abs()));
    let want = 10f64.powf(-1.5) * g0() * g0();
    assert!(close(peak, want, 1e-4), "{peak} vs {want}");
    probe.at(0, &Cmd::StopTestSignal);
    let out = probe.run(&rx(&[0.0, 0.3, 0.3], 9600), 256);
    assert!(close(out.channel(ENG_L)[9599], 0.3 * g0() * g0(), 1e-12));
}

#[test]
fn listen_taps_are_side_effect_free() {
    let cmds = [
        level("eng", src_in("mono"), 0.0),
        level("m1", src_in("mono"), 0.0),
        set_mix("eng", Some(-12.0), None),
        set_mix("m1", Some(-6.0), None),
    ];
    let input = rx(&[0.3], 1024);
    let mut plain = rig(&cmds);
    let a = plain.run(&input, 64);
    let mut tapped = rig(&cmds);
    tapped.at(0, &Cmd::StartListen { mix: mix("eng") });
    tapped.at(0, &Cmd::StartListen { mix: mix("m1") });
    let b = tapped.run(&input, 64);
    assert_eq!(a, b);
    let mut eng = vec![0.0f32; 2048];
    let mut member = vec![0.0f32; 2048];
    tapped.h.taps[0].pop_entire_slice(&mut eng).unwrap();
    tapped.h.taps[1].pop_entire_slice(&mut member).unwrap();
    // Slot 0: the engineer after its limiter, before its volume.
    let pre = (0.3 * g0()) as f32;
    assert!(eng.iter().all(|x| (x - pre).abs() < 1e-6), "{}", eng[0]);
    // Slot 1: the other mix after volume and mute, through the 0 dB listen limiter.
    let post = (0.3 * g0() * 10f64.powf(-0.3) * g0()) as f32;
    assert!(
        member.iter().all(|x| (x - post).abs() < 1e-6),
        "{}",
        member[0]
    );
    assert_eq!(tapped.h.status.tap_overruns.load(Ordering::Relaxed), 0);
    // Nobody draining the engineer tap: the ring fills and overruns count.
    tapped.run(&rx(&[0.3], TAP_RING), 64);
    assert!(tapped.h.status.tap_overruns.load(Ordering::Relaxed) > 0);
    let mut sink = vec![0.0f32; TAP_RING];
    let _ = tapped.h.taps[0].pop_partial_slice(&mut sink);
    let _ = tapped.h.taps[1].pop_partial_slice(&mut sink);
    tapped.at(0, &Cmd::StopListen { mix: mix("m1") });
    tapped.run(&input, 64);
    assert_eq!(tapped.h.taps[1].slots(), 0, "a stopped tap writes nothing");
    assert_eq!(tapped.h.taps[0].slots(), 2048);
}

#[test]
fn fade_in_and_fade_out() {
    let mut r = rig_with(
        SITE,
        &[level("m1", src_in("mono"), 0.0)],
        Flags::default(),
        Options::default(),
    );
    let out = r.run(&rx(&[0.25], 50_000), 32);
    let y = out.channel(M1_L);
    let full = 0.25 * g0() * g0();
    assert!(y[0] < full * 1e-3);
    assert!(close(y[24_000], full * 0.5, 1e-3), "{}", y[24_000]);
    assert!(close(y[49_999], full, 1e-15));
    assert!(!r.h.status.faded_out.load(Ordering::Acquire));
    assert!(push_group(&mut r.h.cmds, 0, &[RtOp::FadeOut]));
    let out = r.run(&rx(&[0.25], 6_000), 32);
    let y = out.channel(M1_L);
    assert!(y[2400] < full && y[2400] > 0.0);
    assert_eq!(y[4800], 0.0);
    assert!(r.h.status.faded_out.load(Ordering::Acquire));
}

#[test]
fn meters_publish_every_3200_samples() {
    let mut r = rig(&[
        level("m1", src_in("mono"), 0.0),
        level("eng", src_in("gs"), 0.0),
        limiter("m1", true, -6.0),
        Cmd::ResetLimiterStats { mix: mix("m1") },
    ]);
    let input = rx(&[0.9, 0.0, 0.0, 0.0, 0.1, 0.05], 3168);
    r.run(&input, 32);
    assert!(!r.h.meters.updated());
    r.run(&rx(&[0.9, 0.0, 0.0, 0.0, 0.1, 0.05], 32), 32);
    assert!(r.h.meters.updated());
    let m = r.p.topo.mix_index(&mix("m1")).unwrap();
    let tr = r.p.topo.mix_index(&mix("tr")).unwrap();
    let eng = r.p.topo.mix_index(&mix("eng")).unwrap();
    let f = r.h.meters.read().clone();
    assert_eq!(f.seq, 1);
    assert_eq!(f.inputs[0], [0.9, 0.9]);
    assert_eq!(f.inputs[1], [0.0, 0.0]);
    assert_eq!(f.inputs[3], [0.1, 0.05]);
    assert!(
        close(f.mixes[m][0], 10f64.powf(-0.3) * g0() * g0(), 1e-9),
        "{:?}",
        f.mixes[m]
    );
    assert!(f.gr_db[m] < -0.5, "{}", f.gr_db[m]);
    // One group strip per mix, mix-major: only the engineer's carries gs.
    assert_eq!(f.groups.len(), 3);
    assert!(
        close(f.groups[eng][0], 0.1 * g0() * g0(), 1e-15),
        "{:?}",
        f.groups
    );
    assert!(close(f.groups[eng][1], 0.05 * g0() * g0(), 1e-15));
    assert_eq!(f.groups[m], [0.0, 0.0]);
    assert_eq!(f.groups[tr], [0.0, 0.0]);
    // 0.9 limited to −6 dB is ≈ −5 dB of GR: every sample since the reset is active.
    assert_eq!(f.active[m], 3200);
    // A silent mix shows no gain reduction.
    assert_eq!(f.gr_db[tr], 0.0);
    r.run(&rx(&[], 3200), 3200);
    let f = r.h.meters.read().clone();
    assert_eq!((f.seq, f.inputs[0]), (2, [0.0, 0.0]));
    // X14 counts from the persisted base.
    let topo = Arc::new(compile(&parse(SITE).unwrap()).unwrap());
    let mut core = Core::new(Arc::clone(&topo), &MixState::default(), 0, Flags::default());
    core.apply(&level("m1", src_in("mono"), 0.0)).unwrap();
    let mut counters = vec![0; topo.mixes.len()];
    counters[m] = 1000;
    let (mut p, mut h) = Processor::new(
        topo,
        &core.state(),
        &counters,
        Options {
            fade_in_ms: 0.0,
            hold: false,
        },
    );
    let run = Offline { block: 3200 }.run(&mut p, &rx(&[2.0], 3200), 5);
    assert!(run.fault.is_none());
    let f = h.meters.read().clone();
    assert_eq!(f.active[m], 1000 + 3200);
    assert!(push_group(
        &mut h.cmds,
        0,
        &[RtOp::ResetLimiter { m: m as u16 }]
    ));
    Offline { block: 3200 }.run(&mut p, &rx(&[], 3200), 5);
    let f = h.meters.read().clone();
    // The reset cleared the base and the count; the GR meter recovers at
    // 8.7 dB/s, so the next 33 ms still count.
    assert_eq!(f.active[m], 3200);
}

#[test]
fn the_fault_injection_op_panics() {
    let mut r = rig(&[]);
    assert!(push_group(&mut r.h.cmds, 64, &[RtOp::Panic]));
    let outs = r.p.topo.tx.len();
    let run = Offline { block: 32 }.run(&mut r.p, &rx(&[], 256), outs);
    let fault = run.fault.expect("fault");
    assert_eq!(fault.frame, 64);
    assert!(fault.message.contains("fault injection"));
}

#[test]
fn the_program_site_runs_and_reports_its_time() {
    let topo = Arc::new(crate::test_support::test_site());
    let (mut p, _h) = Processor::new(
        Arc::clone(&topo),
        &MixState::default(),
        &[],
        Options::default(),
    );
    let run = Offline { block: 32 }.run(&mut p, &dc(&[0.1; 32], 320), topo.tx.len());
    assert!(run.fault.is_none());
    assert_eq!(run.output.channels(), 21);
    assert_eq!(p.time(), 320);
}

/// Runs `p` on a thread; fails after 5 s instead of hanging when a block
/// never ends.
fn run_bounded(mut p: Processor, input: Planar, block: usize) -> (Processor, Planar) {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::spawn(move || {
        let outs = p.topo.tx.len();
        let run = Offline { block }.run(&mut p, &input, outs);
        let _ = tx.send((p, run));
    });
    let (p, run) = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("process returns");
    assert!(run.fault.is_none());
    (p, run.output)
}

#[test]
fn process_returns_after_its_block_even_when_a_group_waits() {
    let Rig { p, mut h, .. } = rig(&[]);
    let (p, out) = run_bounded(p, rx(&[], 64), 32);
    assert_eq!((p.time(), out.frames()), (64, 64));
    // Due at the current sample: 200 single commands, then a group of 400
    // that no longer fits the block's budget and waits whole.
    for _ in 0..200 {
        assert!(push_group(&mut h.cmds, 64, &[RtOp::Nop]));
    }
    assert!(push_group(&mut h.cmds, 64, &[RtOp::Nop; 400]));
    let (p, _) = run_bounded(p, rx(&[], 32), 32);
    assert_eq!(p.time(), 96);
    assert_eq!(h.cmds.slots(), CMD_RING - 400);
    assert_eq!(h.status.deferred.load(Ordering::Relaxed), 1);
    let (p, _) = run_bounded(p, rx(&[], 32), 32);
    assert_eq!((p.time(), h.cmds.slots()), (128, CMD_RING));
}

#[test]
fn a_spent_budget_does_not_cut_the_block() {
    let mut r = rig(&[]);
    for _ in 0..MAX_CMDS_PER_BLOCK {
        assert!(push_group(&mut r.h.cmds, 0, &[RtOp::Nop]));
    }
    // Due mid-block after the budget is spent: nothing more applies in this
    // block, so it runs whole and the command waits for the next one, which
    // makes this a block that left a command for the next block.
    assert!(push_group(&mut r.h.cmds, 16, &[RtOp::Nop]));
    r.run(&rx(&[], 32), 32);
    assert_eq!(r.h.cmds.slots(), CMD_RING - 1);
    assert_eq!(r.h.status.deferred.load(Ordering::Relaxed), 1);
    r.run(&rx(&[], 32), 32);
    assert_eq!(r.h.cmds.slots(), CMD_RING);
    assert_eq!(r.h.status.deferred.load(Ordering::Relaxed), 1);
}

#[test]
fn a_spent_budget_counts_only_what_was_due_in_the_block() {
    let mut r = rig(&[]);
    for _ in 0..MAX_CMDS_PER_BLOCK {
        assert!(push_group(&mut r.h.cmds, 0, &[RtOp::Nop]));
    }
    // Due at the next block's first sample: it was never due in this one.
    assert!(push_group(&mut r.h.cmds, 32, &[RtOp::Nop]));
    r.run(&rx(&[], 32), 32);
    assert_eq!(r.h.cmds.slots(), CMD_RING - 1);
    assert_eq!(r.h.status.deferred.load(Ordering::Relaxed), 0);
    r.run(&rx(&[], 32), 32);
    assert_eq!(r.h.cmds.slots(), CMD_RING);
    assert_eq!(r.h.status.deferred.load(Ordering::Relaxed), 0);
}

#[test]
fn a_waiting_group_counts_its_block_once() {
    let mut r = rig(&[]);
    for _ in 0..200 {
        assert!(push_group(&mut r.h.cmds, 0, &[RtOp::Nop]));
    }
    assert!(push_group(&mut r.h.cmds, 0, &[RtOp::Nop; 400]));
    // One block of four 256-sample segments: the group waits in each of
    // them, and the block counts once.
    r.run(&rx(&[], 4 * SEG), 4 * SEG);
    assert_eq!(r.h.cmds.slots(), CMD_RING - 400);
    assert_eq!(r.h.status.deferred.load(Ordering::Relaxed), 1);
    r.run(&rx(&[], 32), 32);
    assert_eq!(r.h.cmds.slots(), CMD_RING);
    assert_eq!(r.h.status.deferred.load(Ordering::Relaxed), 1);
}

#[test]
fn meter_frames_keep_the_3200_sample_grid_across_odd_blocks() {
    let mut r = rig(&[]);
    for k in 1..=20u64 {
        r.run(&rx(&[], 1000), 1000);
        assert_eq!(r.h.meters.read().seq, k * 1000 / METER_PERIOD, "block {k}");
    }
}

#[test]
fn a_listen_tap_ring_holds_200_ms() {
    let mut r = rig(&[level("eng", src_in("mono"), 0.0)]);
    r.at(0, &Cmd::StartListen { mix: mix("eng") });
    r.run(&rx(&[0.3], 19_200), 32);
    assert_eq!(r.h.taps[0].slots(), 2 * 19_200);
    assert_eq!(r.h.status.tap_overruns.load(Ordering::Relaxed), 0);
    r.run(&rx(&[0.3], 32), 32);
    assert_eq!(r.h.status.tap_overruns.load(Ordering::Relaxed), 1);
}

#[test]
fn switching_the_member_listen_restarts_its_limiter() {
    let mut r = rig(&[
        level("m1", src_in("mono"), 12.0),
        set_mix("m1", Some(12.0), None),
        level("tr", src_in("mono"), 0.0),
    ]);
    let m1 = r.p.topo.mix_index(&mix("m1")).unwrap() as u16;
    let tr = r.p.topo.mix_index(&mix("tr")).unwrap() as u16;
    let listen = |m: u16| {
        [RtOp::Listen {
            slot: 1,
            mix: Some(m),
        }]
    };
    assert!(push_group(&mut r.h.cmds, 0, &listen(m1)));
    // m1 runs hot (≈ +6 dBFS): the 0 dB listen limiter holds it.
    r.run(&rx(&[0.25], 3200), 32);
    let mut hot = vec![0.0f32; 6400];
    r.h.taps[1].pop_entire_slice(&mut hot).unwrap();
    assert!(
        hot[6000..].iter().all(|x| *x > 0.9 && *x < 1.001),
        "{}",
        hot[6399]
    );
    // The quieter translator mix passes untouched at once: no leftover gain
    // reduction from m1.
    assert!(push_group(&mut r.h.cmds, 3200, &listen(tr)));
    r.run(&rx(&[0.25], 64), 32);
    let mut quiet = vec![0.0f32; 128];
    r.h.taps[1].pop_entire_slice(&mut quiet).unwrap();
    let want = (0.25 * g0() * g0()) as f32;
    assert!(
        quiet.iter().all(|x| (x - want).abs() < 1e-6),
        "{:?}",
        &quiet[..4]
    );
}

#[test]
fn a_trim_change_ramps_while_processing() {
    let trim = set_input("mono", |c| {
        if let Cmd::SetInput { trim_db, .. } = c {
            *trim_db = Some(-6.0);
        }
    });
    let mut r = rig(&[level("m1", src_in("mono"), 0.0)]);
    r.at(1000, &trim);
    let out = r.run(&rx(&[0.25], 3000), 64);
    let y = out.channel(M1_L);
    let (t, g2) = (10f64.powf(-0.3), g0() * g0());
    assert!(close(y[999], 0.25 * g2, 1e-15));
    // 10 ms (960 samples) from 1 to t: the first step and the landing.
    let first = 0.25 * (1.0 + (t - 1.0) / 960.0) * g2;
    assert!(close(y[1000], first, 1e-12), "{} vs {first}", y[1000]);
    assert!(y[1958] > 0.25 * t * g2);
    assert!(close(y[1959], 0.25 * t * g2, 1e-15), "{}", y[1959]);
}

#[test]
fn a_mix_eq_shapes_the_mix() {
    let mut eq = iem_engine_proto::Eq::default();
    eq.bands[1].enabled = true;
    eq.bands[1].gain_db = 12.0;
    let mut r = rig(&[
        level("m1", src_in("mono"), 0.0),
        Cmd::SetEq {
            target: EqTarget::Mix(mix("m1")),
            eq,
        },
    ]);
    let out = r.run(&rx(&[0.01], 4000), 64);
    // A +12 dB low shelf lifts DC about four times.
    let y = out.channel(M1_L)[3999];
    assert!(y > 0.03 && y < 0.05, "{y}");
}

#[test]
fn a_tripping_group_strip_counts_in_the_status_and_the_meters() {
    let mut r = rig(&[level("m1", src_in("gs"), 12.0)]);
    // 9e5 passes the inputs; ×3.98 in the strip trips it once per block.
    r.run(&rx(&[0.0, 0.0, 0.0, 0.0, 9e5, 9e5], 3200), 32);
    assert_eq!(r.h.status.trips.load(Ordering::Relaxed), 100);
    let f = r.h.meters.read().clone();
    assert_eq!((f.seq, f.trips), (1, 100));
    let m = r.p.topo.mix_index(&mix("m1")).unwrap();
    assert_eq!(f.groups[m], [0.0, 0.0], "a tripping strip is silenced");
    assert_eq!(f.mixes[m], [0.0, 0.0], "and adds nothing to its mix");
}

#[test]
fn faded_out_is_reported_when_the_fade_ends() {
    let mut r = rig(&[level("m1", src_in("mono"), 0.0)]);
    assert!(push_group(&mut r.h.cmds, 0, &[RtOp::FadeOut]));
    let full = 0.25 * g0() * g0();
    let out = r.run(&rx(&[0.25], 2400), 32);
    assert!(
        !r.h.status.faded_out.load(Ordering::Acquire),
        "halfway through the 50 ms fade"
    );
    assert!(close(out.channel(M1_L)[2399], 0.5 * full, 1e-12));
    assert_eq!(out.channel(M1_R), out.channel(M1_L));
    let out = r.run(&rx(&[0.25], 2400), 32);
    assert!(r.h.status.faded_out.load(Ordering::Acquire));
    assert_eq!(out.channel(M1_L)[2399], 0.0);
    assert_eq!(out.channel(M1_R), out.channel(M1_L));
}

#[test]
fn the_test_sine_starts_upwards_and_fades_out_after_its_ttl() {
    let flags = Flags {
        test_signal: true,
        fault_injection: false,
    };
    let mut r = rig_with(
        SITE,
        &[level("eng", src_in("st"), 0.0)],
        flags,
        Options {
            fade_in_ms: 0.0,
            hold: false,
        },
    );
    r.at(
        0,
        &Cmd::StartTestSignal {
            input: input("st"),
            hz: 1000.0,
            dbfs: -30.0,
            ttl_s: 0.01,
        },
    );
    let out = r.run(&rx(&[], 7000), 256);
    let y = out.channel(ENG_L);
    // 1 kHz at 96 kHz from phase 0: positive over the first half of every
    // 96-sample cycle, negative over the second.
    for (n, v) in y.iter().enumerate().take(960) {
        let k = n % 96;
        if (2..46).contains(&k) {
            assert!(*v > 0.0, "{n}: {v}");
        } else if (50..94).contains(&k) {
            assert!(*v < 0.0, "{n}: {v}");
        }
    }
    // The fade-out starts at the TTL (960 samples) from 0.2 and takes 50 ms:
    // from sample 5300 the envelope is below 0.2 · 459/4800 ≈ 0.019.
    let amp = 10f64.powf(-1.5) * g0() * g0();
    let tail = y[5300..5760].iter().fold(0.0f64, |m, v| m.max(v.abs()));
    assert!(tail > 0.0 && tail < 0.02 * amp, "{tail} vs {amp}");
    assert!(y[5760..].iter().all(|v| *v == 0.0), "silent after the end");
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
        // The return inputs carry the previous block's spare outputs.
        for j in 0..2 {
            let dst = &mut ibuf[(rxn + j) * N..(rxn + j + 1) * N];
            dst.copy_from_slice(&prev_hil[j * N..(j + 1) * N]);
        }
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
