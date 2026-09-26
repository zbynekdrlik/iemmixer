//! A worst-case processor workload shared by the RT-safety tests (`rt.rs`
//! under `assert_no_alloc`, `rtsan.rs` under RealtimeSanitizer): the program
//! site with every level open, hot inputs driving the limiters, a command
//! group every block (volume, input/mix/group EQ, processing, levels of an
//! input and a heard mix, a group strip, solo, listen, limiter raise and
//! lower, test signal, a full import), talkback, both taps, meter reads and a
//! sanitiser trip every 1000 blocks.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use iem_audio_io::{Block, Process};
use iem_engine::cmd::{RtOp, push_group};
use iem_engine::core::{Core, Flags};
use iem_engine::rt::{Options, Processor, RtHandles};
use iem_engine::site::load;
use iem_engine::topology::{Topology, compile};
use iem_engine_proto::{
    Cmd, Eq, EqTarget, GroupId, InputId, Level, Mix, MixGroup, MixId, MixState, Source,
};

pub const BLOCK: usize = 32;

pub fn site_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/test-site.toml")
}

pub fn topology() -> Arc<Topology> {
    Arc::new(compile(&load(&site_path()).unwrap()).unwrap())
}

/// Every level at −12 dB, every group strip at 0 dB.
pub fn open_state(topo: &Topology) -> MixState {
    let open = Level {
        gain_db: -12.0,
        ..Level::default()
    };
    MixState {
        mixes: topo
            .mixes
            .iter()
            .map(|m| {
                let mix = Mix {
                    inputs: topo.inputs.iter().map(|i| (i.id.clone(), open)).collect(),
                    groups: topo
                        .groups
                        .iter()
                        .map(|g| (g.id.clone(), MixGroup::default()))
                        .collect(),
                    mixes: m
                        .mixes
                        .iter()
                        .map(|&s| (topo.mixes[s].id.clone(), open))
                        .collect(),
                    ..Mix::default()
                };
                (m.id.clone(), mix)
            })
            .collect(),
        ..MixState::default()
    }
}

pub struct Scenario {
    pub topo: Arc<Topology>,
    pub groups: Vec<Vec<RtOp>>,
    pub processor: Processor,
    pub handles: RtHandles,
}

fn mix(s: &str) -> MixId {
    MixId::new(s)
}

fn input(s: &str) -> InputId {
    InputId::new(s)
}

fn processing(i: &str, on: bool) -> Cmd {
    Cmd::SetInput {
        input: input(i),
        trim_db: None,
        muted: None,
        processing: Some(on),
    }
}

fn level(m: &str, source: Source, gain_db: f64, pan: f64) -> Cmd {
    Cmd::SetLevel {
        mix: mix(m),
        source,
        gain_db: Some(gain_db),
        pan: Some(pan),
        muted: None,
    }
}

pub fn scenario() -> Scenario {
    let topo = topology();
    let state = open_state(&topo);
    let flags = Flags {
        test_signal: true,
        fault_injection: false,
    };
    let mut core = Core::new(Arc::clone(&topo), &state, 0, flags);
    let mut eq = Eq::default();
    for b in &mut eq.bands {
        b.enabled = true;
        b.gain_db = 6.0;
    }
    let stems = EqTarget::Group {
        mix: mix("member2"),
        group: GroupId::new("stems"),
    };
    let heard = Source::Mix(mix("member2"));
    let cmds = vec![
        Cmd::SetMix {
            mix: mix("member1"),
            volume_db: Some(-3.0),
            muted: None,
        },
        Cmd::SetEq {
            target: EqTarget::Input(input("mic1")),
            eq,
        },
        Cmd::SetEq {
            target: EqTarget::Mix(mix("member2")),
            eq,
        },
        Cmd::SetEq {
            target: stems.clone(),
            eq,
        },
        processing("mic3", false),
        Cmd::SetSolo {
            mix: mix("member3"),
            sources: vec![Source::Input(input("mic2"))],
        },
        Cmd::StartListen {
            mix: mix("engineer"),
        },
        Cmd::StartListen {
            mix: mix("member4"),
        },
        Cmd::SetLimiter {
            mix: mix("member2"),
            enabled: None,
            limit_db: Some(0.0),
        },
        Cmd::StartTestSignal {
            input: input("mic5"),
            hz: 1000.0,
            dbfs: -30.0,
            ttl_s: 0.01,
        },
        level("member1", Source::Input(input("mic1")), -6.0, 0.3),
        level("member1", heard.clone(), -6.0, -0.3),
        Cmd::SetGroup {
            mix: mix("member1"),
            group: GroupId::new("stems"),
            gain_db: Some(-6.0),
            muted: None,
        },
        Cmd::ResetLimiterStats {
            mix: mix("member1"),
        },
        Cmd::SetMix {
            mix: mix("engineer"),
            volume_db: None,
            muted: Some(true),
        },
        processing("mic3", true),
        Cmd::SetSolo {
            mix: mix("member3"),
            sources: vec![],
        },
        Cmd::StopListen {
            mix: mix("member4"),
        },
        Cmd::SetLimiter {
            mix: mix("member2"),
            enabled: Some(false),
            limit_db: Some(-6.0),
        },
        Cmd::SetEq {
            target: EqTarget::Input(input("mic1")),
            eq: Eq::default(),
        },
        Cmd::SetEq {
            target: stems,
            eq: Eq::default(),
        },
        level("member1", Source::Input(input("mic1")), -12.0, 0.0),
        level("member1", heard, -12.0, 0.0),
        Cmd::SetGroup {
            mix: mix("member1"),
            group: GroupId::new("stems"),
            gain_db: Some(0.0),
            muted: None,
        },
        Cmd::SetMix {
            mix: mix("engineer"),
            volume_db: None,
            muted: Some(false),
        },
        Cmd::StopTestSignal,
        Cmd::ImportState {
            state: open_state(&topo),
            baseline: false,
        },
    ];
    let groups = cmds
        .iter()
        .map(|c| core.apply(c).unwrap().rt)
        .filter(|rt| !rt.is_empty())
        .collect();
    let (processor, handles) = Processor::new(Arc::clone(&topo), &state, &[], Options::default());
    Scenario {
        topo,
        groups,
        processor,
        handles,
    }
}

pub struct Buffers {
    pub input: Vec<f64>,
    pub bad: Vec<f64>,
    pub output: Vec<f64>,
    pub talk: Vec<f32>,
    pub drain: Vec<f32>,
}

pub fn buffers(topo: &Topology) -> Buffers {
    let mut bad = vec![0.3; topo.rx.len() * BLOCK];
    bad[7] = f64::NAN;
    Buffers {
        input: vec![0.3; topo.rx.len() * BLOCK],
        bad,
        output: vec![0.0; topo.tx.len() * BLOCK],
        talk: vec![0.25; BLOCK],
        drain: vec![0.0; 8192],
    }
}

/// Runs `blocks` callbacks; allocates nothing itself.
pub fn drive(s: &mut Scenario, b: &mut Buffers, blocks: usize) {
    for k in 0..blocks {
        if let Some(g) = s.groups.get(k % s.groups.len()) {
            push_group(&mut s.handles.cmds, 0, g);
        }
        let _ = s.handles.talkback.push_partial_slice(&b.talk);
        let src = if k % 1000 == 999 { &b.bad } else { &b.input };
        let mut block = Block::new(BLOCK, src, &mut b.output);
        s.processor.process(&mut block);
        for tap in &mut s.handles.taps {
            let _ = tap.pop_partial_slice(&mut b.drain);
        }
        if s.handles.meters.updated() {
            let _ = s.handles.meters.read();
        }
    }
}
