use super::*;

use std::net::Ipv4Addr;

/// Synthetic peers (P6).
fn peer(n: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))
}

fn entry(peer: usize, op1: u16, gap: Gap, count: u64) -> Entry {
    Entry {
        peer,
        op1,
        gap,
        count,
    }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn nothing_counted_reports_nothing() {
    let t0 = Instant::now();
    let mut gaps = Gaps::new(WINDOW);
    for later in [0, 59_999, 60_000, 3_600_000] {
        assert_eq!(gaps.flush(t0 + ms(later)), None, "{later} ms");
    }
}

#[test]
fn a_window_reports_once_it_has_run_its_length() {
    let t0 = Instant::now();
    let mut gaps = Gaps::new(WINDOW);
    assert_eq!(gaps.record(t0, peer(1), 0x2204, Gap::Unanswered), None);
    assert_eq!(
        gaps.record(t0 + ms(10), peer(1), 0x2204, Gap::Unanswered),
        None
    );
    assert_eq!(
        gaps.record(t0 + ms(20), peer(1), 0x2200, Gap::Refused),
        None
    );
    assert_eq!(gaps.flush(t0 + ms(59_999)), None, "a minute has not run");
    let report = gaps.flush(t0 + WINDOW).expect("a minute has run");
    assert_eq!(
        report.entries,
        [
            entry(1, 0x2204, Gap::Unanswered, 2),
            entry(1, 0x2200, Gap::Refused, 1)
        ]
    );
    assert_eq!((report.uncounted, report.requests()), (0, 3));
    // Reported once: the window is over and nothing new was counted.
    assert_eq!(gaps.flush(t0 + WINDOW + ms(1)), None);
    assert_eq!(gaps.flush(t0 + 3 * WINDOW), None);
}

#[test]
fn a_request_after_the_window_reports_it_and_opens_the_next() {
    let t0 = Instant::now();
    let mut gaps = Gaps::new(WINDOW);
    gaps.record(t0, peer(1), 0x2204, Gap::Unanswered);
    let report = gaps
        .record(t0 + WINDOW, peer(1), 0x1001, Gap::Unanswered)
        .expect("the first window has run");
    assert_eq!(report.entries, [entry(1, 0x2204, Gap::Unanswered, 1)]);
    // The request that closed it opened the next window, at its own time.
    assert_eq!(gaps.flush(t0 + WINDOW + ms(59_999)), None);
    let next = gaps.flush(t0 + 2 * WINDOW).expect("the second window");
    assert_eq!(next.entries, [entry(1, 0x1001, Gap::Unanswered, 1)]);
}

#[test]
fn a_quiet_spell_starts_the_next_window_at_its_first_request() {
    let t0 = Instant::now();
    let mut gaps = Gaps::new(WINDOW);
    gaps.record(t0, peer(1), 0x2204, Gap::Unanswered);
    assert!(gaps.flush(t0 + WINDOW).is_some());
    // Ten minutes later one request: its window runs from it.
    let later = t0 + 10 * WINDOW;
    assert_eq!(gaps.record(later, peer(1), 0x2204, Gap::Unanswered), None);
    assert_eq!(gaps.flush(later + ms(59_999)), None);
    assert_eq!(gaps.flush(later + WINDOW).map(|r| r.requests()), Some(1));
}

#[test]
fn peers_are_named_by_ids_in_the_order_first_seen() {
    let t0 = Instant::now();
    let mut gaps = Gaps::new(WINDOW);
    gaps.record(t0, peer(7), 0x2204, Gap::Unanswered);
    gaps.record(t0, peer(3), 0x2204, Gap::Unanswered);
    gaps.record(t0, peer(3), 0x2204, Gap::Unanswered);
    gaps.record(t0, peer(7), 0x2204, Gap::Refused);
    let report = gaps.flush(t0 + WINDOW).unwrap();
    assert_eq!(
        report.entries,
        [
            entry(2, 0x2204, Gap::Unanswered, 2),
            entry(1, 0x2204, Gap::Refused, 1),
            entry(1, 0x2204, Gap::Unanswered, 1),
        ]
    );
    // The ids stay for the server's life, across windows.
    let t1 = t0 + 2 * WINDOW;
    gaps.record(t1, peer(3), 0x3010, Gap::Unanswered);
    gaps.record(t1, peer(9), 0x3010, Gap::Unanswered);
    gaps.record(t1, peer(7), 0x3010, Gap::Unanswered);
    let report = gaps.flush(t1 + WINDOW).unwrap();
    assert_eq!(
        report.entries,
        [
            entry(1, 0x3010, Gap::Unanswered, 1),
            entry(2, 0x3010, Gap::Unanswered, 1),
            entry(3, 0x3010, Gap::Unanswered, 1),
        ]
    );
}

#[test]
fn past_the_peer_limit_every_new_peer_is_peer_0() {
    let t0 = Instant::now();
    let mut gaps = Gaps::new(WINDOW);
    for n in 0..MAX_PEERS + 3 {
        let addr = IpAddr::V4(Ipv4Addr::new(10, 0, 1, n as u8));
        gaps.record(t0, addr, 0x2204, Gap::Unanswered);
    }
    let report = gaps.flush(t0 + WINDOW).unwrap();
    let ids: Vec<usize> = report.entries.iter().map(|e| e.peer).collect();
    // Peer 0 counts the three past the limit, so it comes first.
    let mut want = vec![0];
    want.extend(1..=MAX_PEERS);
    assert_eq!(ids, want);
    assert_eq!(report.entries[0].count, 3);
    assert_eq!(report.requests(), MAX_PEERS as u64 + 3);
}

#[test]
fn past_the_entry_limit_requests_are_only_summed() {
    let t0 = Instant::now();
    let mut gaps = Gaps::new(WINDOW);
    for op1 in 0..MAX_ENTRIES as u16 {
        gaps.record(t0, peer(1), op1, Gap::Unanswered);
    }
    // A new entry past the limit is summed; a counted one still counts.
    gaps.record(t0, peer(1), 0xff00, Gap::Unanswered);
    gaps.record(t0, peer(1), 0xff01, Gap::Refused);
    gaps.record(t0, peer(1), 0x0000, Gap::Unanswered);
    let report = gaps.flush(t0 + WINDOW).unwrap();
    assert_eq!(report.entries.len(), MAX_ENTRIES);
    assert_eq!(report.entries[0], entry(1, 0x0000, Gap::Unanswered, 2));
    assert_eq!(report.uncounted, 2);
    assert_eq!(report.requests(), MAX_ENTRIES as u64 + 3);
    // The next window starts empty.
    gaps.record(t0 + WINDOW, peer(1), 0xff00, Gap::Unanswered);
    let next = gaps.flush(t0 + 2 * WINDOW).unwrap();
    assert_eq!(next.entries, [entry(1, 0xff00, Gap::Unanswered, 1)]);
    assert_eq!(next.uncounted, 0);
}

#[test]
fn a_flood_is_one_report_a_minute_with_every_request_counted() {
    // The live finding (#72, 2026-10-26): 0x2200 and 0x2204, together about
    // 2 250 a second. Three minutes of it, and the server's check every
    // second in between.
    let t0 = Instant::now();
    let mut gaps = Gaps::new(WINDOW);
    let mut reports = Vec::new();
    let mut sent = 0u64;
    for tick in 0..180_000u64 {
        let now = t0 + ms(tick);
        for (k, op1) in [0x2200, 0x2204, 0x2204].into_iter().enumerate() {
            let gap = if k == 0 {
                Gap::Refused
            } else {
                Gap::Unanswered
            };
            if !(tick + k as u64).is_multiple_of(4) {
                sent += 1;
                reports.extend(gaps.record(now, peer(1), op1, gap));
            }
        }
        if (tick + 1).is_multiple_of(1_000) {
            reports.extend(gaps.flush(now));
        }
    }
    reports.extend(gaps.flush(t0 + ms(180_000)));
    assert_eq!(reports.len(), 3, "one report a minute");
    let counted: u64 = reports.iter().map(Report::requests).sum();
    assert_eq!(counted, sent);
    for report in &reports {
        assert_eq!(report.entries.len(), 2);
        assert_eq!(report.entries[0].op1, 0x2204);
        assert_eq!(report.entries[1].op1, 0x2200);
        assert!(report.requests() > 100_000, "{}", report.requests());
    }
}

#[test]
fn a_report_reads_as_ids_opcodes_and_counts() {
    let report = Report {
        entries: vec![
            entry(1, 0x2204, Gap::Unanswered, 134_999),
            entry(1, 0x2200, Gap::Refused, 3),
            entry(0, 0x0000, Gap::Unanswered, 1),
        ],
        uncounted: 0,
    };
    assert_eq!(
        report.to_string(),
        "peer 1 0x2204 unanswered 134999, peer 1 0x2200 refused 3, peer 0 0x0000 unanswered 1"
    );
    assert_eq!(report.requests(), 135_003);
    // Past LISTED entries the rest is one sum; past MAX_ENTRIES too.
    let report = Report {
        entries: (0..LISTED as u16 + 2)
            .map(|op1| entry(2, 0x1000 + op1, Gap::Refused, 5))
            .collect(),
        uncounted: 7,
    };
    let text = report.to_string();
    assert!(
        text.starts_with("peer 2 0x1000 refused 5, peer 2 0x1001 refused 5, "),
        "{text}"
    );
    assert!(text.contains("peer 2 0x100f refused 5"), "{text}");
    assert!(!text.contains("0x1010"), "{text}");
    assert!(
        text.ends_with(
            "peer 2 0x100f refused 5, 2 more entries with 10 requests, 7 requests past the table"
        ),
        "{text}"
    );
    assert_eq!(report.requests(), 5 * (LISTED as u64 + 2) + 7);
    // Nothing past the table: no tail.
    let report = Report {
        entries: vec![entry(1, 0x3014, Gap::Unanswered, 2)],
        uncounted: 0,
    };
    assert_eq!(report.to_string(), "peer 1 0x3014 unanswered 2");
}

#[test]
fn every_gap_reads_as_its_word() {
    let report = Report {
        entries: vec![
            entry(1, 0x0100, Gap::Refused, 4),
            entry(1, 0x0100, Gap::Unanswered, 3),
            entry(2, 0x0101, Gap::Misframed, 2),
            entry(3, 0x0102, Gap::Limited, 1),
        ],
        uncounted: 0,
    };
    assert_eq!(
        report.to_string(),
        "peer 1 0x0100 refused 4, peer 1 0x0100 unanswered 3, \
         peer 2 0x0101 misframed 2, peer 3 0x0102 limited 1"
    );
}
