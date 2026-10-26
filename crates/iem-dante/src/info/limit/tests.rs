use super::*;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// The moments in `from..to` ms after `t0`, one a millisecond, at which a
/// reply of `kind` was allowed.
fn allowed(limit: &mut Limit, t0: Instant, kind: Kind, from: u64, to: u64) -> Vec<u64> {
    (from..to)
        .filter(|&i| limit.allow(t0 + ms(i), kind))
        .collect()
}

#[test]
fn a_burst_of_1000_requests_in_a_second_gets_ten_replies_a_kind_and_the_next_second_ten_more() {
    let t0 = Instant::now();
    let mut limit = Limit::new();
    for kind in BOUNDED {
        // A thousand requests in the first second: the first ten replies.
        assert_eq!(
            allowed(&mut limit, t0, kind, 0, 1_000),
            (0..10).collect::<Vec<u64>>(),
            "{kind:?}"
        );
    }
    for kind in BOUNDED {
        // The next second replies again, from its first moment: the window
        // slides, a reply a whole SPAN old no longer counts.
        assert_eq!(
            allowed(&mut limit, t0, kind, 1_000, 2_000),
            (1_000..1_010).collect::<Vec<u64>>(),
            "{kind:?}"
        );
    }
}

#[test]
fn the_bound_holds_in_any_second_not_only_from_the_first_reply() {
    let t0 = Instant::now();
    let mut limit = Limit::new();
    // One request every 50 ms for ten seconds: 20 a second asked.
    let times = allowed_every(&mut limit, t0, Kind::NetworkInfo, 50, 200);
    for &start in &times {
        let in_span = times
            .iter()
            .filter(|&&t| t >= start && t < start + 1_000)
            .count();
        assert!(
            in_span <= PER_SPAN,
            "{in_span} replies in the second from {start} ms"
        );
    }
    // And the bound is used: ten a second, about a hundred in ten seconds.
    assert!(times.len() >= 95, "{}", times.len());
}

fn allowed_every(limit: &mut Limit, t0: Instant, kind: Kind, step: u64, n: u64) -> Vec<u64> {
    (0..n)
        .map(|k| k * step)
        .filter(|&t| limit.allow(t0 + ms(t), kind))
        .collect()
}

#[test]
fn each_kind_has_a_bound_of_its_own() {
    let t0 = Instant::now();
    let mut limit = Limit::new();
    assert_eq!(
        allowed(&mut limit, t0, Kind::BoardInfo, 0, 100).len(),
        PER_SPAN
    );
    // The board info's bound is used up; the others still answer.
    for kind in [Kind::ProductInfo, Kind::NetworkInfo, Kind::Op0077] {
        assert!(limit.allow(t0 + ms(100), kind), "{kind:?}");
    }
    assert!(!limit.allow(t0 + ms(100), Kind::BoardInfo));
}

#[test]
fn a_request_without_a_reply_is_never_bounded_nor_counted() {
    let t0 = Instant::now();
    let mut limit = Limit::new();
    for i in 0..1_000 {
        assert!(limit.allow(t0 + ms(i), Kind::ClockStats));
        assert!(limit.allow(t0 + ms(i), Kind::Unknown));
    }
    assert_eq!(limit.flush(t0 + 10 * REPORT_EVERY), None);
}

#[test]
fn nothing_dropped_reports_nothing() {
    let t0 = Instant::now();
    let mut limit = Limit::new();
    for i in 0..PER_SPAN as u64 {
        assert!(limit.allow(t0 + ms(i), Kind::BoardInfo));
    }
    for later in [0, 59_999, 60_000, 3_600_000] {
        assert_eq!(limit.flush(t0 + ms(later)), None, "{later} ms");
    }
}

#[test]
fn the_drops_are_reported_once_a_minute_from_the_first_of_them() {
    let t0 = Instant::now();
    let mut limit = Limit::new();
    // A thousand board info requests and twenty network info requests in
    // the first second; the first drop is the eleventh board info request.
    allowed(&mut limit, t0, Kind::BoardInfo, 0, 1_000);
    allowed(&mut limit, t0, Kind::NetworkInfo, 0, 15);
    let first_drop = t0 + ms(10);
    assert_eq!(
        limit.flush(first_drop + ms(59_999)),
        None,
        "a minute has not run"
    );
    let report = limit
        .flush(first_drop + REPORT_EVERY)
        .expect("a minute has run");
    assert_eq!(
        report.counts,
        [(Kind::BoardInfo, 990), (Kind::NetworkInfo, 5)]
    );
    assert_eq!(report.total(), 995);
    // Reported once: nothing was dropped since.
    assert_eq!(limit.flush(first_drop + REPORT_EVERY + ms(1)), None);
    assert_eq!(limit.flush(t0 + 10 * REPORT_EVERY), None);
    // A drop much later opens the next report's minute at its own time.
    let later = t0 + 10 * REPORT_EVERY;
    allowed(&mut limit, later, Kind::Op0077, 0, 11);
    let dropped_at = later + ms(10);
    assert_eq!(limit.flush(dropped_at + ms(59_999)), None);
    let next = limit
        .flush(dropped_at + REPORT_EVERY)
        .expect("the next minute");
    assert_eq!(next.counts, [(Kind::Op0077, 1)]);
}

#[test]
fn a_flood_of_every_kind_is_one_report_a_minute_with_every_drop_counted() {
    // Three minutes of 100 requests a second of each kind, the server's
    // check every second in between.
    let t0 = Instant::now();
    let mut limit = Limit::new();
    let mut reports = Vec::new();
    let mut dropped = 0u64;
    let mut answered = 0u64;
    for tick in 0..18_000u64 {
        let now = t0 + ms(tick * 10);
        for kind in BOUNDED {
            if limit.allow(now, kind) {
                answered += 1;
            } else {
                dropped += 1;
            }
        }
        if (tick + 1).is_multiple_of(100) {
            reports.extend(limit.flush(now));
        }
    }
    reports.extend(limit.flush(t0 + ms(180_000) + REPORT_EVERY));
    assert_eq!(
        answered,
        BOUNDED.len() as u64 * 10 * 180,
        "ten a second per kind"
    );
    let counted: u64 = reports.iter().map(Dropped::total).sum();
    assert_eq!(counted, dropped);
    assert!((3..=4).contains(&reports.len()), "{}", reports.len());
}

#[test]
fn a_report_reads_as_kinds_opcodes_and_counts() {
    let report = Dropped {
        counts: vec![
            (Kind::BoardInfo, 990),
            (Kind::ProductInfo, 3),
            (Kind::NetworkInfo, 5),
            (Kind::Op0077, 1),
        ],
    };
    assert_eq!(
        report.to_string(),
        "0x61 board info 990, 0xc1 product info 3, 0x13 network info 5, 0x77 1"
    );
    assert_eq!(report.total(), 999);
}

#[test]
fn room_says_what_allow_would_and_counts_nothing() {
    let t0 = Instant::now();
    let mut limit = Limit::new();
    for i in 0..10 {
        assert!(limit.room(t0 + ms(i), Kind::ChannelChange));
        assert!(limit.allow(t0 + ms(i), Kind::ChannelChange));
    }
    // The bound is reached: room says no, as often as asked, and nothing is
    // a drop (the notification waits, it is never dropped).
    for i in 10..1_000 {
        assert!(!limit.room(t0 + ms(i), Kind::ChannelChange), "{i}");
    }
    assert_eq!(limit.flush(t0 + ms(1_000) + REPORT_EVERY), None);
    // A span after the oldest of the ten: room again.
    assert!(limit.room(t0 + SPAN, Kind::ChannelChange));
    // The other kinds were never touched; a kind without a reply always may.
    assert!(limit.room(t0, Kind::BoardInfo));
    assert!(limit.room(t0, Kind::Unknown));
    // A drop of the notification's kind, where allow is asked anyway, reads
    // as its name.
    assert!(!limit.allow(t0 + ms(999), Kind::ChannelChange));
    let report = limit.flush(t0 + ms(999) + REPORT_EVERY).unwrap();
    assert_eq!(report.to_string(), "channel change 1");
}
