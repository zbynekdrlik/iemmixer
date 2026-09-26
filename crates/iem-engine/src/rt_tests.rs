//! Unit tests of the RT processor on a small site and on `test-site.toml`.

use super::*;
use crate::cmd::push_group;
use crate::core::{Core, Flags};
use crate::graph::compile;
use crate::site::parse;
use iem_audio_io::{Offline, Planar};
use iem_dsp::pan::gains;
use iem_engine_proto::{BusId, Cmd, EqOwner, InputId, SendId, Source};

/// RX: mono 0, st 1/2, tb 3. TX: eng 0/1, m1 2/3, tr 4, master 5/6.
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
[[engine.buses]]
id = "eng"
kind = "output"
tx = [1, 2]
[[engine.buses]]
id = "m1"
kind = "output"
tx = [3, 4]
[[engine.buses]]
id = "m1.stems"
kind = "stems"
[[engine.buses]]
id = "tr"
kind = "translator"
tx = [5]
[[engine.buses]]
id = "master"
kind = "master"
tx = [6, 7]
[[engine.sends]]
from = ["mono", "st", "tb"]
to = ["eng", "m1"]
tap = "pre"
[[engine.sends]]
from = ["st"]
to = ["m1.stems"]
tap = "pre"
[[engine.sends]]
from = ["m1.stems"]
to = ["m1"]
tap = "post"
[[engine.sends]]
from = ["m1"]
to = ["eng"]
tap = "post"
[[engine.sends]]
from = ["mono"]
to = ["tr"]
tap = "pre"
"#;

const ENG_L: usize = 0;
const M1_L: usize = 2;
const M1_R: usize = 3;
const TR: usize = 4;
const MASTER_L: usize = 5;
const MASTER_R: usize = 6;

fn bus(s: &str) -> BusId {
    BusId::new(s)
}

fn input(s: &str) -> InputId {
    InputId::new(s)
}

fn src_in(s: &str) -> Source {
    Source::Input(input(s))
}

fn send(src: Source, dst: &str, gain_db: f64) -> Cmd {
    Cmd::SetSend {
        id: SendId { src, dst: bus(dst) },
        gain_db: Some(gain_db),
        pan: None,
        muted: None,
    }
}

fn set_bus(b: &str, fader_db: Option<f64>, pan: Option<f64>, muted: Option<bool>) -> Cmd {
    Cmd::SetBus {
        bus: bus(b),
        fader_db,
        pan,
        muted,
    }
}

fn set_input(i: &str, f: impl FnOnce(&mut Cmd)) -> Cmd {
    let mut c = Cmd::SetInput {
        input: input(i),
        trim_db: None,
        muted: None,
        processing: None,
        fader_db: None,
        pan: None,
    };
    f(&mut c);
    c
}

fn limiter(b: &str, enabled: bool, limit_db: f64) -> Cmd {
    Cmd::SetLimiter {
        bus: bus(b),
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
    let graph = Arc::new(compile(&parse(site).unwrap()).unwrap());
    let mut core = Core::new(Arc::clone(&graph), &MixState::default(), 0, flags);
    for c in cmds {
        core.apply(c).unwrap();
    }
    let (p, h) = Processor::new(graph, &core.state(), &[], opts);
    Rig { core, p, h }
}

fn rig(cmds: &[Cmd]) -> Rig {
    rig_with(SITE, cmds, Flags::default(), Options { fade_in_ms: 0.0 })
}

impl Rig {
    /// Applies `cmd` in the core and queues its RT group at sample `at`.
    fn at(&mut self, at: u64, cmd: &Cmd) {
        let out = self.core.apply(cmd).unwrap();
        assert!(push_group(&mut self.h.cmds, at, &out.rt) || out.rt.is_empty());
    }

    fn run(&mut self, input: &Planar, block: usize) -> Planar {
        let outs = self.p.graph.tx.len();
        let run = Offline { block }.run(&mut self.p, input, outs);
        assert!(run.fault.is_none());
        run.output
    }
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
    let mut r = rig(&[send(src_in("mono"), "m1", 0.0)]);
    let out = r.run(&dc(&[0.25, 0.0, 0.0, 0.0], 512), 64);
    let want = 0.25 * g0() * g0();
    for ch in [M1_L, M1_R] {
        assert!(
            out.channel(ch).iter().all(|y| close(*y, want, 1e-15)),
            "{ch}"
        );
    }
    // Nothing reaches a bus without a send, and every TX channel is written.
    assert!(out.channel(ENG_L).iter().all(|y| *y == 0.0));
}

#[test]
fn stereo_keeps_its_channels() {
    let mut r = rig(&[send(src_in("st"), "m1", 0.0)]);
    let out = r.run(&dc(&[0.0, 0.1, 0.2, 0.0], 256), 256);
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
    let mut r = rig(&[send(src_in("mono"), "m1", 0.0), trim]);
    r.at(
        1000,
        &set_input("mono", |c| {
            if let Cmd::SetInput { processing, .. } = c {
                *processing = Some(false);
            }
        }),
    );
    let out = r.run(&dc(&[0.25, 0.0, 0.0, 0.0], 4000), 97);
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
    // With processing on, an enabled band shapes the signal: a +12 dB low
    // shelf lifts DC about four times.
    let mut eq = iem_engine_proto::Eq::default();
    eq.bands[1].enabled = true;
    eq.bands[1].gain_db = 12.0;
    let mut r = rig(&[
        send(src_in("mono"), "m1", 0.0),
        Cmd::SetEq {
            owner: EqOwner::Input(input("mono")),
            eq,
        },
    ]);
    let boosted = r.run(&dc(&[0.01, 0.0, 0.0, 0.0], 4000), 64);
    assert!(
        boosted.channel(M1_L)[3999] > 0.02,
        "{}",
        boosted.channel(M1_L)[3999]
    );
}

#[test]
fn talkback_is_added_before_the_mute_gate() {
    let mut r = rig(&[send(src_in("tb"), "m1", 0.0)]);
    let talk = vec![0.5f32; TALK_RING];
    r.h.talkback.push_entire_slice(&talk).unwrap();
    let out = r.run(&dc(&[0.0; 4], 2048), 32);
    let want = 0.5 * TALKBACK_GAIN * g0() * g0();
    assert!(out.channel(M1_L)[0] < want);
    assert!(
        close(out.channel(M1_L)[2047], want, 1e-9),
        "{}",
        out.channel(M1_L)[2047]
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
    let out = r.run(&dc(&[0.0; 4], 1024), 32);
    assert_eq!(out.channel(M1_L)[1023], 0.0);
    // A partial block of talkback is an underrun.
    let mut short = rig(&[send(src_in("tb"), "m1", 0.0)]);
    short.h.talkback.push_entire_slice(&[0.5f32; 20]).unwrap();
    short.run(&dc(&[0.0; 4], 64), 32);
    assert_eq!(short.h.status.talkback_underruns.load(Ordering::Relaxed), 1);
    // Without talkback samples the gate closes and nothing is added.
    let mut quiet = rig(&[send(src_in("tb"), "m1", 0.0)]);
    let out = quiet.run(&dc(&[0.0; 4], 256), 32);
    assert!(out.channel(M1_L).iter().all(|y| *y == 0.0));
    assert_eq!(quiet.h.status.talkback_underruns.load(Ordering::Relaxed), 0);
}

#[test]
fn pre_sends_ignore_the_input_fader_post_sends_follow_the_bus_fader() {
    let fader = set_input("mono", |c| {
        if let Cmd::SetInput { fader_db, .. } = c {
            *fader_db = Some(-150.0);
        }
    });
    let mut r = rig(&[send(src_in("mono"), "m1", 0.0), fader]);
    let out = r.run(&dc(&[0.25, 0.0, 0.0, 0.0], 256), 64);
    assert!(close(out.channel(M1_L)[255], 0.25 * g0() * g0(), 1e-15));
    assert_eq!(
        out.channel(MASTER_L)[255],
        0.0,
        "the input fader feeds only the master"
    );
    // A post send (stems → m1) follows the stems bus fader.
    let mut r = rig(&[
        send(src_in("st"), "m1.stems", 0.0),
        send(Source::Bus(bus("m1.stems")), "m1", 0.0),
        set_bus("m1.stems", Some(-6.0), None, None),
    ]);
    let out = r.run(&dc(&[0.0, 0.2, 0.2, 0.0], 256), 64);
    let g = gains(0.0).0;
    let want = 0.2 * g * (10f64.powf(-0.3) * g) * g * g;
    assert!(
        close(out.channel(M1_L)[255], want, 1e-15),
        "{}",
        out.channel(M1_L)[255]
    );
}

#[test]
fn output_chain_is_eq_limiter_fader_mute_safety_clamp() {
    let mut r = rig(&[
        send(src_in("mono"), "m1", 0.0),
        limiter("m1", true, -6.0),
        set_bus("m1", Some(12.0), None, None),
        send(Source::Bus(bus("m1")), "eng", 0.0),
        limiter("eng", false, -6.0),
        set_bus("eng", Some(-12.0), None, None),
    ]);
    let out = r.run(&dc(&[1.0, 0.0, 0.0, 0.0], 4096), 128);
    let m1 = out.channel(M1_L);
    assert!(
        m1.iter().all(|y| y.abs() <= 1.0),
        "the TX never exceeds 1.0"
    );
    assert!(m1[4095] > 0.99, "{}", m1[4095]);
    // A9: the engineer reads member 1 after its mute, before the safety stage:
    // 1.0 → limiter 0.501 → +12 dB ≈ 2.0, unclipped, → −12 dB ≈ 0.5.
    let limited = 10f64.powf(-6.0 / 20.0);
    let post = limited * g0() * (10f64.powf(0.6) * g0());
    let eng = post * g0() * (10f64.powf(-0.6) * g0());
    assert!(post > 1.9);
    assert!(
        close(out.channel(ENG_L)[4095], eng, 1e-9),
        "{}",
        out.channel(ENG_L)[4095]
    );
    // Muting the member mutes its bus-to-bus tap too.
    r.at(0, &set_bus("m1", None, None, Some(true)));
    let out = r.run(&dc(&[1.0, 0.0, 0.0, 0.0], 1024), 128);
    assert_eq!(out.channel(M1_L)[1023], 0.0);
    assert_eq!(out.channel(ENG_L)[1023], 0.0);
}

#[test]
fn translator_is_the_half_sum_on_one_channel() {
    let mut r = rig(&[send(src_in("mono"), "tr", 0.0)]);
    let out = r.run(&dc(&[0.4, 0.0, 0.0, 0.0], 256), 64);
    assert!(close(out.channel(TR)[255], 0.4 * g0() * g0(), 1e-15));
    let mut left = rig(&[
        send(src_in("mono"), "tr", 0.0),
        set_bus("tr", None, Some(-1.0), None),
    ]);
    let out = left.run(&dc(&[0.4, 0.0, 0.0, 0.0], 256), 64);
    let (gl, gr) = gains(-1.0);
    assert_eq!(gr, 0.0);
    assert!(
        close(out.channel(TR)[255], (0.4 * g0() * gl) * 0.5, 1e-15),
        "{}",
        out.channel(TR)[255]
    );
}

#[test]
fn master_sums_post_fader_inputs_and_stems() {
    let mut r = rig(&[
        set_input("mono", |c| {
            if let Cmd::SetInput { fader_db, pan, .. } = c {
                *fader_db = Some(-6.0);
                *pan = Some(0.5);
            }
        }),
        send(src_in("st"), "m1.stems", 0.0),
        set_bus("m1.stems", Some(-3.0), None, None),
        set_bus("master", Some(-1.0), None, None),
    ]);
    let out = r.run(&dc(&[0.1, 0.2, 0.3, 0.0], 256), 64);
    let v = 10f64.powf(-0.3);
    let (pl, pr) = gains(0.5);
    let s = 10f64.powf(-3.0 / 20.0) * g0();
    let m = 10f64.powf(-1.0 / 20.0) * g0();
    // Inputs at unity fader also feed the master: st at 0 dB, tb silent.
    let l = (0.1 * v * pl + 0.2 * g0() + 0.2 * g0() * s) * m;
    let rr = (0.1 * v * pr + 0.3 * g0() + 0.3 * g0() * s) * m;
    assert!(
        close(out.channel(MASTER_L)[255], l, 1e-12),
        "{} {l}",
        out.channel(MASTER_L)[255]
    );
    assert!(close(out.channel(MASTER_R)[255], rr, 1e-12));
}

#[test]
fn sanitizer_silences_and_resets_a_tripping_node() {
    let mut r = rig(&[send(src_in("mono"), "m1", 0.0)]);
    let mut input = dc(&[0.25, 0.0, 0.0, 0.0], 256);
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
    let mut huge = dc(&[2e6, 0.0, 0.0, 0.0], 64);
    huge.channel_mut(0)[0] = 2e6;
    r.run(&huge, 64);
    assert_eq!(r.h.status.trips.load(Ordering::Relaxed), 2);
}

#[test]
fn commands_apply_at_their_sample() {
    for block in [256, 97, 1000] {
        let mut r = rig(&[send(src_in("mono"), "m1", 0.0)]);
        r.at(1000, &set_bus("m1", Some(-6.0), None, None));
        let out = r.run(&dc(&[0.25, 0.0, 0.0, 0.0], 3000), block);
        let y = out.channel(M1_L);
        assert_eq!(y[999], y[998], "{block}");
        assert!(y[1000] < y[999], "{block}: the ramp starts at sample 1000");
        assert!(close(
            y[1000 + 959],
            0.25 * g0() * 10f64.powf(-0.3) * g0(),
            1e-15
        ));
    }
}

#[test]
fn at_most_512_commands_per_block_and_groups_stay_whole() {
    let mut r = rig(&[]);
    for _ in 0..600 {
        assert!(push_group(&mut r.h.cmds, 0, &[RtOp::Nop]));
    }
    r.run(&dc(&[0.0; 4], 32), 32);
    assert_eq!(r.h.cmds.slots(), CMD_RING - 88);
    r.run(&dc(&[0.0; 4], 32), 32);
    assert_eq!(r.h.cmds.slots(), CMD_RING);
    for _ in 0..200 {
        push_group(&mut r.h.cmds, 0, &[RtOp::Nop]);
    }
    assert!(push_group(&mut r.h.cmds, 0, &[RtOp::Nop; 400]));
    r.run(&dc(&[0.0; 4], 32), 32);
    assert_eq!(r.h.cmds.slots(), CMD_RING - 400, "the group waits whole");
    assert_eq!(r.h.status.deferred.load(Ordering::Relaxed), 2);
    r.run(&dc(&[0.0; 4], 32), 32);
    assert_eq!(r.h.cmds.slots(), CMD_RING);
}

#[test]
fn limiter_raise_ramps_lower_is_instant() {
    let mut r = rig(&[send(src_in("mono"), "m1", 0.0), limiter("m1", true, -6.0)]);
    r.at(2000, &limiter("m1", true, 0.0));
    r.at(5000, &limiter("m1", true, -6.0));
    let out = r.run(&dc(&[0.9, 0.0, 0.0, 0.0], 6000), 64);
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
fn test_signal_caps_reachable_tx_and_ends_after_its_ttl() {
    let flags = Flags {
        test_signal: true,
        fault_injection: false,
    };
    let mut r = rig_with(
        SITE,
        &[
            send(src_in("st"), "m1", 0.0),
            send(src_in("mono"), "m1", 0.0),
            send(src_in("mono"), "tr", 0.0),
        ],
        flags,
        Options { fade_in_ms: 0.0 },
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
    let out = r.run(&dc(&[0.5, 0.5, 0.5, 0.0], 12_000), 256);
    let m1 = out.channel(M1_L);
    let active = 960 + 4800;
    assert!(
        m1[..active].iter().all(|y| y.abs() <= TEST_CAP),
        "reachable TX capped"
    );
    assert!(m1[100..active].contains(&TEST_CAP));
    assert!(
        close(out.channel(TR)[1000], 0.5 * g0() * g0(), 1e-15),
        "unreachable TX untouched"
    );
    assert!(
        m1[active + 1000] > 0.4,
        "caps lifted: {}",
        m1[active + 1000]
    );
    // The sine replaced the input: its peak is at most −20 dBFS through a 0 dB send.
    let mut probe = rig_with(
        SITE,
        &[send(src_in("st"), "eng", 0.0)],
        flags,
        Options { fade_in_ms: 0.0 },
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
    let out = probe.run(&dc(&[0.0, 0.3, 0.3, 0.0], 9600), 256);
    let peak = out.channel(ENG_L)[5000..]
        .iter()
        .fold(0.0f64, |m, y| m.max(y.abs()));
    let want = 10f64.powf(-1.5) * g0() * g0();
    assert!(close(peak, want, 1e-4), "{peak} vs {want}");
    probe.at(0, &Cmd::StopTestSignal);
    let out = probe.run(&dc(&[0.0, 0.3, 0.3, 0.0], 9600), 256);
    assert!(close(out.channel(ENG_L)[9599], 0.3 * g0() * g0(), 1e-12));
}

#[test]
fn listen_taps_are_side_effect_free() {
    let cmds = [
        send(src_in("mono"), "eng", 0.0),
        send(src_in("mono"), "m1", 0.0),
        set_bus("eng", Some(-12.0), None, None),
        set_bus("m1", Some(-6.0), None, None),
    ];
    let input = dc(&[0.3, 0.0, 0.0, 0.0], 1024);
    let mut plain = rig(&cmds);
    let a = plain.run(&input, 64);
    let mut tapped = rig(&cmds);
    tapped.at(0, &Cmd::StartListen { bus: bus("eng") });
    tapped.at(0, &Cmd::StartListen { bus: bus("m1") });
    let b = tapped.run(&input, 64);
    assert_eq!(a, b);
    let mut eng = vec![0.0f32; 2048];
    let mut member = vec![0.0f32; 2048];
    tapped.h.taps[0].pop_entire_slice(&mut eng).unwrap();
    tapped.h.taps[1].pop_entire_slice(&mut member).unwrap();
    // Slot 0: the engineer after its limiter, before the fader.
    let pre = (0.3 * g0()) as f32;
    assert!(eng.iter().all(|x| (x - pre).abs() < 1e-6), "{}", eng[0]);
    // Slot 1: the member after fader and mute, through the 0 dB listen limiter.
    let post = (0.3 * g0() * 10f64.powf(-0.3) * g0()) as f32;
    assert!(
        member.iter().all(|x| (x - post).abs() < 1e-6),
        "{}",
        member[0]
    );
    assert_eq!(tapped.h.status.tap_overruns.load(Ordering::Relaxed), 0);
    // Nobody draining the engineer tap: the ring fills and overruns count.
    tapped.run(&dc(&[0.3, 0.0, 0.0, 0.0], TAP_RING), 64);
    assert!(tapped.h.status.tap_overruns.load(Ordering::Relaxed) > 0);
    let mut sink = vec![0.0f32; TAP_RING];
    let _ = tapped.h.taps[0].pop_partial_slice(&mut sink);
    let _ = tapped.h.taps[1].pop_partial_slice(&mut sink);
    tapped.at(0, &Cmd::StopListen { bus: bus("m1") });
    tapped.run(&input, 64);
    assert_eq!(tapped.h.taps[1].slots(), 0, "a stopped tap writes nothing");
    assert_eq!(tapped.h.taps[0].slots(), 2048);
}

#[test]
fn fade_in_and_fade_out() {
    let mut r = rig_with(
        SITE,
        &[send(src_in("mono"), "m1", 0.0)],
        Flags::default(),
        Options::default(),
    );
    let out = r.run(&dc(&[0.25, 0.0, 0.0, 0.0], 50_000), 32);
    let y = out.channel(M1_L);
    let full = 0.25 * g0() * g0();
    assert!(y[0] < full * 1e-3);
    assert!(close(y[24_000], full * 0.5, 1e-3), "{}", y[24_000]);
    assert!(close(y[49_999], full, 1e-15));
    assert!(!r.h.status.faded_out.load(Ordering::Acquire));
    assert!(push_group(&mut r.h.cmds, 0, &[RtOp::FadeOut]));
    let out = r.run(&dc(&[0.25, 0.0, 0.0, 0.0], 6_000), 32);
    let y = out.channel(M1_L);
    assert!(y[2400] < full && y[2400] > 0.0);
    assert_eq!(y[4800], 0.0);
    assert!(r.h.status.faded_out.load(Ordering::Acquire));
}

#[test]
fn meters_publish_every_3200_samples() {
    let mut r = rig(&[
        send(src_in("mono"), "m1", 0.0),
        limiter("m1", true, -6.0),
        Cmd::ResetLimiterStats { bus: bus("m1") },
    ]);
    r.run(&dc(&[0.9, 0.0, 0.0, 0.0], 3168), 32);
    assert!(!r.h.meters.updated());
    r.run(&dc(&[0.9, 0.0, 0.0, 0.0], 32), 32);
    assert!(r.h.meters.updated());
    let g = r.p.graph.bus_index(&bus("m1")).unwrap();
    let f = r.h.meters.read().clone();
    assert_eq!(f.seq, 1);
    assert_eq!(f.inputs[0], [0.9, 0.9]);
    assert_eq!(f.inputs[1], [0.0, 0.0]);
    assert!(
        close(f.buses[g][0], 10f64.powf(-0.3) * g0() * g0(), 1e-9),
        "{:?}",
        f.buses[g]
    );
    assert!(f.gr_db[g] < -0.5, "{}", f.gr_db[g]);
    // 0.9 limited to −6 dB is ≈ −5 dB of GR: every sample since the reset is active.
    assert_eq!(f.active[g], 3200);
    assert_eq!(f.gr_db[r.p.graph.bus_index(&bus("tr")).unwrap()], 0.0);
    r.run(&dc(&[0.0, 0.0, 0.0, 0.0], 3200), 3200);
    let f = r.h.meters.read().clone();
    assert_eq!((f.seq, f.inputs[0]), (2, [0.0, 0.0]));
    // X14 counts from the persisted base.
    let graph = Arc::new(compile(&parse(SITE).unwrap()).unwrap());
    let mut core = Core::new(
        Arc::clone(&graph),
        &MixState::default(),
        0,
        Flags::default(),
    );
    core.apply(&send(src_in("mono"), "m1", 0.0)).unwrap();
    let mut counters = vec![0; graph.buses.len()];
    counters[g] = 1000;
    let (mut p, mut h) =
        Processor::new(graph, &core.state(), &counters, Options { fade_in_ms: 0.0 });
    let run = Offline { block: 3200 }.run(&mut p, &dc(&[2.0, 0.0, 0.0, 0.0], 3200), 7);
    assert!(run.fault.is_none());
    let f = h.meters.read().clone();
    assert_eq!(f.active[g], 1000 + 3200);
    assert!(push_group(
        &mut h.cmds,
        0,
        &[RtOp::ResetLimiter { b: g as u16 }]
    ));
    Offline { block: 3200 }.run(&mut p, &dc(&[0.0, 0.0, 0.0, 0.0], 3200), 7);
    let f = h.meters.read().clone();
    // The reset cleared the base and the count; the GR meter recovers at
    // 8.7 dB/s, so the next 33 ms still count.
    assert_eq!(f.active[g], 3200);
}

#[test]
fn the_fault_injection_op_panics() {
    let mut r = rig(&[]);
    assert!(push_group(&mut r.h.cmds, 64, &[RtOp::Panic]));
    let outs = r.p.graph.tx.len();
    let run = Offline { block: 32 }.run(&mut r.p, &dc(&[0.0; 4], 256), outs);
    let fault = run.fault.expect("fault");
    assert_eq!(fault.frame, 64);
    assert!(fault.message.contains("fault injection"));
}

#[test]
fn the_program_site_runs_and_reports_its_time() {
    let graph = Arc::new(crate::test_support::test_site());
    let (mut p, _h) = Processor::new(
        Arc::clone(&graph),
        &MixState::default(),
        &[],
        Options::default(),
    );
    let run = Offline { block: 32 }.run(&mut p, &dc(&[0.1; 32], 320), graph.tx.len());
    assert!(run.fault.is_none());
    assert_eq!(run.output.channels(), 23);
    assert_eq!(p.time(), 320);
}
