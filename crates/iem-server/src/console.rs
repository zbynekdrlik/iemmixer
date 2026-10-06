//! The engineer's surfaces and the server's background jobs (S5 design note
//! §6): the F29 console (inputs, limiter counters, member-less pages, login
//! failures), SOS alerts (F20), the "Back to REAPER" switch (§4.3), and the
//! tasks that merge meters, clear solos after the last connection left (X2)
//! and end silent talk locks (X6).
//!
//! No input level stands for "the band plays" (#38, owner decision
//! 2026-10-06): other devices on the Dante network feed the card's inputs,
//! and whether an event runs is the owner's to say. The meters only feed
//! the pages' meters and the limiter counters; the band-activity banner and
//! notice that read them were removed.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use iem_core::{
    ApiError, ConsoleInfo, ConsoleInput, ConsoleMix, LoginFailures, PageLink, ServerMsg,
};
use iem_engine_proto::{Change, Cmd, MixId};
use tokio::sync::broadcast;

use crate::engine::client::EngineEvent;
use crate::meters::{METER_PERIOD_MS, MeterMerge};
use crate::site_view::{Page, SiteView};
use crate::{AppState, To};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How often solos and talk locks are checked.
pub const JANITOR_PERIOD: Duration = Duration::from_millis(250);

/// The console's data (F29), and whether the site has the "Back to REAPER"
/// switch (§4.3), whose button the console shows.
pub fn console_info(state: &AppState, site: &SiteView) -> ConsoleInfo {
    let mirror = state.engine.mirror();
    let inputs = site
        .inputs
        .iter()
        .map(|i| {
            let s = mirror.input(&i.id);
            ConsoleInput {
                id: i.id.0.clone(),
                name: i.name.clone(),
                trim_db: s.trim_db.max(-150.0) as f32,
                muted: s.muted,
                processing: s.processing,
            }
        })
        .collect();
    drop(mirror);
    let limiters = site
        .mixes
        .iter()
        .map(|m| ConsoleMix {
            id: m.id.0.clone(),
            name: m.name.clone(),
            active_seconds: state.active_seconds(&m.id),
        })
        .collect();
    let pages = site
        .mix_pages()
        .into_iter()
        .map(|p| PageLink {
            id: p.id,
            name: p.name,
        })
        .collect();
    let stats = state.login_guard.stats();
    ConsoleInfo {
        inputs,
        limiters,
        pages,
        login: LoginFailures {
            lan: stats.lan_failures,
            tunnel: stats.tunnel_failures,
            engineer_budget_trips: stats.engineer_budget_trips,
        },
        can_switch: !state.site_config.back_to_reaper.is_empty(),
    }
}

/// Console updates for input changes (engineer pages).
pub fn input_updates(site: &SiteView, changes: &[Change]) -> Vec<ServerMsg> {
    changes
        .iter()
        .filter_map(|c| match c {
            Change::Input { id, state } => site.input(&id.0).map(|i| {
                ServerMsg::InputUpdate(ConsoleInput {
                    id: id.0.clone(),
                    name: i.name.clone(),
                    trim_db: state.trim_db.max(-150.0) as f32,
                    muted: state.muted,
                    processing: state.processing,
                })
            }),
            _ => None,
        })
        .collect()
}

/// A member asks the engineer for help (F20): the engineer's pages show it,
/// the member's page shows it is pending, engineer devices get a push.
pub async fn call_engineer(state: &AppState, page: &Page) {
    let Some(member) = page.member.clone() else {
        return;
    };
    if member == crate::pin_store::ENGINEER_ID {
        return;
    }
    {
        let mut alerts = lock(&state.alerts);
        if alerts.contains_key(&member) {
            return;
        }
        alerts.insert(member.clone(), (member.clone(), page.name.clone()));
    }
    state.broadcast(
        To::Page(crate::pin_store::ENGINEER_ID.into()),
        ServerMsg::EngineerAlert {
            from_member: member.clone(),
            from_name: page.name.clone(),
        },
    );
    state.broadcast(
        To::Page(member.clone()),
        ServerMsg::EngineerAlert {
            from_member: member.clone(),
            from_name: String::new(),
        },
    );
    let payload = serde_json::json!({ "type": "SOS", "name": page.name, "member": member });
    let state = state.clone();
    tokio::spawn(async move {
        crate::notify::push_engineers(&state, payload.to_string().as_bytes()).await;
    });
}

/// Clears alerts: every alert from the engineer's page, the own from a member's.
pub fn clear_alert(state: &AppState, page: &Page) {
    let cleared: Vec<String> = {
        let mut alerts = lock(&state.alerts);
        if page.id == crate::pin_store::ENGINEER_ID {
            alerts.drain().map(|(k, _)| k).collect()
        } else if alerts.remove(&page.id).is_some() {
            vec![page.id.clone()]
        } else {
            Vec::new()
        }
    };
    for m in cleared {
        let msg = ServerMsg::AlertCleared {
            member_id: m.clone(),
        };
        state.broadcast(To::Page(crate::pin_store::ENGINEER_ID.into()), msg.clone());
        state.broadcast(To::Page(m), msg);
    }
}

/// Starts the meter merger and the janitor.
pub fn spawn_tasks(state: AppState) {
    tokio::spawn(meter_task(state.clone()));
    tokio::spawn(janitor_task(state));
}

/// Merges the engine's meter frames for the pages and the limiter counters
/// every [`METER_PERIOD_MS`]. No level here raises anything (#38).
async fn meter_task(state: AppState) {
    let mut rx = state.engine.subscribe();
    let mut merge = MeterMerge::default();
    let mut tick = tokio::time::interval(Duration::from_millis(METER_PERIOD_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(EngineEvent::Meters(m)) => merge.push(&m),
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            },
            _ = tick.tick() => {
                if let Some(m) = merge.take() {
                    lock(&state.active_s).clone_from(&m.active_s);
                    let _ = state.meters_tx.send(Arc::new(m));
                }
            }
        }
    }
}

async fn janitor_task(state: AppState) {
    let mut tick = tokio::time::interval(JANITOR_PERIOD);
    loop {
        tick.tick().await;
        let now = Instant::now();
        let due = lock(&state.solo).due(now);
        for mix in due {
            let has_solo = !state
                .engine
                .mirror()
                .solo(&MixId::new(mix.clone()))
                .is_empty();
            if !has_solo {
                continue;
            }
            tracing::info!(%mix, "clearing the solo 10 s after the last connection left");
            let cmd = Cmd::SetSolo {
                mix: MixId::new(mix.clone()),
                sources: Vec::new(),
            };
            if let Err(e) = state.engine.request(cmd, None).await {
                tracing::warn!(%mix, error = %e, "solo clean-up failed");
            }
        }
        let expired = lock(&state.talk).expire(now);
        if let Some(session) = expired {
            tracing::info!(session, "talk lock released after 2 s of silence");
            state.broadcast(To::Session(session), ServerMsg::TalkReleased);
            state.broadcast(To::All, ServerMsg::EngineerTalking { active: false });
        }
    }
}

#[derive(serde::Deserialize)]
pub struct SwitchRequest {
    pub pin: String,
}

/// `POST /api/mode/event` — the engineer's "Back to REAPER" (§4.3): an
/// engineer token and the engineer PIN (checked like a login), then the
/// configured command is started (no shell, not awaited): 202.
pub async fn back_to_reaper(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SwitchRequest>,
) -> Result<impl IntoResponse, crate::auth::Rejection> {
    let claims = {
        let config = state.config.read().await;
        crate::auth::verify_member_access(
            &headers,
            crate::pin_store::ENGINEER_ID,
            &config.jwt_secret,
        )
        .map_err(IntoResponse::into_response)?
    };
    if !claims.engineer {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiError::new("FORBIDDEN", "The switch is the engineer's")),
        )
            .into_response()
            .into());
    }
    let argv = state.site_config.back_to_reaper.clone();
    let Some((program, args)) = argv.split_first() else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ApiError::new("NOT_CONFIGURED", "No switch on this site")),
        )
            .into_response()
            .into());
    };
    crate::auth::verify_engineer_pin(&state, peer, &headers, &req.pin).await?;
    let child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .spawn();
    match child {
        Ok(mut child) => {
            tracing::warn!(program = %program, by = %claims.sub, "Back to REAPER: switch started");
            tokio::task::spawn_blocking(move || match child.wait() {
                Ok(status) => tracing::info!(%status, "Back to REAPER: switch finished"),
                Err(e) => tracing::error!(error = %e, "Back to REAPER: waiting failed"),
            });
            Ok((
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "ok": true })),
            ))
        }
        Err(e) => {
            tracing::error!(program = %program, error = %e, "Back to REAPER: cannot start the switch");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new(
                    "SWITCH_FAILED",
                    "The switch could not be started",
                )),
            )
                .into_response()
                .into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site_view::tests::{test_config, test_view};
    use iem_engine_proto::{InputId, InputState};

    fn state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let s = AppState::new(test_config(), dir.path());
        (dir, s)
    }

    #[tokio::test]
    async fn the_console_lists_inputs_limiters_pages_and_login_failures() {
        let (_d, s) = state();
        let v = test_view();
        let c = console_info(&s, &v);
        assert_eq!(c.inputs.len(), 24);
        assert_eq!(c.inputs[0].name, "MEMBER1 mic");
        assert!(
            c.inputs
                .iter()
                .all(|i| i.processing && !i.muted && i.trim_db == 0.0)
        );
        assert_eq!(c.limiters.len(), 11);
        assert!(
            c.limiters
                .iter()
                .any(|l| l.id == "translator" && l.name == "Translator")
        );
        assert_eq!(
            c.pages,
            vec![PageLink {
                id: "translator".into(),
                name: "Translator".into()
            }]
        );
        assert_eq!(c.login, LoginFailures::default());
    }

    #[test]
    fn input_changes_become_console_updates() {
        let v = test_view();
        let changes = vec![
            Change::Input {
                id: InputId::new("keys"),
                state: InputState {
                    trim_db: -3.0,
                    muted: true,
                    processing: false,
                    ..InputState::default()
                },
            },
            Change::Input {
                id: InputId::new("ghost"),
                state: InputState::default(),
            },
            Change::LimiterStatsReset {
                mix: MixId::new("member1"),
            },
        ];
        assert_eq!(
            input_updates(&v, &changes),
            vec![ServerMsg::InputUpdate(ConsoleInput {
                id: "keys".into(),
                name: "KEYS".into(),
                trim_db: -3.0,
                muted: true,
                processing: false
            })]
        );
    }

    /// The console says whether this site has the "Back to REAPER" switch
    /// (§4.3): the engineer's settings show its button only then. #38 moved
    /// the button out of the band-activity banner into the console.
    #[tokio::test]
    async fn the_console_tells_whether_the_switch_is_configured() {
        let (_d, s) = state();
        let v = test_view();
        let json = serde_json::to_value(console_info(&s, &v)).unwrap();
        assert_eq!(json["can_switch"], true, "the test site's switch");
        let dir = tempfile::tempdir().unwrap();
        let none = AppState::new(iem_core::Config::default(), dir.path());
        let json = serde_json::to_value(console_info(&none, &v)).unwrap();
        assert_eq!(json["can_switch"], false, "no back_to_reaper: no button");
    }

    #[tokio::test]
    async fn alerts_are_raised_once_and_cleared_by_their_member_or_the_engineer() {
        let (_d, s) = state();
        let v = test_view();
        let mut rx = s.event_tx.subscribe();
        let m1 = v.page("member1").unwrap();
        call_engineer(&s, &m1).await;
        call_engineer(&s, &m1).await;
        let (to, msg) = rx.try_recv().unwrap();
        assert_eq!(to, To::Page("engineer".into()));
        assert_eq!(
            msg,
            ServerMsg::EngineerAlert {
                from_member: "member1".into(),
                from_name: "Member1".into()
            }
        );
        let (to, _) = rx.try_recv().unwrap();
        assert_eq!(to, To::Page("member1".into()));
        assert!(rx.try_recv().is_err(), "a second call is a no-op");
        // The engineer's own page and the translator page raise nothing.
        call_engineer(&s, &v.page("engineer").unwrap()).await;
        call_engineer(&s, &v.page("translator").unwrap()).await;
        assert!(rx.try_recv().is_err());
        call_engineer(&s, &v.page("member2").unwrap()).await;
        while rx.try_recv().is_ok() {}
        clear_alert(&s, &v.page("member3").unwrap());
        assert!(rx.try_recv().is_err(), "nothing of member3's to clear");
        clear_alert(&s, &m1);
        assert_eq!(rx.try_recv().unwrap().0, To::Page("engineer".into()));
        assert_eq!(rx.try_recv().unwrap().0, To::Page("member1".into()));
        assert_eq!(lock(&s.alerts).len(), 1);
        clear_alert(&s, &v.page("engineer").unwrap());
        assert!(lock(&s.alerts).is_empty());
        let (to, msg) = rx.try_recv().unwrap();
        assert_eq!(to, To::Page("engineer".into()));
        assert_eq!(
            msg,
            ServerMsg::AlertCleared {
                member_id: "member2".into()
            }
        );
    }
}
