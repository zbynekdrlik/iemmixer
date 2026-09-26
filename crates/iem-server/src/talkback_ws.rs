//! `/ws/talkback` — the engineer's push-to-talk audio (F18; X5, X6, §5.3):
//! the socket binds to the talk id the mixer session holds (`&talk=<id>` at
//! the upgrade, within 1 s of acquiring); its Opus frames are decoded and
//! played out every 20 ms into the engine's talkback stream. The wire format
//! (one Opus packet per binary message) is the predecessor's.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use iem_core::ApiError;

use crate::AppState;
use crate::mixer_ws::{WsQuery, claims_of};
use crate::talkback_buffer::{FRAME_MS, TalkbackDecoder};

type Reject = (StatusCode, Json<ApiError>);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub async fn ws_talkback(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
) -> Result<impl IntoResponse, Reject> {
    let claims = {
        let config = state.config.read().await;
        claims_of(query.token.as_deref(), &config.jwt_secret)?
    };
    if !claims.engineer {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiError::new("FORBIDDEN", "Talkback is engineer-only")),
        ));
    }
    let id = query.talk.unwrap_or_default();
    if lock(&state.talk).bind(&id, Instant::now()).is_none() {
        tracing::warn!("talkback socket without the held talk id refused");
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiError::new("TALK_NOT_HELD", "Press Talk first")),
        ));
    }
    Ok(ws.on_upgrade(move |socket| session(socket, state, id)))
}

async fn session(mut socket: WebSocket, state: AppState, id: String) {
    tracing::info!("Talkback WebSocket connected");
    let metrics = Arc::clone(&state.talkback_metrics);
    metrics.bitrate_kbps.store(96, Ordering::Relaxed);
    let decoder = match TalkbackDecoder::new() {
        Ok(d) => Arc::new(Mutex::new(d)),
        Err(e) => {
            tracing::error!(error = %e, "no Opus decoder: talkback closed");
            return;
        }
    };
    let playout = {
        let (decoder, state, metrics) = (Arc::clone(&decoder), state.clone(), Arc::clone(&metrics));
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(u64::from(FRAME_MS)));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let (frame, fill, overflows, concealed) = {
                    let mut d = lock(&decoder);
                    let before = d.concealed;
                    let f = d.tick();
                    (f, d.fill_ms(), d.overflows, d.concealed - before)
                };
                metrics.buffer_fill_ms.store(fill, Ordering::Relaxed);
                metrics.buffer_overflows.store(overflows, Ordering::Relaxed);
                metrics.underruns.fetch_add(concealed, Ordering::Relaxed);
                if let Some(frame) = frame
                    && state.media.send_talkback(frame)
                {
                    metrics.packets_out.fetch_add(1, Ordering::Relaxed);
                }
            }
        })
    };
    let mut last = Instant::now();
    loop {
        match socket.recv().await {
            Some(Ok(Message::Binary(data))) => {
                let now = Instant::now();
                metrics.packets_in.fetch_add(1, Ordering::Relaxed);
                metrics.last_packet_age_ms.store(
                    now.duration_since(last).as_millis() as u64,
                    Ordering::Relaxed,
                );
                last = now;
                lock(&state.talk).frame(&id, now);
                lock(&decoder).push(data.to_vec());
            }
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
            Some(Ok(_)) => {}
        }
    }
    lock(&decoder).clear();
    playout.abort();
    tracing::info!("Talkback WebSocket disconnected");
}

/// `GET /api/talkback/diagnostics` (F28): the predecessor's fields; the
/// receiver is now the engine's media pipe.
pub fn diagnostics(state: &AppState) -> serde_json::Value {
    let m = &state.talkback_metrics;
    let receiver = state
        .media
        .connected()
        .then(|| serde_json::Value::String(format!("{}.media", state.media.pipe())))
        .unwrap_or(serde_json::Value::Null);
    let talker = lock(&state.talk)
        .holder()
        .map(|h| serde_json::Value::String(h.to_string()))
        .unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "recv_vst_addr": receiver,
        "active_talker": talker,
        "packets_in": m.packets_in.load(Ordering::Relaxed),
        "packets_out": m.packets_out.load(Ordering::Relaxed),
        "seq_gaps": m.seq_gaps.load(Ordering::Relaxed),
        "buffer_fill_ms": m.buffer_fill_ms.load(Ordering::Relaxed),
        "buffer_overflows": m.buffer_overflows.load(Ordering::Relaxed),
        "last_packet_age_ms": m.last_packet_age_ms.load(Ordering::Relaxed),
        "underruns": m.underruns.load(Ordering::Relaxed),
        "bitrate_kbps": m.bitrate_kbps.load(Ordering::Relaxed),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn diagnostics_keep_the_predecessors_fields() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(iem_core::Config::default(), dir.path());
        let d = diagnostics(&state);
        let keys: Vec<&str> = d.as_object().unwrap().keys().map(String::as_str).collect();
        for k in [
            "recv_vst_addr",
            "active_talker",
            "packets_in",
            "packets_out",
            "seq_gaps",
            "buffer_fill_ms",
            "buffer_overflows",
            "last_packet_age_ms",
            "underruns",
            "bitrate_kbps",
        ] {
            assert!(keys.contains(&k), "{k}");
        }
        assert_eq!(d["recv_vst_addr"], serde_json::Value::Null);
        assert_eq!(d["active_talker"], serde_json::Value::Null);
        lock(&state.talk)
            .acquire(1, "engineer", "aa".into(), Instant::now())
            .unwrap();
        assert_eq!(diagnostics(&state)["active_talker"], "engineer");
    }
}
