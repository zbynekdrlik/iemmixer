//! Cloudflare tunnel watchdog (reaperiem#202).
//!
//! Band members reach the mixer from their phones through `cloudflared`. A venue firewall that blocks QUIC
//! (UDP 7844), or a cloudflared upgrade that drops `--protocol http2` from the
//! service command line, leaves the service RUNNING with zero edge
//! connections: the public URL answers 530 and nobody at the event can tell
//! why. This module runs INSIDE iem-server (no extra service on any box); it
//! detects and reports, and never touches the tunnel (the server's user holds
//! no right over `iem-tunnel.service`, whose own unit restarts cloudflared
//! when it ends):
//!
//! 1. every [`POLL_INTERVAL`] it reads cloudflared's local readiness endpoint
//!    (`Config::tunnel_ready_url`, default `http://127.0.0.1:20241/ready`);
//! 2. the ready-connection count feeds the pure [`TunnelWatch`] state machine
//!    (no I/O, time injected) which decides `Ok` / `Down`;
//! 3. every change is logged and broadcast to all WebSocket clients as
//!    [`ServerMsg::TunnelStatus`]; the same status is served at `GET /api/tunnel`.

use std::time::{Duration, Instant};

use axum::{Json, Router, extract::State, routing::get};
use iem_core::{ServerMsg, TunnelState, TunnelStatusInfo};
use serde::Deserialize;

use crate::AppState;

/// How often cloudflared's `/ready` endpoint is polled.
pub const POLL_INTERVAL: Duration = Duration::from_secs(30);
/// Per-request timeout for the `/ready` poll.
pub const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Pure tunnel state machine: no I/O, the caller injects `now`.
#[derive(Debug, Clone)]
pub struct TunnelWatch {
    state: TunnelState,
    /// When the current state began.
    since: Instant,
    ready_connections: u32,
}

impl TunnelWatch {
    /// Start optimistic (`Ok`, 0 connections); the first poll runs immediately.
    pub fn new(now: Instant) -> Self {
        Self {
            state: TunnelState::Ok,
            since: now,
            ready_connections: 0,
        }
    }

    /// Current public state.
    pub fn state(&self) -> TunnelState {
        self.state
    }

    /// Feed one poll result (`ready` = ready edge connections; fetch/parse
    /// failures count as 0) observed at `now`. Returns whether the state or
    /// the ready-connection count changed, so the new status is broadcast.
    pub fn observe(&mut self, ready: u32, now: Instant) -> bool {
        let before = (self.state, self.ready_connections);
        let state = if ready > 0 {
            TunnelState::Ok
        } else {
            TunnelState::Down
        };
        if state != self.state {
            self.state = state;
            self.since = now;
        }
        self.ready_connections = ready;
        before != (self.state, self.ready_connections)
    }

    /// Status snapshot for clients, relative to `now`.
    pub fn status(&self, now: Instant) -> TunnelStatusInfo {
        TunnelStatusInfo {
            state: self.state,
            ready_connections: self.ready_connections,
            since_secs: now.saturating_duration_since(self.since).as_secs(),
        }
    }
}

#[derive(Deserialize)]
struct ReadyBody {
    #[serde(rename = "readyConnections")]
    ready_connections: u32,
}

/// Parse a cloudflared `/ready` response. Only HTTP 200 with a valid
/// `{"readyConnections":N}` body yields `Ok(N)`; cloudflared answers 503 when
/// it has no edge connection.
pub fn parse_ready(http_status: u16, body: &str) -> Result<u32, String> {
    if http_status != 200 {
        let snippet: String = body.chars().take(200).collect();
        return Err(format!("HTTP {http_status} body={snippet:?}"));
    }
    serde_json::from_str::<ReadyBody>(body)
        .map(|b| b.ready_connections)
        .map_err(|e| format!("unparseable /ready body ({e}): {body:?}"))
}

/// GET the readiness endpoint and parse it (see [`parse_ready`]).
pub async fn fetch_ready(client: &reqwest::Client, url: &str) -> Result<u32, String> {
    let resp = client
        .get(url)
        .timeout(READY_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("request failed: {e:?}"))?;
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("reading body failed (HTTP {status}): {e:?}"))?;
    parse_ready(status, &body)
}

/// One watchdog cycle: poll `/ready`, advance the state machine, log and
/// broadcast changes. Returns whether anything changed.
pub async fn watch_tick(state: &AppState, now: Instant) -> bool {
    let url = state.config.read().await.tunnel_ready_url.clone();
    let ready = match fetch_ready(&state.http_client, &url).await {
        Ok(n) => {
            tracing::debug!(url = %url, ready_connections = n, "tunnel /ready poll");
            n
        }
        Err(reason) => {
            tracing::debug!(
                url = %url,
                reason = %reason,
                "tunnel /ready poll failed — counting as 0 ready connections"
            );
            0
        }
    };

    let (prev, changed, next) = {
        let mut watch = state.tunnel_watch.write().await;
        let prev = watch.status(now);
        let changed = watch.observe(ready, now);
        (prev, changed, watch.status(now))
    };
    log_transition(&prev, &next, changed);

    if changed {
        // Err only means no WebSocket client is connected right now; the
        // status is also sent on connect and served at /api/tunnel.
        let receivers = state
            .event_tx
            .send((crate::To::All, ServerMsg::TunnelStatus(next)))
            .unwrap_or(0);
        tracing::debug!(receivers, state = ?next.state, "tunnel status broadcast");
    }
    changed
}

fn log_transition(prev: &TunnelStatusInfo, next: &TunnelStatusInfo, changed: bool) {
    match (prev.state, next.state) {
        (TunnelState::Ok, TunnelState::Down) => tracing::warn!(
            ready_connections = next.ready_connections,
            "tunnel DOWN — cloudflared reports 0 ready edge connections"
        ),
        (TunnelState::Down, TunnelState::Ok) => tracing::info!(
            ready_connections = next.ready_connections,
            down_for_secs = prev.since_secs,
            "tunnel RECOVERED"
        ),
        (TunnelState::Ok, TunnelState::Ok) if changed => tracing::info!(
            from = prev.ready_connections,
            to = next.ready_connections,
            "tunnel ready connections changed"
        ),
        _ => {}
    }
}

/// Spawn the watchdog loop (started from `start_server`).
pub fn spawn_tunnel_watch(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let url = state.config.read().await.tunnel_ready_url.clone();
        tracing::info!(
            url = %url,
            poll_secs = POLL_INTERVAL.as_secs(),
            "tunnel watchdog started"
        );
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            watch_tick(&state, Instant::now()).await;
        }
    })
}

/// Current status as the WebSocket message sent on connect.
pub async fn current_status_msg(state: &AppState) -> ServerMsg {
    ServerMsg::TunnelStatus(state.tunnel_watch.read().await.status(Instant::now()))
}

/// `GET /api/tunnel` — the same JSON as the `TunnelStatus` WebSocket payload.
pub async fn get_tunnel_status(State(state): State<AppState>) -> Json<TunnelStatusInfo> {
    Json(state.tunnel_watch.read().await.status(Instant::now()))
}

/// Routes owned by this module.
pub fn tunnel_routes() -> Router<AppState> {
    Router::new().route("/api/tunnel", get(get_tunnel_status))
}

#[cfg(test)]
mod tests;
