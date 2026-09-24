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
fn new_watch_is_ok_with_no_connections_and_no_restart() {
    let t0 = Instant::now();
    let w = TunnelWatch::new(t0);
    assert_eq!(w.state(), TunnelState::Ok);
    assert_eq!(w.restart_count(), 0);
    assert_eq!(
        status_at(&w, t0 + secs(7)),
        TunnelStatusInfo {
            state: TunnelState::Ok,
            ready_connections: 0,
            since_secs: 7,
            last_restart_secs_ago: None,
            last_restart_ok: None,
        }
    );
}

#[test]
fn positive_poll_keeps_ok_and_reports_count_change() {
    let t0 = Instant::now();
    let mut w = TunnelWatch::new(t0);
    let d = w.observe(4, t0 + secs(1));
    assert_eq!(
        d,
        Decision {
            should_restart: false,
            changed: true
        }
    );
    // Same count again → nothing changed.
    let d = w.observe(4, t0 + secs(31));
    assert_eq!(
        d,
        Decision {
            should_restart: false,
            changed: false
        }
    );
    // Different positive count → changed, still Ok, `since` preserved.
    let d = w.observe(2, t0 + secs(61));
    assert!(d.changed);
    assert!(!d.should_restart);
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
fn zero_poll_goes_down_immediately_without_restart() {
    let t0 = Instant::now();
    let mut w = TunnelWatch::new(t0);
    w.observe(4, t0);
    let d = w.observe(0, t0 + secs(30));
    assert_eq!(
        d,
        Decision {
            should_restart: false,
            changed: true
        }
    );
    let st = status_at(&w, t0 + secs(40));
    assert_eq!(st.state, TunnelState::Down);
    assert_eq!(st.ready_connections, 0);
    assert_eq!(st.since_secs, 10);
}

#[test]
fn no_restart_before_120s_of_zeros() {
    let t0 = Instant::now();
    let mut w = TunnelWatch::new(t0);
    for t in [0, 30, 60, 90, 119] {
        let d = w.observe(0, t0 + secs(t));
        assert!(!d.should_restart, "restart too early at {t}s");
        assert_eq!(w.state(), TunnelState::Down);
    }
    // Unchanged Down poll is not a change.
    assert!(!w.observe(0, t0 + secs(119)).changed);
    assert_eq!(w.restart_count(), 0);
}

#[test]
fn exactly_one_restart_at_120s() {
    let t0 = Instant::now();
    let mut w = TunnelWatch::new(t0);
    w.observe(0, t0);
    let d = w.observe(0, t0 + secs(120));
    assert_eq!(
        d,
        Decision {
            should_restart: true,
            changed: true
        }
    );
    assert_eq!(w.state(), TunnelState::Restarting);
    assert_eq!(w.restart_count(), 1);
    let st = status_at(&w, t0 + secs(125));
    assert_eq!(st.since_secs, 125, "since = when the tunnel went down");
    assert_eq!(st.last_restart_secs_ago, Some(5));
    // Following polls inside the grace window do not restart again.
    for t in [150, 179] {
        let d = w.observe(0, t0 + secs(t));
        assert!(!d.should_restart);
        assert!(!d.changed);
        assert_eq!(w.state(), TunnelState::Restarting);
    }
    assert_eq!(w.restart_count(), 1);
}

#[test]
fn restarting_falls_back_to_down_after_grace_keeping_down_since() {
    let t0 = Instant::now();
    let mut w = TunnelWatch::new(t0);
    w.observe(0, t0);
    w.observe(0, t0 + secs(120)); // restart
    let d = w.observe(0, t0 + secs(180)); // grace (60 s) elapsed
    assert_eq!(
        d,
        Decision {
            should_restart: false,
            changed: true
        }
    );
    assert_eq!(w.state(), TunnelState::Down);
    assert_eq!(status_at(&w, t0 + secs(180)).since_secs, 180);
}

#[test]
fn cooldown_blocks_second_restart_within_10_min() {
    let t0 = Instant::now();
    let mut w = TunnelWatch::new(t0);
    w.observe(0, t0);
    assert!(w.observe(0, t0 + secs(120)).should_restart);
    for t in (150..=719).step_by(30).chain([719]) {
        let d = w.observe(0, t0 + secs(t));
        assert!(!d.should_restart, "second restart inside cooldown at {t}s");
    }
    assert_eq!(w.restart_count(), 1);
    // 600 s after the first restart (t=720) the next one is allowed.
    let d = w.observe(0, t0 + secs(720));
    assert!(d.should_restart);
    assert!(d.changed);
    assert_eq!(w.state(), TunnelState::Restarting);
    assert_eq!(w.restart_count(), 2);
    assert_eq!(status_at(&w, t0 + secs(720)).last_restart_secs_ago, Some(0));
}

#[test]
fn new_outage_after_recovery_still_respects_cooldown() {
    let t0 = Instant::now();
    let mut w = TunnelWatch::new(t0);
    w.observe(0, t0);
    assert!(w.observe(0, t0 + secs(120)).should_restart);
    // Recovered.
    let d = w.observe(4, t0 + secs(150));
    assert!(d.changed);
    assert_eq!(w.state(), TunnelState::Ok);
    assert_eq!(status_at(&w, t0 + secs(150)).since_secs, 0);
    // Down again at 300 s; 120 s later (420 s) the cooldown (until 720 s) still blocks.
    w.observe(0, t0 + secs(300));
    assert_eq!(
        status_at(&w, t0 + secs(300)).since_secs,
        0,
        "new outage resets since"
    );
    assert!(!w.observe(0, t0 + secs(420)).should_restart);
    assert!(!w.observe(0, t0 + secs(690)).should_restart);
    // Cooldown over → restart.
    assert!(w.observe(0, t0 + secs(720)).should_restart);
    assert_eq!(w.restart_count(), 2);
}

#[test]
fn recovery_during_restarting_goes_ok_and_resets() {
    let t0 = Instant::now();
    let mut w = TunnelWatch::new(t0);
    w.observe(0, t0);
    w.observe(0, t0 + secs(120));
    let d = w.observe(3, t0 + secs(130));
    assert_eq!(
        d,
        Decision {
            should_restart: false,
            changed: true
        }
    );
    let st = status_at(&w, t0 + secs(140));
    assert_eq!(st.state, TunnelState::Ok);
    assert_eq!(st.ready_connections, 3);
    assert_eq!(st.since_secs, 10);
    assert_eq!(st.last_restart_secs_ago, Some(20));
    // A fresh outage needs another full 120 s before a restart check.
    w.observe(0, t0 + secs(800));
    assert!(!w.observe(0, t0 + secs(919)).should_restart);
    assert!(w.observe(0, t0 + secs(920)).should_restart);
}

#[test]
fn restart_result_is_recorded_and_reset_by_the_next_restart() {
    let t0 = Instant::now();
    let mut w = TunnelWatch::new(t0);
    w.observe(0, t0);
    assert!(w.observe(0, t0 + secs(120)).should_restart);
    assert_eq!(w.status(t0 + secs(121)).last_restart_ok, None);
    assert!(
        w.record_restart_result(false),
        "None -> Some(false) is a change"
    );
    assert!(
        !w.record_restart_result(false),
        "same result is not a change"
    );
    assert_eq!(w.status(t0 + secs(125)).last_restart_ok, Some(false));
    assert!(w.record_restart_result(true));
    assert_eq!(w.status(t0 + secs(126)).last_restart_ok, Some(true));
    // The next restart starts pending again.
    w.observe(0, t0 + secs(180));
    assert!(w.observe(0, t0 + secs(720)).should_restart);
    assert_eq!(w.status(t0 + secs(720)).last_restart_ok, None);
}

#[test]
fn new_outage_clears_the_previous_restart_outcome() {
    let t0 = Instant::now();
    let mut w = TunnelWatch::new(t0);
    w.observe(0, t0);
    assert!(w.observe(0, t0 + secs(120)).should_restart);
    w.record_restart_result(false);
    // Still down after the grace: the failure stays visible.
    w.observe(0, t0 + secs(180));
    assert_eq!(w.status(t0 + secs(180)).last_restart_ok, Some(false));
    // Recovery keeps it for diagnostics…
    w.observe(4, t0 + secs(200));
    assert_eq!(w.status(t0 + secs(200)).last_restart_ok, Some(false));
    // …but a NEW outage starts without a stale "repair failed".
    w.observe(0, t0 + secs(900));
    let st = w.status(t0 + secs(900));
    assert_eq!(st.state, TunnelState::Down);
    assert_eq!(st.last_restart_ok, None);
    assert_eq!(st.last_restart_secs_ago, Some(780));
}

#[test]
fn stop_accepted_only_for_ok_and_not_active() {
    assert!(stop_accepted(Some(0)));
    assert!(stop_accepted(Some(SC_ERR_SERVICE_NOT_ACTIVE)));
    assert!(stop_accepted(Some(1062)));
    assert!(
        !stop_accepted(Some(5)),
        "access denied: service still running"
    );
    assert!(!stop_accepted(Some(SC_ERR_SERVICE_ALREADY_RUNNING)));
    assert!(!stop_accepted(Some(1)));
    assert!(!stop_accepted(None));
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

#[test]
fn parse_sc_state_reads_the_state_code() {
    let running = "\r\nSERVICE_NAME: cloudflared \r\n        TYPE               : 10  WIN32_OWN_PROCESS  \r\n        STATE              : 4  RUNNING \r\n        WIN32_EXIT_CODE    : 0  (0x0)\r\n";
    assert_eq!(parse_sc_state(running), Some(4));
    let stopped = "        STATE              : 1  STOPPED \r\n";
    assert_eq!(parse_sc_state(stopped), Some(SC_STATE_STOPPED));
    assert_eq!(parse_sc_state("        STATE : 3  STOP_PENDING"), Some(3));
}

#[test]
fn parse_sc_state_rejects_garbage() {
    assert_eq!(parse_sc_state(""), None);
    assert_eq!(parse_sc_state("[SC] OpenService FAILED 1060"), None);
    assert_eq!(
        parse_sc_state("        STATE              : X  RUNNING"),
        None
    );
    assert_eq!(parse_sc_state("        STATE_OTHER        4"), None);
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

    let d = watch_tick(&state, t0).await;
    assert_eq!(
        d,
        Decision {
            should_restart: false,
            changed: true
        }
    );
    let (mid, msg) = rx.try_recv().expect("first poll must broadcast");
    assert!(mid.is_empty(), "tunnel status goes to every client");
    match msg {
        ServerMsg::TunnelStatus(info) => {
            assert_eq!(info.state, TunnelState::Ok);
            assert_eq!(info.ready_connections, 4);
            assert_eq!(info.last_restart_secs_ago, None);
        }
        other => panic!("expected TunnelStatus, got {other:?}"),
    }

    // Unchanged poll: no broadcast.
    let d = watch_tick(&state, t0 + POLL_INTERVAL).await;
    assert!(!d.changed);
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
async fn tick_outage_restart_and_recovery_sequence() {
    let ready = Arc::new(AtomicU32::new(0));
    let base = fake_cloudflared(ready.clone()).await;
    let (state, _dir) = test_state(format!("{base}/ready"));
    let mut rx = state.event_tx.subscribe();
    let t0 = Instant::now();

    // 0 connections → Down, broadcast, no restart yet.
    let d = watch_tick(&state, t0).await;
    assert!(d.changed && !d.should_restart);
    match rx.try_recv().unwrap().1 {
        ServerMsg::TunnelStatus(info) => assert_eq!(info.state, TunnelState::Down),
        other => panic!("expected TunnelStatus, got {other:?}"),
    }
    assert!(logs_contain("tunnel DOWN"));
    assert!(logs_contain("HTTP 503"));

    // 120 s later → restart requested, Restarting broadcast.
    let d = watch_tick(&state, t0 + DOWN_BEFORE_RESTART).await;
    assert!(d.should_restart && d.changed);
    match rx.try_recv().unwrap().1 {
        ServerMsg::TunnelStatus(info) => {
            assert_eq!(info.state, TunnelState::Restarting);
            assert_eq!(info.last_restart_secs_ago, Some(0));
        }
        other => panic!("expected TunnelStatus, got {other:?}"),
    }
    assert!(logs_contain("restarting cloudflared service"));

    // Grace over, still 0 → back to Down.
    let d = watch_tick(&state, t0 + DOWN_BEFORE_RESTART + RESTART_GRACE).await;
    assert!(d.changed && !d.should_restart);
    assert!(logs_contain("still down after cloudflared restart"));
    assert_eq!(state.tunnel_watch.read().await.state(), TunnelState::Down);

    // cloudflared reconnects → Ok.
    ready.store(4, Ordering::SeqCst);
    let d = watch_tick(&state, t0 + secs(200)).await;
    assert!(d.changed && !d.should_restart);
    assert_eq!(state.tunnel_watch.read().await.state(), TunnelState::Ok);
    assert!(logs_contain("tunnel RECOVERED"));
    assert!(logs_contain("down_for_secs=200"));
    assert!(!logs_contain("tunnel ready connections changed"));
}

#[tokio::test]
#[tracing_test::traced_test]
async fn record_restart_broadcasts_only_changes_and_logs_outcome() {
    let (state, _dir) = test_state(closed_port_url());
    let t0 = Instant::now();
    {
        let mut w = state.tunnel_watch.write().await;
        w.observe(0, t0);
        assert!(w.observe(0, t0 + DOWN_BEFORE_RESTART).should_restart);
    }
    let mut rx = state.event_tx.subscribe();

    record_restart(&state, false, t0 + secs(125)).await;
    match rx.try_recv().expect("failed restart must broadcast").1 {
        ServerMsg::TunnelStatus(info) => {
            assert_eq!(info.state, TunnelState::Restarting);
            assert_eq!(info.last_restart_ok, Some(false));
            assert_eq!(info.last_restart_secs_ago, Some(5));
        }
        other => panic!("expected TunnelStatus, got {other:?}"),
    }
    assert!(logs_contain("cloudflared restart FAILED"));

    // Same outcome again → no broadcast.
    record_restart(&state, false, t0 + secs(126)).await;
    assert!(rx.try_recv().is_err());

    record_restart(&state, true, t0 + secs(127)).await;
    match rx.try_recv().expect("changed outcome must broadcast").1 {
        ServerMsg::TunnelStatus(info) => assert_eq!(info.last_restart_ok, Some(true)),
        other => panic!("expected TunnelStatus, got {other:?}"),
    }
    assert!(logs_contain("cloudflared restart finished OK"));
}

#[tokio::test]
async fn tick_unreachable_endpoint_counts_as_zero() {
    let (state, _dir) = test_state(closed_port_url());
    let d = watch_tick(&state, Instant::now()).await;
    assert!(d.changed);
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
    assert!(json["last_restart_secs_ago"].is_null());
    assert!(json["last_restart_ok"].is_null());
}
