//! The pipeline's tests that `rt_tests.rs` does not hold (#36): the mix's
//! own sanitiser count, which the move to this module made new to the
//! diff-scoped mutation gate and which no other test reached (an input and
//! a group strip trip there, a mix behind its limiter never does).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use iem_audio_io::{Offline, Planar};
use iem_engine_proto::{Cmd, InputId, MixId, MixState, Source};

use crate::core::{Core, Flags};
use crate::rt::{Options, Processor};
use crate::site::parse;
use crate::topology::compile;

/// RX: mono 0. TX: m1 0/1, eng 2/3; eng hears nothing.
const SITE: &str = r#"
[engine]
channels = 4
engineer = "eng"
[[engine.inputs]]
id = "mono"
rx = [1]
[[engine.mixes]]
id = "m1"
tx = [1, 2]
[[engine.mixes]]
id = "eng"
tx = [3, 4]
"#;

#[test]
fn a_tripping_mix_counts_in_the_status_and_the_meters() {
    let topo = Arc::new(compile(&parse(SITE).unwrap()).unwrap());
    let mut core = Core::new(Arc::clone(&topo), &MixState::default(), 0, Flags::default());
    let m1 = MixId::new("m1");
    // 9e5 passes the input (X1 at 1e6); +12 dB at the level and at the
    // volume, with the limiter off, trips the mix once per block.
    for cmd in [
        Cmd::SetLevel {
            mix: m1.clone(),
            source: Source::Input(InputId::new("mono")),
            gain_db: Some(12.0),
            pan: None,
            muted: None,
        },
        Cmd::SetLimiter {
            mix: m1.clone(),
            enabled: Some(false),
            limit_db: None,
        },
        Cmd::SetMix {
            mix: m1.clone(),
            volume_db: Some(12.0),
            muted: None,
        },
    ] {
        core.apply(&cmd).unwrap();
    }
    let opts = Options {
        fade_in_ms: 0.0,
        hold: false,
    };
    let (mut p, mut h) = Processor::new(Arc::clone(&topo), &core.state(), &[], opts);
    let mut input = Planar::new(topo.rx.len(), 3200);
    input.channel_mut(0).fill(9e5);
    let outs = p.outputs();
    let run = Offline { block: 32 }.run(&mut p, &input, outs);
    assert!(run.fault.is_none(), "{:?}", run.fault);
    assert_eq!(h.status.trips.load(Ordering::Relaxed), 100);
    let f = h.meters.read().clone();
    assert_eq!((f.seq, f.trips), (1, 100));
    let m = topo.mix_index(&m1).unwrap();
    assert_eq!(f.mixes[m], [0.0, 0.0], "a tripping mix is silenced");
    assert!(
        run.output.channel(0).iter().all(|y| *y == 0.0),
        "and so is its TX"
    );
}
