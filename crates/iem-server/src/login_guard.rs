//! Login protection (program spec §5.3). Pure admission logic with the clock
//! injected, plus a bounded gate for argon2id work:
//!
//! - budgets count failures only, separately for LAN and tunnel clients;
//! - `CF-Connecting-IP` is trusted only from a peer that is this host
//!   (loopback, or one of its own addresses read at start): the tunnel
//!   connector runs on the same PC, whichever of its addresses its origin
//!   targets (design note §6). From any other peer the header is ignored;
//!   an IPv6 client is keyed by its /64, so it cannot rotate addresses
//!   within its prefix to reset its budgets;
//! - per (client, member): three free failures, then 1, 2, 4 … s;
//! - per client: 20 failures in 10 minutes → 60 s spacing;
//! - engineer budget per origin: the engineer PIN works from any member login,
//!   so every failure counts; over 30 in an hour → 5 s spacing for the origin;
//! - every delay ≤ 60 s — never a lockout; the caller answers 429 +
//!   `Retry-After` before any hashing.

use std::collections::{HashMap, HashSet, VecDeque};
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

/// This host's own addresses: a TCP peer with one of them is a process on
/// this PC — the tunnel connector, whether its origin targets 127.0.0.1 or
/// the host's LAN address. Loopback always counts. No other machine can use
/// them as its peer address: a TCP connection needs the handshake back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostAddrs(HashSet<IpAddr>);

impl HostAddrs {
    /// Loopback and `addrs` (in canonical form).
    pub fn new(addrs: impl IntoIterator<Item = IpAddr>) -> Self {
        Self(addrs.into_iter().map(|ip| ip.to_canonical()).collect())
    }

    /// This host's interface addresses, read once at start; loopback only
    /// when they cannot be read.
    pub fn read() -> Self {
        match local_ip_address::list_afinet_netifas() {
            Ok(interfaces) => Self::new(interfaces.into_iter().map(|(_, ip)| ip)),
            Err(e) => {
                tracing::warn!(error = %e, "the host's addresses are unreadable: CF-Connecting-IP counts from loopback only");
                Self::default()
            }
        }
    }

    /// Whether `peer` is this host.
    pub fn contains(&self, peer: IpAddr) -> bool {
        let peer = peer.to_canonical();
        peer.is_loopback() || self.0.contains(&peer)
    }
}

impl ClientKey {
    /// `CF-Connecting-IP` counts only when the TCP peer is this host (the
    /// tunnel connector); every other peer is a LAN client keyed by its own
    /// address, whatever header it sends. A header that is no address falls
    /// back to the peer.
    pub fn from_request(peer: IpAddr, headers: &HeaderMap, host: &HostAddrs) -> Self {
        let peer = peer.to_canonical();
        if host.contains(peer)
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
    /// Where `CF-Connecting-IP` is trusted from.
    host: HostAddrs,
}

impl LoginGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// A guard that trusts `CF-Connecting-IP` from `host`'s addresses and
    /// loopback ([`LoginGuard::new`]: loopback only).
    pub fn for_host(host: HostAddrs) -> Self {
        Self {
            inner: Mutex::default(),
            host,
        }
    }

    /// Whether `peer` is this host (loopback or one of the addresses the
    /// guard trusts CF-Connecting-IP from): `/api/peer`'s `host`.
    pub fn is_host(&self, peer: IpAddr) -> bool {
        self.host.contains(peer)
    }

    /// The budget key of a request from `peer` with `headers`.
    pub fn client(&self, peer: IpAddr, headers: &HeaderMap) -> ClientKey {
        ClientKey::from_request(peer, headers, &self.host)
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
mod tests;
