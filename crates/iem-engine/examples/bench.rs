//! CPU benchmark of `process()` at B = 32, 96 kHz (period 333.3 µs; program
//! spec I2, §3.5; design note §3.8). Two cases on `config/test-site.toml`:
//!
//! - `typical`: every send open, sine inputs, EQs flat, the site at rest;
//! - `worst`: all 220 EQ bands enabled and moving, every send ramping, the
//!   limiters in gain reduction, both listen taps, talkback, a test signal and
//!   a 512-command group every block.
//!
//! Prints p50/p99/p99.9/max per case. Exits 1 when the typical median
//! exceeds 25 % of the period or the worst-case median exceeds the period;
//! hosted runners preempt, so tails are reported, not gated (the §3.5 p99.9
//! gate is measured on the PC in S7).
//!
//! `cargo run --release -p iem-engine --example bench`

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use iem_audio_io::{Block, Process};
use iem_dsp::eq::{BandKind, EqParams};
use iem_engine::cmd::{RtOp, push_group};
use iem_engine::graph::{Graph, compile};
use iem_engine::params::InputParams;
use iem_engine::rt::{Options, Processor};
use iem_engine::site::load;
use iem_engine::{MAX_CMDS_PER_BLOCK, SAMPLE_RATE};
use iem_engine_proto::{MixState, SendEntry, SendState};

const B: usize = 32;
const WARMUP: usize = 3_000;
const CALLS: usize = 30_000;

fn graph() -> Arc<Graph> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/test-site.toml");
    Arc::new(compile(&load(&path).expect("site")).expect("graph"))
}

fn open(graph: &Graph) -> MixState {
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

fn eq(shift: f64) -> EqParams {
    let mut p = EqParams::standard_flat();
    for (k, b) in p.bands.iter_mut().enumerate() {
        b.enabled = true;
        b.gain_lin = if k == 0 { 1.0 } else { 1.5 + shift };
        b.freq_hz *= 1.0 + shift;
        if b.kind == BandKind::Peak {
            b.bw_oct = 1.0 + shift;
        }
    }
    p
}

/// Two alternating groups of 512 commands that keep every ramp moving.
fn worst_groups(graph: &Graph) -> [Vec<RtOp>; 2] {
    [0.0, 0.25].map(|shift| {
        let mut ops = Vec::new();
        for s in 0..graph.sends.len() {
            ops.push(RtOp::Send {
                s: s as u16,
                gain: 0.25 + shift,
                pan: shift - 0.1,
                muted: false,
            });
        }
        for i in 0..graph.inputs.len() {
            ops.push(RtOp::InputEq {
                i: i as u16,
                eq: eq(shift),
            });
            ops.push(RtOp::Input {
                i: i as u16,
                p: InputParams {
                    trim: 1.0 + shift,
                    muted: false,
                    processing: true,
                    fader: 0.5 + shift,
                    pan: shift,
                },
            });
        }
        for b in 0..graph.buses.len() {
            if graph.has_eq(b) {
                ops.push(RtOp::BusEq {
                    b: b as u16,
                    eq: eq(shift),
                });
            }
            ops.push(RtOp::Bus {
                b: b as u16,
                fader: 1.0 + shift,
                pan: -shift,
                muted: false,
            });
            if graph.has_limiter(b) {
                ops.push(RtOp::Limiter {
                    b: b as u16,
                    enabled: true,
                    limit_db: -6.0 + 12.0 * shift,
                });
            }
        }
        // The budget's worst case: exactly 512 commands every block.
        ops.resize(MAX_CMDS_PER_BLOCK, RtOp::Nop);
        ops
    })
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    let i = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

fn bench(name: &str, worst: bool) -> f64 {
    let graph = graph();
    let (mut p, mut h) = Processor::new(Arc::clone(&graph), &open(&graph), &[], Options::default());
    let groups = worst_groups(&graph);
    if worst {
        let engineer = graph.engineer as u16;
        let member = graph
            .bus_index(&iem_engine_proto::BusId::new("member4"))
            .unwrap_or(0) as u16;
        push_group(
            &mut h.cmds,
            0,
            &[
                RtOp::Listen {
                    slot: 0,
                    bus: Some(engineer),
                },
                RtOp::Listen {
                    slot: 1,
                    bus: Some(member),
                },
                RtOp::TestSignal {
                    i: 4,
                    hz: 1000.0,
                    amp: 0.1,
                    ttl: u64::MAX / 2,
                },
            ],
        );
    }
    let amp = if worst { 0.8 } else { 0.1 };
    let mut input = vec![0.0; graph.rx.len() * B];
    let mut output = vec![0.0; graph.tx.len() * B];
    let talk = vec![0.3f32; B];
    let mut drain = vec![0.0f32; 4 * B];
    let mut times = Vec::with_capacity(CALLS);
    let rate = f64::from(SAMPLE_RATE);
    for call in 0..WARMUP + CALLS {
        for (ch, x) in input.chunks_mut(B).enumerate() {
            for (k, v) in x.iter_mut().enumerate() {
                let t = (call * B + k) as f64 / rate;
                *v = amp * (std::f64::consts::TAU * (200.0 + 37.0 * ch as f64) * t).sin();
            }
        }
        if worst {
            push_group(&mut h.cmds, 0, &groups[call % 2]);
            let _ = h.talkback.push_partial_slice(&talk);
        }
        let mut block = Block::new(B, &input, &mut output);
        let t0 = Instant::now();
        p.process(&mut block);
        let dt = t0.elapsed().as_secs_f64() * 1e6;
        if call >= WARMUP {
            times.push(dt);
        }
        for tap in &mut h.taps {
            let _ = tap.pop_partial_slice(&mut drain);
        }
    }
    times.sort_by(f64::total_cmp);
    let period = B as f64 / rate * 1e6;
    let (p50, p99, p999, max) = (
        percentile(&times, 0.5),
        percentile(&times, 0.99),
        percentile(&times, 0.999),
        times[times.len() - 1],
    );
    println!(
        "bench {name}: p50 {p50:.1} µs ({:.1} %), p99 {p99:.1} µs ({:.1} %), p99.9 {p999:.1} µs ({:.1} %), max {max:.1} µs; period {period:.1} µs, B = {B}, {CALLS} calls",
        100.0 * p50 / period,
        100.0 * p99 / period,
        100.0 * p999 / period,
    );
    p50 / period
}

fn main() {
    let typical = bench("typical", false);
    let worst = bench("worst", true);
    if typical > 0.25 {
        eprintln!("bench: the typical median exceeds 25 % of the 32-sample period");
        std::process::exit(1);
    }
    if worst > 1.0 {
        eprintln!("bench: the worst-case median exceeds the 32-sample period");
        std::process::exit(1);
    }
}
