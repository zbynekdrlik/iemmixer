use super::*;

/// Ten years of a grandmaster's uptime at 96 kHz, as a position.
const T: u64 = 10 * 365 * 86_400 * 96_000;
const RATE: u32 = 96_000;

fn fill(src: &mut Source, stamp: u64, n: usize, rings: &[Ring]) -> (Vec<i32>, Fill) {
    let mut out = vec![0x5555_5500; n];
    let f = src.fill(stamp, RATE, &mut out, rings, None);
    (out, f)
}

/// The largest sample's level, dBFS (full scale 2³¹).
fn peak_dbfs(v: &[i32]) -> f64 {
    let peak = v.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
    20.0 * (f64::from(peak) / 2f64.powi(31)).log10()
}

#[test]
fn silence_is_zero() {
    let (out, f) = fill(&mut Source::Silence, T, 64, &[]);
    assert_eq!((out, f), (vec![0; 64], Fill { holes: 0 }));
    assert_eq!(Source::default(), Source::Silence);
}

#[test]
fn the_counter_names_its_own_stamp() {
    // Across the counter's own wrap at 65 536.
    let stamp = 65_536 * 1_000_003 - 3;
    let (out, f) = fill(&mut Source::Counter, stamp, 8, &[]);
    assert_eq!(
        out,
        [
            0xff_fd00, 0xff_fe00, 0xff_ff00, 0, 0x100, 0x200, 0x300, 0x400
        ]
    );
    assert_eq!(f, Fill { holes: 0 });
    let (out, _) = fill(&mut Source::Counter, T, 1_000, &[]);
    for (i, v) in out.iter().enumerate() {
        assert_eq!(v & 0xff, 0, "the low byte is clear");
        assert_eq!((v >> 8) as u64, (T + i as u64) % 65_536);
    }
    // About -42 dBFS at its top.
    let (out, _) = fill(&mut Source::Counter, 0, 65_536, &[]);
    let level = peak_dbfs(&out);
    assert!((level - -42.14).abs() < 0.01, "{level}");
}

#[test]
fn the_sine_never_exceeds_minus_20_dbfs() {
    let cap = (f64::from(i32::MAX) * 0.1).ceil() as u32;
    for dbfs in [
        0.0,
        10.0,
        -20.0,
        -19.99,
        -6.0,
        f64::NAN,
        f64::INFINITY,
        1e300,
    ] {
        for hz in [
            1_000.0,
            997.0,
            20.0,
            47_999.0,
            48_000.0,
            96_000.0,
            1e12,
            -5.0,
            f64::NAN,
        ] {
            let mut src = Source::Sine { hz, dbfs };
            let (out, f) = fill(&mut src, T, 4_096, &[]);
            let peak = out.iter().map(|s| s.unsigned_abs()).max().unwrap();
            assert!(peak <= cap, "{hz} Hz at {dbfs} dBFS: {peak}");
            assert_eq!(f, Fill { holes: 0 });
        }
    }
    // Asked for -20, it is -20; for -40, -40 (over whole periods).
    for (dbfs, want) in [(-20.0, -20.0), (-40.0, -40.0), (0.0, -20.0)] {
        let mut src = Source::Sine { hz: 1_000.0, dbfs };
        let (out, _) = fill(&mut src, T, 9_600, &[]);
        let level = peak_dbfs(&out);
        assert!((level - want).abs() < 0.001, "{dbfs}: {level}");
    }
    // Silent below everything, or without a frequency.
    for (hz, dbfs) in [
        (1_000.0, f64::NEG_INFINITY),
        (0.0, -20.0),
        (f64::NAN, -20.0),
    ] {
        let (out, _) = fill(&mut Source::Sine { hz, dbfs }, T, 256, &[]);
        assert!(out.iter().all(|&v| v == 0), "{hz} {dbfs}");
    }
}

#[test]
fn the_sine_is_a_function_of_the_stamp() {
    let mut src = Source::Sine {
        hz: 997.0,
        dbfs: -20.0,
    };
    // One second at 997 Hz: 1 994 zero crossings.
    let (out, _) = fill(&mut src, T, RATE as usize, &[]);
    let crossings = out.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
    assert!((1_993..=1_995).contains(&crossings), "{crossings}");
    // Packet by packet, the same samples as in one go.
    let (whole, _) = fill(&mut src, T, 96, &[]);
    let (a, _) = fill(&mut src, T, 32, &[]);
    let (b, _) = fill(&mut src, T + 32, 64, &[]);
    assert_eq!(whole, [a, b].concat());
    // And it starts where the position puts it, not at the call: a second
    // later (a whole number of 997 Hz periods) it repeats.
    let (later, _) = fill(&mut src, T + u64::from(RATE), 96, &[]);
    for (x, y) in whole.iter().zip(&later) {
        assert!((x - y).abs() <= 1, "{x} {y}");
    }
    // At position 0 its phase is 0.
    let (start, _) = fill(&mut src, 0, 2, &[]);
    assert_eq!(start[0], 0);
    assert!(start[1] > 0);
}

#[test]
fn echo_is_the_rx_sample_offset_earlier() {
    let rings = [Ring::new(), Ring::new()];
    let offset = 229;
    let sent: Vec<i32> = (0..64).map(|i| (i + 1) << 8).collect();
    rings[1].write_at(T - offset, &sent);
    let mut src = Source::Echo {
        rx: 1,
        offset: offset as u32,
    };
    let (out, f) = fill(&mut src, T, 64, &rings);
    assert_eq!((out, f), (sent.clone(), Fill { holes: 0 }));
    // Another stamp, the same rule.
    let (out, _) = fill(&mut src, T + 10, 4, &rings);
    assert_eq!(out, sent[10..14]);
    // The other ring holds none of it.
    let mut other = Source::Echo {
        rx: 0,
        offset: offset as u32,
    };
    assert_eq!(fill(&mut other, T, 4, &rings).1, Fill { holes: 4 });
}

#[test]
fn echo_of_a_hole_is_silence_and_counted() {
    let rings = [Ring::new()];
    rings[0].write_at(T - 100, &[5 << 8; 16]);
    let mut src = Source::Echo { rx: 0, offset: 100 };
    // Half of it held, half a hole.
    let (out, f) = fill(&mut src, T + 8, 16, &rings);
    assert_eq!(&out[..8], &[5 << 8; 8]);
    assert_eq!(&out[8..], &[0; 8]);
    assert_eq!(f, Fill { holes: 8 });
    // An RX channel that does not exist: all silence, all holes.
    let mut none = Source::Echo { rx: 1, offset: 0 };
    let (out, f) = fill(&mut none, T, 16, &rings);
    assert_eq!((out, f), (vec![0; 16], Fill { holes: 16 }));
    // An offset past the start of time wraps, a hole like any other.
    let mut early = Source::Echo {
        rx: 0,
        offset: u32::MAX,
    };
    assert_eq!(
        fill(&mut early, 5, 4, &rings),
        (vec![0; 4], Fill { holes: 4 })
    );
}

#[test]
fn the_engine_source_is_its_own_ring_at_the_stamp_a_hole_silence() {
    let own = Ring::new();
    let block: Vec<i32> = (0..32).map(|i| (i + 1) << 8).collect();
    own.put_at(T, &block);
    // RX rings are not read: RX 1 holds other samples at the same stamps.
    let rings = [Ring::new()];
    rings[0].write_at(T, &[7 << 8; 64]);
    let mut src = Source::Engine;
    let mut out = vec![0x5555_5500; 64];
    let f = src.fill(T, RATE, &mut out, &rings, Some(&own));
    assert_eq!(&out[..32], &block[..]);
    assert_eq!(&out[32..], &[0; 32], "not written: silence");
    assert_eq!(f, Fill { holes: 32 });
    // Another stamp, the same rule.
    let f = src.fill(T + 30, RATE, &mut out[..4], &rings, Some(&own));
    assert_eq!(&out[..4], &[31 << 8, 32 << 8, 0, 0]);
    assert_eq!(f, Fill { holes: 2 });
    // No ring of its own: all silence, all holes.
    let (out, f) = fill(&mut Source::Engine, T, 16, &rings);
    assert_eq!((out, f), (vec![0; 16], Fill { holes: 16 }));
    // The other sources never read it.
    let mut out = vec![0x5555_5500; 4];
    Source::Silence.fill(T, RATE, &mut out, &rings, Some(&own));
    assert_eq!(out, [0; 4]);
}
