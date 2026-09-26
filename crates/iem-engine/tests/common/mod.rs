//! A worst-case processor workload shared by the RT-safety tests (`rt.rs`
//! under `assert_no_alloc`, `rtsan.rs` under RealtimeSanitizer): the program
//! site with every send open, hot inputs driving the limiters, a command
//! group every block (fader, EQ, processing, solo, listen, limiter raise and
//! lower, test signal, a full import), talkback, both taps, meter reads and a
//! sanitiser trip every 1000 blocks.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use iem_audio_io::{Block, Process};
use iem_engine::cmd::{RtOp, push_group};
use iem_engine::core::{Core, Flags};
use iem_engine::graph::{Graph, compile};
use iem_engine::rt::{Options, Processor, RtHandles};
use iem_engine::site::load;
use iem_engine_proto::{
    BusId, Cmd, Eq, EqOwner, InputId, MixState, SendEntry, SendId, SendState, Source,
};

pub const BLOCK: usize = 32;

pub fn site_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/test-site.toml")
}

pub fn graph() -> Arc<Graph> {
    Arc::new(compile(&load(&site_path()).unwrap()).unwrap())
}

/// Every send at −12 dB.
pub fn open_state(graph: &Graph) -> MixState {
    MixState {
        sends: graph
            .sends
            .iter()
            .map(|e| SendEntry {
                id: e.id.clone(),
                state: SendState {
                    gain_db: -12.0,
                    ..SendState::default()
                },
            })
            .collect(),
        ..MixState::default()
    }
}

pub struct Scenario {
    pub graph: Arc<Graph>,
    pub groups: Vec<Vec<RtOp>>,
    pub processor: Processor,
    pub handles: RtHandles,
}

fn bus(s: &str) -> BusId {
    BusId::new(s)
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
        fader_db: None,
        pan: None,
    }
}

pub fn scenario() -> Scenario {
    let graph = graph();
    let state = open_state(&graph);
    let flags = Flags {
        test_signal: true,
        fault_injection: false,
    };
    let mut core = Core::new(Arc::clone(&graph), &state, 0, flags);
    let mut eq = Eq::default();
    for b in &mut eq.bands {
        b.enabled = true;
        b.gain_db = 6.0;
    }
    let send = SendId {
        src: Source::Input(input("mic1")),
        dst: bus("member1"),
    };
    let cmds = vec![
        Cmd::SetBus {
            bus: bus("member1"),
            fader_db: Some(-3.0),
            pan: Some(0.4),
            muted: None,
        },
        Cmd::SetEq {
            owner: EqOwner::Input(input("mic1")),
            eq,
        },
        Cmd::SetEq {
            owner: EqOwner::Bus(bus("member2")),
            eq,
        },
        processing("mic3", false),
        Cmd::SetSolo {
            scope: bus("member3"),
            sources: vec![Source::Input(input("mic2"))],
        },
        Cmd::StartListen {
            bus: bus("engineer"),
        },
        Cmd::StartListen {
            bus: bus("member4"),
        },
        Cmd::SetLimiter {
            bus: bus("member2"),
            enabled: None,
            limit_db: Some(0.0),
        },
        Cmd::StartTestSignal {
            input: input("mic5"),
            hz: 1000.0,
            dbfs: -30.0,
            ttl_s: 0.01,
        },
        Cmd::SetSend {
            id: send.clone(),
            gain_db: Some(-6.0),
            pan: Some(0.3),
            muted: None,
        },
        Cmd::ResetLimiterStats {
            bus: bus("member1"),
        },
        Cmd::SetBus {
            bus: bus("engineer"),
            fader_db: None,
            pan: None,
            muted: Some(true),
        },
        processing("mic3", true),
        Cmd::SetSolo {
            scope: bus("member3"),
            sources: vec![],
        },
        Cmd::StopListen {
            bus: bus("member4"),
        },
        Cmd::SetLimiter {
            bus: bus("member2"),
            enabled: Some(false),
            limit_db: Some(-6.0),
        },
        Cmd::SetEq {
            owner: EqOwner::Input(input("mic1")),
            eq: Eq::default(),
        },
        Cmd::SetSend {
            id: send,
            gain_db: Some(-12.0),
            pan: Some(0.0),
            muted: None,
        },
        Cmd::SetBus {
            bus: bus("engineer"),
            fader_db: None,
            pan: None,
            muted: Some(false),
        },
        Cmd::StopTestSignal,
        Cmd::ImportState {
            state: open_state(&graph),
            baseline: false,
        },
    ];
    let groups = cmds
        .iter()
        .map(|c| core.apply(c).unwrap().rt)
        .filter(|rt| !rt.is_empty())
        .collect();
    let (processor, handles) = Processor::new(Arc::clone(&graph), &state, &[], Options::default());
    Scenario {
        graph,
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

pub fn buffers(graph: &Graph) -> Buffers {
    let mut bad = vec![0.3; graph.rx.len() * BLOCK];
    bad[7] = f64::NAN;
    Buffers {
        input: vec![0.3; graph.rx.len() * BLOCK],
        bad,
        output: vec![0.0; graph.tx.len() * BLOCK],
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
