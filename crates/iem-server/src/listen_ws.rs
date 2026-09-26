//! `/ws/audio` — Listen (F15, F17; X3, X4): the engineer listens to their
//! own mix (the engine's tap after the limiter, slot 0) or one other mix
//! (after its mute, through the listen limiter, slot 1) as binary Opus
//! frames; the wire format is the predecessor's. `ListenStart{member_id}`
//! starts the engine's tap; the last listener of a mix leaving stops it; a
//! second different mix while one is heard gets `no_source`.

use std::time::Duration;

use axum::{
    Json,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use iem_core::{ApiError, ClientMsg, ServerMsg};
use iem_engine_proto::{Cmd, ErrCode, MixId};
use tokio::sync::broadcast;
use tokio::time::Instant;

use crate::AppState;
use crate::engine::client::EngineError;
use crate::mixer_ws::{WsQuery, claims_of};

type Reject = (StatusCode, Json<ApiError>);

/// A listener more than this many frames behind skips to the newest.
pub const MAX_BEHIND: usize = 4;

pub async fn ws_audio(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
) -> Result<impl IntoResponse, Reject> {
    let claims = {
        let config = state.config.read().await;
        claims_of(query.token.as_deref(), &config.jwt_secret)?
    };
    if !claims.engineer {
        tracing::warn!(sub = %claims.sub, "Audio WS denied: not engineer");
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiError::new(
                "FORBIDDEN",
                "Audio streaming is engineer-only",
            )),
        ));
    }
    Ok(ws.on_upgrade(move |socket| session(socket, state)))
}

fn status(s: &str, target: Option<String>) -> Message {
    Message::Text(
        serde_json::to_string(&ServerMsg::AudioStatus {
            status: s.into(),
            target,
        })
        .unwrap_or_default()
        .into(),
    )
}

/// The listen slot of `mix` (0: the engineer's mix).
pub fn slot_of(state: &AppState, mix: &MixId) -> usize {
    let engineer = state
        .engine
        .mirror()
        .topology
        .as_ref()
        .map(|t| t.engineer.clone());
    usize::from(engineer.as_ref() != Some(mix))
}

/// Registers one more listener of `mix`; the first one starts the tap.
async fn start(state: &AppState, mix: &MixId) -> Result<(), EngineError> {
    state
        .engine
        .request(Cmd::StartListen { mix: mix.clone() }, None)
        .await?;
    *state
        .listeners
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(mix.clone())
        .or_default() += 1;
    Ok(())
}

/// One listener of `mix` left; the last one stops the tap.
async fn stop(state: &AppState, mix: &MixId) {
    let last = {
        let mut l = state
            .listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match l.get_mut(mix) {
            Some(n) if *n > 1 => {
                *n -= 1;
                false
            }
            Some(_) => {
                l.remove(mix);
                true
            }
            None => false,
        }
    };
    if last
        && let Err(e) = state
            .engine
            .request(Cmd::StopListen { mix: mix.clone() }, None)
            .await
    {
        tracing::warn!(%mix, error = %e, "stopping the listen tap failed");
    }
}

async fn session(mut socket: WebSocket, state: AppState) {
    tracing::info!("Audio WebSocket connected");
    let mut current: Option<(MixId, String)> = None;
    let mut rx: Option<broadcast::Receiver<bytes::Bytes>> = None;
    let mut last_audio = Instant::now();
    let mut first_logged = false;
    loop {
        let listening = rx.is_some();
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMsg>(&text) {
                    Ok(ClientMsg::ListenStart { member_id }) => {
                        if let Some((mix, _)) = current.take() {
                            stop(&state, &mix).await;
                        }
                        rx = None;
                        let Some(page) = state.page(&member_id) else {
                            if socket.send(status("no_source", None)).await.is_err() {
                                break;
                            }
                            continue;
                        };
                        match start(&state, &page.mix).await {
                            Ok(()) => {
                                tracing::info!(target = %member_id, mix = %page.mix, "Audio listen started");
                                rx = Some(state.media.subscribe(slot_of(&state, &page.mix)));
                                current = Some((page.mix.clone(), member_id.clone()));
                                last_audio = Instant::now();
                                if socket.send(status("listening", Some(member_id))).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                if !matches!(&e, EngineError::Refused(b) if b.code == ErrCode::NoSource) {
                                    tracing::warn!(error = %e, "listen start failed");
                                }
                                if socket.send(status("no_source", None)).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Ok(ClientMsg::ListenStop) => {
                        tracing::info!("Audio listen stopped");
                        if let Some((mix, _)) = current.take() {
                            stop(&state, &mix).await;
                        }
                        rx = None;
                        if socket.send(status("stopped", None)).await.is_err() {
                            break;
                        }
                    }
                    _ => {}
                },
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            frame = async { match rx.as_mut() { Some(r) => r.recv().await, None => std::future::pending().await } }, if listening => {
                match frame {
                    Ok(data) => {
                        if rx.as_ref().is_some_and(|r| r.len() > MAX_BEHIND) {
                            continue;
                        }
                        last_audio = Instant::now();
                        if socket.send(Message::Binary(data)).await.is_err() {
                            break;
                        }
                        state.media.forwarded();
                        if !first_logged {
                            tracing::info!("first binary frame forwarded on /ws/audio");
                            first_logged = true;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(skipped = n, "listen frames skipped");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(5)), if listening => {
                if last_audio.elapsed() > Duration::from_secs(5) {
                    if socket.send(status("no_source", None)).await.is_err() {
                        break;
                    }
                    last_audio = Instant::now();
                }
            }
        }
    }
    if let Some((mix, _)) = current.take() {
        stop(&state, &mix).await;
    }
    tracing::info!("Audio WebSocket disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_engineer_mix_is_slot_zero_once_the_topology_is_known() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(crate::site_view::tests::test_config(), dir.path());
        // No topology yet: every mix is taken as another mix's.
        assert_eq!(slot_of(&state, &MixId::new("engineer")), 1);
        assert_eq!(slot_of(&state, &MixId::new("member1")), 1);
    }
}
