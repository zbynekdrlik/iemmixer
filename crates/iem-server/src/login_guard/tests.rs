//! The login guard's tests: limiter tables, client keys, bounded memory, the hashing gate.

use super::*;
use std::net::Ipv4Addr;

fn lan(last: u8) -> ClientKey {
    ClientKey {
        origin: Origin::Lan,
        ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, last)),
    }
}

fn tunnel(last: u8) -> ClientKey {
    ClientKey {
        origin: Origin::Tunnel,
        ip: IpAddr::V4(Ipv4Addr::new(203, 0, 113, last)),
    }
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn headers(cf: Option<&str>) -> HeaderMap {
    let mut h = HeaderMap::new();
    if let Some(value) = cf {
        h.insert("cf-connecting-ip", value.parse().unwrap());
    }
    h
}

#[test]
fn streak_delay_doubles_from_the_third_failure_and_caps_at_sixty_seconds() {
    let table = [
        (0, 0),
        (1, 0),
        (2, 0),
        (3, 1),
        (4, 2),
        (5, 4),
        (6, 8),
        (7, 16),
        (8, 32),
        (9, 60),
        (10, 60),
        (u32::MAX, 60),
    ];
    for (failures, expected) in table {
        assert_eq!(
            streak_delay(failures),
            secs(expected),
            "failures={failures}"
        );
    }
}

#[test]
fn three_failures_then_backoff() {
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    let c = lan(1);
    for _ in 0..3 {
        assert_eq!(guard.check(&c, "member1", t0), Ok(()));
        guard.record_failure(&c, "member1", t0);
    }
    assert_eq!(guard.check(&c, "member1", t0), Err(secs(1)));
    assert_eq!(
        guard.check(&c, "member1", t0 + Duration::from_millis(400)),
        Err(Duration::from_millis(600))
    );
    assert_eq!(guard.check(&c, "member1", t0 + secs(1)), Ok(()));
    guard.record_failure(&c, "member1", t0 + secs(1));
    assert_eq!(guard.check(&c, "member1", t0 + secs(1)), Err(secs(2)));
}

#[test]
fn the_streak_is_per_member() {
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    for _ in 0..3 {
        guard.record_failure(&lan(1), "member1", t0);
    }
    assert_eq!(guard.check(&lan(1), "member2", t0), Ok(()));
}

#[test]
fn success_clears_the_streak() {
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    for _ in 0..3 {
        guard.record_failure(&lan(1), "member1", t0);
    }
    guard.record_success(&lan(1), "member1");
    assert_eq!(guard.check(&lan(1), "member1", t0), Ok(()));
}

#[test]
fn the_streak_decays_after_fifteen_quiet_minutes() {
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    for _ in 0..5 {
        guard.record_failure(&lan(1), "member1", t0);
    }
    assert_eq!(guard.check(&lan(1), "member1", t0 + STREAK_DECAY), Ok(()));
    guard.record_failure(&lan(1), "member1", t0 + STREAK_DECAY);
    assert_eq!(guard.check(&lan(1), "member1", t0 + STREAK_DECAY), Ok(()));
}

#[test]
fn a_streak_survives_fourteen_quiet_minutes() {
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    let c = lan(1);
    for _ in 0..5 {
        guard.record_failure(&c, "member1", t0);
    }
    let later = t0 + secs(14 * 60);
    guard.record_failure(&c, "member1", later);
    assert_eq!(
        guard.check(&c, "member1", later),
        Err(secs(8)),
        "the sixth failure of the streak owes 8 s"
    );
}

#[test]
fn a_failure_after_fifteen_quiet_minutes_starts_a_new_streak() {
    // No check in between: record_failure itself forgets the old streak.
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    let c = lan(1);
    for _ in 0..5 {
        guard.record_failure(&c, "member1", t0);
    }
    let later = t0 + STREAK_DECAY;
    guard.record_failure(&c, "member1", later);
    assert_eq!(guard.check(&c, "member1", later), Ok(()));
}

#[test]
fn a_new_member_streak_keeps_the_other_streaks() {
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    let c = lan(1);
    for _ in 0..3 {
        guard.record_failure(&c, "member1", t0);
    }
    guard.record_failure(&c, "member2", t0);
    assert_eq!(guard.check(&c, "member1", t0), Err(secs(1)));
}

#[test]
fn a_new_client_keeps_the_other_client_windows() {
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    for i in 0..20u64 {
        guard.record_failure(&lan(5), &format!("member{i}"), t0);
    }
    guard.record_failure(&lan(6), "member1", t0);
    assert_eq!(guard.check(&lan(5), "fresh", t0), Err(CLIENT_SPACING));
    assert_eq!(
        guard.stats(),
        LoginStats {
            lan_failures: 21,
            tunnel_failures: 0,
            engineer_budget_trips: 0
        }
    );
}

#[test]
fn the_client_budget_spans_ten_minutes() {
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    let c = lan(7);
    for i in 0..19u64 {
        guard.record_failure(&c, &format!("member{i}"), t0);
    }
    let later = t0 + secs(9 * 60);
    guard.record_failure(&c, "member19", later);
    assert_eq!(guard.check(&c, "fresh", later), Err(CLIENT_SPACING));
}

#[test]
fn the_engineer_budget_counts_the_last_hour_only() {
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    for i in 0..31u8 {
        guard.record_failure(&tunnel(i), &format!("member{i}"), t0);
    }
    let within = t0 + secs(59 * 60);
    assert_eq!(guard.check(&tunnel(200), "member1", within), Ok(()));
    assert_eq!(
        guard.check(&tunnel(201), "member2", within),
        Err(ENGINEER_SPACING),
        "59 minutes later the origin is still over its budget"
    );
    let after = t0 + ENGINEER_WINDOW;
    assert_eq!(guard.check(&tunnel(202), "member1", after), Ok(()));
    assert_eq!(
        guard.check(&tunnel(203), "member2", after),
        Ok(()),
        "an hour after the failures the origin is no longer spaced"
    );
}

#[test]
fn lan_and_tunnel_budgets_are_separate_for_the_same_address() {
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    let ip = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7));
    let via_tunnel = ClientKey {
        origin: Origin::Tunnel,
        ip,
    };
    let on_lan = ClientKey {
        origin: Origin::Lan,
        ip,
    };
    for _ in 0..3 {
        guard.record_failure(&via_tunnel, "member1", t0);
    }
    assert!(guard.check(&via_tunnel, "member1", t0).is_err());
    assert_eq!(guard.check(&on_lan, "member1", t0), Ok(()));
}

#[test]
fn a_client_is_spaced_sixty_seconds_after_twenty_failures_in_ten_minutes() {
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    let c = lan(2);
    for i in 0..20u64 {
        let member = format!("member{i}");
        assert_eq!(guard.check(&c, &member, t0 + secs(i)), Ok(()));
        guard.record_failure(&c, &member, t0 + secs(i));
    }
    let last = t0 + secs(19);
    assert_eq!(guard.check(&c, "fresh", last + secs(10)), Err(secs(50)));
    assert_eq!(guard.check(&c, "fresh", last + secs(60)), Ok(()));
}

#[test]
fn old_failures_leave_the_client_window() {
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    for i in 0..20u64 {
        guard.record_failure(&lan(3), &format!("member{i}"), t0);
    }
    assert!(guard.check(&lan(3), "fresh", t0 + secs(1)).is_err());
    assert_eq!(guard.check(&lan(3), "fresh", t0 + CLIENT_WINDOW), Ok(()));
}

#[test]
fn engineer_budget_spaces_a_whole_origin_after_thirty_failures_an_hour() {
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    let mut effects = Vec::new();
    for i in 0..31u8 {
        let member = format!("member{i}");
        assert_eq!(guard.check(&tunnel(i), &member, t0), Ok(()));
        effects.push(guard.record_failure(&tunnel(i), &member, t0));
    }
    assert_eq!(
        effects
            .iter()
            .filter(|e| **e == FailureEffect::EngineerBudgetExhausted)
            .count(),
        1
    );
    assert_eq!(effects[30], FailureEffect::EngineerBudgetExhausted);
    assert_eq!(
        guard.check(&tunnel(200), "member1", t0),
        Err(ENGINEER_SPACING)
    );
    assert_eq!(
        guard.check(&tunnel(200), "member1", t0 + ENGINEER_SPACING),
        Ok(())
    );
    assert_eq!(
        guard.check(&tunnel(201), "member2", t0 + ENGINEER_SPACING + secs(1)),
        Err(secs(4))
    );
    assert_eq!(
        guard.check(&lan(9), "member1", t0),
        Ok(()),
        "LAN is never slowed by tunnel failures"
    );
    assert_eq!(
        guard.stats(),
        LoginStats {
            lan_failures: 0,
            tunnel_failures: 31,
            engineer_budget_trips: 1
        }
    );
}

#[test]
fn lan_origin_budget_spacing_is_intended() {
    // By design: one person guessing from the venue network slows every LAN
    // login to one per 5 s once the LAN origin has more than 30 failures in
    // an hour (any member login may carry an engineer-PIN guess); tunnel
    // logins keep their own budget.
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    for i in 0..31u8 {
        guard.record_failure(&lan(i), &format!("member{i}"), t0);
    }
    assert_eq!(guard.check(&lan(200), "member1", t0), Ok(()));
    assert_eq!(guard.check(&lan(201), "member2", t0), Err(ENGINEER_SPACING));
    assert_eq!(guard.check(&tunnel(1), "member1", t0), Ok(()));
}

#[test]
fn in_flight_attempts_are_bounded_by_the_gate() {
    // Admission does not reserve: attempts of one client that are still
    // hashing are all admitted, so their number is bounded only by the
    // hashing gate (HASH_CONCURRENCY running + HASH_QUEUE waiting, see
    // `hash_gate_refuses_beyond_concurrency_plus_queue`); once their
    // failures are recorded they apply to the next attempt.
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    let c = lan(4);
    let in_flight = HASH_CONCURRENCY + HASH_QUEUE;
    for _ in 0..in_flight {
        assert_eq!(guard.check(&c, "member1", t0), Ok(()));
    }
    for _ in 0..in_flight {
        guard.record_failure(&c, "member1", t0);
    }
    assert!(guard.check(&c, "member1", t0).is_err());
}

#[test]
fn every_delay_is_at_most_sixty_seconds() {
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    let c = tunnel(77);
    for n in 0..200u64 {
        let at = t0 + Duration::from_millis(n * 10);
        if let Err(wait) = guard.check(&c, "member1", at) {
            assert!(wait <= MAX_DELAY, "wait {wait:?}");
        }
        guard.record_failure(&c, "member1", at);
    }
}

#[test]
fn tracked_keys_are_bounded() {
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    for i in 0..(MAX_TRACKED as u32 + 10) {
        let c = ClientKey {
            origin: Origin::Lan,
            ip: IpAddr::V4(Ipv4Addr::from(0x0a00_0000 + i)),
        };
        guard.record_failure(&c, "member1", t0 + Duration::from_millis(u64::from(i)));
    }
    let inner = guard.inner.lock().unwrap();
    assert!(inner.streaks.len() <= MAX_TRACKED);
    assert!(inner.clients.len() <= MAX_TRACKED);
    assert!(inner.lan.failures.len() <= ENGINEER_WINDOW_FAILURES + 1);
}

#[test]
fn cf_header_is_trusted_only_from_loopback() {
    let loopback: IpAddr = "127.0.0.1".parse().unwrap();
    assert_eq!(
        ClientKey::from_request(
            loopback,
            &headers(Some("203.0.113.5")),
            &HostAddrs::default()
        ),
        ClientKey {
            origin: Origin::Tunnel,
            ip: "203.0.113.5".parse().unwrap()
        }
    );
    let lan_peer: IpAddr = "10.0.0.20".parse().unwrap();
    assert_eq!(
        ClientKey::from_request(
            lan_peer,
            &headers(Some("203.0.113.5")),
            &HostAddrs::default()
        ),
        ClientKey {
            origin: Origin::Lan,
            ip: lan_peer
        }
    );
}

const HOST: &str = "192.0.2.10";

/// A host with one LAN address besides loopback.
fn host() -> HostAddrs {
    HostAddrs::new([HOST.parse().unwrap()])
}

#[test]
fn cf_header_is_trusted_only_from_the_host() {
    let guard = LoginGuard::for_host(host());
    let cf = headers(Some("198.51.100.7"));
    let tunnel = ClientKey {
        origin: Origin::Tunnel,
        ip: "198.51.100.7".parse().unwrap(),
    };
    assert_eq!(guard.client("127.0.0.1".parse().unwrap(), &cf), tunnel);
    assert_eq!(
        guard.client(HOST.parse().unwrap(), &cf),
        tunnel,
        "the connector on the host's LAN address"
    );
    let outsider: IpAddr = "192.0.2.50".parse().unwrap();
    assert_eq!(
        guard.client(outsider, &cf),
        ClientKey {
            origin: Origin::Lan,
            ip: outsider
        },
        "a forged header from another machine changes nothing"
    );
    let host_ip: IpAddr = HOST.parse().unwrap();
    assert_eq!(
        LoginGuard::new().client(host_ip, &cf),
        ClientKey {
            origin: Origin::Lan,
            ip: host_ip
        },
        "without the host's addresses only loopback counts"
    );
    assert_eq!(
        guard.client(host_ip, &headers(Some("not-an-ip"))),
        ClientKey {
            origin: Origin::Lan,
            ip: host_ip
        },
        "a malformed header falls back to the peer"
    );
}

#[test]
fn a_forged_header_limits_only_its_sender() {
    let guard = LoginGuard::for_host(host());
    let t0 = Instant::now();
    let outsider: IpAddr = "192.0.2.50".parse().unwrap();
    let forged = guard.client(outsider, &headers(Some("198.51.100.7")));
    for _ in 0..3 {
        guard.record_failure(&forged, "member1", t0);
    }
    let plain = guard.client(outsider, &headers(None));
    assert_eq!(
        guard.check(&plain, "member1", t0),
        Err(secs(1)),
        "limited per 192.0.2.50"
    );
    let named = guard.client("127.0.0.1".parse().unwrap(), &headers(Some("198.51.100.7")));
    assert_eq!(
        guard.check(&named, "member1", t0),
        Ok(()),
        "the address in the header is untouched"
    );
}

#[test]
fn the_host_is_loopback_and_its_listed_addresses() {
    let h = HostAddrs::new([HOST.parse().unwrap(), "::ffff:192.0.2.11".parse().unwrap()]);
    assert!(h.contains("127.0.0.1".parse().unwrap()));
    assert!(h.contains("::1".parse().unwrap()));
    assert!(h.contains(HOST.parse().unwrap()));
    assert!(h.contains("::ffff:192.0.2.10".parse().unwrap()));
    assert!(h.contains("192.0.2.11".parse().unwrap()), "canonical form");
    assert!(!h.contains("192.0.2.50".parse().unwrap()));
    assert!(!HostAddrs::default().contains(HOST.parse().unwrap()));
    assert!(HostAddrs::default().contains("127.0.0.1".parse().unwrap()));
}

#[test]
fn the_host_addresses_are_its_interfaces() {
    let listed: Vec<IpAddr> = local_ip_address::list_afinet_netifas()
        .expect("the interface list")
        .into_iter()
        .map(|(_, ip)| ip)
        .collect();
    assert!(!listed.is_empty());
    assert_eq!(HostAddrs::read(), HostAddrs::new(listed));
}

#[test]
fn loopback_without_header_is_a_local_lan_client() {
    let loopback: IpAddr = "127.0.0.1".parse().unwrap();
    assert_eq!(
        ClientKey::from_request(loopback, &headers(None), &HostAddrs::default()),
        ClientKey {
            origin: Origin::Lan,
            ip: loopback
        }
    );
}

#[test]
fn ipv4_mapped_loopback_counts_as_loopback() {
    let mapped: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
    assert_eq!(
        ClientKey::from_request(mapped, &headers(Some("203.0.113.6")), &HostAddrs::default())
            .origin,
        Origin::Tunnel
    );
}

#[test]
fn garbage_header_falls_back_to_the_peer() {
    let loopback: IpAddr = "127.0.0.1".parse().unwrap();
    assert_eq!(
        ClientKey::from_request(loopback, &headers(Some("not-an-ip")), &HostAddrs::default()),
        ClientKey {
            origin: Origin::Lan,
            ip: loopback
        }
    );
}

#[test]
fn ipv6_clients_share_a_budget_per_64_prefix() {
    let loopback: IpAddr = "127.0.0.1".parse().unwrap();
    let a = ClientKey::from_request(
        loopback,
        &headers(Some("2001:db8:1:2:aaaa::1")),
        &HostAddrs::default(),
    );
    let b = ClientKey::from_request(
        loopback,
        &headers(Some("2001:db8:1:2:bbbb:cccc:dddd:eeee")),
        &HostAddrs::default(),
    );
    let other = ClientKey::from_request(
        loopback,
        &headers(Some("2001:db8:1:3::1")),
        &HostAddrs::default(),
    );
    assert_eq!(a, b, "one /64 is one client");
    assert_ne!(a, other);
    assert_eq!(a.ip, "2001:db8:1:2::".parse::<IpAddr>().unwrap());
    let v4 = ClientKey::from_request(
        loopback,
        &headers(Some("203.0.113.8")),
        &HostAddrs::default(),
    );
    assert_eq!(
        v4.ip,
        "203.0.113.8".parse::<IpAddr>().unwrap(),
        "IPv4 keys are unchanged"
    );
    let guard = LoginGuard::new();
    let t0 = Instant::now();
    for _ in 0..3 {
        guard.record_failure(&a, "member1", t0);
    }
    assert!(
        guard.check(&b, "member1", t0).is_err(),
        "a rotated address inherits the streak"
    );
    assert_eq!(guard.check(&other, "member1", t0), Ok(()));
}

#[test]
fn header_value_is_trimmed() {
    let loopback: IpAddr = "127.0.0.1".parse().unwrap();
    assert_eq!(
        ClientKey::from_request(
            loopback,
            &headers(Some(" 203.0.113.7 ")),
            &HostAddrs::default()
        )
        .ip,
        "203.0.113.7".parse::<IpAddr>().unwrap()
    );
}

#[tokio::test]
async fn hash_gate_refuses_beyond_concurrency_plus_queue() {
    let gate = Arc::new(HashGate::new(2, 8));
    let first = gate.acquire().await.expect("first permit");
    let second = gate.acquire().await.expect("second permit");
    let mut waiters = Vec::new();
    for _ in 0..8 {
        let g = Arc::clone(&gate);
        waiters.push(tokio::spawn(async move { g.acquire().await.is_some() }));
    }
    for _ in 0..1000 {
        if gate.waiting() == 8 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(gate.waiting(), 8);
    assert!(
        gate.acquire().await.is_none(),
        "the 11th concurrent request is refused at once"
    );
    drop(first);
    drop(second);
    for waiter in waiters {
        assert!(waiter.await.unwrap());
    }
    assert_eq!(gate.waiting(), 0);
}

#[tokio::test]
async fn a_gate_without_a_queue_refuses_at_once() {
    // Bounded: a gate that queued here would wait forever for a permit.
    let gate = HashGate::new(0, 0);
    let refused = tokio::time::timeout(Duration::from_secs(5), gate.acquire()).await;
    assert!(matches!(refused, Ok(None)), "expected an immediate refusal");
    assert_eq!(gate.waiting(), 0);
}

#[tokio::test]
async fn a_cancelled_waiter_frees_its_queue_place() {
    let gate = Arc::new(HashGate::new(1, 1));
    let _held = gate.acquire().await.expect("permit");
    let g = Arc::clone(&gate);
    let waiter = tokio::spawn(async move { g.acquire().await.is_some() });
    for _ in 0..1000 {
        if gate.waiting() == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(gate.waiting(), 1);
    waiter.abort();
    let _ = waiter.await;
    assert_eq!(gate.waiting(), 0);
}
