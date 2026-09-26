//! Parity of the linear mix (program spec §3.5; design note §4):
//!
//! - the impulse oracle: an independent reference model, written from A2–A11
//!   over the declarative site (not the engine's graph), gives the gain from
//!   every RX channel to every TX channel; the engine's `Offline` output of
//!   staggered impulses equals it within 1e-12 and is exactly zero elsewhere;
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
use iem_engine::graph::Graph;
use iem_engine::rt::{Options, Processor};
use iem_engine::site::{Site, load};
use iem_engine_proto::{
    BandKind, BusId, BusKind, BusState, Cmd, Eq, EqOwner, InputId, InputState, Limiter, MixState,
    SendEntry, SendId, SendState, Source, Tap, db_to_lin,
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
// The reference model (A2–A11), independent of the engine's graph code.

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
    memo: HashMap<String, Sig>,
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
            memo: HashMap::new(),
        }
    }

    fn input_state(&self, id: &str) -> InputState {
        self.state
            .inputs
            .get(&InputId::new(id))
            .copied()
            .unwrap_or_default()
    }

    fn bus_state(&self, id: &str) -> BusState {
        self.state
            .buses
            .get(&BusId::new(id))
            .copied()
            .unwrap_or_default()
    }

    fn kind(&self, id: &str) -> Option<BusKind> {
        self.site.buses.iter().find(|b| b.id == id).map(|b| b.kind)
    }

    /// A2, A3, A4: the pre-fader tap (EQs are flat here).
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

    /// A11: an input's post-fader output (no mute there: the gate is in `pre`).
    fn input_post(&self, id: &str) -> Sig {
        let s = self.input_state(id);
        self.pre(id)
            .scaled(send_gains(db_to_lin(s.fader_db), false, s.pan))
    }

    fn send_state(&self, src: &Source, dst: &str) -> SendState {
        let id = SendId {
            src: src.clone(),
            dst: BusId::new(dst),
        };
        self.state
            .sends
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.state)
            .unwrap_or_default()
    }

    /// A7, A8, A9, A10, A11: a bus's output after its fader and mute (O).
    fn bus(&mut self, id: &str) -> Sig {
        if let Some(s) = self.memo.get(id) {
            return s.clone();
        }
        let site: &'a Site = self.site;
        let mut sum = Sig::zero(self.rx);
        for family in &site.sends {
            for to in family.to.iter().filter(|t| t.as_str() == id) {
                for from in &family.from {
                    let (src, sig) = match family.tap {
                        // A6: mode 3 reads pre-fader post-FX.
                        Tap::Pre => (Source::Input(InputId::new(from.clone())), self.pre(from)),
                        // A6/A9: mode 0 reads post-fader post-mute.
                        Tap::Post => (Source::Bus(BusId::new(from.clone())), self.bus(from)),
                    };
                    let st = self.send_state(&src, to);
                    sum.add(&sig.scaled(send_gains(db_to_lin(st.gain_db), st.muted, st.pan)));
                }
            }
        }
        if self.kind(id) == Some(BusKind::Master) {
            for input in &site.inputs {
                sum.add(&self.input_post(&input.id));
            }
            let stems: Vec<String> = site
                .buses
                .iter()
                .filter(|b| b.kind == BusKind::Stems)
                .map(|b| b.id.clone())
                .collect();
            for s in stems {
                let o = self.bus(&s);
                sum.add(&o);
            }
        }
        let b = self.bus_state(id);
        let out = sum.scaled(send_gains(db_to_lin(b.fader_db), b.muted, b.pan));
        self.memo.insert(id.to_owned(), out.clone());
        out
    }

    /// Gain from each RX channel to each TX channel, in TX order.
    fn tx_matrix(&mut self) -> Vec<Vec<f64>> {
        let mut rows = Vec::new();
        let buses: Vec<(String, BusKind, usize)> = self
            .site
            .buses
            .iter()
            .map(|b| (b.id.clone(), b.kind, b.tx.len()))
            .collect();
        for (id, kind, tx) in buses {
            if tx == 0 {
                continue;
            }
            let o = self.bus(&id);
            if kind == BusKind::Translator {
                // A10: (gL·L + gR·R)/2 on its one channel.
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

fn db(rng: &mut Rng, lo: f64, hi: f64) -> f64 {
    rng.range(lo, hi)
}

/// A random state with flat EQs (the oracle's linear case).
fn random_state(graph: &Graph, rng: &mut Rng) -> MixState {
    let mut s = MixState::default();
    for n in &graph.inputs {
        s.inputs.insert(
            n.id.clone(),
            InputState {
                trim_db: db(rng, -12.0, 12.0),
                muted: rng.chance(0.2),
                processing: rng.chance(0.7),
                fader_db: db(rng, -30.0, 6.0),
                pan: rng.range(-1.0, 1.0),
                eq: Eq::default(),
            },
        );
    }
    for n in &graph.buses {
        s.buses.insert(
            n.id.clone(),
            BusState {
                fader_db: db(rng, -30.0, 6.0),
                pan: rng.range(-1.0, 1.0),
                muted: rng.chance(0.1),
                eq: Eq::default(),
                limiter: Limiter {
                    enabled: rng.chance(0.8),
                    limit_db: rng.range(-6.0, 0.0),
                },
            },
        );
    }
    for e in &graph.sends {
        s.sends.push(SendEntry {
            id: e.id.clone(),
            state: SendState {
                gain_db: if rng.chance(0.05) {
                    -150.0
                } else {
                    db(rng, -30.0, 6.0)
                },
                pan: rng.range(-1.0, 1.0),
                muted: rng.chance(0.2),
            },
        });
    }
    s
}

const AMP: f64 = 1e-3;

fn impulse_times(rx: usize) -> Vec<usize> {
    (0..rx).map(|k| 64 + 16 * k).collect()
}

fn render(graph: &Arc<Graph>, state: &MixState, input: &Planar, block: usize) -> Planar {
    let (mut p, _h) = Processor::new(Arc::clone(graph), state, &[], Options { fade_in_ms: 0.0 });
    let run = Offline { block }.run(&mut p, input, graph.tx.len());
    assert!(run.fault.is_none());
    run.output
}

#[test]
fn impulse_oracle_matches_random_states() {
    let site = site();
    let graph = common::graph();
    let times = impulse_times(graph.rx.len());
    let frames = times.last().unwrap() + 64;
    let mut input = Planar::new(graph.rx.len(), frames);
    for (k, t) in times.iter().enumerate() {
        input.channel_mut(k)[*t] = AMP;
    }
    let mut worst = 0.0f64;
    let mut nonzero = 0usize;
    for seed in 1..=8u64 {
        let mut rng = Rng(0x0dd5_eed0 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let state = random_state(&graph, &mut rng);
        let mut model = Model::new(&site, &state);
        assert_eq!(model.rx, graph.rx.len());
        let expected = model.tx_matrix();
        assert_eq!(expected.len(), graph.tx.len());
        let out = render(&graph, &state, &input, 32);
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
fn the_reference_model_encodes_a3_a6_a9_a10() {
    let site = site();
    let graph = common::graph();
    let mut state = MixState::default();
    let send = |s: &mut MixState, src: Source, dst: &str, gain_db: f64| {
        s.sends.push(SendEntry {
            id: SendId {
                src,
                dst: BusId::new(dst),
            },
            state: SendState {
                gain_db,
                ..SendState::default()
            },
        });
    };
    let input = |s: &str| Source::Input(InputId::new(s));
    send(&mut state, input("mic1"), "member2", 0.0);
    send(&mut state, input("mic2"), "member2", 0.0);
    send(&mut state, input("drums"), "member2.stems", 0.0);
    send(
        &mut state,
        Source::Bus(BusId::new("member2.stems")),
        "member2",
        0.0,
    );
    send(
        &mut state,
        Source::Bus(BusId::new("member2")),
        "member1",
        0.0,
    );
    send(&mut state, input("hand1"), "translator", 0.0);
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
            fader_db: -150.0,
            ..InputState::default()
        },
    );
    state.buses.insert(
        BusId::new("member2.stems"),
        BusState {
            fader_db: -6.0,
            ..BusState::default()
        },
    );
    state.buses.insert(
        BusId::new("member2"),
        BusState {
            fader_db: 12.0,
            ..BusState::default()
        },
    );
    let mut model = Model::new(&site, &state);
    let m = model.tx_matrix();
    let col = |id: &str| graph.inputs[graph.input_index(&InputId::new(id)).unwrap()].rx[0];
    let row = |id: &str| graph.buses[graph.bus_index(&BusId::new(id)).unwrap()].tx[0].unwrap();
    let g = gains(0.0).0;
    // A3: a muted input reaches nothing.
    assert!(m.iter().all(|r| r[col("mic1")] == 0.0));
    // A6: the input fader at −∞ does not touch the pre-fader send.
    assert_eq!(m[row("member2")][col("mic2")], g * (db_to_lin(12.0) * g));
    // A6: a post send follows the stems fader.
    let stems = g * (db_to_lin(-6.0) * g) * g * (db_to_lin(12.0) * g);
    assert!((m[row("member2")][col("drums")] - stems).abs() < 1e-15);
    // A9: the elevated member reads member 2 post-fader, unclipped (> 1).
    let post = g * (db_to_lin(12.0) * g);
    assert!(post > 3.9);
    assert!((m[row("member1")][col("mic2")] - post * g * g).abs() < 1e-15);
    // A10: the translator carries only its send, downmixed.
    assert!((m[row("translator")][col("hand1")] - g * g).abs() < 1e-15);
    assert_eq!(m[row("translator")][col("hand2")], 0.0);
    // The engine agrees on this state too (impulses kept below every limit).
    let times = impulse_times(graph.rx.len());
    let mut input = Planar::new(graph.rx.len(), times.last().unwrap() + 64);
    for (k, t) in times.iter().enumerate() {
        input.channel_mut(k)[*t] = AMP;
    }
    let out = render(&graph, &state, &input, 64);
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

fn random_cmd(graph: &Graph, rng: &mut Rng) -> Cmd {
    let bus_of = |rng: &mut Rng, kinds: &[BusKind]| loop {
        let n = &graph.buses[rng.below(graph.buses.len())];
        if kinds.contains(&n.kind) {
            return n.id.clone();
        }
    };
    let input_of = |rng: &mut Rng| graph.inputs[rng.below(graph.inputs.len())].id.clone();
    match rng.below(9) {
        0 => Cmd::SetBus {
            bus: bus_of(
                rng,
                &[
                    BusKind::Output,
                    BusKind::Stems,
                    BusKind::Translator,
                    BusKind::Master,
                ],
            ),
            fader_db: Some(rng.range(-20.0, 6.0)),
            pan: Some(rng.range(-1.0, 1.0)),
            muted: Some(rng.chance(0.2)),
        },
        1 | 2 => Cmd::SetSend {
            id: graph.sends[rng.below(graph.sends.len())].id.clone(),
            gain_db: Some(rng.range(-20.0, 6.0)),
            pan: Some(rng.range(-1.0, 1.0)),
            muted: Some(rng.chance(0.2)),
        },
        3 => Cmd::SetEq {
            owner: EqOwner::Input(input_of(rng)),
            eq: random_eq(rng),
        },
        4 => Cmd::SetEq {
            owner: EqOwner::Bus(bus_of(rng, &[BusKind::Output, BusKind::Stems])),
            eq: random_eq(rng),
        },
        5 => Cmd::SetLimiter {
            bus: bus_of(rng, &[BusKind::Output]),
            enabled: Some(rng.chance(0.7)),
            limit_db: Some(rng.range(-6.0, 0.0)),
        },
        6 => {
            let scope = bus_of(rng, &[BusKind::Output]);
            let src = if rng.chance(0.5) {
                vec![Source::Input(input_of(rng))]
            } else {
                vec![]
            };
            Cmd::SetSolo {
                scope,
                sources: src,
            }
        }
        7 => Cmd::SetInput {
            input: input_of(rng),
            trim_db: Some(rng.range(-12.0, 12.0)),
            muted: Some(rng.chance(0.2)),
            processing: Some(rng.chance(0.6)),
            fader_db: Some(rng.range(-20.0, 6.0)),
            pan: Some(rng.range(-1.0, 1.0)),
        },
        _ => Cmd::StartListen {
            bus: bus_of(rng, &[BusKind::Output]),
        },
    }
}

#[test]
fn outputs_do_not_depend_on_the_block_size() {
    let graph = common::graph();
    let mut rng = Rng(0x1b10_c512_e000_0001);
    let mut state = common::open_state(&graph);
    for n in &graph.inputs {
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
    for n in &graph.buses {
        state.buses.insert(
            n.id.clone(),
            BusState {
                eq: random_eq(&mut rng),
                ..BusState::default()
            },
        );
    }
    let frames = 24_000;
    let input = hot_material(graph.rx.len(), frames, &mut rng);
    // One schedule of commands at fixed sample indices, applied through the core.
    let mut core = Core::new(Arc::clone(&graph), &state, 0, Flags::default());
    let mut schedule: Vec<(u64, Vec<RtOp>)> = Vec::new();
    let mut at = 100u64;
    while schedule.len() < 40 {
        at += 100 + rng.below(450) as u64;
        let cmd = random_cmd(&graph, &mut rng);
        if let Ok(out) = core.apply(&cmd)
            && !out.rt.is_empty()
        {
            schedule.push((at, out.rt));
        }
    }
    assert!(schedule.last().unwrap().0 < frames as u64);
    let run = |block: usize| {
        let (mut p, mut h) = Processor::new(Arc::clone(&graph), &state, &[], Options::default());
        for (at, ops) in &schedule {
            assert!(push_group(&mut h.cmds, *at, ops));
        }
        let out = Offline { block }.run(&mut p, &input, graph.tx.len());
        assert!(out.fault.is_none());
        out.output
    };
    let reference = run(32);
    let loud = (0..graph.tx.len())
        .filter(|&ch| reference.channel(ch).iter().any(|y| y.abs() > 0.05))
        .count();
    assert!(loud >= 10, "the material reaches the outputs: {loud}");
    let mut worst = 0.0f64;
    for block in [64, 97, 256] {
        let other = run(block);
        for ch in 0..graph.tx.len() {
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
        graph.tx.len()
    );
}
