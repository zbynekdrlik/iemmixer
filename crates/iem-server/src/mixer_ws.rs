//! `/ws/{page}` — the mixer page's WebSocket (UI protocol v2; S5 design note
//! §5, §6). The first message is the hello; then the page's state from the
//! mirror, pins and hides, solo, alerts, network mode and tunnel status.
//! Commands become engine requests (tagged with this session as their
//! origin); engine deltas become UI updates for every other session on the
//! same page; meters arrive every 100 ms.

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
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: CLOSE_RELOAD,
            reason: "reload".into(),
        })))
        .await;
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
pub fn addressed(to: &To, page: &str, engineer: bool, session: u64) -> bool {
    match to {
        To::All => true,
        To::Page(p) => p == page,
        To::Engineers => engineer,
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
    if socket.send(text(&hello)).await.is_err() {
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
    if viewer.engineer {
        first.push(crate::console::activity_msg(&state));
    }
    for m in &first {
        if socket.send(text(m)).await.is_err() {
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

    loop {
        let out: Vec<ServerMsg> = tokio::select! {
            msg = socket.recv() => match msg {
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
                Ok((to, msg)) if addressed(&to, &page.id, viewer.engineer, session) => vec![msg],
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
            if socket.send(text(m)).await.is_err() {
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
mod tests {
    use super::*;
    use crate::site_view::tests::test_view;
    use iem_engine_proto::{GroupId, InputId};

    fn claims(sub: &str, engineer: bool) -> iem_core::AuthClaims {
        iem_core::AuthClaims {
            sub: sub.into(),
            engineer,
            exp: u64::MAX,
            iat: 0,
        }
    }

    fn viewer(sub: &str, engineer: bool) -> Viewer {
        Viewer {
            sub: sub.into(),
            engineer,
        }
    }

    /// What `handle` has answered this session so far.
    fn answers(rx: &mut mpsc::UnboundedReceiver<ServerMsg>) -> Vec<ServerMsg> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    /// The app events broadcast so far.
    fn events(rx: &mut broadcast::Receiver<(To, ServerMsg)>) -> Vec<(To, ServerMsg)> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    #[test]
    fn members_open_their_own_page_the_engineer_any() {
        let v = test_view();
        let m3 = v.page("member3").unwrap();
        let t = v.page("translator").unwrap();
        assert!(may_open(&claims("member3", false), &m3));
        assert!(!may_open(&claims("member2", false), &m3));
        assert!(may_open(&claims("engineer", true), &m3));
        assert!(may_open(&claims("engineer", true), &t));
        assert!(
            !may_open(&claims("translator", false), &t),
            "no member owns it"
        );
    }

    #[test]
    fn a_session_hears_back_only_the_mutes_its_solo_shows() {
        use crate::engine::mirror::Mirror;
        use iem_engine_proto::{EngineMsg, MixState, Solo, Transient};
        let v = test_view();
        let page = v.page("member2").unwrap();
        let mic2 = Source::Input(InputId::new("mic2"));
        let mut m = Mirror::default();
        let mut transient = Transient::default();
        transient.solo.push(Solo {
            mix: MixId::new("member2"),
            sources: vec![mic2.clone()],
        });
        m.apply(&EngineMsg::State {
            rev: 1,
            state: MixState::default(),
            transient,
        });
        let level = Change::Level {
            mix: MixId::new("member2"),
            source: mic2.clone(),
            level: Default::default(),
        };
        assert!(own_echo(&v, &m, &page, std::slice::from_ref(&level)).is_empty());
        let echo = own_echo(
            &v,
            &m,
            &page,
            &[
                level,
                Change::Solo {
                    mix: MixId::new("member2"),
                    sources: vec![mic2],
                },
            ],
        );
        assert_eq!(echo.len(), 24, "every channel, no SoloUpdate");
        let audible: Vec<&str> = echo
            .iter()
            .filter_map(|u| match u {
                ServerMsg::ChannelUpdate {
                    id, muted: false, ..
                } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(audible, ["mic2"]);
    }

    #[test]
    fn the_protocol_range_is_min_to_ours() {
        assert!(proto_ok(UI_PROTO));
        assert!(proto_ok(MIN_CLIENT_PROTO));
        assert!(!proto_ok(MIN_CLIENT_PROTO - 1));
        assert!(!proto_ok(UI_PROTO + 1));
    }

    #[test]
    fn tokens_are_required_valid_and_fresh() {
        let secret = "s";
        assert_eq!(
            claims_of(None, secret).unwrap_err().0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            claims_of(Some("garbage"), secret).unwrap_err().0,
            StatusCode::UNAUTHORIZED
        );
        let token = |exp: u64| {
            jsonwebtoken::encode(
                &jsonwebtoken::Header::default(),
                &iem_core::AuthClaims {
                    sub: "member1".into(),
                    engineer: false,
                    exp,
                    iat: 0,
                },
                &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
            )
            .unwrap()
        };
        let fresh = token(u64::MAX / 2);
        assert_eq!(
            claims_of(Some(fresh.as_str()), secret).unwrap().sub,
            "member1"
        );
        // Expired inside the JWT library's 60 s leeway: our own check.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let (code, body) = claims_of(Some(token(now - 10).as_str()), secret).unwrap_err();
        assert_eq!(
            (code, body.0.code.as_str()),
            (StatusCode::UNAUTHORIZED, "TOKEN_EXPIRED")
        );
        // Long expired: refused by the JWT validation itself.
        let (code, body) = claims_of(Some(token(1).as_str()), secret).unwrap_err();
        assert_eq!(
            (code, body.0.code.as_str()),
            (StatusCode::UNAUTHORIZED, "UNAUTHORIZED")
        );
    }

    #[test]
    fn a_token_is_valid_through_its_expiry_second() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let exp = now + 3600;
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &iem_core::AuthClaims {
                sub: "member1".into(),
                engineer: false,
                exp,
                iat: 0,
            },
            &jsonwebtoken::EncodingKey::from_secret(b"s"),
        )
        .unwrap();
        assert_eq!(
            claims_at(Some(token.as_str()), "s", exp - 1).unwrap().exp,
            exp
        );
        assert_eq!(claims_at(Some(token.as_str()), "s", exp).unwrap().exp, exp);
        let (code, body) = claims_at(Some(token.as_str()), "s", exp + 1).unwrap_err();
        assert_eq!(
            (code, body.0.code.as_str()),
            (StatusCode::UNAUTHORIZED, "TOKEN_EXPIRED")
        );
    }

    #[tokio::test]
    async fn sos_talk_and_limiter_commands_need_no_engine() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(crate::site_view::tests::test_config(), dir.path());
        let m3_page = state.page("member3").unwrap();
        let eng_page = state.page("engineer").unwrap();
        let (m3, eng) = (viewer("member3", false), viewer("engineer", true));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut app = state.event_tx.subscribe();

        // SOS: raised by the member, cleared by them.
        handle(&state, 1, &m3_page, &m3, ClientMsg::CallEngineer, &tx).await;
        assert!(lock(&state.alerts).contains_key("member3"));
        let alert = ServerMsg::EngineerAlert {
            from_member: "member3".into(),
            from_name: "Member3".into(),
        };
        assert!(events(&mut app).contains(&(To::Page("engineer".into()), alert)));
        handle(&state, 1, &m3_page, &m3, ClientMsg::ClearAlert, &tx).await;
        assert!(lock(&state.alerts).is_empty());
        let cleared = ServerMsg::AlertCleared {
            member_id: "member3".into(),
        };
        assert!(events(&mut app).contains(&(To::Page("member3".into()), cleared)));

        // Talkback: the engineer's only.
        handle(&state, 1, &m3_page, &m3, ClientMsg::TalkStart, &tx).await;
        assert!(answers(&mut rx).is_empty(), "a member cannot talk");
        assert!(lock(&state.talk).holder().is_none());
        handle(&state, 7, &eng_page, &eng, ClientMsg::TalkStart, &tx).await;
        match answers(&mut rx).as_slice() {
            [ServerMsg::TalkAcquired { talk_id }] => assert!(!talk_id.is_empty()),
            other => panic!("{other:?}"),
        }
        let talking = ServerMsg::EngineerTalking { active: true };
        assert!(events(&mut app).contains(&(To::All, talking)));
        handle(&state, 7, &eng_page, &eng, ClientMsg::TalkStop, &tx).await;
        assert_eq!(answers(&mut rx), vec![ServerMsg::TalkReleased]);

        // The limiter's answer needs only the mirror.
        handle(&state, 1, &m3_page, &m3, ClientMsg::GetLimiterParams, &tx).await;
        match answers(&mut rx).as_slice() {
            [
                ServerMsg::LimiterParams {
                    mix, track_name, ..
                },
            ] => assert_eq!(
                (mix.as_str(), track_name.as_str()),
                ("member3", view::OUT_NAME)
            ),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_closed_connection_releases_talk_and_starts_the_solo_grace() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(crate::site_view::tests::test_config(), dir.path());
        let page = state.page("engineer").unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut app = state.event_tx.subscribe();
        lock(&state.solo).connected("engineer");
        let eng = viewer("engineer", true);
        handle(&state, 7, &page, &eng, ClientMsg::TalkStart, &tx).await;
        assert_eq!(answers(&mut rx).len(), 1, "TalkAcquired");
        assert!(lock(&state.talk).holder().is_some());

        cleanup(&state, &page, 7);
        assert!(lock(&state.talk).holder().is_none(), "the lock is released");
        let silent = ServerMsg::EngineerTalking { active: false };
        assert!(events(&mut app).contains(&(To::All, silent)));
        let later = Instant::now() + crate::solo::SOLO_GRACE;
        assert_eq!(lock(&state.solo).due(later), ["engineer"]);
    }

    #[test]
    fn alerts_catch_up_on_the_engineer_page_and_the_members_own() {
        let mut active = std::collections::HashMap::new();
        assert_eq!(alert_catchup(&active, "engineer"), None);
        assert_eq!(alert_catchup(&active, "member1"), None);
        active.insert(
            "member2".to_string(),
            ("member2".to_string(), "Member2".to_string()),
        );
        active.insert(
            "member1".to_string(),
            ("member1".to_string(), "Member1".to_string()),
        );
        match alert_catchup(&active, "engineer") {
            Some(ServerMsg::ActiveAlerts { alerts }) => {
                let who: Vec<&str> = alerts.iter().map(|a| a.from_member.as_str()).collect();
                assert_eq!(who, ["member1", "member2"]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            alert_catchup(&active, "member1"),
            Some(ServerMsg::EngineerAlert {
                from_member: "member1".into(),
                from_name: String::new()
            })
        );
        assert_eq!(alert_catchup(&active, "member3"), None);
    }

    #[test]
    fn events_reach_the_addressed_connections() {
        assert!(addressed(&To::All, "member1", false, 1));
        assert!(addressed(&To::Page("member1".into()), "member1", false, 1));
        assert!(!addressed(&To::Page("member1".into()), "member2", true, 1));
        assert!(addressed(&To::Engineers, "member2", true, 1));
        assert!(!addressed(&To::Engineers, "member2", false, 1));
        assert!(addressed(&To::Session(7), "member2", false, 7));
        assert!(!addressed(&To::Session(7), "member2", true, 8));
    }

    #[test]
    fn page_mix_changes_are_told_apart() {
        let m1 = MixId::new("member1");
        let level = |mix: &str| Cmd::SetLevel {
            mix: MixId::new(mix),
            source: Source::Input(InputId::new("mic1")),
            gain_db: Some(0.0),
            pan: None,
            muted: None,
        };
        assert!(changes_mix(&level("member1"), &m1));
        assert!(!changes_mix(&level("member2"), &m1));
        assert!(changes_mix(
            &Cmd::SetMix {
                mix: m1.clone(),
                volume_db: None,
                muted: Some(true)
            },
            &m1
        ));
        assert!(changes_mix(
            &Cmd::SetGroup {
                mix: m1.clone(),
                group: GroupId::new("stems"),
                gain_db: None,
                muted: None
            },
            &m1
        ));
        assert!(changes_mix(
            &Cmd::SetLimiter {
                mix: m1.clone(),
                enabled: None,
                limit_db: Some(-3.0)
            },
            &m1
        ));
        let eq = iem_engine_proto::Eq::default();
        assert!(changes_mix(
            &Cmd::SetEq {
                target: EqTarget::Group {
                    mix: m1.clone(),
                    group: GroupId::new("stems")
                },
                eq
            },
            &m1
        ));
        assert!(changes_mix(
            &Cmd::SetEq {
                target: EqTarget::Mix(m1.clone()),
                eq
            },
            &m1
        ));
        assert!(!changes_mix(
            &Cmd::SetEq {
                target: EqTarget::Input(InputId::new("mic1")),
                eq
            },
            &m1
        ));
        assert!(!changes_mix(
            &Cmd::SetSolo {
                mix: m1.clone(),
                sources: vec![]
            },
            &m1
        ));
        assert!(changes_mix(
            &Cmd::Batch {
                ops: vec![Cmd::Ping, level("member1")]
            },
            &m1
        ));
        assert!(!changes_mix(
            &Cmd::Batch {
                ops: vec![Cmd::Ping]
            },
            &m1
        ));
    }

    /// Commands, engine events and meters against the real engine (NullRt).
    #[cfg(unix)]
    mod live {
        use super::*;
        use crate::engine::testkit::EngineHarness;
        use std::sync::Arc;

        #[tokio::test]
        async fn pins_hides_eq_and_the_console_are_answered_from_the_engine() {
            let h = EngineHarness::start();
            let (_d, s) = h.state().await;
            let page = s.page("member2").unwrap();
            let m2 = viewer("member2", false);
            let (tx, mut rx) = mpsc::unbounded_channel();
            let mut app = s.event_tx.subscribe();

            // Pins and hides: unknown ids are left out, the rest saved and
            // told to every connection of the page.
            let pins = ClientMsg::UpdateCustomization {
                pinned: vec!["mic1".into(), "nothing".into()],
                hidden: vec!["keys".into()],
            };
            handle(&s, 1, &page, &m2, pins, &tx).await;
            let c = s.band.customization("member2").unwrap();
            assert_eq!(c.pinned, vec![Source::Input(InputId::new("mic1"))]);
            assert_eq!(c.hidden, vec![Source::Input(InputId::new("keys"))]);
            let told = ServerMsg::CustomizationUpdate {
                pinned: vec!["mic1".into()],
                hidden: vec!["keys".into()],
            };
            assert!(events(&mut app).contains(&(To::Page("member2".into()), told)));

            let eq = ClientMsg::GetEqParams {
                target: "member2".into(),
            };
            handle(&s, 1, &page, &m2, eq, &tx).await;
            match answers(&mut rx).as_slice() {
                [
                    ServerMsg::EqParams {
                        target,
                        track_name,
                        bands,
                    },
                ] => {
                    assert_eq!(
                        (target.as_str(), track_name.as_str()),
                        ("member2", view::OUT_NAME)
                    );
                    assert!(!bands.is_empty());
                }
                other => panic!("{other:?}"),
            }

            // The console is the engineer's.
            handle(&s, 1, &page, &m2, ClientMsg::GetConsole, &tx).await;
            assert!(answers(&mut rx).is_empty(), "not a member's");
            let eng_page = s.page("engineer").unwrap();
            let eng = viewer("engineer", true);
            handle(&s, 2, &eng_page, &eng, ClientMsg::GetConsole, &tx).await;
            match answers(&mut rx).as_slice() {
                [ServerMsg::Console(c)] => assert_eq!(c.inputs.len(), 24),
                other => panic!("{other:?}"),
            }
        }

        #[tokio::test]
        async fn engine_events_and_meters_become_page_messages() {
            let h = EngineHarness::start();
            let (_d, s) = h.state().await;
            let page = s.page("member2").unwrap();
            let m2 = viewer("member2", false);
            let out = engine_event(&s, 5, &page, &m2, EngineEvent::Connected);
            assert!(
                matches!(
                    out.as_slice(),
                    [
                        ServerMsg::ConnectionChanged { connected: true },
                        ServerMsg::State {
                            connected: true,
                            ..
                        }
                    ]
                ),
                "{out:?}"
            );
            assert_eq!(
                engine_event(&s, 5, &page, &m2, EngineEvent::Disconnected),
                vec![ServerMsg::ConnectionChanged { connected: false }]
            );

            // Session 5 changed a level: it hears nothing back, the others
            // (and changes without a session) get the update.
            let (mix, keys) = (MixId::new("member2"), Source::Input(InputId::new("keys")));
            let set = Cmd::SetLevel {
                mix: mix.clone(),
                source: keys.clone(),
                gain_db: Some(-6.0),
                pan: None,
                muted: None,
            };
            s.engine.request_applied(set, Some(5)).await.unwrap();
            let level = s.engine.mirror().level(&mix, &keys);
            let changes = Arc::new(vec![Change::Level {
                mix,
                source: keys,
                level,
            }]);
            let changed = |origin| EngineEvent::Changed {
                origin,
                changes: Arc::clone(&changes),
            };
            assert!(
                engine_event(&s, 5, &page, &m2, changed(Some(5))).is_empty(),
                "its own change is not echoed"
            );
            let update = ServerMsg::ChannelUpdate {
                id: "keys".into(),
                level_db: -6.0,
                muted: false,
                pan: 0.5,
            };
            assert_eq!(
                engine_event(&s, 5, &page, &m2, changed(Some(6))),
                vec![update.clone()]
            );
            assert_eq!(engine_event(&s, 5, &page, &m2, changed(None)), vec![update]);

            // Meters: inputs and mixes by id, and the page's stems strip.
            let topo = s.engine.mirror().topology.clone().unwrap();
            let merged = crate::meters::Merged {
                inputs: vec![[0.5, 0.25]; topo.inputs.len()],
                mixes: vec![[0.125, 0.125]; topo.mixes.len()],
                groups: vec![[0.0, 0.0]; topo.mixes.len() * topo.groups.len()],
                active_s: Vec::new(),
            };
            match meters_msg(&s, &page, &merged) {
                Some(ServerMsg::Meters { meters }) => {
                    assert_eq!(meters.len(), 24 + 11 + 1);
                    assert_eq!(meters["keys"], [0.5, 0.25]);
                    assert_eq!(meters["member2"], [0.125, 0.125]);
                    assert_eq!(meters["stems"], [0.0, 0.0]);
                }
                other => panic!("{other:?}"),
            }
        }
    }
}
