//! The worst-case scenario driven through the Dante backend's pump (Gen 3
//! phase C, plan Task 3; Linux): the real `Processor` of `scenario()` inside
//! `iem_dante::stream::Pump`, its inputs read from the device's RX rings
//! (hot topology inputs; the D5(b) loopback returns fed one block later from
//! the spare outputs' engine rings, so the round-trip probe measures), its
//! outputs quantized into the device's engine rings, on the channels
//! `iem_engine::dante::check` maps for a device with every channel of the
//! test site and HIL's spare pair. Every buffer, ring and socket is built
//! before the blocks run. `rt.rs` runs it under `assert_no_alloc`, `rtsan.rs`
//! under RealtimeSanitizer (`Pump::block` carries the real-time attribute),
//! each at both of the device's blocks (`iem_dante::stream::BLOCKS`: 32 and
//! the box's 16, #72).

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use iem_dante::device::DeviceConfig;
use iem_dante::media::ring::Ring;
use iem_dante::media::rx::{RxPlan, RxRoute, RxShared};
use iem_dante::media::source::Source;
use iem_dante::media::tx::TxShared;
use iem_dante::stream::{Map, Pump, PumpStats, quantize};
use iem_engine::cmd::push_group;
use iem_engine::dante::{check, timing};
use iem_engine::rt::{Processor, RtHandles};

use super::{BLOCK, HIL, Scenario, scenario};

/// Ten years of a grandmaster's uptime at 96 kHz, a multiple of either
/// block: the first block's first input stamp.
pub const T: u64 = 10 * 365 * 86_400 * 96_000;
const RATE: u32 = 96_000;

pub struct PumpRig {
    pub pump: Pump<Processor>,
    pub stats: Arc<PumpStats>,
    pub rx: Arc<RxShared>,
    pub tx: Arc<TxShared>,
    pub map: Map,
    /// B: the device's `block`, the pump's frames a block.
    pub block: usize,
    /// D: an output stamped t is the input stamped t − D.
    pub offset: u64,
    pub groups: Vec<Vec<iem_engine::cmd::RtOp>>,
    pub handles: RtHandles,
    /// The largest D5(b) loopback round trip the processor reported.
    pub measured: u64,
    talk: Vec<f32>,
    drain: Vec<f32>,
    /// One channel's block of samples (the loopback's copy).
    samples: Vec<i32>,
}

/// The test device with exactly the test site's channels and HIL's spare
/// pair: 132 RX, 95 TX.
fn device() -> DeviceConfig {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/test-dante-device.toml"),
    )
    .unwrap();
    let mut cfg = DeviceConfig::from_toml(&text).unwrap();
    cfg.rx = (1..=132).map(|n| format!("RX {n}")).collect();
    cfg.tx = (1..=95).map(|n| format!("TX {n}")).collect();
    cfg
}

/// The scenario's processor in a pump on the device's rings at the
/// device's `block` (16 or 32), hot input written ahead for `blocks`
/// blocks of the scenario's [`BLOCK`] (the run's samples, [`blocks_of`]).
pub fn pump_rig(blocks: usize, block: usize) -> PumpRig {
    let Scenario {
        topo,
        groups,
        processor,
        handles,
        ..
    } = scenario();
    let mut cfg = device();
    cfg.block = block as u64;
    let map = check(&cfg, &topo, &HIL).unwrap();
    assert_eq!(map.rx.len(), processor.input_channels());
    assert_eq!(map.tx.len(), processor.outputs());
    let rings: Arc<[Ring]> = (0..cfg.rx.len())
        .map(|_| Ring::new())
        .collect::<Vec<_>>()
        .into();
    // Hot topology inputs (0.3) for every block; the returns stay empty
    // until the loopback writes them.
    let hot = vec![quantize(0.3, 24); blocks * BLOCK + block];
    for &rx in map.rx.iter().take(topo.rx.len()) {
        rings[rx].write_at(T, &hot);
    }
    let rx = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::clone(&rings)).unwrap());
    // Every RX channel routed (to some slot of flow 0): the pump reads them.
    rx.publish(
        |_| RxPlan::default(),
        |c| {
            Some(RxRoute {
                flow: 0,
                slot: c % 8,
            })
        },
    );
    let tx = Arc::new(TxShared::new(cfg.clone(), Ipv4Addr::LOCALHOST, rings, 1, &map.tx).unwrap());
    for &c in &map.tx {
        assert!(tx.set_source(c, Source::Engine));
    }
    let timing = timing(&cfg);
    assert_eq!(
        timing.block, block as u64,
        "the pump runs the device's block"
    );
    let stats = Arc::new(PumpStats::new(timing, RATE));
    let pump = Pump::new(
        processor,
        map.clone(),
        timing,
        cfg.encoding,
        Arc::clone(&rx),
        Arc::clone(&tx),
        Arc::clone(&stats),
    );
    PumpRig {
        pump,
        stats,
        rx,
        tx,
        map,
        block,
        offset: timing.offset(),
        groups,
        handles,
        measured: 0,
        talk: vec![0.25; block],
        drain: vec![0.0; 8192],
        samples: vec![0; block],
    }
}

/// The pump blocks of the scenario's run at the rig's block: as many
/// samples as `blocks` blocks of the scenario's [`BLOCK`] (300 of 32: 9600).
pub fn blocks_of(r: &PumpRig, blocks: usize) -> usize {
    blocks * BLOCK / r.block
}

/// Runs `blocks` pump blocks from stamp [`T`] as the media clock would,
/// with the scenario's command groups, talkback, taps and meters, and the
/// one-block D5(b) loopback through the rings; allocates nothing itself.
/// One command group every [`BLOCK`] samples, whatever the rig's block, so
/// the scenario keeps its timeline in samples: at one group a block,
/// block 16 would halve the HIL test signal's run between its start and
/// its stop, and its fades (the sine's 50 ms, the engine's 500 ms fade-in)
/// would never reach the loopback probe's onset.
pub fn pump_drive(r: &mut PumpRig, blocks: usize) {
    let returns = r.map.rx.len() - HIL.len();
    let spare = r.map.tx.len() - HIL.len();
    let every = BLOCK / r.block;
    for k in 0..blocks {
        if k % every == 0
            && let Some(g) = r.groups.get(k / every % r.groups.len())
        {
            push_group(&mut r.handles.cmds, 0, g);
        }
        let _ = r.handles.talkback.push_partial_slice(&r.talk);
        let start = T + (k * r.block) as u64;
        r.pump.block(start);
        // Each spare output's block is its return input in the next one.
        for j in 0..HIL.len() {
            let out = r.tx.engine_ring(r.map.tx[spare + j]).unwrap();
            out.read_at(start + r.offset, &mut r.samples);
            r.rx.rings()[r.map.rx[returns + j]].write_at(start + r.block as u64, &r.samples);
        }
        let rt = r.handles.status.loopback_samples.load(Ordering::Relaxed);
        r.measured = r.measured.max(rt);
        for tap in &mut r.handles.taps {
            let _ = tap.pop_partial_slice(&mut r.drain);
        }
        if r.handles.meters.updated() {
            let _ = r.handles.meters.read();
        }
    }
}

/// The largest magnitude the engine wrote to its TX rings in the last
/// block of `blocks`.
pub fn loudest_output(r: &mut PumpRig, blocks: usize) -> i64 {
    let at = T + ((blocks - 1) * r.block) as u64 + r.offset;
    let mut loudest = 0i64;
    for &c in &r.map.tx {
        r.tx.engine_ring(c).unwrap().read_at(at, &mut r.samples);
        loudest = r
            .samples
            .iter()
            .fold(loudest, |m, &s| m.max(i64::from(s).abs()));
    }
    loudest
}
