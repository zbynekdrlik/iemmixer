use super::*;
use crate::media::source::Source;

/// Ten years of a grandmaster's uptime at 96 kHz, as a position.
const T: u64 = 10 * 365 * 86_400 * 96_000;
const RATE: u32 = 96_000;

/// The counter as it returns `lag` samples late: position p carries the
/// counter of p − lag.
fn counter(start: u64, n: usize, lag: u64) -> Vec<i32> {
    (0..n as u64)
        .map(|i| (((start + i - lag) % 65_536) << 8) as i32)
        .collect()
}

/// Feeds `samples` (from position `start`) in packets of 32.
fn feed(c: &mut CounterCheck, start: u64, samples: &[i32]) {
    for (i, chunk) in samples.chunks(32).enumerate() {
        c.check(start + 32 * i as u64, chunk);
    }
}

fn counted(stats: CounterStats) -> (u64, u64, u64, u64, u64, u64) {
    (
        stats.mismatch,
        stats.mismatch_frames,
        stats.dropout,
        stats.dropout_frames,
        stats.slips,
        u64::from(stats.no_return),
    )
}

const CLEAN: (u64, u64, u64, u64, u64, u64) = (0, 0, 0, 0, 0, 0);

#[test]
fn a_clean_counter_has_no_event() {
    let mut c = CounterCheck::new(96_000);
    let mut out = vec![0; 96_000];
    Source::Counter.fill(T, RATE, &mut out, &[], None);
    feed(&mut c, T, &out);
    let s = c.stats();
    assert_eq!(counted(s), CLEAN);
    assert_eq!(s.lag, Some(0));
    assert_eq!(
        (s.blind, s.checked),
        (u64::from(CONFIRM), 96_000 - u64::from(CONFIRM))
    );
    // A loop that returns it 229 samples late, across the counter's wrap.
    let mut c = CounterCheck::new(96_000);
    feed(
        &mut c,
        65_536 * 7 - 1_000,
        &counter(65_536 * 7 - 1_000, 4_000, 229),
    );
    assert_eq!(counted(c.stats()), CLEAN);
    assert_eq!(c.stats().lag, Some(229));
    // The counter's own zero is a sample like any other, not a hole.
    let zero_at = 65_536 * 7 + 229;
    let s = c.stats();
    assert!(zero_at < 65_536 * 7 - 1_000 + 4_000 && s.dropout == 0);
}

#[test]
fn one_dropped_frame_is_one_dropout() {
    let mut samples = counter(T, 2_000, 0);
    samples[1_000] = 0;
    let mut c = CounterCheck::new(96_000);
    feed(&mut c, T, &samples);
    assert_eq!(counted(c.stats()), (0, 0, 1, 1, 0, 0));
    assert_eq!(c.stats().checked + c.stats().blind, 2_000);
    // A run of them is one dropout of its length; two runs are two.
    samples[1_001] = 0;
    samples[1_002] = 0;
    samples[1_500] = 0;
    let mut c = CounterCheck::new(96_000);
    feed(&mut c, T, &samples);
    assert_eq!(counted(c.stats()), (0, 0, 2, 4, 0, 0));
}

#[test]
fn a_shift_by_k_is_slip_k() {
    for k in [1i64, 3, 37, -1, -5, -192] {
        let lag = (1_000 + k) as u64;
        let mut samples = counter(T, 1_000, 1_000);
        samples.extend(counter(T + 1_000, 1_000, lag));
        let mut c = CounterCheck::new(96_000);
        feed(&mut c, T, &samples);
        let s = c.stats();
        assert_eq!(counted(s), (0, 0, 0, 0, 1, 0), "{k}");
        assert_eq!(s.last_slip, k as i32, "{k}");
        assert_eq!(s.lag, Some(lag as u16), "{k}");
        // Every sample was judged: none lost to the confirmation.
        assert_eq!(s.checked, 2_000 - u64::from(CONFIRM), "{k}");
    }
    // A slip whose confirmation spans two calls, by a lag past half the
    // counter: read as the shorter way round.
    let mut c = CounterCheck::new(96_000);
    c.check(T, &counter(T, 100, 0));
    c.check(T + 100, &counter(T + 100, 2, 40_000));
    c.check(T + 102, &counter(T + 102, 100, 40_000));
    assert_eq!(c.stats().last_slip, 40_000 - 65_536);
    assert_eq!(c.stats().slips, 1);
    // A slip after a dropout: both.
    let mut samples = counter(T, 1_000, 0);
    samples.extend([0; 10]);
    samples.extend(counter(T + 1_010, 1_000, 2));
    let mut c = CounterCheck::new(96_000);
    feed(&mut c, T, &samples);
    assert_eq!(counted(c.stats()), (0, 0, 1, 10, 1, 0));
    assert_eq!(c.stats().last_slip, 2);
}

#[test]
fn a_changed_value_is_a_mismatch() {
    let clean = counter(T, 2_000, 0);
    // A value that is no counter word (its low byte set, or its top).
    for bad in [0x0123_4567, -256, 0x0100_0000, 1] {
        let mut samples = clean.clone();
        samples[1_000] = bad;
        let mut c = CounterCheck::new(96_000);
        feed(&mut c, T, &samples);
        assert_eq!(counted(c.stats()), (1, 1, 0, 0, 0, 0), "{bad:#x}");
    }
    // A counter word of another lag, fewer than CONFIRM in a row: a
    // mismatch of their length, never a slip.
    for n in 1..CONFIRM as usize {
        let mut samples = clean.clone();
        samples[1_000..1_000 + n].copy_from_slice(&counter(T + 1_000, n, 50));
        let mut c = CounterCheck::new(96_000);
        feed(&mut c, T, &samples);
        assert_eq!(counted(c.stats()), (1, n as u64, 0, 0, 0, 0), "{n}");
        assert_eq!(c.stats().lag, Some(0));
        // Every sample was judged once, the mismatched ones too.
        assert_eq!(c.stats().checked + c.stats().blind, 2_000, "{n}");
    }
    // Two lags in turn, then garbage: one mismatch run.
    let mut samples = clean.clone();
    samples[1_000..1_002].copy_from_slice(&counter(T + 1_000, 2, 50));
    samples[1_002..1_004].copy_from_slice(&counter(T + 1_002, 2, 70));
    samples[1_004] = 0x55;
    let mut c = CounterCheck::new(96_000);
    feed(&mut c, T, &samples);
    assert_eq!(counted(c.stats()), (1, 5, 0, 0, 0, 0));
    // A mismatch right before a dropout: two events.
    let mut samples = clean.clone();
    samples[1_000] = 0x55;
    samples[1_001] = 0;
    let mut c = CounterCheck::new(96_000);
    feed(&mut c, T, &samples);
    assert_eq!(counted(c.stats()), (1, 1, 1, 1, 0, 0));
    // A short other lag broken by a hole: the mismatch, then the dropout.
    let mut samples = clean.clone();
    samples[1_000..1_002].copy_from_slice(&counter(T + 1_000, 2, 50));
    samples[1_002] = 0;
    let mut c = CounterCheck::new(96_000);
    feed(&mut c, T, &samples);
    assert_eq!(counted(c.stats()), (1, 2, 1, 1, 0, 0));
}

#[test]
fn silence_is_blind() {
    let mut c = CounterCheck::new(1_000);
    c.check(T, &[0; 999]);
    let s = c.stats();
    assert_eq!((s.blind, s.checked, s.lag), (999, 0, None));
    assert_eq!(counted(s), CLEAN);
    // No lock for no_return_after samples: no_return.
    c.check(T + 999, &[0]);
    assert!(c.stats().no_return);
    // Garbage is blind too before a lock, and keeps it so.
    c.check(T + 1_000, &[0x55; 10]);
    assert!(c.stats().no_return);
    assert_eq!(c.stats().blind, 1_010);
    // A lag that does not hold for CONFIRM samples locks nothing.
    let mut broken = counter(T + 1_010, CONFIRM as usize - 1, 0);
    broken.push(0x55);
    c.check(T + 1_010, &broken);
    assert_eq!(c.stats().lag, None);
    // The counter arrives: locked, no_return clear.
    c.check(T + 2_000, &counter(T + 2_000, 100, 0));
    let s = c.stats();
    assert_eq!((s.lag, s.no_return), (Some(0), false));
    assert_eq!(counted(s), CLEAN);
    // Locked, silence is a dropout (the stream is gone), not blind.
    c.check(T + 2_100, &[0; 50]);
    assert_eq!(counted(c.stats()), (0, 0, 1, 50, 0, 0));
}

#[test]
fn a_counter_that_never_locks_reads_no_return_only_at_its_bound() {
    let mut c = CounterCheck::new(64);
    // Valid words of a new lag every sample: never CONFIRM in a row.
    let samples: Vec<i32> = (0..63u64)
        .map(|i| (((i * 7) % 65_536) << 8) as i32)
        .collect();
    c.check(T, &samples);
    assert!(!c.stats().no_return);
    c.check(T + 63, &[0x55]);
    assert!(c.stats().no_return);
}

/// The sine as a flow of `bits` carries it.
fn sine(hz: f64, dbfs: f64, start: u64, n: usize, rate: u32, bits: u32) -> Vec<i32> {
    let mut out = vec![0; n];
    Source::Sine { hz, dbfs }.fill(start, rate, &mut out, &[], None);
    let mask = (!0u32 << (32 - bits)) as i32;
    out.iter().map(|v| v & mask).collect()
}

fn sine_feed(c: &mut SineCheck, start: u64, samples: &[i32]) {
    for (i, chunk) in samples.chunks(32).enumerate() {
        c.check(start + 32 * i as u64, chunk);
    }
}

#[test]
fn a_clean_sine_has_no_discontinuity() {
    for (hz, dbfs, rate, bits) in [
        (1_000.0, -20.0, 96_000, 24),
        (997.0, -20.0, 48_000, 24),
        (1_000.0, -60.0, 96_000, 24),
        (440.0, -20.0, 96_000, 16),
        (5_000.0, -30.0, 96_000, 32),
        (24_000.0, -20.0, 96_000, 24),
    ] {
        let samples = sine(hz, dbfs, T, 2 * rate as usize, rate, bits);
        let mut c = SineCheck::new(hz, rate, bits as u8);
        sine_feed(&mut c, T, &samples);
        let s = c.stats();
        assert_eq!(
            (s.discontinuities, s.discontinuity_frames, s.blind),
            (0, 0, 0),
            "{hz} Hz {dbfs} dBFS {rate} {bits}"
        );
        assert_eq!(s.checked, 2 * u64::from(rate) - 4, "{hz}");
    }
}

#[test]
fn one_repeated_sample_is_found() {
    // At every phase of one 1 kHz period, its crest included.
    let clean = sine(1_000.0, -20.0, T, 2_000, RATE, 24);
    for m in 1_000..1_096 {
        let mut samples = clean.clone();
        samples[m] = samples[m - 1];
        let mut c = SineCheck::new(1_000.0, RATE, 24);
        sine_feed(&mut c, T, &samples);
        let s = c.stats();
        assert_eq!(s.discontinuities, 1, "repeat at {m}");
        assert!((1..=5).contains(&s.discontinuity_frames), "{m}: {s:?}");
    }
    // A lost sample (the rest one earlier) and a hole: one each.
    let mut lost = clean.clone();
    lost.remove(1_010);
    lost.push(0);
    let mut c = SineCheck::new(1_000.0, RATE, 24);
    sine_feed(&mut c, T, &lost[..1_900]);
    assert_eq!(c.stats().discontinuities, 1);
    let mut hole = clean.clone();
    hole[1_010] = 0;
    let mut c = SineCheck::new(1_000.0, RATE, 24);
    sine_feed(&mut c, T, &hole);
    assert_eq!(c.stats().discontinuities, 1);
    // Two repeats far apart: two.
    let mut two = clean.clone();
    two[500] = two[499];
    two[1_500] = two[1_499];
    let mut c = SineCheck::new(1_000.0, RATE, 24);
    sine_feed(&mut c, T, &two);
    assert_eq!(c.stats().discontinuities, 2);
    // At a quarter of the rate (2 cos ω = 0, the samples 0, A, 0, -A) a
    // repeat's residuals alternate with good ones: bad, good, bad, good,
    // bad is still one run of three.
    let quarter = sine(24_000.0, -20.0, T, 2_000, RATE, 24);
    let mut q = quarter.clone();
    q[1_002] = q[1_001];
    let mut c = SineCheck::new(24_000.0, RATE, 24);
    sine_feed(&mut c, T, &q);
    let s = c.stats();
    assert_eq!((s.discontinuities, s.discontinuity_frames), (1, 3), "{s:?}");
    // A second repeat three good residuals after the first run's last bad
    // one is a second run; two good ones after, the same run.
    for (second, runs) in [(1_010, 2), (1_009, 1)] {
        let mut q = quarter.clone();
        q[1_002] = q[1_001];
        q[second] = q[second - 1];
        let mut c = SineCheck::new(24_000.0, RATE, 24);
        sine_feed(&mut c, T, &q);
        assert_eq!(c.stats().discontinuities, runs, "{second}");
    }
}

/// The sine under the HIL test signal's linear fades: in over `fade_in`
/// samples, held, out over `fade_out`, then silence; as `bits` carry it.
fn faded(hz: f64, fade_in: usize, hold: usize, fade_out: usize, bits: u32) -> Vec<i32> {
    let n = fade_in + hold + fade_out + 1_000;
    let full = sine(hz, -20.0, T, n, RATE, 32);
    let mask = (!0u32 << (32 - bits)) as i32;
    full.iter()
        .enumerate()
        .map(|(i, &v)| {
            let gain = if i < fade_in {
                i as f64 / fade_in as f64
            } else if i < fade_in + hold {
                1.0
            } else if i < fade_in + hold + fade_out {
                1.0 - (i - fade_in - hold) as f64 / fade_out as f64
            } else {
                0.0
            };
            (f64::from(v) * gain).round() as i32 & mask
        })
        .collect()
}

#[test]
fn the_test_signal_s_fades_are_no_discontinuity() {
    // The engine's HIL signal: 500 ms in, 50 ms out (rt.rs FADE_IN_MS,
    // FADE_MS), at 96 kHz.
    for (hz, bits) in [(1_000.0, 24), (1_000.0, 16), (5_000.0, 24), (440.0, 24)] {
        let samples = faded(hz, 48_000, 9_600, 4_800, bits);
        let mut c = SineCheck::new(hz, RATE, bits as u8);
        sine_feed(&mut c, T, &samples);
        let s = c.stats();
        assert_eq!(s.discontinuities, 0, "{hz} Hz {bits} bits: {s:?}");
        assert!(s.blind > 900, "{hz}: the silence after it is blind: {s:?}");
    }
    // The noise floor alone would not hold the kink where the 50 ms
    // fade-out ends (2·slope·sin ω, 112 LSB at 5 kHz at 24 bits): the
    // recent peak's bound does.
    let samples = faded(5_000.0, 48_000, 9_600, 4_800, 24);
    let slope = f64::from(i32::MAX) * 0.1 / 4_800.0;
    let w = std::f64::consts::TAU * 5_000.0 / f64::from(RATE);
    assert!(2.0 * slope * w.sin() > SINE_FLOOR_LSB * 256.0);
    // A repeat inside the fade-out is still found.
    let mut glitch = samples.clone();
    glitch[48_000 + 9_600 + 1_000] = glitch[48_000 + 9_600 + 999];
    let mut c = SineCheck::new(5_000.0, RATE, 24);
    sine_feed(&mut c, T, &glitch);
    assert_eq!(c.stats().discontinuities, 1);
}

#[test]
fn silence_and_a_gap_between_calls_are_no_discontinuity() {
    let mut c = SineCheck::new(1_000.0, RATE, 24);
    c.check(T, &[0; 500]);
    let s = c.stats();
    assert_eq!((s.blind, s.checked, s.discontinuities), (496, 0, 0));
    // Calls that do not continue each other: each starts afresh.
    let mut c = SineCheck::new(1_000.0, RATE, 24);
    c.check(T, &sine(1_000.0, -20.0, T, 100, RATE, 24));
    c.check(T + 1_013, &sine(1_000.0, -20.0, T + 1_013, 100, RATE, 24));
    let s = c.stats();
    assert_eq!((s.discontinuities, s.checked), (0, 96 + 96));
    // But calls that continue each other are judged across the seam.
    let mut c = SineCheck::new(1_000.0, RATE, 24);
    let samples = sine(1_000.0, -20.0, T, 200, RATE, 24);
    c.check(T, &samples[..100]);
    c.check(T + 100, &samples[101..]);
    assert_eq!(
        c.stats().discontinuities,
        1,
        "position 100 holds sample 101"
    );
    // A sine that stops: one discontinuity, then blind.
    let mut c = SineCheck::new(1_000.0, RATE, 24);
    let mut stops = sine(1_000.0, -20.0, T, 1_000, RATE, 24);
    stops[500..].fill(0);
    sine_feed(&mut c, T, &stops);
    let s = c.stats();
    assert_eq!(s.discontinuities, 1);
    assert_eq!(s.blind, 496);
}

#[test]
fn the_recent_peak_decays_with_its_time_constant() {
    for rate in [96_000u32, 48_000] {
        let mut c = SineCheck::new(1_000.0, rate, 24);
        let loud = i32::MAX / 10;
        c.check(T, &[loud]);
        assert_eq!(c.peak, f64::from(loud));
        // One time constant of silence: 1/e of it left.
        let hold = (SINE_HOLD_S * f64::from(rate)) as usize;
        c.check(T + 1, &vec![0; hold]);
        let left = c.peak / f64::from(loud);
        assert!((left - (-1f64).exp()).abs() < 0.001, "{rate}: {left}");
        // A louder sample takes over at once.
        c.check(T + 1 + hold as u64, &[-loud]);
        assert_eq!(c.peak, f64::from(loud));
    }
}

#[test]
fn a_residual_at_the_bound_is_no_discontinuity() {
    // At 0 Hz the weights are exact (4 and 6) and the residual of
    // x4, 0, 0, 0, x is x + x4: with x4 = 100 above the floor (32 at 32
    // bits) and the peak's -60 dB under it, the bound is the floor.
    for (x, bad) in [(-68, 0), (-67, 1), (-69, 0), (-133, 1), (-132, 0)] {
        let mut c = SineCheck::new(0.0, RATE, 32);
        c.check(T, &[100, 0, 0, 0, x]);
        let s = c.stats();
        assert_eq!((s.checked, s.discontinuities), (1, bad), "{x}");
    }
}
