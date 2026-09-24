//! Login protection (program spec §5.3). Pure admission logic with the clock
//! injected, plus a bounded gate for argon2id work:
//!
//! - budgets count failures only, separately for LAN and tunnel clients;
//! - `CF-Connecting-IP` is trusted only from a loopback peer (the tunnel
//!   connector on the same PC); an IPv6 client is keyed by its /64, so it
//!   cannot rotate addresses within its prefix to reset its budgets;
//! - per (client, member): three free failures, then 1, 2, 4 … s;
//! - per client: 20 failures in 10 minutes → 60 s spacing;
//! - engineer budget per origin: the engineer PIN works from any member login,
//!   so every failure counts; over 30 in an hour → 5 s spacing for the origin;
//! - every delay ≤ 60 s — never a lockout; the caller answers 429 +
//!   `Retry-After` before any hashing.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv6Addr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Consecutive failures per (client, member) that carry no delay.
pub const FREE_FAILURES: u32 = 3;
/// A (client, member) streak is forgotten after this long without a failure.
pub const STREAK_DECAY: Duration = Duration::from_secs(15 * 60);
/// Upper bound of every delay (never a lockout).
pub const MAX_DELAY: Duration = Duration::from_secs(60);
/// Sliding window of the per-client budget (across members).
pub const CLIENT_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Failures per client within [`CLIENT_WINDOW`] before spacing applies.
pub const CLIENT_WINDOW_FAILURES: usize = 20;
/// Spacing once a client has used its window budget.
pub const CLIENT_SPACING: Duration = Duration::from_secs(60);
/// Sliding window of the engineer budget (per origin).
pub const ENGINEER_WINDOW: Duration = Duration::from_secs(60 * 60);
/// Failures per origin within [`ENGINEER_WINDOW`] before origin spacing applies.
pub const ENGINEER_WINDOW_FAILURES: usize = 30;
/// Spacing between admitted attempts of an origin over its engineer budget.
pub const ENGINEER_SPACING: Duration = Duration::from_secs(5);
/// Upper bound of tracked keys per map (memory bound under a flood).
pub const MAX_TRACKED: usize = 4096;
/// argon2id hashes running at once (each costs 19 MiB).
pub const HASH_CONCURRENCY: usize = 2;
/// Requests allowed to wait for a hashing slot; more get 429 at once.
pub const HASH_QUEUE: usize = 8;

/// Where a login comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    Lan,
    Tunnel,
}

/// Budget key of a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientKey {
    pub origin: Origin,
    pub ip: IpAddr,
}

/// Budget key of an address: IPv4 as is, IPv6 reduced to its /64 (a single
/// subscriber usually holds a whole /64). LAN peers are IPv4 in practice: the
/// listeners bind `0.0.0.0`.
fn budget_ip(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        v4 => v4,
    }
}

impl ClientKey {
    /// `CF-Connecting-IP` counts only when the TCP peer is loopback; every
    /// other peer is a LAN client keyed by its own address.
    pub fn from_request(peer: IpAddr, headers: &HeaderMap) -> Self {
        let peer = peer.to_canonical();
        if peer.is_loopback()
            && let Some(ip) = headers
                .get("cf-connecting-ip")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<IpAddr>().ok())
        {
            return Self {
                origin: Origin::Tunnel,
                ip: budget_ip(ip),
            };
        }
        Self {
            origin: Origin::Lan,
            ip: budget_ip(peer),
        }
    }
}

/// What a recorded failure changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureEffect {
    Counted,
    /// This failure pushed its origin over the engineer budget.
    EngineerBudgetExhausted,
}

/// Counters for the engineer page (wired in S5).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct LoginStats {
    pub lan_failures: u64,
    pub tunnel_failures: u64,
    pub engineer_budget_trips: u64,
}

#[derive(Debug, Clone, Copy)]
struct Streak {
    failures: u32,
    last_failure: Instant,
}

#[derive(Debug, Default)]
struct OriginBudget {
    failures: VecDeque<Instant>,
    last_admitted: Option<Instant>,
}

#[derive(Debug, Default)]
struct Inner {
    streaks: HashMap<(ClientKey, String), Streak>,
    clients: HashMap<ClientKey, VecDeque<Instant>>,
    lan: OriginBudget,
    tunnel: OriginBudget,
    stats: LoginStats,
}

impl Inner {
    fn origin_mut(&mut self, origin: Origin) -> &mut OriginBudget {
        match origin {
            Origin::Lan => &mut self.lan,
            Origin::Tunnel => &mut self.tunnel,
        }
    }
}

/// Delay owed after `failures` consecutive failures: none up to
/// [`FREE_FAILURES`] − 1, then 1, 2, 4 … s, capped at [`MAX_DELAY`].
pub fn streak_delay(failures: u32) -> Duration {
    if failures < FREE_FAILURES {
        return Duration::ZERO;
    }
    let exponent = failures - FREE_FAILURES;
    if exponent >= 6 {
        return MAX_DELAY;
    }
    Duration::from_secs(1u64 << exponent).min(MAX_DELAY)
}

fn prune(window: &mut VecDeque<Instant>, now: Instant, span: Duration) {
    while let Some(&oldest) = window.front() {
        if now.saturating_duration_since(oldest) >= span {
            window.pop_front();
        } else {
            break;
        }
    }
}

fn evict_oldest_streak(map: &mut HashMap<(ClientKey, String), Streak>) {
    if let Some(key) = map
        .iter()
        .min_by_key(|(_, s)| s.last_failure)
        .map(|(k, _)| k.clone())
    {
        map.remove(&key);
    }
}

fn evict_oldest_client(map: &mut HashMap<ClientKey, VecDeque<Instant>>) {
    if let Some(key) = map
        .iter()
        .min_by_key(|(_, w)| w.back().copied())
        .map(|(k, _)| *k)
    {
        map.remove(&key);
    }
}

/// Failure bookkeeping shared by every login and PIN-change request.
#[derive(Debug, Default)]
pub struct LoginGuard {
    inner: Mutex<Inner>,
}

impl LoginGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Admission before any hashing. `Err(wait)` → answer 429 with `Retry-After`.
    pub fn check(&self, client: &ClientKey, member: &str, now: Instant) -> Result<(), Duration> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (*client, member.to_string());
        if let Some(streak) = inner.streaks.get(&key).copied() {
            let since = now.saturating_duration_since(streak.last_failure);
            if since >= STREAK_DECAY {
                inner.streaks.remove(&key);
            } else {
                let delay = streak_delay(streak.failures);
                if since < delay {
                    return Err(delay - since);
                }
            }
        }
        if let Some(window) = inner.clients.get_mut(client) {
            prune(window, now, CLIENT_WINDOW);
            if window.len() >= CLIENT_WINDOW_FAILURES
                && let Some(&last) = window.back()
            {
                let since = now.saturating_duration_since(last);
                if since < CLIENT_SPACING {
                    return Err(CLIENT_SPACING - since);
                }
            }
        }
        let budget = inner.origin_mut(client.origin);
        prune(&mut budget.failures, now, ENGINEER_WINDOW);
        if budget.failures.len() > ENGINEER_WINDOW_FAILURES
            && let Some(last) = budget.last_admitted
        {
            let since = now.saturating_duration_since(last);
            if since < ENGINEER_SPACING {
                return Err(ENGINEER_SPACING - since);
            }
        }
        budget.last_admitted = Some(now);
        Ok(())
    }

    /// Count a failed PIN check.
    pub fn record_failure(&self, client: &ClientKey, member: &str, now: Instant) -> FailureEffect {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (*client, member.to_string());
        let failures = match inner.streaks.get(&key) {
            Some(streak) if now.saturating_duration_since(streak.last_failure) < STREAK_DECAY => {
                streak.failures.saturating_add(1)
            }
            _ => 1,
        };
        if !inner.streaks.contains_key(&key) && inner.streaks.len() >= MAX_TRACKED {
            evict_oldest_streak(&mut inner.streaks);
        }
        inner.streaks.insert(
            key,
            Streak {
                failures,
                last_failure: now,
            },
        );

        if !inner.clients.contains_key(client) && inner.clients.len() >= MAX_TRACKED {
            evict_oldest_client(&mut inner.clients);
        }
        let window = inner.clients.entry(*client).or_default();
        prune(window, now, CLIENT_WINDOW);
        window.push_back(now);
        if window.len() > CLIENT_WINDOW_FAILURES {
            window.pop_front();
        }

        match client.origin {
            Origin::Lan => inner.stats.lan_failures += 1,
            Origin::Tunnel => inner.stats.tunnel_failures += 1,
        }
        let budget = inner.origin_mut(client.origin);
        prune(&mut budget.failures, now, ENGINEER_WINDOW);
        let before = budget.failures.len();
        budget.failures.push_back(now);
        if budget.failures.len() > ENGINEER_WINDOW_FAILURES + 1 {
            budget.failures.pop_front();
        }
        if before == ENGINEER_WINDOW_FAILURES {
            inner.stats.engineer_budget_trips += 1;
            FailureEffect::EngineerBudgetExhausted
        } else {
            FailureEffect::Counted
        }
    }

    /// A successful PIN check ends that (client, member) streak.
    pub fn record_success(&self, client: &ClientKey, member: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.streaks.remove(&(*client, member.to_string()));
    }

    pub fn stats(&self) -> LoginStats {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stats
    }
}

/// Bounded concurrency for argon2id work: `concurrency` running, at most
/// `max_waiting` queued; beyond that `acquire` returns `None` at once.
#[derive(Debug)]
pub struct HashGate {
    permits: Arc<Semaphore>,
    waiting: AtomicUsize,
    max_waiting: usize,
}

struct WaitingSlot<'a>(&'a AtomicUsize);

impl Drop for WaitingSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl HashGate {
    pub fn new(concurrency: usize, max_waiting: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(concurrency)),
            waiting: AtomicUsize::new(0),
            max_waiting,
        }
    }

    /// A hashing slot, or `None` when the queue is full. A cancelled waiter
    /// (client gone) frees its queue place.
    pub async fn acquire(&self) -> Option<OwnedSemaphorePermit> {
        if let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() {
            return Some(permit);
        }
        if self.waiting.fetch_add(1, Ordering::SeqCst) >= self.max_waiting {
            self.waiting.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        let _slot = WaitingSlot(&self.waiting);
        Arc::clone(&self.permits).acquire_owned().await.ok()
    }

    /// Requests currently queued for a slot.
    pub fn waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
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
            ClientKey::from_request(loopback, &headers(Some("203.0.113.5"))),
            ClientKey {
                origin: Origin::Tunnel,
                ip: "203.0.113.5".parse().unwrap()
            }
        );
        let lan_peer: IpAddr = "10.0.0.20".parse().unwrap();
        assert_eq!(
            ClientKey::from_request(lan_peer, &headers(Some("203.0.113.5"))),
            ClientKey {
                origin: Origin::Lan,
                ip: lan_peer
            }
        );
    }

    #[test]
    fn loopback_without_header_is_a_local_lan_client() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(
            ClientKey::from_request(loopback, &headers(None)),
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
            ClientKey::from_request(mapped, &headers(Some("203.0.113.6"))).origin,
            Origin::Tunnel
        );
    }

    #[test]
    fn garbage_header_falls_back_to_the_peer() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(
            ClientKey::from_request(loopback, &headers(Some("not-an-ip"))),
            ClientKey {
                origin: Origin::Lan,
                ip: loopback
            }
        );
    }

    #[test]
    fn ipv6_clients_share_a_budget_per_64_prefix() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        let a = ClientKey::from_request(loopback, &headers(Some("2001:db8:1:2:aaaa::1")));
        let b =
            ClientKey::from_request(loopback, &headers(Some("2001:db8:1:2:bbbb:cccc:dddd:eeee")));
        let other = ClientKey::from_request(loopback, &headers(Some("2001:db8:1:3::1")));
        assert_eq!(a, b, "one /64 is one client");
        assert_ne!(a, other);
        assert_eq!(a.ip, "2001:db8:1:2::".parse::<IpAddr>().unwrap());
        let v4 = ClientKey::from_request(loopback, &headers(Some("203.0.113.8")));
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
            ClientKey::from_request(loopback, &headers(Some(" 203.0.113.7 "))).ip,
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
}
