use super::*;

const P: u64 = 333_333; // 32 samples at 96 kHz

#[test]
fn periods_and_levels() {
    assert_eq!(period_ns(32, 96_000.0), P);
    assert_eq!(period_ns(64, 96_000.0), 666_667);
    assert_eq!(period_ns(48, 96_000.0), 500_000);
    assert_eq!(dbfs(1.0), 0.0);
    assert!((dbfs(ACTIVITY_THRESHOLD) + 50.0).abs() < 1e-9);
    assert_eq!(dbfs(0.0), -150.0);
    assert_eq!(dbfs(-1.0), -150.0);
    assert_eq!(dbfs(1e-12), -150.0);
}

#[test]
fn classify_boundaries() {
    assert_eq!(classify(P, P), Gap::OnTime);
    assert_eq!(classify(300, 200), Gap::OnTime);
    assert_eq!(classify(301, 200), Gap::Late);
    assert_eq!(classify(399, 200), Gap::Late);
    assert_eq!(classify(400, 200), Gap::Missed);
    assert_eq!(classify(0, 200), Gap::OnTime);
    assert_eq!(classify(2 * P - 1, P), Gap::Late);
    assert_eq!(classify(2 * P, P), Gap::Missed);
}

#[test]
fn drift_needs_a_second_and_has_the_card_sign() {
    assert_eq!(drift_ppm((0, 0), (999_999_999, 96_000), 96_000.0), None);
    assert_eq!(drift_ppm((5, 0), (4, 0), 96_000.0), None);
    assert_eq!(
        drift_ppm((0, 0), (1_000_000_000, 96_000), 96_000.0),
        Some(0.0)
    );
    let fast = drift_ppm((0, 0), (10_000_000_000, 960_096), 96_000.0).unwrap();
    assert!((fast - 100.0).abs() < 1e-6, "{fast}");
    let slow = drift_ppm((1_000_000_000, 10), (3_000_000_000, 10 + 191_904), 96_000.0).unwrap();
    assert!((slow + 500.0).abs() < 1e-6, "{slow}");
}

#[test]
fn histogram_quantiles() {
    let h = Histogram::default();
    assert_eq!(h.snapshot().quantile_ns(0.5), 0);
    for _ in 0..98 {
        h.record(333_400);
    }
    h.record(1_200_000);
    h.record(9_000_000);
    let s = h.snapshot();
    assert_eq!(s.total(), 100);
    assert_eq!(s.max_ns, 9_000_000);
    assert_eq!(s.quantile_ns(0.5), 334_000);
    assert_eq!(s.quantile_ns(0.98), 334_000);
    assert_eq!(s.quantile_ns(0.99), 1_201_000);
    assert_eq!(s.quantile_ns(1.0), 9_000_000);
    assert_eq!(s.quantile_ns(0.0), 334_000);
    assert_eq!(s.summary_us(), [334.0, 1201.0, 9000.0, 9000.0]);
    let one = Histogram::default();
    one.record(700);
    assert_eq!(one.snapshot().quantile_ns(0.5), 700);
    assert_eq!(one.snapshot().counts.len(), BUCKETS);
    one.record(u64::MAX);
    assert_eq!(one.snapshot().counts[BUCKETS - 1], 1);
}

#[test]
fn warmup_callbacks_are_counted_but_not_judged() {
    let t = Telemetry::new(32, 96_000.0);
    let mut at = 1_000;
    for i in 0..WARMUP {
        t.on_callback(at, Some(i as i64 * 7));
        at += 10 * P;
    }
    let s = t.snapshot();
    assert_eq!(
        (
            s.callbacks,
            s.late,
            s.missed,
            s.position_gaps,
            s.interval.total()
        ),
        (WARMUP, 0, 0, 0, 0)
    );
    assert_eq!(s.first_callback_ns, 1_000);
}

#[test]
fn late_missed_and_position_gaps_are_counted() {
    let t = Telemetry::new(32, 96_000.0);
    let mut at = 1_000;
    let mut pos = 0;
    for _ in 0..WARMUP {
        t.on_callback(at, Some(pos));
        at += P;
        pos += 32;
    }
    t.on_callback(at, Some(pos)); // on time
    at += P * 3 / 2 + 1;
    pos += 32;
    t.on_callback(at, Some(pos)); // late
    at += 2 * P;
    pos += 64;
    t.on_callback(at, Some(pos)); // missed, one gap
    at += P;
    t.on_callback(at, None); // no position: no gap judged
    let s = t.snapshot();
    assert_eq!(
        (s.callbacks, s.late, s.missed, s.position_gaps),
        (WARMUP + 4, 1, 1, 1)
    );
    assert_eq!(s.interval.total(), 4);
    assert_eq!(t.callbacks(), WARMUP + 4);
}

#[test]
fn counters_are_the_snapshots_counts_without_histograms() {
    let t = Telemetry::new(32, 96_000.0);
    assert_eq!(t.counters(), Counters::default());
    let mut at = 1_000;
    for _ in 0..WARMUP {
        t.on_callback(at, None);
        at += P;
    }
    t.on_callback(at, None); // on time
    at += P * 3 / 2 + 1;
    t.on_callback(at, None); // late
    at += 2 * P;
    t.on_callback(at, None); // missed
    at += 2 * P;
    t.on_callback(at, None); // missed
    t.on_done(P + 1); // overrun
    t.on_done(7_000);
    let c = t.counters();
    assert_eq!(
        c,
        Counters {
            callbacks: WARMUP + 4,
            late: 1,
            missed: 2,
            overruns: 1,
            max_ns: P + 1,
        }
    );
    let s = t.snapshot();
    assert_eq!(
        (c.callbacks, c.late, c.missed, c.overruns, c.max_ns),
        (s.callbacks, s.late, s.missed, s.overruns, s.duration.max_ns)
    );
}

#[test]
fn counters_add_up_across_reopens_and_keep_the_longest_callback() {
    let first = Counters {
        callbacks: 100,
        late: 1,
        missed: 2,
        overruns: 3,
        max_ns: 900,
    };
    let later = Counters {
        callbacks: 40,
        late: 5,
        missed: 7,
        overruns: 11,
        max_ns: 400,
    };
    let sum = Counters {
        callbacks: 140,
        late: 6,
        missed: 9,
        overruns: 14,
        max_ns: 900,
    };
    assert_eq!(first.plus(later), sum);
    assert_eq!(later.plus(first), sum);
    assert_eq!(Counters::default().plus(later), later);
    let full = Counters {
        callbacks: u64::MAX,
        ..Counters::default()
    };
    assert_eq!(full.plus(first).callbacks, u64::MAX);
}

#[test]
fn overruns_are_longer_than_a_period() {
    let t = Telemetry::new(32, 96_000.0);
    t.on_done(P);
    assert_eq!(
        t.snapshot().overruns,
        0,
        "exactly one period is not an overrun"
    );
    t.on_done(P + 1);
    assert_eq!(
        t.snapshot().overruns,
        1,
        "longer than a period is an overrun"
    );
    t.on_done(10);
    let s = t.snapshot();
    assert_eq!(s.overruns, 1, "a short callback is not an overrun");
    assert_eq!(s.duration.total(), 3);
    assert_eq!(s.period_ns, P);
    assert_eq!(t.period_ns(), P);
}

#[test]
fn drift_is_anchored_after_the_warmup_burst() {
    // 48 samples at 96 kHz: exactly 500 µs. The priming callbacks arrive
    // in a burst inside start(); the card then runs exactly on time.
    const Q: u64 = 500_000;
    let t = Telemetry::new(48, 96_000.0);
    assert_eq!(t.snapshot().drift_ppm, None);
    let mut pos = 0;
    for i in 0..WARMUP {
        t.on_callback(1_000 + i, Some(pos));
        pos += 48;
    }
    assert_eq!(t.snapshot().drift_ppm, None);
    for k in 0..=2_000 {
        t.on_callback(1_007 + (k + 1) * Q, Some(pos));
        pos += 48;
    }
    let s = t.snapshot();
    assert_eq!(
        (s.drift_ppm, s.position_gaps, s.late, s.missed),
        (Some(0.0), 0, 0, 0)
    );
    // Callbacks without a position leave the drift's end where it was.
    t.on_callback(1_007 + 2_002 * Q, None);
    assert_eq!(t.snapshot().drift_ppm, Some(0.0));
}

#[test]
fn a_missing_position_is_not_a_gap_but_a_jump_after_warmup_is() {
    let t = Telemetry::new(32, 96_000.0);
    let mut at = 1_000;
    let mut pos = 0;
    for _ in 0..WARMUP {
        t.on_callback(at, Some(pos));
        at += P;
        pos += 32;
    }
    // The first judged callback jumps by two buffers: one gap.
    t.on_callback(at, Some(pos + 32));
    pos += 64;
    at += P;
    assert_eq!(t.snapshot().position_gaps, 1);
    t.on_callback(at, Some(pos));
    pos += 32;
    at += P;
    // One callback without a position: the next one has nothing to compare with.
    t.on_callback(at, None);
    pos += 32;
    at += P;
    t.on_callback(at, Some(pos));
    pos += 32;
    at += P;
    t.on_callback(at, Some(pos));
    assert_eq!(t.snapshot().position_gaps, 1);
    at += P;
    t.on_callback(at, Some(pos + 96));
    assert_eq!(t.snapshot().position_gaps, 2);
}

#[test]
fn input_peaks_keep_each_inputs_maximum_until_taken() {
    let p = InputPeaks::new(3);
    assert_eq!(p.take(), [0.0, 0.0, 0.0]);
    p.record(0, 0.25);
    p.record(0, 0.5);
    p.record(0, 0.125);
    p.record(0, f64::NAN);
    p.record(2, 0.0625);
    p.record(3, 1.0); // the card has no fourth input
    assert_eq!(p.take(), [0.5, 0.0, 0.0625]);
    assert_eq!(p.take(), [0.0, 0.0, 0.0]);
    p.record(1, -0.75);
    p.record(1, f64::INFINITY);
    assert_eq!(p.take(), [0.0, 0.75, 0.0]);
    assert!(InputPeaks::new(0).take().is_empty());
}

#[test]
fn watched_inputs_are_card_numbers_from_one() {
    assert_eq!(Watched::parse("all"), Ok(Watched::All));
    assert_eq!(Watched::parse("3"), Ok(Watched::Inputs(vec![2])));
    assert_eq!(
        Watched::parse("121-124,103-105,104,101"),
        Ok(Watched::Inputs(vec![
            100, 102, 103, 104, 120, 121, 122, 123
        ]))
    );
    assert_eq!(Watched::parse("7-7"), Ok(Watched::Inputs(vec![6])));
    assert_eq!(
        Watched::parse(&MAX_INPUT.to_string()),
        Ok(Watched::Inputs(vec![MAX_INPUT - 1]))
    );
    for bad in [
        "", "ALL", "0", "3,", ",3", "3-", "-3", "5-3", "3-4-5", "x", "3 4", "1-1025",
    ] {
        assert!(Watched::parse(bad).is_err(), "{bad:?}");
    }
    assert!(Watched::parse(&(MAX_INPUT + 1).to_string()).is_err());
}

#[test]
fn watched_inputs_must_exist_on_the_card() {
    let w = Watched::parse("101-110,121-124").unwrap();
    assert_eq!(w.check(124), Ok(()));
    assert!(w.check(123).is_err());
    assert_eq!(Watched::All.check(0), Ok(()));
    assert_eq!(Watched::Inputs(vec![]).check(0), Ok(()));
}

#[test]
fn the_guard_hears_only_the_watched_inputs() {
    let loud_unused = [0.0, 0.76, 0.001, 0.002];
    let stage = Watched::parse("3-4").unwrap();
    assert_eq!(stage.peak(&loud_unused), 0.002);
    assert_eq!(Watched::All.peak(&loud_unused), 0.76);
    assert_eq!(Watched::parse("9").unwrap().peak(&loud_unused), 0.0);
    assert_eq!(Watched::All.peak(&[]), 0.0);
    assert_eq!(
        stage.numbers(),
        Some(vec![3, 4]),
        "the report names card inputs from 1"
    );
    assert_eq!(Watched::All.numbers(), None);
}

#[test]
fn the_loudest_inputs_are_listed_loudest_first() {
    let mut l = Loudest::default();
    assert_eq!((l.max(), l.top(5)), (0.0, vec![]));
    l.observe(&[0.0, 0.5, 0.25]);
    l.observe(&[0.125, 0.0625, 0.25, 0.75, 0.0, 0.5]);
    l.observe(&[0.0]);
    assert_eq!(l.max(), 0.75);
    assert_eq!(
        l.top(5),
        [(3, 0.75), (1, 0.5), (5, 0.5), (2, 0.25), (0, 0.125)]
    );
    assert_eq!(l.top(2), [(3, 0.75), (1, 0.5)]);
    // Silent inputs are not listed.
    let mut quiet = Loudest::default();
    quiet.observe(&[0.0, 0.0, 1e-9]);
    assert_eq!(quiet.top(5), [(2, 1e-9)]);
}

#[test]
fn driver_messages_are_answered() {
    use selector::*;
    for s in [
        ENGINE_VERSION,
        RESET_REQUEST,
        BUFFER_SIZE_CHANGE,
        RESYNC_REQUEST,
        LATENCIES_CHANGED,
        SUPPORTS_TIME_INFO,
        OVERLOAD,
    ] {
        assert_eq!(reply(SELECTOR_SUPPORTED, s), 1, "{s}");
    }
    for s in [SELECTOR_SUPPORTED, SUPPORTS_TIME_CODE, 9, 0] {
        assert_eq!(reply(SELECTOR_SUPPORTED, s), 0, "{s}");
    }
    let answers: Vec<i32> = [
        ENGINE_VERSION,
        RESET_REQUEST,
        BUFFER_SIZE_CHANGE,
        RESYNC_REQUEST,
        LATENCIES_CHANGED,
        SUPPORTS_TIME_INFO,
        SUPPORTS_TIME_CODE,
        OVERLOAD,
        99,
    ]
    .into_iter()
    .map(|s| reply(s, 0))
    .collect();
    assert_eq!(answers, [2, 1, 0, 1, 1, 1, 0, 0, 0]);
}

/// The owner thread logs why it reopens (#9 2026-09-28): a reset request
/// and a buffer size change are told apart, and taken once.
#[test]
fn a_reopen_request_says_which_message_asked() {
    use selector::*;
    let t = Telemetry::new(32, 96_000.0);
    let reset = Requested {
        reset: true,
        buffer_size: false,
    };
    let size = Requested {
        reset: false,
        buffer_size: true,
    };
    assert_eq!(t.take_requests(), Requested::default());
    t.driver_message(RESET_REQUEST, 0);
    assert_eq!(t.take_requests(), reset);
    assert_eq!(t.take_requests(), Requested::default());
    t.driver_message(BUFFER_SIZE_CHANGE, 64);
    assert_eq!(t.take_requests(), size);
    t.driver_message(RESET_REQUEST, 0);
    t.driver_message(BUFFER_SIZE_CHANGE, 64);
    t.driver_message(RESET_REQUEST, 0);
    assert_eq!(
        t.take_requests(),
        Requested {
            reset: true,
            buffer_size: true
        }
    );
    // Nothing else asks for a reopen.
    for sel in [
        SELECTOR_SUPPORTED,
        ENGINE_VERSION,
        RESYNC_REQUEST,
        LATENCIES_CHANGED,
        SUPPORTS_TIME_INFO,
        SUPPORTS_TIME_CODE,
        OVERLOAD,
        0,
        99,
    ] {
        t.driver_message(sel, RESET_REQUEST);
    }
    t.on_rate_change();
    assert_eq!(t.take_requests(), Requested::default());
    // `take_reopen` is either of them, taken once too.
    t.driver_message(BUFFER_SIZE_CHANGE, 64);
    assert!(t.take_reopen());
    assert_eq!(t.take_requests(), Requested::default());
    assert!(!Requested::default().any());
    assert!(reset.any());
    assert!(size.any());
}

#[test]
fn driver_messages_are_counted_and_resets_request_a_reopen() {
    use selector::*;
    let t = Telemetry::new(32, 96_000.0);
    assert_eq!(t.driver_message(ENGINE_VERSION, 0), 2);
    assert_eq!(t.driver_message(SELECTOR_SUPPORTED, OVERLOAD), 1);
    assert!(!t.take_reopen());
    assert_eq!(t.driver_message(RESET_REQUEST, 0), 1);
    assert!(t.take_reopen());
    assert!(!t.take_reopen());
    assert_eq!(t.driver_message(BUFFER_SIZE_CHANGE, 64), 0);
    assert!(t.take_reopen());
    assert_eq!(t.driver_message(RESYNC_REQUEST, 0), 1);
    assert_eq!(t.driver_message(LATENCIES_CHANGED, 0), 1);
    assert!(!t.take_reopen());
    assert_eq!(t.driver_message(OVERLOAD, 0), 0);
    assert_eq!(t.driver_message(OVERLOAD, 0), 0);
    assert_eq!(t.rate_changes(), 0);
    t.on_rate_change();
    assert_eq!(t.rate_changes(), 1);
    let s = t.snapshot();
    assert_eq!(
        (
            s.resets,
            s.buffer_size_changes,
            s.resyncs,
            s.latency_changes,
            s.overloads,
            s.rate_changes
        ),
        (1, 1, 1, 1, 2, 1)
    );
}

#[test]
fn activity_needs_consecutive_seconds_above_minus_50_dbfs() {
    let mut g = ActivityGuard::new(3);
    assert!(!g.observe(0.01));
    assert!(!g.observe(0.01));
    assert!(!g.observe(ACTIVITY_THRESHOLD));
    assert!(!g.observe(0.01));
    assert!(!g.observe(0.01));
    assert!(g.observe(0.01));
    assert!(g.observe(1.0));
    assert!(!g.observe(0.0));
    let mut now = ActivityGuard::new(1);
    assert!(now.observe(ACTIVITY_THRESHOLD * 1.001));
}

fn g(kind: GlitchKind, at_ns: u64, value: u64) -> Glitch {
    Glitch { kind, at_ns, value }
}

#[test]
fn glitch_log_keeps_its_capacity_and_counts_drops() {
    let log = GlitchLog::new(3);
    for i in 0..4 {
        log.push(g(GlitchKind::Late, i, 10 + i));
    }
    let mut out = Vec::new();
    log.drain(&mut out);
    assert_eq!(out.iter().map(|x| x.at_ns).collect::<Vec<_>>(), [0, 1, 2]);
    assert_eq!(log.dropped(), 1);
    // After a drain the ring takes three more and wraps around its slots.
    for i in 10..13 {
        log.push(g(GlitchKind::Missed, i, i));
    }
    out.clear();
    log.drain(&mut out);
    assert_eq!(
        out.iter().map(|x| x.at_ns).collect::<Vec<_>>(),
        [10, 11, 12]
    );
    out.clear();
    log.drain(&mut out);
    assert!(out.is_empty());
    assert_eq!(log.dropped(), 1);
}

#[test]
fn glitch_kinds_and_large_values_round_trip() {
    let log = GlitchLog::new(8);
    let all = [
        g(GlitchKind::Late, 1, 500_001),
        g(GlitchKind::Missed, 2, 700_000),
        g(GlitchKind::Overrun, 3, 400_000),
        g(GlitchKind::PositionGap, 4, 64),
        g(GlitchKind::PositionBack, 5, 32),
    ];
    for x in all {
        log.push(x);
    }
    log.push(g(GlitchKind::PositionBack, 6, u64::MAX));
    let mut out = Vec::new();
    log.drain(&mut out);
    assert_eq!(&out[..5], &all);
    // Five kinds take three bits: a value is clamped below 2^61.
    assert_eq!(out[5], g(GlitchKind::PositionBack, 6, (1 << 61) - 1));
    assert_eq!(
        [
            GlitchKind::Late,
            GlitchKind::Missed,
            GlitchKind::Overrun,
            GlitchKind::PositionGap,
            GlitchKind::PositionBack
        ]
        .map(GlitchKind::name),
        ["late", "missed", "overrun", "position-gap", "position-back"]
    );
}

/// A step back keeps its direction: with only the magnitude, a step back
/// by one buffer would read like a normal advance.
#[test]
fn a_position_step_back_is_its_own_glitch_kind() {
    let t = Telemetry::new(32, 96_000.0);
    let mut at = 1_000;
    let mut last = 1_000;
    for _ in 0..WARMUP {
        t.on_callback(at, Some(last));
        at += P;
        last += 32;
    }
    last -= 32; // the last priming callback's position
    let mut times = Vec::new();
    for pos in [last - 32, last - 32, last, last - 100, last + 64] {
        t.on_callback(at, Some(pos));
        times.push(at);
        at += P;
    }
    let mut out = Vec::new();
    t.drain_glitches(&mut out);
    assert_eq!(
        out,
        [
            g(GlitchKind::PositionBack, times[0], 32),
            // Standing still is a gap of 0, not a step back.
            g(GlitchKind::PositionGap, times[1], 0),
            g(GlitchKind::PositionBack, times[3], 100),
            g(GlitchKind::PositionGap, times[4], 164),
        ]
    );
    assert_eq!(t.snapshot().position_gaps, 4);
}

#[test]
fn judged_glitches_enter_the_log_with_their_times() {
    let t = Telemetry::new(32, 96_000.0);
    let mut at = 1_000;
    let mut pos = 0;
    for _ in 0..WARMUP {
        t.on_callback(at, Some(pos));
        at += P;
        pos += 32;
    }
    let prev = at - P;
    let late_at = prev + P * 3 / 2 + 1;
    t.on_callback(late_at, Some(pos));
    let missed_at = late_at + 2 * P;
    t.on_callback(missed_at, Some(pos + 32 + 64));
    t.on_done(P + 1);
    t.on_done(P);
    let mut out = Vec::new();
    t.drain_glitches(&mut out);
    assert_eq!(
        out,
        [
            g(GlitchKind::Late, late_at, P * 3 / 2 + 1),
            g(GlitchKind::Missed, missed_at, 2 * P),
            g(GlitchKind::PositionGap, missed_at, 96),
            g(GlitchKind::Overrun, missed_at, P + 1),
        ]
    );
    assert_eq!(t.snapshot().glitches_dropped, 0);
}

#[test]
fn warmup_callbacks_leave_no_glitch() {
    let t = Telemetry::new(32, 96_000.0);
    for i in 0..WARMUP {
        t.on_callback(1_000 + i * 10 * P, Some(i as i64 * 7));
    }
    let mut out = Vec::new();
    t.drain_glitches(&mut out);
    assert!(out.is_empty());
}

#[test]
fn callback_cpus_and_thread_switches_are_counted() {
    let t = Telemetry::new(32, 96_000.0);
    let mut at = 1_000;
    // As the host calls them: the thread first, then the callback's entry.
    let mut callback = |cpu: u32, thread: u32| {
        t.on_thread(cpu, thread);
        t.on_callback(at, None);
        at += P;
    };
    for _ in 0..WARMUP {
        callback(14, 900);
    }
    callback(14, 900);
    callback(14, 900);
    callback(3, 900);
    callback(63, 900);
    callback(64, 901);
    let s = t.snapshot();
    assert_eq!(s.callback_cpus, [(3, 1), (14, WARMUP + 2), (63, 1)]);
    assert_eq!(
        (s.cpu_other, s.callback_thread, s.thread_switches),
        (1, 900, 1)
    );
    let fresh = Telemetry::new(32, 96_000.0).snapshot();
    assert_eq!(
        (
            fresh.callback_cpus.len(),
            fresh.callback_thread,
            fresh.thread_switches
        ),
        (0, 0, 0)
    );
}

/// The priming callbacks may arrive in a burst inside `start()`, on the
/// thread that called it: they neither name the callback thread nor count
/// as switches. The first callback after the warm-up names it.
#[test]
fn priming_callbacks_neither_name_the_callback_thread_nor_switch_it() {
    let t = Telemetry::new(32, 96_000.0);
    let mut at = 1_000;
    let mut callback = |thread: u32| {
        t.on_thread(5, thread);
        t.on_callback(at, None);
        at += P;
    };
    // The burst on the owner thread (7), its last callback on the driver's.
    for _ in 0..WARMUP - 1 {
        callback(7);
    }
    callback(900);
    let s = t.snapshot();
    assert_eq!(
        (s.callbacks, s.callback_thread, s.thread_switches),
        (WARMUP, 0, 0),
        "priming callbacks name no thread"
    );
    // The first judged callback names the thread; another thread switches.
    callback(901);
    callback(902);
    callback(901);
    callback(901);
    let s = t.snapshot();
    assert_eq!(
        (s.callbacks, s.callback_thread, s.thread_switches),
        (WARMUP + 4, 901, 1)
    );
    // Every callback's processor is counted, the priming ones too.
    assert_eq!(s.callback_cpus, [(5, WARMUP + 4)]);
}

#[test]
fn gap_scan_counts_gaps_at_the_threshold_and_keeps_the_largest() {
    let mut s = GapScan::new(10_000);
    s.observe(0, 9_999);
    s.observe(9_999, 19_999);
    assert_eq!((s.summary().reads, s.summary().over), (2, 1));
    for i in 0..40_u64 {
        s.observe(1_000_000 * i, 1_000_000 * i + 20_000 + i);
    }
    let sum = s.summary();
    assert_eq!((sum.reads, sum.over, sum.gaps.total()), (42, 41, 41));
    assert_eq!(sum.largest.len(), LARGEST);
    assert_eq!(sum.largest[0], (39_000_000, 20_039));
    assert_eq!(sum.largest[LARGEST - 1], (8_000_000, 20_008));
    assert!(sum.largest.windows(2).all(|w| w[0].1 >= w[1].1));
    // A gap smaller than, or equal to, the smallest kept one changes nothing.
    s.observe(0, 10_001);
    s.observe(5, 5 + 20_008);
    assert_eq!(s.summary().largest, sum.largest);
}

#[test]
fn gap_scan_ignores_a_gap_strictly_below_the_threshold() {
    // A read gap of 9_999 ns is below the 10_000 ns threshold, so it is not
    // a gap: the read is counted, `over` and the histogram are not. This
    // pins `gap < threshold` against `gap == threshold` — the equal case is
    // over (the test above), the strictly-under case is not.
    let mut s = GapScan::new(10_000);
    s.observe(0, 9_999);
    let sum = s.summary();
    assert_eq!((sum.reads, sum.over, sum.gaps.total()), (1, 0, 0));
}
