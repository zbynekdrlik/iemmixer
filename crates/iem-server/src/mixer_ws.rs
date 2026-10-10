//! `/ws/{page}` — the mixer page's WebSocket (UI protocol v2; S5 design note
//! §5, §6). The first message is the hello; then the page's state from the
//! mirror, pins and hides, solo, alerts, network mode and tunnel status.
//! Commands become engine requests (tagged with this session as their
//! origin); engine deltas become UI updates for every other session on the
//! same page; meters arrive every 100 ms. A client silent for
//! [`ws_alive::SILENT_FOR`] (no command, no pong to the session's pings) is
//! gone, and its session ends; every send is bounded (`ws_alive`, #10).

use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use axum::{
    Json,
    extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use iem_core::{ApiError, ClientMsg, MIN_CLIENT_PROTO, ServerMsg, UI_PROTO};
use iem_engine_proto::{Change, Cmd, EqTarget, MixId, Source};
use tokio::sync::{broadcast, mpsc};

use crate::engine::client::EngineEvent;
use crate::site_view::Page;
use crate::view::{self, Viewer};
use crate::ws_alive::{self, Due};
use crate::{AppState, To};

/// Close code telling a page without a (current) protocol to reload.
pub const CLOSE_RELOAD: u16 = 4001;

/// Query parameters of the page and media sockets.
#[derive(Debug, serde::Deserialize)]
pub struct WsQuery {
    pub token: Option<String>,
    #[serde(default)]
    pub proto: Option<u16>,
    #[serde(default)]
    pub talk: Option<String>,
    /// `/ws/audio`: `1` asks for the listen probe (S7, `listen_ws::asks_for_probe`).
    #[serde(default)]
    pub hil: Option<u8>,
}

type Reject = (StatusCode, Json<ApiError>);

/// The token's claims if present, valid and unexpired.
pub fn claims_of(token: Option<&str>, secret: &str) -> Result<iem_core::AuthClaims, Reject> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    claims_at(token, secret, now)
}

/// [`claims_of`] at the Unix time `now` (seconds): a token is valid through
/// the second of its `exp`.
pub fn claims_at(
    token: Option<&str>,
    secret: &str,
    now: u64,
) -> Result<iem_core::AuthClaims, Reject> {
    let token = token.ok_or((StatusCode::UNAUTHORIZED, Json(ApiError::unauthorized())))?;
    let claims = crate::auth::extract_claims(token, secret)
        .ok_or((StatusCode::UNAUTHORIZED, Json(ApiError::unauthorized())))?;
    if claims.exp < now {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiError::new("TOKEN_EXPIRED", "Token has expired")),
        ));
    }
    Ok(claims)
}

/// Whether a connection with these claims may open `page`: its own member
/// page, or any page for the engineer (member-less pages are the engineer's).
pub fn may_open(claims: &iem_core::AuthClaims, page: &Page) -> bool {
    claims.engineer || (page.member.as_deref() == Some(claims.sub.as_str()))
}

/// Whether a client speaking `proto` is served.
pub fn proto_ok(proto: u16) -> bool {
    (MIN_CLIENT_PROTO..=UI_PROTO).contains(&proto)
}

pub async fn ws_mixer(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(page_id): Path<String>,
    Query(query): Query<WsQuery>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, Reject> {
    let (claims, network_mode) = {
        let config = state.config.read().await;
        let claims = claims_of(query.token.as_deref(), &config.jwt_secret)?;
        (
            claims,
            crate::routes::detect_network_mode(&headers, &config.local_public_ip),
        )
    };
    if !claims.engineer && claims.sub != page_id {
        tracing::warn!(page = %page_id, sub = %claims.sub, "WS denied: cross-member access");
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiError::new(
                "FORBIDDEN",
                "Access denied to this member's mixer",
            )),
        ));
    }
    let page = state
        .page(&page_id)
        .ok_or((StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))))?;
    if !may_open(&claims, &page) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiError::new("FORBIDDEN", "This page is the engineer's")),
        ));
    }
    let viewer = Viewer {
        sub: claims.sub,
        engineer: claims.engineer,
    };
    let proto = query.proto;
    Ok(ws.on_upgrade(move |socket| async move {
        match proto {
            Some(p) => session(socket, state, page, viewer, network_mode, p).await,
            None => {
                tracing::info!(page = %page.id, "WS without a protocol: asking the page to reload");
                close_reload(socket).await;
            }
        }
    }))
}

async fn close_reload(mut socket: WebSocket) {
    let close = Message::Close(Some(CloseFrame {
        code: CLOSE_RELOAD,
        reason: "reload".into(),
    }));
    // Bounded like every send of a session (`ws_alive`, #10).
    let _ = ws_alive::within(ws_alive::CLOSE_WITHIN, socket.send(close)).await;
}

fn text(msg: &ServerMsg) -> Message {
    Message::Text(serde_json::to_string(msg).unwrap_or_default().into())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The page's full state now.
fn full_state(state: &AppState, page: &Page, viewer: &Viewer) -> ServerMsg {
    match state.site() {
        Some(site) => view::state_msg(
            &site,
            &state.engine.mirror(),
            page,
            viewer,
            state.engine.connected(),
        ),
        None => ServerMsg::State {
            channels: Vec::new(),
            connected: false,
            global_level_db: None,
            global_muted: None,
            mix: Some(page.mix.0.clone()),
            stems_level_db: None,
            stems_muted: None,
            group: None,
        },
    }
}

fn solo_msg(state: &AppState, page: &Page) -> ServerMsg {
    ServerMsg::SoloUpdate {
        soloed: state
            .engine
            .mirror()
            .solo(&page.mix)
            .iter()
            .map(ToString::to_string)
            .collect(),
    }
}

fn customization_msg(state: &AppState, page: &Page) -> ServerMsg {
    let (pinned, hidden) = match page.member.as_deref().map(|m| state.band.customization(m)) {
        Some(Ok(c)) => (
            c.pinned.iter().map(ToString::to_string).collect(),
            c.hidden.iter().map(ToString::to_string).collect(),
        ),
        Some(Err(e)) => {
            tracing::error!(page = %page.id, error = %e, "pins and hides unreadable");
            (Vec::new(), Vec::new())
        }
        None => (Vec::new(), Vec::new()),
    };
    ServerMsg::CustomizationUpdate { pinned, hidden }
}

/// The alert catch-up a connection gets: every active alert on the
/// engineer's page, a member's own alert on their page.
pub fn alert_catchup(
    active: &std::collections::HashMap<String, (String, String)>,
    page: &str,
) -> Option<ServerMsg> {
    if page == crate::pin_store::ENGINEER_ID {
        if active.is_empty() {
            return None;
        }
        let mut alerts: Vec<iem_core::AlertInfo> = active
            .values()
            .map(|(from_member, from_name)| iem_core::AlertInfo {
                from_member: from_member.clone(),
                from_name: from_name.clone(),
            })
            .collect();
        alerts.sort_by(|a, b| a.from_member.cmp(&b.from_member));
        Some(ServerMsg::ActiveAlerts { alerts })
    } else if active.contains_key(page) {
        Some(ServerMsg::EngineerAlert {
            from_member: page.to_string(),
            from_name: String::new(),
        })
    } else {
        None
    }
}

/// Whether an app event for `to` goes to this connection.
pub fn addressed(to: &To, page: &str, session: u64) -> bool {
    match to {
        To::All => true,
        To::Page(p) => p == page,
        To::Session(s) => *s == session,
    }
}

async fn session(
    mut socket: WebSocket,
    state: AppState,
    page: Page,
    viewer: Viewer,
    network_mode: String,
    proto: u16,
) {
    let session = state.next_session();
    tracing::info!(page = %page.id, session, network_mode = %network_mode, "WebSocket connected");
    let hello = ServerMsg::Hello {
        proto: UI_PROTO,
        build: iem_core::VERSION.to_string(),
        min_client_proto: MIN_CLIENT_PROTO,
    };
    if !ws_alive::send(socket.send(text(&hello))).await {
        return;
    }
    if !proto_ok(proto) {
        tracing::info!(page = %page.id, proto, "WS protocol out of range: the page reloads");
        close_reload(socket).await;
        return;
    }
    let mut engine_rx = state.engine.subscribe();
    let mut meters_rx = state.meters_tx.subscribe();
    let mut app_rx = state.event_tx.subscribe();
    lock(&state.solo).connected(&page.mix.0);

    let mut first = vec![
        full_state(&state, &page, &viewer),
        customization_msg(&state, &page),
        solo_msg(&state, &page),
    ];
    if let Some(m) = alert_catchup(&lock(&state.alerts), &page.id) {
        first.push(m);
    }
    first.push(ServerMsg::NetworkMode { mode: network_mode });
    first.push(crate::tunnel_watch::current_status_msg(&state).await);
    for m in &first {
        if !ws_alive::send(socket.send(text(m))).await {
            cleanup(&state, &page, session);
            return;
        }
    }

    // Commands run one at a time in their own task, so a slow engine never
    // stops meters and updates.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ServerMsg>();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<ClientMsg>();
    let worker = {
        let (state, page, viewer) = (state.clone(), page.clone(), viewer.clone());
        tokio::spawn(async move {
            while let Some(msg) = cmd_rx.recv().await {
                handle(&state, session, &page, &viewer, msg, &out_tx).await;
            }
        })
    };

    // A client that answers nothing for `ws_alive::SILENT_FOR` is gone
    // (#10): pinged, it answers by itself when it is there.
    let mut alive = ws_alive::Keepalive::default();
    loop {
        let out: Vec<ServerMsg> = tokio::select! {
            msg = socket.recv() => {
                if matches!(msg, Some(Ok(_))) {
                    alive.heard();
                }
                match msg {
                    Some(Ok(Message::Text(t))) => {
                        match serde_json::from_str::<ClientMsg>(&t) {
                            Ok(cmd) => {
                                let _ = cmd_tx.send(cmd);
                            }
                            Err(e) => tracing::warn!(page = %page.id, error = %e, "unreadable WS command"),
                        }
                        Vec::new()
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => Vec::new(),
                }
            },
            due = alive.due() => match due {
                Due::Gone(silent) => {
                    tracing::warn!(page = %page.id, session, silent_s = silent.as_secs(), "WebSocket client silent: closing");
                    let _ = ws_alive::within(ws_alive::CLOSE_WITHIN, socket.send(ws_alive::silent_close())).await;
                    break;
                }
                Due::Ping => {
                    if !ws_alive::send(socket.send(ws_alive::ping())).await {
                        break;
                    }
                    Vec::new()
                }
            },
            ev = engine_rx.recv() => match ev {
                Ok(ev) => engine_event(&state, session, &page, &viewer, ev),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(page = %page.id, skipped = n, "engine events lagged: full state");
                    vec![full_state(&state, &page, &viewer), solo_msg(&state, &page)]
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            m = meters_rx.recv() => match m {
                Ok(merged) => meters_msg(&state, &page, &merged).into_iter().collect(),
                Err(broadcast::error::RecvError::Lagged(_)) => Vec::new(),
                Err(broadcast::error::RecvError::Closed) => break,
            },
            ev = app_rx.recv() => match ev {
                Ok((to, msg)) if addressed(&to, &page.id, session) => vec![msg],
                Ok(_) => Vec::new(),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(page = %page.id, skipped = n, "app events lagged");
                    Vec::new()
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            Some(m) = out_rx.recv() => vec![m],
        };
        let mut failed = false;
        for m in &out {
            if !ws_alive::send(socket.send(text(m))).await {
                failed = true;
                break;
            }
        }
        if failed {
            break;
        }
    }
    worker.abort();
    cleanup(&state, &page, session);
    tracing::info!(page = %page.id, session, "WebSocket disconnected");
}

fn cleanup(state: &AppState, page: &Page, session: u64) {
    lock(&state.solo).disconnected(&page.mix.0, Instant::now());
    if lock(&state.talk).release(session) {
        state.broadcast(To::All, ServerMsg::EngineerTalking { active: false });
    }
}

fn meters_msg(state: &AppState, page: &Page, merged: &crate::meters::Merged) -> Option<ServerMsg> {
    let site = state.site()?;
    let topo = state.engine.mirror().topology.clone()?;
    Some(ServerMsg::Meters {
        meters: crate::meters::page_meters(merged, &topo, page, site.group.as_ref()),
    })
}

/// The UI messages one engine event produces for this connection.
fn engine_event(
    state: &AppState,
    session: u64,
    page: &Page,
    viewer: &Viewer,
    ev: EngineEvent,
) -> Vec<ServerMsg> {
    match ev {
        EngineEvent::Connected => vec![
            ServerMsg::ConnectionChanged { connected: true },
            full_state(state, page, viewer),
        ],
        EngineEvent::Disconnected => vec![ServerMsg::ConnectionChanged { connected: false }],
        EngineEvent::Reset => vec![full_state(state, page, viewer), solo_msg(state, page)],
        EngineEvent::Changed { origin, changes } => {
            let Some(site) = state.site() else {
                return Vec::new();
            };
            let mut out = if origin == Some(session) {
                own_echo(&site, &state.engine.mirror(), page, &changes)
            } else {
                view::updates_for(&site, &state.engine.mirror(), page, &changes)
            };
            if viewer.engineer {
                out.extend(crate::console::input_updates(&site, &changes));
            }
            out
        }
        EngineEvent::Meters(_) | EngineEvent::Status(_) | EngineEvent::Alarm(_) => Vec::new(),
    }
}

/// What a session hears back from its own change: nothing (it applied the
/// change optimistically), except the channel mutes a solo change shows (the
/// mask is the server's view; the page cannot know every level's own mute).
pub fn own_echo(
    site: &crate::site_view::SiteView,
    mirror: &crate::engine::mirror::Mirror,
    page: &Page,
    changes: &[Change],
) -> Vec<ServerMsg> {
    let solo: Vec<Change> = changes
        .iter()
        .filter(|c| matches!(c, Change::Solo { .. }))
        .cloned()
        .collect();
    view::updates_for(site, mirror, page, &solo)
        .into_iter()
        .filter(|m| matches!(m, ServerMsg::ChannelUpdate { .. }))
        .collect()
}

/// Whether `cmd` persistently changes the mix of `mix`'s page (the daily
/// auto-snapshot is taken before such a change).
pub fn changes_mix(cmd: &Cmd, mix: &MixId) -> bool {
    match cmd {
        Cmd::SetLevel { mix: m, .. }
        | Cmd::SetMix { mix: m, .. }
        | Cmd::SetGroup { mix: m, .. }
        | Cmd::SetLimiter { mix: m, .. } => m == mix,
        Cmd::SetEq { target, .. } => match target {
            EqTarget::Mix(m) | EqTarget::Group { mix: m, .. } => m == mix,
            EqTarget::Input(_) => false,
        },
        Cmd::Batch { ops } => ops.iter().any(|c| changes_mix(c, mix)),
        _ => false,
    }
}

/// One UI command.
async fn handle(
    state: &AppState,
    session: u64,
    page: &Page,
    viewer: &Viewer,
    msg: ClientMsg,
    out: &mpsc::UnboundedSender<ServerMsg>,
) {
    let site = state.site();
    let send = |m: ServerMsg| {
        let _ = out.send(m);
    };
    match &msg {
        ClientMsg::UpdateCustomization { pinned, hidden } => {
            let (Some(member), Some(site)) = (page.member.as_deref(), site.as_deref()) else {
                return;
            };
            let known = |ids: &[String]| -> (Vec<String>, Vec<Source>) {
                ids.iter()
                    .filter_map(|id| view::source(site, page, id).map(|s| (id.clone(), s)))
                    .unzip()
            };
            let (pinned_ids, pinned_src) = known(pinned);
            let (hidden_ids, hidden_src) = known(hidden);
            if let Err(e) = state
                .band
                .save_customization(member, pinned_src, hidden_src)
            {
                tracing::error!(%member, error = %e, "saving pins and hides failed");
                return;
            }
            state.broadcast(
                To::Page(page.id.clone()),
                ServerMsg::CustomizationUpdate {
                    pinned: pinned_ids,
                    hidden: hidden_ids,
                },
            );
        }
        ClientMsg::GetEqParams { target } => {
            let Some(site) = site else { return };
            match view::eq_params_msg(&site, &state.engine.mirror(), page, viewer, target) {
                Ok(m) => send(m),
                Err(e) => {
                    tracing::warn!(page = %page.id, %target, error = %e, "EQ request refused")
                }
            }
        }
        ClientMsg::GetLimiterParams => {
            let active = state.active_seconds(&page.mix);
            send(view::limiter_msg(&state.engine.mirror(), page, active));
        }
        ClientMsg::GetConsole => {
            if viewer.engineer
                && let Some(site) = site
            {
                send(ServerMsg::Console(crate::console::console_info(
                    state, &site,
                )));
            }
        }
        ClientMsg::CallEngineer => crate::console::call_engineer(state, page).await,
        ClientMsg::ClearAlert => crate::console::clear_alert(state, page),
        ClientMsg::TalkStart => {
            if !viewer.engineer {
                return;
            }
            let got = lock(&state.talk).acquire(
                session,
                &viewer.sub,
                crate::talk::new_talk_id(),
                Instant::now(),
            );
            match got {
                Ok(talk_id) => {
                    send(ServerMsg::TalkAcquired { talk_id });
                    state.broadcast(To::All, ServerMsg::EngineerTalking { active: true });
                }
                Err(holder) => send(ServerMsg::TalkBusy { holder }),
            }
        }
        ClientMsg::TalkStop => {
            if lock(&state.talk).release(session) {
                send(ServerMsg::TalkReleased);
                state.broadcast(To::All, ServerMsg::EngineerTalking { active: false });
            }
        }
        ClientMsg::ListenStart { .. } | ClientMsg::ListenStop => {}
        _ => {
            let Some(site) = site else {
                tracing::warn!(page = %page.id, "command before the engine's topology: dropped");
                return;
            };
            let cmds = view::command(&site, &state.engine.mirror(), page, viewer, &msg);
            match cmds {
                Ok(cmds) => {
                    if cmds.iter().any(|c| changes_mix(c, &page.mix)) {
                        state.auto_snapshot(page);
                    }
                    for cmd in cmds {
                        if let Err(e) = state.engine.request_applied(cmd, Some(session)).await {
                            tracing::warn!(page = %page.id, error = %e, "WS command failed");
                        }
                    }
                }
                Err(e) => tracing::warn!(page = %page.id, error = %e, "WS command refused"),
            }
        }
    }
}

#[cfg(test)]
mod tests;
