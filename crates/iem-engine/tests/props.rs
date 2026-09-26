//! Randomised requests and command groups (S0/S2 hand-off: the engine caps
//! every field from any sender; design note §4). `IEM_FUZZ_ITERS` and
//! `IEM_FUZZ_SEED` scale and seed the run (the `fuzz` CI job raises them);
//! the coverage-guided target lives in `fuzz/`.

mod common;

use std::sync::Arc;

use iem_audio_io::{Offline, Planar};
use iem_engine::MAX_CMDS_PER_BLOCK;
use iem_engine::cmd::push_group;
use iem_engine::core::{Core, Flags};
use iem_engine::rt::{Options, Processor};
use iem_engine::topology::Topology;
use iem_engine_proto::{
    BandKind, ClientMsg, Cmd, Eq, EqTarget, GroupId, InputId, InputState, Level, Limiter, Mix,
    MixGroup, MixId, MixOut, MixState, Source, parse_client,
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
    fn below(&mut self, n: usize) -> usize {
        (self.bits() % n.max(1) as u64) as usize
    }
    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }
}

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

const WEIRD: [f64; 9] = [
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
    1e308,
    -1e308,
    0.0,
    -0.0,
    1e-320,
    -150.0,
];

fn value(rng: &mut Rng) -> f64 {
    if rng.chance(0.15) {
        WEIRD[rng.below(WEIRD.len())]
    } else {
        (rng.unit() - 0.5) * 400.0
    }
}

fn opt(rng: &mut Rng) -> Option<f64> {
    rng.chance(0.7).then(|| value(rng))
}

fn flag(rng: &mut Rng) -> Option<bool> {
    rng.chance(0.5).then(|| rng.chance(0.5))
}

fn input_id(rng: &mut Rng, g: &Topology) -> InputId {
    if rng.chance(0.05) {
        InputId::new(["", "ghost", "MIC1", "x".repeat(300).as_str()][rng.below(4)])
    } else {
        g.inputs[rng.below(g.inputs.len())].id.clone()
    }
}

fn mix_id(rng: &mut Rng, g: &Topology) -> MixId {
    if rng.chance(0.05) {
        MixId::new(["", "ghost", "Member1", "stems"][rng.below(4)])
    } else {
        g.mixes[rng.below(g.mixes.len())].id.clone()
    }
}

fn group_id(rng: &mut Rng) -> GroupId {
    GroupId::new(if rng.chance(0.9) { "stems" } else { "ghost" })
}

fn eq(rng: &mut Rng) -> Eq {
    let mut e = Eq::default();
    for b in &mut e.bands {
        b.kind = [
            BandKind::HighPass,
            BandKind::LowShelf,
            BandKind::Peak,
            BandKind::HighShelf,
        ][rng.below(4)];
        b.enabled = rng.chance(0.5);
        b.freq_hz = value(rng) * 100.0;
        b.gain_db = value(rng);
        b.bw_oct = value(rng) / 50.0;
    }
    e.gain_db = value(rng);
    e
}

fn source(rng: &mut Rng, g: &Topology) -> Source {
    if rng.chance(0.3) {
        Source::Mix(mix_id(rng, g))
    } else {
        Source::Input(input_id(rng, g))
    }
}

fn level(rng: &mut Rng) -> Level {
    Level {
        gain_db: value(rng),
        pan: value(rng),
        muted: rng.chance(0.3),
    }
}

fn random_state(rng: &mut Rng, g: &Topology) -> MixState {
    let mut s = MixState::default();
    for _ in 0..rng.below(30) {
        s.inputs.insert(
            input_id(rng, g),
            InputState {
                trim_db: value(rng),
                muted: rng.chance(0.3),
                processing: rng.chance(0.5),
                eq: eq(rng),
            },
        );
        let mut mix = Mix {
            out: MixOut {
                volume_db: value(rng),
                muted: rng.chance(0.3),
                eq: eq(rng),
                limiter: Limiter {
                    enabled: rng.chance(0.5),
                    limit_db: value(rng),
                },
            },
            ..Mix::default()
        };
        for _ in 0..rng.below(8) {
            mix.inputs.insert(input_id(rng, g), level(rng));
            mix.mixes.insert(mix_id(rng, g), level(rng));
        }
        mix.groups.insert(
            group_id(rng),
            MixGroup {
                gain_db: value(rng),
                muted: rng.chance(0.3),
                eq: eq(rng),
            },
        );
        s.mixes.insert(mix_id(rng, g), mix);
    }
    s
}

fn command(rng: &mut Rng, g: &Topology, depth: u32) -> Cmd {
    match rng.below(if depth > 0 { 15 } else { 16 }) {
        0 => Cmd::SetInput {
            input: input_id(rng, g),
            trim_db: opt(rng),
            muted: flag(rng),
            processing: flag(rng),
        },
        1 => Cmd::SetMix {
            mix: mix_id(rng, g),
            volume_db: opt(rng),
            muted: flag(rng),
        },
        2 | 3 => Cmd::SetLevel {
            mix: mix_id(rng, g),
            source: source(rng, g),
            gain_db: opt(rng),
            pan: opt(rng),
            muted: flag(rng),
        },
        4 => Cmd::SetEq {
            target: match rng.below(3) {
                0 => EqTarget::Input(input_id(rng, g)),
                1 => EqTarget::Mix(mix_id(rng, g)),
                _ => EqTarget::Group {
                    mix: mix_id(rng, g),
                    group: group_id(rng),
                },
            },
            eq: eq(rng),
        },
        5 => Cmd::SetLimiter {
            mix: mix_id(rng, g),
            enabled: flag(rng),
            limit_db: opt(rng),
        },
        6 => Cmd::ResetLimiterStats {
            mix: mix_id(rng, g),
        },
        7 => Cmd::SetSolo {
            mix: mix_id(rng, g),
            sources: (0..rng.below(4)).map(|_| source(rng, g)).collect(),
        },
        8 => Cmd::StartListen {
            mix: mix_id(rng, g),
        },
        9 => Cmd::StopListen {
            mix: mix_id(rng, g),
        },
        10 => Cmd::StartTestSignal {
            input: input_id(rng, g),
            hz: value(rng) * 50.0,
            dbfs: value(rng),
            ttl_s: value(rng),
        },
        11 => Cmd::StopTestSignal,
        12 => Cmd::ImportState {
            state: random_state(rng, g),
            baseline: rng.chance(0.5),
        },
        13 => [Cmd::GetState, Cmd::Ping, Cmd::SaveNow, Cmd::InjectFault][rng.below(4)].clone(),
        14 => Cmd::SetGroup {
            mix: mix_id(rng, g),
            group: group_id(rng),
            gain_db: opt(rng),
            muted: flag(rng),
        },
        _ => Cmd::Batch {
            ops: (0..rng.below(8))
                .map(|_| command(rng, g, depth + 1))
                .collect(),
        },
    }
}

fn mutate(rng: &mut Rng, mut bytes: Vec<u8>) -> Vec<u8> {
    for _ in 0..1 + rng.below(4) {
        if bytes.is_empty() {
            break;
        }
        let at = rng.below(bytes.len());
        match rng.below(4) {
            0 => bytes[at] = rng.bits() as u8,
            1 => bytes.truncate(at),
            2 => {
                let alphabet = b"9e-{}\"[,:";
                bytes.insert(at, alphabet[rng.below(alphabet.len())]);
            }
            _ => {
                bytes.remove(at);
            }
        }
    }
    bytes
}

fn in_range(v: f64, lo: f64, hi: f64) -> bool {
    v.is_finite() && v >= lo && v <= hi
}

fn assert_level_capped(what: &str, l: &Level) {
    assert!(
        in_range(l.gain_db, -150.0, 12.0),
        "{what} gain {}",
        l.gain_db
    );
    assert!(in_range(l.pan, -1.0, 1.0), "{what} pan {}", l.pan);
}

fn assert_capped(core: &Core) {
    let s = core.state();
    for (id, i) in &s.inputs {
        assert!(in_range(i.trim_db, -150.0, 24.0), "{id} trim {}", i.trim_db);
        assert_eq_capped(&i.eq);
    }
    for (id, m) in &s.mixes {
        assert!(
            in_range(m.out.volume_db, -150.0, 12.0),
            "{id} volume {}",
            m.out.volume_db
        );
        assert!(
            in_range(m.out.limiter.limit_db, -6.0, 0.0),
            "{id} limit {}",
            m.out.limiter.limit_db
        );
        assert_eq_capped(&m.out.eq);
        for (src, l) in &m.inputs {
            assert_level_capped(&format!("{id} {src}"), l);
        }
        for (src, l) in &m.mixes {
            assert_level_capped(&format!("{id} {src}"), l);
        }
        for (g, strip) in &m.groups {
            assert!(
                in_range(strip.gain_db, -150.0, 12.0),
                "{id} {g} {}",
                strip.gain_db
            );
            assert_eq_capped(&strip.eq);
        }
    }
    let t = core.transient();
    if let Some(ts) = t.test_signal {
        assert!(in_range(ts.dbfs, -120.0, -20.0) && in_range(ts.ttl_s, 0.001, 120.0));
        assert!(in_range(ts.hz, 20.0, 20_000.0));
    }
}

fn assert_eq_capped(e: &Eq) {
    assert!(in_range(e.gain_db, -150.0, 12.05));
    for b in &e.bands {
        assert!(in_range(b.freq_hz, 20.0, 24_000.0), "{}", b.freq_hz);
        assert!(in_range(b.gain_db, -150.0, 12.05));
        assert!(in_range(b.bw_oct, 0.01, 4.0));
    }
}

#[test]
fn random_requests_never_panic_and_keep_state_in_caps() {
    let g = common::topology();
    let mut rng = Rng(env("IEM_FUZZ_SEED", 0x5eed_0003) | 1);
    let iters = env("IEM_FUZZ_ITERS", 2_000);
    let flags = Flags {
        test_signal: true,
        fault_injection: true,
    };
    let mut core = Core::new(Arc::clone(&g), &common::open_state(&g), 0, flags);
    let mut applied = 0;
    for k in 0..iters {
        let cmd = command(&mut rng, &g, 0);
        let before = core.rev();
        let direct = core.apply(&cmd);
        if let Ok(out) = &direct {
            applied += 1;
            assert!(out.rev == before || out.rev == before + 1);
            if !matches!(cmd, Cmd::ImportState { .. }) {
                assert!(out.rt.len() <= MAX_CMDS_PER_BLOCK, "{}", out.rt.len());
            }
        } else {
            assert_eq!(core.rev(), before, "a refused request changes nothing");
        }
        // The same request through the wire (JSON cannot carry NaN or ∞).
        let req = ClientMsg::Request {
            id: k,
            origin: None,
            cmd,
        };
        let json = serde_json::to_vec(&req).unwrap_or_default();
        let wire = if rng.chance(0.5) {
            mutate(&mut rng, json)
        } else {
            json
        };
        if let Ok(ClientMsg::Request { cmd, .. }) = parse_client(&wire) {
            let _ = core.apply(&cmd);
        }
        if rng.chance(0.2) {
            let noise: Vec<u8> = (0..rng.below(64)).map(|_| rng.bits() as u8).collect();
            assert!(parse_client(&noise).is_err() || noise.starts_with(b"{"));
        }
        if k % 50 == 0 {
            assert_capped(&core);
        }
    }
    assert_capped(&core);
    assert!(applied > iters / 10, "{applied} of {iters} applied");
    println!(
        "props: {iters} requests, {applied} applied directly, final rev {}",
        core.rev()
    );
}

#[test]
fn random_command_groups_render_finite_bounded_output() {
    let g = common::topology();
    let mut rng = Rng(env("IEM_FUZZ_SEED", 0x5eed_0004) | 1);
    let blocks = env("IEM_FUZZ_ITERS", 2_000) / 10;
    let flags = Flags {
        test_signal: true,
        fault_injection: false,
    };
    let state = common::open_state(&g);
    let mut core = Core::new(Arc::clone(&g), &state, 0, flags);
    let (mut p, mut h) = Processor::new(Arc::clone(&g), &state, &[], Options { fade_in_ms: 0.0 });
    let mut input = Planar::new(g.rx.len(), 97);
    for k in 0..blocks {
        for _ in 0..rng.below(3) {
            if let Ok(out) = core.apply(&command(&mut rng, &g, 0)) {
                for chunk in out.rt.chunks(MAX_CMDS_PER_BLOCK) {
                    push_group(&mut h.cmds, 0, chunk);
                }
            }
        }
        for ch in 0..g.rx.len() {
            for x in input.channel_mut(ch) {
                *x = if rng.chance(0.001) {
                    WEIRD[rng.below(WEIRD.len())]
                } else {
                    (rng.unit() - 0.5) * 4.0
                };
            }
        }
        let run = Offline { block: 32 }.run(&mut p, &input, g.tx.len());
        assert!(run.fault.is_none(), "block {k}");
        for ch in 0..g.tx.len() {
            assert!(
                run.output
                    .channel(ch)
                    .iter()
                    .all(|y| y.is_finite() && y.abs() <= 1.0),
                "block {k} tx {ch}"
            );
        }
        let _ = h.taps[0].pop_partial_slice(&mut [0.0f32; 4096]);
        let _ = h.taps[1].pop_partial_slice(&mut [0.0f32; 4096]);
    }
}
