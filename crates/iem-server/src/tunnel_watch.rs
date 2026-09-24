//! Cloudflare tunnel watchdog (reaperiem#202).
//!
//! Band members reach the mixer from their phones through the `cloudflared`
//! Windows service on the iem PC. A venue firewall that blocks QUIC
//! (UDP 7844), or a cloudflared upgrade that drops `--protocol http2` from the
//! service command line, leaves the service RUNNING with zero edge
//! connections: the public URL answers 530 and nobody at the event can tell
//! why. This module runs INSIDE iem-server (no extra service on any box):
//!
//! 1. every [`POLL_INTERVAL`] it reads cloudflared's local readiness endpoint
//!    (`Config::tunnel_ready_url`, default `http://127.0.0.1:20241/ready`);
//! 2. the ready-connection count feeds the pure [`TunnelWatch`] state machine
//!    (no I/O, time injected) which decides `Ok` / `Down` / `Restarting` and
//!    whether the service must be restarted;
//! 3. after [`DOWN_BEFORE_RESTART`] of continuous zero connections the
//!    `cloudflared` service is restarted with `sc.exe` (Windows only), at most
//!    once per [`RESTART_COOLDOWN`];
//! 4. every change is broadcast to all WebSocket clients as
//!    [`ServerMsg::TunnelStatus`]; the same status is served at `GET /api/tunnel`.
//!
//! The app runs unelevated; granting it start/stop rights on the
//! `cloudflared` service is a one-time elevated setup step owned by S6.

use std::time::{Duration, Instant};

use axum::{Json, Router, extract::State, routing::get};
use iem_core::{ServerMsg, TunnelState, TunnelStatusInfo};
use serde::Deserialize;

use crate::AppState;

/// How often cloudflared's `/ready` endpoint is polled.
pub const POLL_INTERVAL: Duration = Duration::from_secs(30);
/// Continuous zero-connection time before the service is restarted.
pub const DOWN_BEFORE_RESTART: Duration = Duration::from_secs(120);
/// Minimum time between two service restarts.
pub const RESTART_COOLDOWN: Duration = Duration::from_secs(600);
/// How long the state stays `Restarting` after a restart without recovery
/// before it falls back to `Down`.
pub const RESTART_GRACE: Duration = Duration::from_secs(60);
/// Per-request timeout for the `/ready` poll.
pub const READY_TIMEOUT: Duration = Duration::from_secs(5);
/// Windows service name of the Cloudflare tunnel.
pub const SERVICE_NAME: &str = "cloudflared";
/// Upper bound for waiting until `sc.exe stop` has really stopped the service.
/// cloudflared drains in-flight requests for its 30 s `--grace-period`.
pub const SERVICE_STOP_WAIT: Duration = Duration::from_secs(45);
/// `STATE` code printed by `sc.exe query` for a stopped service.
pub const SC_STATE_STOPPED: u32 = 1;
/// `sc.exe stop` exit code when the service is already stopped.
pub const SC_ERR_SERVICE_NOT_ACTIVE: i32 = 1062;
/// `sc.exe start` exit code while the service is still running/stopping.
pub const SC_ERR_SERVICE_ALREADY_RUNNING: i32 = 1056;
/// `STATE` code printed by `sc.exe query` for a running service.
pub const SC_STATE_RUNNING: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// At least one ready connection since `since`.
    Ok { since: Instant },
    /// Zero ready connections continuously since `since`.
    Down { since: Instant },
    /// Down since `since`; the service was restarted at `restarted_at`.
    Restarting {
        since: Instant,
        restarted_at: Instant,
    },
}

/// Outcome of one observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// The caller must restart the cloudflared service now.
    pub should_restart: bool,
    /// State or ready-connection count changed — broadcast the new status.
    pub changed: bool,
}

/// Pure tunnel state machine: no I/O, the caller injects `now`.
#[derive(Debug, Clone)]
pub struct TunnelWatch {
    phase: Phase,
    ready_connections: u32,
    last_restart: Option<Instant>,
    last_restart_ok: Option<bool>,
    restart_count: u32,
}

impl TunnelWatch {
    /// Start optimistic (`Ok`, 0 connections); the first poll runs immediately.
    pub fn new(now: Instant) -> Self {
        Self {
            phase: Phase::Ok { since: now },
            ready_connections: 0,
            last_restart: None,
            last_restart_ok: None,
            restart_count: 0,
        }
    }

    /// Current public state.
    pub fn state(&self) -> TunnelState {
        match self.phase {
            Phase::Ok { .. } => TunnelState::Ok,
            Phase::Down { .. } => TunnelState::Down,
            Phase::Restarting { .. } => TunnelState::Restarting,
        }
    }

    /// How many restarts this watchdog has requested since the app started.
    pub fn restart_count(&self) -> u32 {
        self.restart_count
    }

    /// Feed one poll result (`ready` = ready edge connections; fetch/parse
    /// failures count as 0) observed at `now`.
    pub fn observe(&mut self, ready: u32, now: Instant) -> Decision {
        let before = (self.state(), self.ready_connections);
        let mut should_restart = false;
        self.phase = if ready > 0 {
            match self.phase {
                Phase::Ok { since } => Phase::Ok { since },
                Phase::Down { .. } | Phase::Restarting { .. } => Phase::Ok { since: now },
            }
        } else {
            match self.phase {
                Phase::Ok { .. } => {
                    // A new outage: a previous outage's repair result is stale.
                    self.last_restart_ok = None;
                    Phase::Down { since: now }
                }
                Phase::Down { since } => {
                    if self.restart_due(since, now) {
                        should_restart = true;
                        self.last_restart = Some(now);
                        self.last_restart_ok = None;
                        self.restart_count += 1;
                        Phase::Restarting {
                            since,
                            restarted_at: now,
                        }
                    } else {
                        Phase::Down { since }
                    }
                }
                Phase::Restarting {
                    since,
                    restarted_at,
                } => {
                    if now.saturating_duration_since(restarted_at) >= RESTART_GRACE {
                        Phase::Down { since }
                    } else {
                        Phase::Restarting {
                            since,
                            restarted_at,
                        }
                    }
                }
            }
        };
        self.ready_connections = ready;
        Decision {
            should_restart,
            changed: before != (self.state(), self.ready_connections),
        }
    }

    /// Record the outcome of the restart requested by the last `observe`.
    /// Returns whether the published status changed.
    pub fn record_restart_result(&mut self, ok: bool) -> bool {
        let changed = self.last_restart_ok != Some(ok);
        self.last_restart_ok = Some(ok);
        changed
    }

    /// Down long enough AND outside the cooldown of the previous restart.
    fn restart_due(&self, down_since: Instant, now: Instant) -> bool {
        let down_long_enough = now.saturating_duration_since(down_since) >= DOWN_BEFORE_RESTART;
        let cooled_down = self
            .last_restart
            .is_none_or(|at| now.saturating_duration_since(at) >= RESTART_COOLDOWN);
        down_long_enough && cooled_down
    }

    /// Status snapshot for clients, relative to `now`.
    pub fn status(&self, now: Instant) -> TunnelStatusInfo {
        let since = match self.phase {
            Phase::Ok { since } | Phase::Down { since } | Phase::Restarting { since, .. } => since,
        };
        TunnelStatusInfo {
            state: self.state(),
            ready_connections: self.ready_connections,
            since_secs: now.saturating_duration_since(since).as_secs(),
            last_restart_secs_ago: self
                .last_restart
                .map(|at| now.saturating_duration_since(at).as_secs()),
            last_restart_ok: self.last_restart_ok,
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

/// Whether `sc.exe stop` left the service stopping/stopped: 0 = stop accepted,
/// 1062 = it was not running. Anything else (e.g. 5 access denied) means the
/// service is still running and waiting for STOPPED is pointless.
pub fn stop_accepted(exit_code: Option<i32>) -> bool {
    matches!(exit_code, Some(0) | Some(SC_ERR_SERVICE_NOT_ACTIVE))
}

/// Extract the numeric `STATE` from `sc.exe query <service>` output
/// (`        STATE              : 4  RUNNING`).
pub fn parse_sc_state(sc_query_stdout: &str) -> Option<u32> {
    sc_query_stdout.lines().find_map(|line| {
        let rest = line.trim_start().strip_prefix("STATE")?;
        let value = rest.trim_start().strip_prefix(':')?;
        value.split_whitespace().next()?.parse().ok()
    })
}

/// One watchdog cycle: poll `/ready`, advance the state machine, log and
/// broadcast changes. Returns the decision; the caller performs the restart.
pub async fn watch_tick(state: &AppState, now: Instant) -> Decision {
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

    let (prev, decision, next, restart_count) = {
        let mut watch = state.tunnel_watch.write().await;
        let prev = watch.status(now);
        let decision = watch.observe(ready, now);
        (prev, decision, watch.status(now), watch.restart_count())
    };
    log_transition(&prev, &next, decision, restart_count);

    if decision.changed {
        // Err only means no WebSocket client is connected right now; the
        // status is also sent on connect and served at /api/tunnel.
        let receivers = state
            .event_tx
            .send((String::new(), ServerMsg::TunnelStatus(next)))
            .unwrap_or(0);
        tracing::debug!(receivers, state = ?next.state, "tunnel status broadcast");
    }
    decision
}

fn log_transition(
    prev: &TunnelStatusInfo,
    next: &TunnelStatusInfo,
    decision: Decision,
    restart_count: u32,
) {
    match (prev.state, next.state) {
        (TunnelState::Ok, TunnelState::Down) => tracing::warn!(
            ready_connections = next.ready_connections,
            restart_after_secs = DOWN_BEFORE_RESTART.as_secs(),
            "tunnel DOWN — cloudflared reports 0 ready edge connections"
        ),
        (TunnelState::Down, TunnelState::Restarting) => tracing::warn!(
            down_for_secs = next.since_secs,
            restart_count,
            service = SERVICE_NAME,
            "tunnel down too long — restarting cloudflared service"
        ),
        (TunnelState::Restarting, TunnelState::Down) => tracing::warn!(
            down_for_secs = next.since_secs,
            last_restart_secs_ago = ?next.last_restart_secs_ago,
            cooldown_secs = RESTART_COOLDOWN.as_secs(),
            "tunnel still down after cloudflared restart — next restart after cooldown"
        ),
        (TunnelState::Down | TunnelState::Restarting, TunnelState::Ok) => tracing::info!(
            ready_connections = next.ready_connections,
            down_for_secs = prev.since_secs,
            restart_count,
            "tunnel RECOVERED"
        ),
        (TunnelState::Ok, TunnelState::Ok) if decision.changed => tracing::info!(
            from = prev.ready_connections,
            to = next.ready_connections,
            "tunnel ready connections changed"
        ),
        _ => {}
    }
}

/// Store the restart outcome and broadcast the status if it changed
/// (the engineer indicator shows a failed repair).
pub async fn record_restart(state: &AppState, ok: bool, now: Instant) {
    let (changed, status) = {
        let mut watch = state.tunnel_watch.write().await;
        let changed = watch.record_restart_result(ok);
        (changed, watch.status(now))
    };
    if ok {
        tracing::info!(
            last_restart_secs_ago = ?status.last_restart_secs_ago,
            state = ?status.state,
            "cloudflared restart finished OK"
        );
    } else {
        tracing::error!(
            service = SERVICE_NAME,
            "cloudflared restart FAILED — see the sc.exe lines above"
        );
    }
    if changed {
        let receivers = state
            .event_tx
            .send((String::new(), ServerMsg::TunnelStatus(status)))
            .unwrap_or(0);
        tracing::debug!(receivers, ok, "tunnel restart result broadcast");
    }
}

/// Restart the cloudflared Windows service; `true` when `sc.exe start`
/// succeeded. Blocking — call via `spawn_blocking`. Always `false` (logged)
/// on non-Windows hosts.
pub fn restart_service_blocking(service: &str) -> bool {
    #[cfg(windows)]
    {
        restart_via_sc_windows(service)
    }
    #[cfg(not(windows))]
    {
        tracing::warn!(
            service,
            "tunnel watchdog: service restart requested, but restarting is only supported on Windows — skipped"
        );
        false
    }
}

#[cfg(windows)]
struct ScOutput {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[cfg(windows)]
fn run_sc_windows(args: &[&str]) -> Result<ScOutput, String> {
    use std::os::windows::process::CommandExt;
    // The app is a GUI (tray) process — never flash a console window.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("sc.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("spawning sc.exe {args:?} failed: {e:?}"))?;
    Ok(ScOutput {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Poll `sc.exe query` until the service is STOPPED or `wait` elapsed.
#[cfg(windows)]
fn wait_stopped_sc_windows(service: &str, wait: Duration) -> Option<u32> {
    let started = Instant::now();
    loop {
        let sc_state = run_sc_windows(&["query", service])
            .ok()
            .and_then(|o| parse_sc_state(&o.stdout));
        if sc_state == Some(SC_STATE_STOPPED) || started.elapsed() >= wait {
            tracing::info!(
                service,
                sc_state = ?sc_state,
                waited = ?started.elapsed(),
                "cloudflared stop wait finished"
            );
            return sc_state;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// `sc.exe start`, logged; returns the exit code.
#[cfg(windows)]
fn start_sc_windows(service: &str) -> Option<i32> {
    match run_sc_windows(&["start", service]) {
        Ok(out) if out.code == Some(0) => {
            tracing::info!(
                service,
                stdout = %out.stdout.trim(),
                "sc.exe start OK — cloudflared restarted"
            );
            out.code
        }
        Ok(out) => {
            tracing::error!(
                service,
                exit_code = ?out.code,
                stdout = %out.stdout.trim(),
                stderr = %out.stderr.trim(),
                "sc.exe start FAILED — is the start/stop ACE for the app user missing? \
                 Run scripts/windows/setup-cloudflared-service.ps1 elevated"
            );
            out.code
        }
        Err(e) => {
            tracing::error!(service, error = %e, "sc.exe start could not run");
            None
        }
    }
}

/// `sc.exe stop`, wait for STOPPED, `sc.exe start` (retried once if the
/// service was still stopping). `sc query/stop/start` never print the service
/// command line, so the tunnel token is never logged.
#[cfg(windows)]
fn restart_via_sc_windows(service: &str) -> bool {
    let started = Instant::now();
    let stop_code = match run_sc_windows(&["stop", service]) {
        Ok(out) => {
            tracing::info!(
                service,
                exit_code = ?out.code,
                stdout = %out.stdout.trim(),
                stderr = %out.stderr.trim(),
                "sc.exe stop finished"
            );
            out.code
        }
        Err(e) => {
            tracing::error!(service, error = %e, "sc.exe stop could not run");
            None
        }
    };
    if stop_accepted(stop_code) {
        let sc_state = wait_stopped_sc_windows(service, SERVICE_STOP_WAIT);
        if sc_state != Some(SC_STATE_STOPPED) {
            tracing::warn!(
                service,
                sc_state = ?sc_state,
                "service not STOPPED within the wait — starting anyway"
            );
        }
    } else {
        tracing::error!(
            service,
            exit_code = ?stop_code,
            "sc.exe stop refused — trying start anyway"
        );
    }

    let mut start_code = start_sc_windows(service);
    if start_code == Some(SC_ERR_SERVICE_ALREADY_RUNNING) && stop_accepted(stop_code) {
        tracing::warn!(
            service,
            "service still stopping — waiting again, retrying start once"
        );
        wait_stopped_sc_windows(service, SERVICE_STOP_WAIT);
        start_code = start_sc_windows(service);
    }
    let ok = if start_code == Some(SC_ERR_SERVICE_ALREADY_RUNNING) {
        // e.g. SCM recovery already brought it back — RUNNING is success.
        let sc_state = run_sc_windows(&["query", service])
            .ok()
            .and_then(|o| parse_sc_state(&o.stdout));
        tracing::info!(service, sc_state = ?sc_state, "start reported already running — checked state");
        sc_state == Some(SC_STATE_RUNNING)
    } else {
        start_code == Some(0)
    };
    tracing::info!(service, ok, elapsed = ?started.elapsed(), "cloudflared restart attempt finished");
    ok
}

/// Spawn the watchdog loop (started from `start_server`).
pub fn spawn_tunnel_watch(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let url = state.config.read().await.tunnel_ready_url.clone();
        tracing::info!(
            url = %url,
            poll_secs = POLL_INTERVAL.as_secs(),
            restart_after_secs = DOWN_BEFORE_RESTART.as_secs(),
            cooldown_secs = RESTART_COOLDOWN.as_secs(),
            service = SERVICE_NAME,
            "tunnel watchdog started"
        );
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let decision = watch_tick(&state, Instant::now()).await;
            if decision.should_restart {
                let restart =
                    tokio::task::spawn_blocking(|| restart_service_blocking(SERVICE_NAME));
                let ok = restart.await.unwrap_or_else(|e| {
                    tracing::error!(error = ?e, "cloudflared restart task panicked");
                    false
                });
                record_restart(&state, ok, Instant::now()).await;
            }
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
