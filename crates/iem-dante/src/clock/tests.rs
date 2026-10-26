use super::*;

/// The fleet's D (wall = PTP + D), a grandmaster up 100 s, a box up 50 s.
const D: i64 = 1_790_000_000_000_000_000;
const PTP: i64 = 100_000_000_000;
const MONO: i64 = 50_000_000_000;
const K: i64 = PTP - MONO;

fn reading(date: Option<DateState>) -> Reading {
    Reading {
        wall_ns: D + PTP,
        locked: true,
        settled: true,
        mode: 3,
        freq_ppb: -750,
        date,
    }
}

/// D in effect, the last step 10 minutes ago.
fn steady(d: i64) -> Option<DateState> {
    Some(DateState {
        d_ns: d,
        step_at_ptp_ns: Some(PTP - 600_000_000_000),
        slewing: false,
    })
}

#[test]
fn k_is_wall_minus_d_minus_mono() {
    let r = reading(steady(D));
    assert_eq!(sample(&r, D + PTP, MONO), Sample::Usable { k_ns: K });
    // The box's wall is read, never dantesync's: 3 ms later both clocks
    // moved on and k is the same.
    assert_eq!(
        sample(&r, D + PTP + 3_000_000, MONO + 3_000_000),
        Sample::Usable { k_ns: K }
    );
    // A grandmaster that booted after the box: k is negative.
    assert_eq!(
        sample(&r, D + 1_000, MONO),
        Sample::Usable { k_ns: 1_000 - MONO }
    );
}

#[test]
fn a_date_step_moves_wall_and_d_together_and_k_stays() {
    for step in [200_000_000, -200_000_000] {
        let r = reading(steady(D + step));
        assert_eq!(
            sample(&r, D + step + PTP, MONO),
            Sample::Usable { k_ns: K },
            "{step}"
        );
    }
    // A wall that stepped while the D read stayed (a reply built across the
    // step) is a different k: one odd poll, which the tracker holds.
    let r = reading(steady(D));
    assert_eq!(
        sample(&r, D + PTP + 200_000_000, MONO),
        Sample::Usable {
            k_ns: K + 200_000_000
        }
    );
}

#[test]
fn no_anchor_within_two_seconds_of_a_scheduled_step() {
    let at = |step_at: i64| {
        let date = DateState {
            d_ns: D,
            step_at_ptp_ns: Some(step_at),
            slewing: false,
        };
        sample(&reading(Some(date)), D + PTP, MONO)
    };
    // Due within 2 s, or done within 2 s: no anchor.
    assert_eq!(at(PTP + STEP_GUARD_NS), Sample::StepNear);
    assert_eq!(at(PTP + 1), Sample::StepNear);
    assert_eq!(at(PTP), Sample::StepNear);
    assert_eq!(at(PTP - STEP_GUARD_NS), Sample::StepNear);
    // Further away: usable.
    assert_eq!(at(PTP + STEP_GUARD_NS + 1), Sample::Usable { k_ns: K });
    assert_eq!(at(PTP - STEP_GUARD_NS - 1), Sample::Usable { k_ns: K });
    // No step named (a slew's announce): usable when no slew runs.
    let date = DateState {
        d_ns: D,
        step_at_ptp_ns: None,
        slewing: false,
    };
    assert_eq!(
        sample(&reading(Some(date)), D + PTP, MONO),
        Sample::Usable { k_ns: K }
    );
}

#[test]
fn none_while_a_slew_runs() {
    let date = DateState {
        d_ns: D,
        step_at_ptp_ns: None,
        slewing: true,
    };
    assert_eq!(sample(&reading(Some(date)), D + PTP, MONO), Sample::Slewing);
    // The tracker takes none of it.
    let mut t = Tracker::new();
    assert_eq!(t.feed(Sample::Slewing), Event::NotReady);
    assert_eq!(t.feed(Sample::StepNear), Event::NotReady);
    assert_eq!(t.k(), None);
}

#[test]
fn not_locked_is_not_ready() {
    let mut r = reading(steady(D));
    r.locked = false;
    assert_eq!(sample(&r, D + PTP, MONO), Sample::NotLocked);
    // Unlocked wins over every other reason.
    r.date = Some(DateState {
        d_ns: D,
        step_at_ptp_ns: Some(PTP),
        slewing: true,
    });
    assert_eq!(sample(&r, D + PTP, MONO), Sample::NotLocked);
    r.date = None;
    assert_eq!(sample(&r, D + PTP, MONO), Sample::NotLocked);
    // Locked without a date offset: no D, no k.
    assert_eq!(sample(&reading(None), D + PTP, MONO), Sample::NoDateState);
    let mut t = Tracker::new();
    for s in [
        Sample::NotLocked,
        Sample::NoDateState,
        Sample::NoReply,
        Sample::StepNear,
    ] {
        assert_eq!(t.feed(s), Event::NotReady, "{s:?}");
    }
    assert_eq!(t.k(), None);
    assert_eq!(t.feed(Sample::Usable { k_ns: K }), Event::Ready { k_ns: K });
    assert_eq!(t.k(), Some(K));
}

#[test]
fn one_odd_poll_is_held_three_re_anchor() {
    let mut t = Tracker::new();
    assert_eq!(t.feed(Sample::Usable { k_ns: K }), Event::Ready { k_ns: K });
    // Polls that agree within the tolerance keep k as it was.
    assert_eq!(
        t.feed(Sample::Usable {
            k_ns: K + TOLERANCE_NS
        }),
        Event::Held
    );
    assert_eq!(
        t.feed(Sample::Usable {
            k_ns: K - TOLERANCE_NS
        }),
        Event::Held
    );
    assert_eq!(t.k(), Some(K));
    // One odd poll is held; an agreeing one ends the run.
    let odd = K + 200_000_000;
    assert_eq!(t.feed(Sample::Usable { k_ns: odd }), Event::Held);
    assert_eq!(t.feed(Sample::Usable { k_ns: odd }), Event::Held);
    assert_eq!(t.feed(Sample::Usable { k_ns: K }), Event::Held);
    assert_eq!(t.feed(Sample::Usable { k_ns: odd }), Event::Held);
    assert_eq!(t.feed(Sample::Usable { k_ns: odd }), Event::Held);
    assert_eq!(t.k(), Some(K));
    // The third in a row re-anchors, to the latest.
    assert_eq!(
        t.feed(Sample::Usable { k_ns: odd + 5 }),
        Event::ReAnchored {
            delta_ns: 200_000_005
        }
    );
    assert_eq!(t.k(), Some(odd + 5));
    // From there, the old k is the odd one.
    assert_eq!(t.feed(Sample::Usable { k_ns: odd }), Event::Held);
    assert_eq!(t.feed(Sample::Usable { k_ns: K }), Event::Held);
    assert_eq!(t.feed(Sample::Usable { k_ns: K }), Event::Held);
    assert_eq!(
        t.feed(Sample::Usable { k_ns: K - 7 }),
        Event::ReAnchored {
            delta_ns: K - 7 - (odd + 5)
        }
    );
}

#[test]
fn a_k_just_past_the_tolerance_is_odd_and_odd_polls_must_agree() {
    let mut t = Tracker::new();
    t.feed(Sample::Usable { k_ns: K });
    // Three polls each just past the tolerance, agreeing with each other.
    for _ in 1..CONFIRM {
        assert_eq!(
            t.feed(Sample::Usable {
                k_ns: K + TOLERANCE_NS + 1
            }),
            Event::Held
        );
    }
    assert_eq!(
        t.feed(Sample::Usable {
            k_ns: K + TOLERANCE_NS + 1
        }),
        Event::ReAnchored {
            delta_ns: TOLERANCE_NS + 1
        }
    );
    // Odd polls that disagree with each other start the count again.
    let mut t = Tracker::new();
    t.feed(Sample::Usable { k_ns: K });
    let a = K + 1_000_000;
    let b = a + TOLERANCE_NS + 1;
    for k in [a, a, b, b, a, a] {
        assert_eq!(t.feed(Sample::Usable { k_ns: k }), Event::Held, "{k}");
    }
    assert_eq!(t.k(), Some(K));
    // Odd polls within the tolerance of the first odd one count together.
    assert_eq!(
        t.feed(Sample::Usable {
            k_ns: a + TOLERANCE_NS
        }),
        Event::ReAnchored {
            delta_ns: 1_000_000 + TOLERANCE_NS
        }
    );
    // An unusable poll breaks a run too.
    let mut t = Tracker::new();
    t.feed(Sample::Usable { k_ns: K });
    for s in [
        Sample::Usable { k_ns: a },
        Sample::Usable { k_ns: a },
        Sample::NotLocked,
        Sample::Usable { k_ns: a },
        Sample::Usable { k_ns: a },
    ] {
        assert_ne!(
            t.feed(s),
            Event::ReAnchored {
                delta_ns: 1_000_000
            },
            "{s:?}"
        );
    }
    assert_eq!(t.k(), Some(K));
}

#[test]
fn an_unlock_after_ready_is_holdover_and_keeps_k() {
    let mut t = Tracker::new();
    t.feed(Sample::Usable { k_ns: K });
    for s in [
        Sample::NotLocked,
        Sample::NoReply,
        Sample::NoDateState,
        Sample::StepNear,
        Sample::Slewing,
    ] {
        assert_eq!(t.feed(s), Event::Holdover, "{s:?}");
        assert_eq!(t.k(), Some(K));
    }
    // Back with the same k: held, no re-anchor; the clock never stopped.
    assert_eq!(t.feed(Sample::Usable { k_ns: K + 3 }), Event::Held);
    assert_eq!(t.k(), Some(K));
}

#[test]
fn media_time_survives_years_of_uptime() {
    // Ten years of a grandmaster's uptime at 96 kHz.
    let ten_years_ns: u64 = 10 * 365 * 86_400 * 1_000_000_000;
    assert_eq!(to_samples(ten_years_ns, 96_000), 10 * 365 * 86_400 * 96_000);
    // Rounded down, never up.
    assert_eq!(to_samples(10_416, 96_000), 0);
    assert_eq!(to_samples(10_417, 96_000), 1);
    assert_eq!(to_samples(1_000_000_000, 48_000), 48_000);
    // The whole u64 range: no overflow (u64::MAX ns × 192 kHz).
    assert_eq!(
        to_samples(u64::MAX, 192_000),
        ((u64::MAX as u128) * 192_000 / 1_000_000_000) as u64
    );
    // A stamp and its sample index round-trip after ten years too.
    let n = to_samples(ten_years_ns, 96_000) + 12_345;
    let (secs, sub) = stamp(n, 96_000);
    assert_eq!(u64::from(secs), n / 96_000);
    assert_eq!(sub, 12_345);
    assert_eq!(from_stamp(secs, sub, 96_000, n + 96_000 * 3_600), n);
    // A rate of 0 reads as 1 and never divides by zero.
    assert_eq!(to_samples(3_000_000_000, 0), 3);
    assert_eq!(stamp(7, 0), (7, 0));
    assert_eq!(from_stamp(7, 0, 0, 9), 7);
}

#[test]
fn stamps_split_and_join_across_the_seconds_wrap() {
    let rate = 96_000u32;
    let wrap = (1u64 << 32) * u64::from(rate);
    for n in [wrap - 96_001, wrap - 1, wrap, wrap + 1, wrap + 95_999] {
        let (secs, sub) = stamp(n, rate);
        assert_eq!(u64::from(secs), (n / u64::from(rate)) % (1 << 32), "{n}");
        assert_eq!(u64::from(sub), n % u64::from(rate), "{n}");
        // Joined near the clock, on either side of the wrap.
        for near in [n - 10_000, n, n + 10_000, wrap] {
            assert_eq!(from_stamp(secs, sub, rate, near), n, "{n} near {near}");
        }
    }
    assert_eq!(stamp(wrap, rate), (0, 0));
    assert_eq!(stamp(wrap - 1, rate), (u32::MAX, rate - 1));
    // The nearest of the stamp's repeats, never more than half a period
    // away.
    let n = 5 * wrap + 777;
    let (secs, sub) = stamp(n, rate);
    assert_eq!(from_stamp(secs, sub, rate, n + wrap / 2 - 1), n);
    assert_eq!(from_stamp(secs, sub, rate, n - wrap / 2 + 1), n);
    assert_eq!(from_stamp(secs, sub, rate, n + wrap / 2 + 1), n + wrap);
    assert_eq!(from_stamp(secs, sub, rate, n - wrap / 2 - 1), n - wrap);
    // A tie goes to the later one.
    assert_eq!(from_stamp(secs, sub, rate, n + wrap / 2), n + wrap);
    // Near the clock's start the stamp never maps below zero.
    assert_eq!(from_stamp(u32::MAX, 5, rate, 3), wrap - u64::from(rate) + 5);
    assert_eq!(from_stamp(0, 5, rate, 3), 5);
    assert_eq!(from_stamp(0, 0, rate, 10), 0);
}

#[test]
fn the_manual_clock_moves_only_when_asked() {
    let c = ManualClock::new(1_000);
    assert_eq!(c.now_ns(), Some(1_000));
    assert_eq!(c.now_ns(), Some(1_000));
    c.sleep_until(5_000);
    assert_eq!(c.now_ns(), Some(5_000));
    // A past time does not move it back.
    c.sleep_until(2_000);
    assert_eq!(c.now_ns(), Some(5_000));
    c.advance(10);
    assert_eq!(c.now_ns(), Some(5_010));
    assert_eq!(c.epoch(), 0);
    c.re_anchor(-3_000);
    assert_eq!((c.now_ns(), c.epoch()), (Some(2_010), 1));
    c.re_anchor(500);
    assert_eq!((c.now_ns(), c.epoch()), (Some(2_510), 2));
    c.set_ready(false);
    assert_eq!(c.now_ns(), None);
    c.set_ready(true);
    assert_eq!(c.now_ns(), Some(2_510));
}

#[cfg(target_os = "linux")]
mod linux {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::super::*;
    use crate::clock::dsyx::tests::locked_reply;

    /// One hour: a k whose sign error would sleep for hours.
    const HOUR: i64 = 3_600_000_000_000;

    /// The kernel's clock `id`, read here independently of the module.
    fn kernel(id: libc::clockid_t) -> i64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: a valid, writable timespec.
        unsafe { libc::clock_gettime(id, &mut ts) };
        ts.tv_sec * 1_000_000_000 + ts.tv_nsec
    }

    #[test]
    fn the_box_clocks_read_the_kernel_s() {
        for _ in 0..1_000 {
            let before = kernel(libc::CLOCK_MONOTONIC);
            let m = monotonic_ns();
            let after = kernel(libc::CLOCK_MONOTONIC);
            assert!(before <= m && m <= after, "{before} {m} {after}");
            let before = kernel(libc::CLOCK_REALTIME);
            let w = realtime_ns();
            let after = kernel(libc::CLOCK_REALTIME);
            assert!(before <= w && w <= after, "{before} {w} {after}");
            // After 2026-01-01 and before 2100.
            assert!(
                w > 1_767_225_600_000_000_000 && w < 4_102_444_800_000_000_000,
                "{w}"
            );
        }
    }

    #[test]
    fn the_mono_clock_is_monotonic_plus_k_and_counts_re_anchors() {
        let c = MonoClock::new();
        assert_eq!((c.now_ns(), c.k_ns(), c.epoch()), (None, None, 0));
        c.set_k(HOUR);
        let before = monotonic_ns();
        let now = c.now_ns().unwrap() as i64;
        let after = monotonic_ns();
        assert!(
            now >= before + HOUR && now <= after + HOUR,
            "{before} {now} {after}"
        );
        assert_eq!((c.k_ns(), c.epoch()), (Some(HOUR), 0));
        // The same k again changes nothing; another k re-anchors.
        c.set_k(HOUR);
        assert_eq!(c.epoch(), 0);
        c.set_k(HOUR - 5);
        assert_eq!((c.k_ns(), c.epoch()), (Some(HOUR - 5), 1));
        c.set_k(HOUR + 7);
        assert_eq!(c.epoch(), 2);
        // A k that would put PTP time below zero reads as not ready.
        c.set_k(-monotonic_ns() - HOUR);
        assert_eq!(c.now_ns(), None);
    }

    /// `sleep_until` on a thread, bounded at 5 s: a mutant that sleeps for
    /// hours fails here instead of hanging the test run.
    fn sleeps_for(c: Arc<MonoClock>, ptp_ns: u64) -> Duration {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let t0 = Instant::now();
            c.sleep_until(ptp_ns);
            let _ = tx.send(t0.elapsed());
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("sleep_until returned within 5 s")
    }

    #[test]
    fn the_mono_clock_sleeps_until_a_ptp_time() {
        let c = Arc::new(MonoClock::new());
        c.set_k(HOUR);
        let target = c.now_ns().unwrap() + 30_000_000;
        let slept = sleeps_for(c.clone(), target);
        assert!(slept >= Duration::from_millis(29), "{slept:?}");
        assert!(slept < Duration::from_secs(2), "{slept:?}");
        assert!(c.now_ns().unwrap() >= target);
        // A past time returns at once.
        let slept = sleeps_for(c.clone(), target - 1_000_000_000);
        assert!(slept < Duration::from_millis(20), "{slept:?}");
        // A negative k: the same. Smaller than the time since boot, which a
        // fresh CI runner keeps under an hour (CI run 40289615289: k = -1 h
        // made the PTP time negative, so now_ns was None).
        c.set_k(-1_000_000);
        let target = c.now_ns().unwrap() + 30_000_000;
        let slept = sleeps_for(c.clone(), target);
        assert!(slept >= Duration::from_millis(29), "{slept:?}");
        assert!(slept < Duration::from_secs(2), "{slept:?}");
        // Before the clock is ready: a short nap, whatever the time asked.
        let c = Arc::new(MonoClock::new());
        let slept = sleeps_for(c, u64::MAX);
        assert!(slept >= Duration::from_millis(9), "{slept:?}");
        assert!(slept < Duration::from_secs(2), "{slept:?}");
    }

    /// The poller against a stand-in for dantesync's time server on
    /// loopback: a locked reply with D, the last step long ago.
    #[tokio::test]
    async fn the_poller_anchors_the_clock_from_dantesync_s_reply() {
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();
        let d: i64 = 1_790_000_000_000_000_000;
        let serving = tokio::spawn(async move {
            let mut buf = [0u8; 128];
            loop {
                let Ok((n, from)) = server.recv_from(&mut buf).await else {
                    return;
                };
                assert_eq!(n, dsyx::REQUEST_LEN);
                assert_eq!(&buf[0..4], b"DSYX");
                let id = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
                // A stale reply to another request first: skipped.
                let wall = realtime_ns();
                let stale = locked_reply(id.wrapping_sub(1), wall, d, wall - d - HOUR, 1);
                server.send_to(&stale, from).await.unwrap();
                let reply = locked_reply(id, wall, d, wall - d - HOUR, -750);
                server.send_to(&reply, from).await.unwrap();
            }
        });
        let clock = Arc::new(MonoClock::new());
        let freq = Arc::new(AtomicI32::new(0));
        let polling = tokio::spawn(crate::clock::linux::poll(addr, clock.clone(), freq.clone()));
        let t0 = Instant::now();
        while clock.k_ns().is_none() && t0.elapsed() < Duration::from_secs(5) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        polling.abort();
        serving.abort();
        assert!(clock.k_ns().is_some(), "ready within 5 s");
        assert_eq!(freq.load(Ordering::Relaxed), -750);
        // PTP now = the box's wall less D, within the loop's few ms.
        let ptp = clock.now_ns().unwrap() as i64;
        let expect = realtime_ns() - d;
        assert!((ptp - expect).abs() < 50_000_000, "{ptp} {expect}");
        assert_eq!(clock.epoch(), 0);
    }

    /// The stand-in answers the next request with `make(its id)`, or not at
    /// all; returns the reply sent.
    async fn answer(
        server: &tokio::net::UdpSocket,
        make: Option<fn(u32) -> Vec<u8>>,
    ) -> Option<(u32, Vec<u8>)> {
        let mut buf = [0u8; 128];
        let (n, from) = server.recv_from(&mut buf).await.unwrap();
        assert_eq!((n, &buf[0..4]), (dsyx::REQUEST_LEN, &b"DSYX"[..]));
        let id = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let reply = make?(id);
        server.send_to(&reply, from).await.unwrap();
        Some((id, reply))
    }

    const D: i64 = 1_790_000_000_000_000_000;

    /// A locked reply now, the last step an hour ago.
    fn locked_now(id: u32) -> Vec<u8> {
        let wall = realtime_ns();
        locked_reply(id, wall, D, wall - D - HOUR, -750)
    }

    /// The same, not locked.
    fn unlocked_now(id: u32) -> Vec<u8> {
        let mut b = locked_now(id);
        b[41] = 0;
        b[36..40].copy_from_slice(&12_000i32.to_be_bytes());
        b
    }

    /// One poll, the stand-in answering it with `make` (or not), bounded at
    /// 5 s.
    async fn step(
        poller: &mut Poller,
        clock: &MonoClock,
        freq: &AtomicI32,
        server: &tokio::net::UdpSocket,
        make: Option<fn(u32) -> Vec<u8>>,
    ) -> (Polled, Option<(u32, Vec<u8>)>) {
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(poller.once(clock, freq), answer(server, make))
        })
        .await
        .expect("a poll within 5 s")
    }

    /// One poll at a time (the `clock` subcommand's step): each reports the
    /// reply, its sample, the event and the anchored k, and sets the clock
    /// and the frequency as the service's poller does.
    #[tokio::test]
    async fn a_poll_reports_its_reply_sample_event_and_k() {
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();
        let clock = MonoClock::new();
        let freq = AtomicI32::new(0);
        let mut poller = Poller::connect(addr).await.unwrap();

        // Not locked: no k.
        let (p, sent) = step(&mut poller, &clock, &freq, &server, Some(unlocked_now)).await;
        let (id, reply) = sent.unwrap();
        let r = dsyx::parse(&reply, id).unwrap();
        assert!(!r.locked);
        assert_eq!(
            p,
            Polled {
                reading: Some(r),
                sample: Sample::NotLocked,
                event: Event::NotReady,
                k_ns: None,
            }
        );
        assert_eq!((clock.k_ns(), freq.load(Ordering::Relaxed)), (None, 12_000));

        // Locked: the first k, the clock ready with it.
        let (p, sent) = step(&mut poller, &clock, &freq, &server, Some(locked_now)).await;
        let (id, reply) = sent.unwrap();
        assert_eq!(p.reading, Some(dsyx::parse(&reply, id).unwrap()));
        let Sample::Usable { k_ns } = p.sample else {
            panic!("{p:?}");
        };
        assert_eq!(
            (p.event, p.k_ns, clock.k_ns()),
            (Event::Ready { k_ns }, Some(k_ns), Some(k_ns))
        );
        assert_eq!(freq.load(Ordering::Relaxed), -750);
        // k = PTP now less the box's monotonic clock, within the loop's ms.
        let expect = realtime_ns() - D - monotonic_ns();
        assert!((k_ns - expect).abs() < 50_000_000, "{k_ns} {expect}");

        // Silence: holdover, k and the frequency kept.
        let (p, sent) = step(&mut poller, &clock, &freq, &server, None).await;
        assert_eq!(sent, None);
        assert_eq!(
            p,
            Polled {
                reading: None,
                sample: Sample::NoReply,
                event: Event::Holdover,
                k_ns: Some(k_ns),
            }
        );
        assert_eq!(
            (clock.k_ns(), clock.epoch(), freq.load(Ordering::Relaxed)),
            (Some(k_ns), 0, -750)
        );
    }

    /// No answer: the clock stays not ready and the poller keeps polling.
    #[tokio::test]
    async fn the_poller_waits_for_a_silent_dantesync() {
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();
        let clock = Arc::new(MonoClock::new());
        let freq = Arc::new(AtomicI32::new(7));
        let polling = tokio::spawn(crate::clock::linux::poll(addr, clock.clone(), freq.clone()));
        let mut buf = [0u8; 128];
        for _ in 0..2 {
            let got =
                tokio::time::timeout(Duration::from_secs(3), server.recv_from(&mut buf)).await;
            assert!(matches!(got, Ok(Ok((64, _)))), "a poll a second");
        }
        polling.abort();
        assert_eq!((clock.k_ns(), freq.load(Ordering::Relaxed)), (None, 7));
    }
}
