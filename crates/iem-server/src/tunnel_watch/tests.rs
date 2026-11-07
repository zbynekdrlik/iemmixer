//! Tests for the Cloudflare tunnel watchdog (reaperiem#202).

use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tower::util::ServiceExt;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn status_at(w: &TunnelWatch, now: Instant) -> TunnelStatusInfo {
    w.status(now)
}

// ---------------------------------------------------------------
// Pure state machine
// ---------------------------------------------------------------

#[test]
fn new_watch_is_ok_with_no_connections() {
    let t0 = Instant::now();
    let w = TunnelWatch::new(t0);
    assert_eq!(w.state(), TunnelState::Ok);
    assert_eq!(
        status_at(&w, t0 + secs(7)),
        TunnelStatusInfo {
            state: TunnelState::Ok,
            ready_connections: 0,
            since_secs: 7,
        }
    );
}

#[test]
fn positive_poll_keeps_ok_and_reports_count_change() {
    let t0 = Instant::now();
    let mut w = TunnelWatch::new(t0);
    assert!(w.observe(4, t0 + secs(1)));
    // Same count again → nothing changed.
    assert!(!w.observe(4, t0 + secs(31)));
    // Different positive count → changed, still Ok, `since` preserved.
    assert!(w.observe(2, t0 + secs(61)));
    let st = status_at(&w, t0 + secs(61));
    assert_eq!(st.state, TunnelState::Ok);
    assert_eq!(st.ready_connections, 2);
    assert_eq!(st.since_secs, 61);
}

#[test]
fn single_connection_counts_as_ok() {
    let t0 = Instant::now();
    let mut w = TunnelWatch::new(t0);
    w.observe(0, t0);
    assert_eq!(w.state(), TunnelState::Down);
    w.observe(1, t0 + secs(30));
    assert_eq!(w.state(), TunnelState::Ok);
}

#[test]
fn zero_poll_goes_down_immediately() {
    let t0 = Instant::now();
    let mut w = TunnelWatch::new(t0);
    w.observe(4, t0);
    assert!(w.observe(0, t0 + secs(30)));
    let st = status_at(&w, t0 + secs(40));
    assert_eq!(st.state, TunnelState::Down);
    assert_eq!(st.ready_connections, 0);
    assert_eq!(st.since_secs, 10);
}

#[test]
fn a_long_outage_stays_down_keeping_its_start_and_does_not_change() {
    let t0 = Instant::now();
    let mut w = TunnelWatch::new(t0);
    assert!(w.observe(0, t0));
    for t in (30..=3600).step_by(30) {
        assert!(!w.observe(0, t0 + secs(t)), "unchanged at {t}s");
    }
    let st = status_at(&w, t0 + secs(3600));
    assert_eq!(st.state, TunnelState::Down);
    assert_eq!(st.since_secs, 3600);
}

#[test]
fn recovery_goes_ok_and_restarts_since() {
    let t0 = Instant::now();
    let mut w = TunnelWatch::new(t0);
    w.observe(0, t0);
    assert!(w.observe(3, t0 + secs(130)));
    let st = status_at(&w, t0 + secs(140));
    assert_eq!(st.state, TunnelState::Ok);
    assert_eq!(st.ready_connections, 3);
    assert_eq!(st.since_secs, 10);
}

// ---------------------------------------------------------------
// Parsers
// ---------------------------------------------------------------

#[test]
fn parse_ready_valid_json() {
    assert_eq!(
        parse_ready(200, r#"{"status":200,"readyConnections":4}"#),
        Ok(4)
    );
}

#[test]
fn parse_ready_zero_connections() {
    assert_eq!(
        parse_ready(200, r#"{"status":200,"readyConnections":0}"#),
        Ok(0)
    );
}

#[test]
fn parse_ready_malformed_body_is_error() {
    let err = parse_ready(200, "<html>nope</html>").unwrap_err();
    assert!(err.contains("unparseable"), "{err}");
    assert!(parse_ready(200, r#"{"status":200}"#).is_err());
}

#[test]
fn parse_ready_non_200_is_error_even_with_valid_body() {
    let err = parse_ready(503, r#"{"status":503,"readyConnections":0}"#).unwrap_err();
    assert!(err.contains("HTTP 503"), "{err}");
    assert!(parse_ready(201, r#"{"status":201,"readyConnections":4}"#).is_err());
}

// ---------------------------------------------------------------
// I/O: fetch + tick + routes against a local fake cloudflared
// ---------------------------------------------------------------

/// Local fake of cloudflared's metrics server: `/ready` serves the
/// current `ready` count (HTTP 503 when 0, like cloudflared), `/garbage`
/// serves a non-JSON 200.
async fn fake_cloudflared(ready: Arc<AtomicU32>) -> String {
    let app = Router::new()
        .route(
            "/ready",
            get(move || {
                let n = ready.load(Ordering::SeqCst);
                async move {
                    let code = if n == 0 {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::OK
                    };
                    (
                        code,
                        format!(r#"{{"status":{},"readyConnections":{n}}}"#, code.as_u16()),
                    )
                }
            }),
        )
        .route("/garbage", get(|| async { "not json" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn closed_port_url() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    format!("http://{addr}/ready")
}

fn test_state(ready_url: String) -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = iem_core::Config {
        tunnel_ready_url: ready_url,
        ..iem_core::Config::default()
    };
    (AppState::new(config, dir.path()), dir)
}

#[tokio::test]
async fn fetch_ready_reads_live_endpoint() {
    let ready = Arc::new(AtomicU32::new(4));
    let base = fake_cloudflared(ready.clone()).await;
    let client = reqwest::Client::new();
    assert_eq!(fetch_ready(&client, &format!("{base}/ready")).await, Ok(4));
    ready.store(0, Ordering::SeqCst);
    let err = fetch_ready(&client, &format!("{base}/ready"))
        .await
        .unwrap_err();
    assert!(err.contains("HTTP 503"), "{err}");
    let err = fetch_ready(&client, &format!("{base}/garbage"))
        .await
        .unwrap_err();
    assert!(err.contains("unparseable"), "{err}");
}

#[tokio::test]
async fn fetch_ready_unreachable_is_error() {
    let client = reqwest::Client::new();
    let err = fetch_ready(&client, &closed_port_url()).await.unwrap_err();
    assert!(err.contains("request failed"), "{err}");
}

#[tokio::test]
#[tracing_test::traced_test]
async fn tick_healthy_broadcasts_once_per_change() {
    let ready = Arc::new(AtomicU32::new(4));
    let base = fake_cloudflared(ready.clone()).await;
    let (state, _dir) = test_state(format!("{base}/ready"));
    let mut rx = state.event_tx.subscribe();
    let t0 = Instant::now();

    assert!(watch_tick(&state, t0).await);
    let (to, msg) = rx.try_recv().expect("first poll must broadcast");
    assert_eq!(to, crate::To::All, "tunnel status goes to every client");
    match msg {
        ServerMsg::TunnelStatus(info) => {
            assert_eq!(info.state, TunnelState::Ok);
            assert_eq!(info.ready_connections, 4);
        }
        other => panic!("expected TunnelStatus, got {other:?}"),
    }

    // Unchanged poll: no broadcast.
    assert!(!watch_tick(&state, t0 + POLL_INTERVAL).await);
    assert!(
        rx.try_recv().is_err(),
        "unchanged status must not broadcast"
    );

    assert!(logs_contain("tunnel /ready poll"));
    assert!(logs_contain("ready_connections=4"));
    logs_assert(|lines: &[&str]| {
        match lines
            .iter()
            .filter(|l| l.contains("tunnel ready connections changed"))
            .count()
        {
            1 => Ok(()),
            n => Err(format!("expected 1 count-change log, got {n}")),
        }
    });
}

#[tokio::test]
#[tracing_test::traced_test]
async fn tick_outage_and_recovery_sequence() {
    let ready = Arc::new(AtomicU32::new(0));
    let base = fake_cloudflared(ready.clone()).await;
    let (state, _dir) = test_state(format!("{base}/ready"));
    let mut rx = state.event_tx.subscribe();
    let t0 = Instant::now();

    // 0 connections → Down, broadcast.
    assert!(watch_tick(&state, t0).await);
    match rx.try_recv().unwrap().1 {
        ServerMsg::TunnelStatus(info) => assert_eq!(info.state, TunnelState::Down),
        other => panic!("expected TunnelStatus, got {other:?}"),
    }
    assert!(logs_contain("tunnel DOWN"));
    assert!(logs_contain("HTTP 503"));

    // Still 0 → nothing to say.
    assert!(!watch_tick(&state, t0 + secs(120)).await);
    assert!(rx.try_recv().is_err());

    // cloudflared reconnects → Ok.
    ready.store(4, Ordering::SeqCst);
    assert!(watch_tick(&state, t0 + secs(200)).await);
    assert_eq!(state.tunnel_watch.read().await.state(), TunnelState::Ok);
    assert!(logs_contain("tunnel RECOVERED"));
    assert!(logs_contain("down_for_secs=200"));
    assert!(!logs_contain("tunnel ready connections changed"));
}

#[tokio::test]
async fn tick_unreachable_endpoint_counts_as_zero() {
    let (state, _dir) = test_state(closed_port_url());
    assert!(watch_tick(&state, Instant::now()).await);
    let st = state.tunnel_watch.read().await.status(Instant::now());
    assert_eq!(st.state, TunnelState::Down);
    assert_eq!(st.ready_connections, 0);
}

#[tokio::test]
async fn current_status_msg_reflects_the_watch() {
    let (state, _dir) = test_state(closed_port_url());
    state.tunnel_watch.write().await.observe(2, Instant::now());
    match current_status_msg(&state).await {
        ServerMsg::TunnelStatus(info) => {
            assert_eq!(info.state, TunnelState::Ok);
            assert_eq!(info.ready_connections, 2);
        }
        other => panic!("expected TunnelStatus, got {other:?}"),
    }
}

#[tokio::test]
async fn api_tunnel_is_wired_into_the_app_router() {
    let (state, _dir) = test_state(closed_port_url());
    state.tunnel_watch.write().await.observe(3, Instant::now());
    let app = crate::routes::api_routes(state.clone()).with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/tunnel")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let info: TunnelStatusInfo = serde_json::from_slice(&body).unwrap();
    assert_eq!(info.state, TunnelState::Ok);
    assert_eq!(info.ready_connections, 3);
}

#[tokio::test]
async fn api_tunnel_returns_status_json() {
    let (state, _dir) = test_state(closed_port_url());
    state.tunnel_watch.write().await.observe(0, Instant::now());
    let app = tunnel_routes().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/tunnel")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["state"], "Down");
    assert_eq!(json["ready_connections"], 0);
    assert!(json["since_secs"].is_u64());
}
