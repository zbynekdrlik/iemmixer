//! Parity of the linear mix (program spec §3.5; #20 design note §3, §8):
//!
//! - the impulse oracle: an independent reference model, written from the
//!   design note's model (A2–A10) over the declarative site (not the engine's
//!   topology code), gives the gain from every RX channel to every TX channel;
//!   the engine's `Offline` output of staggered impulses equals it within
//!   1e-12 and is exactly zero elsewhere;
//! - block-size invariance: hot material, EQs and limiters working, commands
//!   inside blocks; blocks of 32, 64, 97 and 256 agree bit for bit (§3.5
//!   asks ≤ 1e-12).
//!
//! `cargo test --release -p iem-engine --test parity -- --nocapture` prints
//! the maximum errors (the `engine` CI job reports them).

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use iem_audio_io::{Offline, Planar};
use iem_dsp::pan::{gains, send_gains};
use iem_engine::cmd::{RtOp, push_group};
use iem_engine::core::{Core, Flags};
use iem_engine::rt::{Options, Processor};
use iem_engine::site::{Site, load};
use iem_engine::topology::Topology;
use iem_engine_proto::{
    BandKind, Cmd, Eq, EqTarget, GroupId, InputId, InputState, Level, Limiter, Mix, MixGroup,
    MixId, MixOut, MixState, Source, db_to_lin,
};

struct Rng(u64);

impl Rng {
    fn bits(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn unit(&mut self) -> f64 {
        (self.bits() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.unit()
    }
    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }
    fn below(&mut self, n: usize) -> usize {
        (self.bits() % n as u64) as usize
    }
}

// ---------------------------------------------------------------------------
// The reference model (design note §3), independent of the engine's code.

/// A stereo signal as gains from each RX channel: rows L and R.
#[derive(Clone)]
struct Sig([Vec<f64>; 2]);

impl Sig {
    fn zero(n: usize) -> Self {
        Self([vec![0.0; n], vec![0.0; n]])
    }
    fn scaled(&self, (gl, gr): (f64, f64)) -> Self {
        Self([
            self.0[0].iter().map(|x| x * gl).collect(),
            self.0[1].iter().map(|x| x * gr).collect(),
        ])
    }
    fn add(&mut self, other: &Sig) {
        for row in 0..2 {
            for (a, b) in self.0[row].iter_mut().zip(&other.0[row]) {
                *a += b;
            }
        }
    }
}

struct Model<'a> {
    site: &'a Site,
    state: &'a MixState,
    /// RX column of each input channel, in site order.
    columns: HashMap<&'a str, [usize; 2]>,
    rx: usize,
    /// Each mix's output O, in declaration order.
    out: HashMap<String, Sig>,
}

impl<'a> Model<'a> {
    fn new(site: &'a Site, state: &'a MixState) -> Self {
        let mut columns = HashMap::new();
        let mut rx = 0;
        for input in &site.inputs {
            let c = if input.rx.len() == 2 {
                [rx, rx + 1]
            } else {
                [rx, rx]
            };
            columns.insert(input.id.as_str(), c);
            rx += input.rx.len();
        }
        Self {
            site,
            state,
            columns,
            rx,
            out: HashMap::new(),
        }
    }

    fn input_state(&self, id: &str) -> InputState {
        self.state
            .inputs
            .get(&InputId::new(id))
            .copied()
            .unwrap_or_default()
    }

    fn mix_state(&self, id: &str) -> Mix {
        self.state
            .mixes
            .get(&MixId::new(id))
            .cloned()
            .unwrap_or_default()
    }

    /// A2, A3, A4: an input's signal P (EQs are flat here).
    fn pre(&self, id: &str) -> Sig {
        let s = self.input_state(id);
        let mut sig = Sig::zero(self.rx);
        let [cl, cr] = self.columns[id];
        let trim = if s.processing {
            db_to_lin(s.trim_db)
        } else {
            1.0
        };
        let gate = if s.muted { 0.0 } else { 1.0 };
        sig.0[0][cl] = trim * gate;
        sig.0[1][cr] = trim * gate;
        sig
    }

    /// A5: a level's gain pair.
    fn law(l: &Level) -> (f64, f64) {
        send_gains(db_to_lin(l.gain_db), l.muted, l.pan)
    }

    /// A7–A9: a mix's output O after its volume and mute.
    fn mix(&mut self, id: &str) -> Sig {
        let site: &'a Site = self.site;
        let spec = site.mixes.iter().find(|m| m.id == id).unwrap();
        let state = self.mix_state(id);
        let level = |input: &str| {
            state
                .inputs
                .get(&InputId::new(input))
                .copied()
                .unwrap_or_default()
        };
        let grouped: Vec<&str> = site
            .groups
            .iter()
            .flat_map(|g| g.inputs.iter().map(String::as_str))
            .collect();
        let mut sum = Sig::zero(self.rx);
        // A6: every ungrouped input at its level, read at P.
        for input in &site.inputs {
            if !grouped.contains(&input.id.as_str()) {
                sum.add(&self.pre(&input.id).scaled(Self::law(&level(&input.id))));
            }
        }
        // A7: each group's strip: its inputs at their levels → fader → mute.
        for group in &site.groups {
            let mut g = Sig::zero(self.rx);
            for input in &group.inputs {
                g.add(&self.pre(input).scaled(Self::law(&level(input))));
            }
            let strip = state
                .groups
                .get(&GroupId::new(group.id.clone()))
                .copied()
                .unwrap_or_default();
            sum.add(&g.scaled(send_gains(db_to_lin(strip.gain_db), strip.muted, 0.0)));
        }
        // A9: the mixes it hears, after their mute, unclipped.
        for heard in &spec.mixes {
            let l = state
                .mixes
                .get(&MixId::new(heard.clone()))
                .copied()
                .unwrap_or_default();
            let o = self.out[heard.as_str()].clone();
            sum.add(&o.scaled(Self::law(&l)));
        }
        // A8: volume and mute (the EQ is flat, the limiter below its threshold).
        let o = sum.scaled(send_gains(
            db_to_lin(state.out.volume_db),
            state.out.muted,
            0.0,
        ));
        self.out.insert(id.to_owned(), o.clone());
        o
    }

    /// Gain from each RX channel to each TX channel, in TX order.
    fn tx_matrix(&mut self) -> Vec<Vec<f64>> {
        let mut rows = Vec::new();
        let mixes: Vec<(String, usize)> = self
            .site
            .mixes
            .iter()
            .map(|m| (m.id.clone(), m.tx.len()))
            .collect();
        for (id, tx) in mixes {
            let o = self.mix(&id);
            if tx == 1 {
                // A10: (L + R)/2 on its one channel.
                rows.push(
                    o.0[0]
                        .iter()
                        .zip(&o.0[1])
                        .map(|(l, r)| (l + r) * 0.5)
                        .collect(),
                );
            } else {
                rows.push(o.0[0].clone());
                rows.push(o.0[1].clone());
            }
        }
        rows
    }
}

// ---------------------------------------------------------------------------

fn site() -> Site {
    load(&common::site_path()).unwrap()
}

fn level(rng: &mut Rng) -> Level {
    Level {
        gain_db: if rng.chance(0.05) {
            -150.0
        } else {
            rng.range(-30.0, 6.0)
        },
        pan: rng.range(-1.0, 1.0),
        muted: rng.chance(0.2),
    }
}

/// A random state with flat EQs (the oracle's linear case).
fn random_state(topo: &Topology, rng: &mut Rng) -> MixState {
    let mut s = MixState::default();
    for n in &topo.inputs {
        s.inputs.insert(
            n.id.clone(),
            InputState {
                trim_db: rng.range(-12.0, 12.0),
                muted: rng.chance(0.2),
                processing: rng.chance(0.7),
                eq: Eq::default(),
            },
        );
    }
    for n in &topo.mixes {
        let mut mix = Mix {
            out: MixOut {
                volume_db: rng.range(-30.0, 6.0),
                muted: rng.chance(0.1),
                eq: Eq::default(),
                limiter: Limiter {
                    enabled: rng.chance(0.8),
                    limit_db: rng.range(-6.0, 0.0),
                },
            },
            ..Mix::default()
        };
        for i in &topo.inputs {
            mix.inputs.insert(i.id.clone(), level(rng));
        }
        for g in &topo.groups {
            mix.groups.insert(
                g.id.clone(),
                MixGroup {
                    gain_db: rng.range(-30.0, 6.0),
                    muted: rng.chance(0.1),
                    eq: Eq::default(),
                },
            );
        }
        for &h in &n.mixes {
            mix.mixes.insert(topo.mixes[h].id.clone(), level(rng));
        }
        s.mixes.insert(n.id.clone(), mix);
    }
    s
}

/// Small enough that no limiter or safety stage acts on any path.
const AMP: f64 = 1e-4;

fn impulse_times(rx: usize) -> Vec<usize> {
    (0..rx).map(|k| 64 + 16 * k).collect()
}

fn impulses(rx: usize) -> Planar {
    let times = impulse_times(rx);
    let mut input = Planar::new(rx, times.last().unwrap() + 64);
    for (k, t) in times.iter().enumerate() {
        input.channel_mut(k)[*t] = AMP;
    }
    input
}

fn render(topo: &Arc<Topology>, state: &MixState, input: &Planar, block: usize) -> Planar {
    let (mut p, _h) = Processor::new(
        Arc::clone(topo),
        state,
        &[],
        Options {
            fade_in_ms: 0.0,
            hold: false,
        },
    );
    let run = Offline { block }.run(&mut p, input, topo.tx.len());
    assert!(run.fault.is_none());
    run.output
}

#[test]
fn impulse_oracle_matches_random_states() {
    let site = site();
    let topo = common::topology();
    let times = impulse_times(topo.rx.len());
    let input = impulses(topo.rx.len());
    let mut worst = 0.0f64;
    let mut nonzero = 0usize;
    for seed in 1..=8u64 {
        let mut rng = Rng(0x0dd5_eed0 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let state = random_state(&topo, &mut rng);
        let mut model = Model::new(&site, &state);
        assert_eq!(model.rx, topo.rx.len());
        let expected = model.tx_matrix();
        assert_eq!(expected.len(), topo.tx.len());
        let out = render(&topo, &state, &input, 32);
        for (tx, row) in expected.iter().enumerate() {
            let y = out.channel(tx);
            for (k, t) in times.iter().enumerate() {
                let err = (y[*t] / AMP - row[k]).abs();
                worst = worst.max(err);
                assert!(
                    err <= 1e-12,
                    "seed {seed} tx {tx} rx {k}: {} vs {}",
                    y[*t] / AMP,
                    row[k]
                );
                if row[k] != 0.0 {
                    nonzero += 1;
                }
            }
            for (i, v) in y.iter().enumerate() {
                if !times.contains(&i) {
                    assert_eq!(
                        *v, 0.0,
                        "seed {seed} tx {tx} sample {i}: zero latency, no leakage"
                    );
                }
            }
        }
    }
    assert!(nonzero > 1000, "the states route signal: {nonzero}");
    println!("oracle max error {worst:e} over 8 random states ({nonzero} non-zero paths)");
}

#[test]
fn the_reference_model_encodes_a3_a6_a7_a9_a10() {
    let site = site();
    let topo = common::topology();
    let mut state = MixState::default();
    let set = |s: &mut MixState, mix: &str, source: Source, gain_db: f64| {
        let m = s.mixes.entry(MixId::new(mix)).or_default();
        let l = Level {
            gain_db,
            ..Level::default()
        };
        match source {
            Source::Input(id) => m.inputs.insert(id, l),
            Source::Mix(id) => m.mixes.insert(id, l),
        };
    };
    let input = |s: &str| Source::Input(InputId::new(s));
    set(&mut state, "member2", input("mic1"), 0.0);
    set(&mut state, "member2", input("mic2"), 0.0);
    set(&mut state, "member2", input("drums"), 0.0);
    set(
        &mut state,
        "member1",
        Source::Mix(MixId::new("member2")),
        0.0,
    );
    set(&mut state, "translator", input("hand1"), 0.0);
    set(&mut state, "member3", input("drums"), 0.0);
    state
        .mixes
        .get_mut(&MixId::new("member3"))
        .unwrap()
        .groups
        .insert(
            GroupId::new("stems"),
            MixGroup {
                muted: true,
                ..MixGroup::default()
            },
        );
    state.inputs.insert(
        InputId::new("mic1"),
        InputState {
            muted: true,
            ..InputState::default()
        },
    );
    state.inputs.insert(
        InputId::new("mic2"),
        InputState {
            trim_db: -6.0,
            processing: false,
            ..InputState::default()
        },
    );
    let m2 = state.mixes.get_mut(&MixId::new("member2")).unwrap();
    m2.groups.insert(
        GroupId::new("stems"),
        MixGroup {
            gain_db: -6.0,
            ..MixGroup::default()
        },
    );
    m2.out.volume_db = 12.0;
    let mut model = Model::new(&site, &state);
    let m = model.tx_matrix();
    let col = |id: &str| topo.inputs[topo.input_index(&InputId::new(id)).unwrap()].rx[0];
    let row = |id: &str| topo.mixes[topo.mix_index(&MixId::new(id)).unwrap()].tx[0].unwrap();
    let g = gains(0.0).0;
    // A3: a muted input reaches nothing.
    assert!(m.iter().all(|r| r[col("mic1")] == 0.0));
    // A6: a level reads the input after its processing: trim is ignored
    // while processing is off.
    assert_eq!(m[row("member2")][col("mic2")], g * (db_to_lin(12.0) * g));
    // A7: a grouped input reaches its mix through the group's strip.
    let stems = g * (db_to_lin(-6.0) * g) * (db_to_lin(12.0) * g);
    assert!((m[row("member2")][col("drums")] - stems).abs() < 1e-15);
    // Only through it: with member3's strip muted, drums at 0 dB are silent there.
    assert_eq!(m[row("member3")][col("drums")], 0.0);
    // A9: member1 hears member2 after its volume, unclipped (> 1).
    let post = g * (db_to_lin(12.0) * g);
    assert!(post > 3.9);
    assert!((m[row("member1")][col("mic2")] - post * g * g).abs() < 1e-15);
    // A10: the translator carries only its level, downmixed.
    assert!((m[row("translator")][col("hand1")] - g * g).abs() < 1e-15);
    assert_eq!(m[row("translator")][col("hand2")], 0.0);
    // The engine agrees on this state too (impulses kept below every limit).
    let times = impulse_times(topo.rx.len());
    let out = render(&topo, &state, &impulses(topo.rx.len()), 64);
    for (tx, r) in m.iter().enumerate() {
        for (k, t) in times.iter().enumerate() {
            assert!((out.channel(tx)[*t] / AMP - r[k]).abs() <= 1e-12);
        }
    }
}

// ---------------------------------------------------------------------------

fn random_eq(rng: &mut Rng) -> Eq {
    let mut eq = Eq::default();
    for b in &mut eq.bands {
        b.enabled = rng.chance(0.6);
        b.freq_hz = 2f64.powf(rng.range(4.5, 14.5));
        b.gain_db = rng.range(-12.0, 12.0);
        b.bw_oct = rng.range(0.1, 3.0);
    }
    eq.gain_db = rng.range(-3.0, 3.0);
    eq
}

fn hot_material(rx: usize, frames: usize, rng: &mut Rng) -> Planar {
    let mut p = Planar::new(rx, frames);
    for ch in 0..rx {
        let amp = rng.range(0.2, 2.0);
        let hz = rng.range(40.0, 8000.0);
        let noise = rng.range(0.0, 0.5);
        for (i, x) in p.channel_mut(ch).iter_mut().enumerate() {
            let t = i as f64 / 96_000.0;
            *x = amp * (std::f64::consts::TAU * hz * t).sin() + noise * rng.range(-1.0, 1.0);
        }
    }
    p
}

fn random_cmd(topo: &Topology, rng: &mut Rng) -> Cmd {
    let m = rng.below(topo.mixes.len());
    let mix = topo.mixes[m].id.clone();
    let input_of = |rng: &mut Rng| topo.inputs[rng.below(topo.inputs.len())].id.clone();
    let stems = GroupId::new("stems");
    match rng.below(10) {
        0 => Cmd::SetMix {
            mix,
            volume_db: Some(rng.range(-20.0, 6.0)),
            muted: Some(rng.chance(0.2)),
        },
        1 | 2 => {
            let heard = &topo.mixes[m].mixes;
            let source = if !heard.is_empty() && rng.chance(0.3) {
                Source::Mix(topo.mixes[heard[rng.below(heard.len())]].id.clone())
            } else {
                Source::Input(input_of(rng))
            };
            Cmd::SetLevel {
                mix,
                source,
                gain_db: Some(rng.range(-20.0, 6.0)),
                pan: Some(rng.range(-1.0, 1.0)),
                muted: Some(rng.chance(0.2)),
            }
        }
        3 => Cmd::SetEq {
            target: EqTarget::Input(input_of(rng)),
            eq: random_eq(rng),
        },
        4 => Cmd::SetEq {
            target: if rng.chance(0.5) {
                EqTarget::Mix(mix)
            } else {
                EqTarget::Group { mix, group: stems }
            },
            eq: random_eq(rng),
        },
        5 => Cmd::SetLimiter {
            mix,
            enabled: Some(rng.chance(0.7)),
            limit_db: Some(rng.range(-6.0, 0.0)),
        },
        6 => Cmd::SetSolo {
            mix,
            sources: if rng.chance(0.5) {
                vec![Source::Input(input_of(rng))]
            } else {
                vec![]
            },
        },
        7 => Cmd::SetInput {
            input: input_of(rng),
            trim_db: Some(rng.range(-12.0, 12.0)),
            muted: Some(rng.chance(0.2)),
            processing: Some(rng.chance(0.6)),
        },
        8 => Cmd::SetGroup {
            mix,
            group: stems,
            gain_db: Some(rng.range(-20.0, 6.0)),
            muted: Some(rng.chance(0.2)),
        },
        _ => Cmd::StartListen { mix },
    }
}

#[test]
fn outputs_do_not_depend_on_the_block_size() {
    let topo = common::topology();
    let mut rng = Rng(0x1b10_c512_e000_0001);
    let mut state = common::open_state(&topo);
    for n in &topo.inputs {
        let mut eq = random_eq(&mut rng);
        eq.bands[0].kind = BandKind::HighPass;
        state.inputs.insert(
            n.id.clone(),
            InputState {
                eq,
                processing: rng.chance(0.8),
                ..InputState::default()
            },
        );
    }
    for mix in state.mixes.values_mut() {
        mix.out.eq = random_eq(&mut rng);
        for strip in mix.groups.values_mut() {
            strip.eq = random_eq(&mut rng);
        }
    }
    let frames = 24_000;
    let input = hot_material(topo.rx.len(), frames, &mut rng);
    // One schedule of commands at fixed sample indices, applied through the core.
    let mut core = Core::new(Arc::clone(&topo), &state, 0, Flags::default());
    let mut schedule: Vec<(u64, Vec<RtOp>)> = Vec::new();
    let mut at = 100u64;
    while schedule.len() < 40 {
        at += 100 + rng.below(450) as u64;
        let cmd = random_cmd(&topo, &mut rng);
        if let Ok(out) = core.apply(&cmd)
            && !out.rt.is_empty()
        {
            schedule.push((at, out.rt));
        }
    }
    assert!(schedule.last().unwrap().0 < frames as u64);
    let run = |block: usize| {
        let (mut p, mut h) = Processor::new(Arc::clone(&topo), &state, &[], Options::default());
        for (at, ops) in &schedule {
            assert!(push_group(&mut h.cmds, *at, ops));
        }
        let out = Offline { block }.run(&mut p, &input, topo.tx.len());
        assert!(out.fault.is_none());
        out.output
    };
    let reference = run(32);
    let loud = (0..topo.tx.len())
        .filter(|&ch| reference.channel(ch).iter().any(|y| y.abs() > 0.05))
        .count();
    assert!(loud >= 10, "the material reaches the outputs: {loud}");
    let mut worst = 0.0f64;
    for block in [64, 97, 256] {
        let other = run(block);
        for ch in 0..topo.tx.len() {
            for (a, b) in reference.channel(ch).iter().zip(other.channel(ch)) {
                worst = worst.max((a - b).abs());
            }
        }
        // §3.5 allows 1e-12; the design (every ramp per sample, blocks cut
        // at command times) promises bit-identical output.
        assert_eq!(worst, 0.0, "block {block}: {worst:e}");
    }
    println!(
        "invariance max difference {worst:e} (blocks 32/64/97/256, {} commands, {frames} samples x {} TX)",
        schedule.len(),
        topo.tx.len()
    );
}

/// The HIL schedule: 30 random mix commands, a HIL signal on one spare
/// output (1 003) stopped inside a block (6 011), and one on both spare
/// outputs that runs to its end (12 007, 0.02 s). With `listen` both signals
/// carry the listen probe (S7), and both listen slots are listened from 50.
fn hil_schedule(
    topo: &Arc<Topology>,
    state: &MixState,
    rng: &mut Rng,
    listen: bool,
) -> Vec<(u64, Vec<RtOp>)> {
    let flags = Flags {
        test_signal: true,
        fault_injection: false,
    };
    let mut core = Core::new(Arc::clone(topo), state, 0, flags).with_hil(common::HIL.to_vec());
    let mut schedule: Vec<(u64, Vec<RtOp>)> = Vec::new();
    if listen {
        for m in ["engineer", "member1"] {
            let start = Cmd::StartListen { mix: MixId::new(m) };
            schedule.push((50, core.apply(&start).unwrap().rt));
        }
    }
    let random = schedule.len() + 30;
    let mut at = 100u64;
    while schedule.len() < random {
        at += 100 + rng.below(450) as u64;
        let cmd = random_cmd(topo, rng);
        if let Ok(out) = core.apply(&cmd)
            && !out.rt.is_empty()
        {
            schedule.push((at, out.rt));
        }
    }
    let hil = |ttl_s: f64, card_tx: &[u16]| Cmd::HilTestSignal {
        input: InputId::new("mic2"),
        hz: 997.0,
        dbfs: -24.0,
        ttl_s,
        card_tx: card_tx.to_vec(),
        listen,
    };
    for (at, cmd) in [
        (1_003, hil(0.1, &common::HIL[1..])),
        (6_011, Cmd::StopTestSignal),
        (12_007, hil(0.02, &common::HIL)),
    ] {
        schedule.push((at, core.apply(&cmd).unwrap().rt));
    }
    schedule.sort_by_key(|(at, _)| *at);
    schedule
}

/// The HIL outputs (S6; the owner's decision on #9 of 2026-09-28) keep the
/// invariance: a HIL signal on one spare output, stopped inside a block, and
/// one on both spare outputs that runs to its end, with random mix commands
/// between them, render bit for bit alike at 32/64/97/256 on every output,
/// the spare outputs after the topology's TX included.
#[test]
fn hil_outputs_do_not_depend_on_the_block_size() {
    let topo = common::topology();
    let mut rng = Rng(0x1b10_c512_e000_0002);
    let state = common::open_state(&topo);
    let frames = 24_000;
    let input = hot_material(topo.rx.len(), frames, &mut rng);
    let schedule = hil_schedule(&topo, &state, &mut rng, false);
    assert!(schedule.last().unwrap().0 < frames as u64);
    let tx = topo.tx.len();
    let run = |block: usize| {
        let (mut p, mut h) = Processor::with_hil(
            Arc::clone(&topo),
            &state,
            &[],
            Options::default(),
            common::HIL.len(),
            0,
        );
        for (at, ops) in &schedule {
            assert!(push_group(&mut h.cmds, *at, ops));
        }
        let outs = p.outputs();
        assert_eq!(outs, tx + common::HIL.len());
        let out = Offline { block }.run(&mut p, &input, outs);
        assert!(out.fault.is_none());
        out.output
    };
    let reference = run(32);
    // Both spare outputs carried the signal, the first only in the second
    // signal; neither sounds after its end.
    for ch in [tx, tx + 1] {
        let y = reference.channel(ch);
        assert!(y.iter().any(|v| v.abs() > 1e-3), "spare output {ch}");
        assert!(y[19_000..].iter().all(|v| *v == 0.0), "spare output {ch}");
    }
    assert!(reference.channel(tx)[..12_007].iter().all(|v| *v == 0.0));
    let mut worst = 0.0f64;
    for block in [64, 97, 256] {
        let other = run(block);
        for ch in 0..tx + common::HIL.len() {
            for (a, b) in reference.channel(ch).iter().zip(other.channel(ch)) {
                worst = worst.max((a - b).abs());
            }
        }
        assert_eq!(worst, 0.0, "block {block}: {worst:e}");
    }
    println!(
        "HIL invariance max difference {worst:e} (blocks 32/64/97/256, {} commands, {frames} samples x {} outputs)",
        schedule.len(),
        tx + common::HIL.len()
    );
}

/// Moves everything `c` holds to the end of `into`.
fn drain(c: &mut rtrb::Consumer<f32>, into: &mut Vec<f32>) {
    let mut got = vec![0.0f32; c.slots()];
    c.pop_entire_slice(&mut got).unwrap();
    into.extend(got);
}

/// The listen probe (S7, #10; the owner's rule of #9 2026-09-28) on the HIL
/// schedule with `listen` and both listen slots listened: at 32/64/97/256
/// every mix's TX is exactly 0.0 inside each signal, both listen taps carry
/// exactly 0.0 there (their cadence kept), the probe taps carry the masked
/// spare output's own samples, and outputs, taps and probe taps are bit for
/// bit alike across the block sizes.
#[test]
fn a_listen_probe_keeps_every_mix_tx_zero_and_its_taps_do_not_depend_on_the_block_size() {
    // Rendered in 50 ms pieces (the rings hold 200 ms), every ring drained
    // after each.
    const PIECE: usize = 4_800;
    let topo = common::topology();
    let mut rng = Rng(0x1b10_c512_e000_0002);
    let state = common::open_state(&topo);
    let frames = 24_000;
    let input = hot_material(topo.rx.len(), frames, &mut rng);
    let schedule = hil_schedule(&topo, &state, &mut rng, true);
    // The signals: from 1 003 to the stop at 6 011 and its 50 ms fade-out;
    // from 12 007 for 0.02 s and the fade-out. The taps start at 50.
    let spans = [(1_003, 6_011 + 4_800), (12_007, 12_007 + 1_920 + 4_800)];
    let first_tap = 50;
    let (tx, rxn) = (topo.tx.len(), topo.rx.len());
    let run = |block: usize| {
        let (mut p, mut h) = Processor::with_hil(
            Arc::clone(&topo),
            &state,
            &[],
            Options::default(),
            common::HIL.len(),
            0,
        );
        for (at, ops) in &schedule {
            assert!(push_group(&mut h.cmds, *at, ops));
        }
        let outs = p.outputs();
        let mut out = vec![Vec::with_capacity(frames); outs];
        let mut taps: [Vec<f32>; 2] = Default::default();
        let mut probes: [Vec<f32>; 2] = Default::default();
        for start in (0..frames).step_by(PIECE) {
            let mut piece = Planar::new(rxn, PIECE);
            for ch in 0..rxn {
                piece
                    .channel_mut(ch)
                    .copy_from_slice(&input.channel(ch)[start..start + PIECE]);
            }
            let rendered = Offline { block }.run(&mut p, &piece, outs);
            assert!(rendered.fault.is_none());
            for (ch, all) in out.iter_mut().enumerate() {
                all.extend_from_slice(rendered.output.channel(ch));
            }
            for k in 0..2 {
                drain(&mut h.taps[k], &mut taps[k]);
                drain(&mut h.probes[k], &mut probes[k]);
            }
        }
        (out, taps, probes)
    };
    let (reference, ref_taps, ref_probes) = run(32);
    for &(a, b) in &spans {
        for (ch, y) in reference[..tx].iter().enumerate() {
            assert!(
                y[a..b].iter().all(|v| *v == 0.0),
                "TX {ch} sounded in {a}..{b}"
            );
        }
    }
    // Spare output 95 carries both signals: the probe's own samples.
    let want: Vec<f32> = spans
        .iter()
        .flat_map(|&(a, b)| reference[tx + 1][a..b].iter().map(|y| *y as f32))
        .collect();
    assert!(want.iter().any(|y| y.abs() > 1e-3));
    for k in 0..2 {
        let tap = &ref_taps[k];
        assert_eq!(tap.len(), 2 * (frames - first_tap), "tap {k}");
        for &(a, b) in &spans {
            let during = &tap[2 * (a - first_tap)..2 * (b - first_tap)];
            assert!(
                during.iter().all(|x| *x == 0.0),
                "tap {k} carried its mix in {a}..{b}"
            );
        }
        assert!(
            tap.iter().any(|x| x.abs() > 0.01),
            "tap {k} carries its mix outside the signals"
        );
        let probe = &ref_probes[k];
        assert_eq!(probe.len(), 2 * want.len(), "probe {k}");
        assert!(
            probe.iter().step_by(2).eq(want.iter()),
            "probe {k}: the spare output's own samples"
        );
        assert!(probe.chunks(2).all(|p| p[0] == p[1]), "probe {k}: l == r");
    }
    for block in [64, 97, 256] {
        let (out, taps, probes) = run(block);
        assert!(out == reference, "block {block}: the outputs differ");
        assert!(taps == ref_taps, "block {block}: the listen taps differ");
        assert!(probes == ref_probes, "block {block}: the probe taps differ");
    }
    println!(
        "listen probe invariance: bit-identical (blocks 32/64/97/256, {} commands, {} probe frames x 2 slots)",
        schedule.len(),
        want.len()
    );
}
