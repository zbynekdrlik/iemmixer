//! Unit tests of peers (#58 design §8): a mix hears a peer's own output,
//! without the mixes and peers that peer hears, so two sets can hear each
//! other; a shadowed peer's own output follows its EQ, limiter and volume
//! and is sanitised like the mix.

use super::*;

/// RX: x 0, y 1 (mono). TX: a 0/1, b 2/3, eng 4/5. a and b hear each
/// other as peers; the engineer hears nothing.
const PEERS: &str = r#"
[engine]
channels = 16
engineer = "eng"
[[engine.inputs]]
id = "x"
rx = [1]
[[engine.inputs]]
id = "y"
rx = [2]
[[engine.mixes]]
id = "a"
tx = [1, 2]
peers = ["b"]
[[engine.mixes]]
id = "b"
tx = [3, 4]
peers = ["a"]
[[engine.mixes]]
id = "eng"
tx = [5, 6]
"#;

const A_L: usize = 0;
const A_R: usize = 1;
const B_L: usize = 2;
const B_R: usize = 3;

fn peer(m: &str) -> Source {
    Source::Mix(mix(m))
}

fn peers_rig(cmds: &[Cmd]) -> Rig {
    rig_with(
        PEERS,
        cmds,
        Flags::default(),
        Options {
            fade_in_ms: 0.0,
            hold: false,
        },
    )
}

/// `frames` of x and y.
fn xy(x: f64, y: f64, frames: usize) -> Planar {
    dc(&[x, y], frames)
}

#[test]
fn a_peer_hears_the_other_mixs_own_output_without_what_that_mix_hears() {
    let cmds = [
        level("a", src_in("x"), 0.0),
        level("b", src_in("y"), 0.0),
        level("a", peer("b"), 0.0),
        level("b", peer("a"), 0.0),
    ];
    let mut r = peers_rig(&cmds);
    assert_eq!(r.p.topo.mixes.iter().filter(|n| n.shadow).count(), 2);
    let (x, y) = (0.25, 0.125);
    let out = r.run(&xy(x, y, 512), 32);
    // Each level and each fader at 0 dB, centred, is g0. a's own output is
    // x·g0², b's is y·g0²; each mix adds the other's own output at its
    // level, then its own fader: nothing of itself comes back through the
    // other (no doubling, no feedback).
    let g = g0();
    let want_a = (x * g + g * (y * g * g)) * g;
    let want_b = (y * g + g * (x * g * g)) * g;
    for (ch, want) in [(A_L, want_a), (A_R, want_a), (B_L, want_b), (B_R, want_b)] {
        assert!(
            out.channel(ch).iter().all(|v| close(*v, want, 1e-15)),
            "TX {ch}: {} vs {want}",
            out.channel(ch)[511]
        );
    }
    // Hearing b whole would have added b's x·g0⁴ again at a's level and
    // fader (x·g0⁶ ≈ 0.03): the tolerance above leaves no room for it.
    // Block-size independence with two shadows reading each other.
    let mut odd = peers_rig(&cmds);
    assert_eq!(odd.run(&xy(x, y, 512), 97), out);
}

#[test]
fn a_shadow_follows_the_mixs_eq_limiter_and_volume() {
    // b carries y and hears a only at an off level, so its TX is its own
    // output; a hears nothing but b, at −6 dB, so a's TX is b's own output
    // at a's level and fader, well below a's limiter.
    let mut r = peers_rig(&[level("b", src_in("y"), 0.0), level("a", peer("b"), -6.0)]);
    let mut eq = iem_engine_proto::Eq::default();
    eq.bands[1].enabled = true;
    eq.bands[1].gain_db = 12.0;
    // The +12 dB low shelf drives b into its −6 dB limiter, the limit then
    // rises to −3 dB, then the volume falls to −9 dB.
    r.at(
        2000,
        &Cmd::SetEq {
            target: EqTarget::Mix(mix("b")),
            eq,
        },
    );
    r.at(4000, &limiter("b", true, -3.0));
    r.at(6000, &set_mix("b", Some(-9.0), None));
    let out = r.run(&xy(0.0, 0.2, 9000), 64);
    let k = 10f64.powf(-0.3) * g0() * g0();
    for (a, b) in [(A_L, B_L), (A_R, B_R)] {
        let (ya, yb) = (out.channel(a), out.channel(b));
        for (t, (va, vb)) in ya.iter().zip(yb).enumerate() {
            assert!(close(*va, vb * k, 1e-12), "{t}: {va} vs {}", vb * k);
        }
    }
    // Each command changed b's own output, so a heard each change.
    let b = out.channel(B_L);
    let (plain, shelved, raised, end) = (b[1999], b[3999], b[5999], b[8999]);
    assert!(close(plain, 0.2 * g0() * g0(), 1e-12), "{plain}");
    assert!(shelved > plain + 0.05, "{shelved} vs {plain}");
    assert!(raised > shelved + 0.05, "{raised} vs {shelved}");
    assert!(end < raised - 0.05, "{end} vs {raised}");
}

#[test]
fn a_tripping_shadow_resets_and_counts() {
    // 9e5 passes the input (X1 at 1e6); +12 dB at b's level and volume with
    // its limiter out trips b's own output and its shadow once per block.
    let cmds = [
        level("b", src_in("y"), 12.0),
        limiter("b", false, -6.0),
        set_mix("b", Some(12.0), None),
        level("a", peer("b"), -6.0),
    ];
    let mut r = peers_rig(&cmds);
    // b's limiter, disabled at start, fades out over its first 10 ms.
    r.run(&xy(0.0, 9e5, 3200), 32);
    let before = r.h.status.trips.load(Ordering::Relaxed);
    assert!(before > 0);
    let out = r.run(&xy(0.0, 9e5, 3200), 32);
    let trips = r.h.status.trips.load(Ordering::Relaxed) - before;
    assert_eq!(trips, 2 * 100, "b and its shadow, once per 32-sample block");
    assert!(out.channel(B_L).iter().all(|v| *v == 0.0), "b is silenced");
    assert!(
        out.channel(A_L).iter().all(|v| *v == 0.0),
        "a hears nothing of a tripping shadow"
    );
    // Back to a quiet y: the shadow, reset, follows b again (a at −6 dB).
    let out = r.run(&xy(0.0, 0.05, 3200), 32);
    let tail = r.h.status.trips.load(Ordering::Relaxed) - before;
    assert_eq!(tail, trips, "no more trips");
    let k = 10f64.powf(-0.3) * g0() * g0();
    let (ya, yb) = (out.channel(A_L), out.channel(B_L));
    assert!(yb[3199] > 0.1, "{}", yb[3199]);
    for (t, (va, vb)) in ya.iter().zip(yb).enumerate() {
        assert!(close(*va, vb * k, 1e-12), "{t}: {va} vs {}", vb * k);
    }
}
