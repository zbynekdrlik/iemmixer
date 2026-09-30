//! The engineer's surfaces and the server's background jobs (S5 design note
//! §6): the F29 console (inputs, limiter counters, member-less pages, login
//! failures), SOS alerts (F20), the band-activity alarm and the "Back to
//! REAPER" switch (§4.2, §4.3), and the tasks that merge meters, clear
//! solos after the last connection left (X2) and end silent talk locks (X6).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use iem_core::{
    ActivityConfig, ApiError, ConsoleInfo, ConsoleInput, ConsoleMix, LoginFailures, PageLink,
    ServerMsg,
};
use iem_engine_proto::{Change, Cmd, Meters, MixId};
use tokio::sync::broadcast;

use crate::activity::{BandActivity, watched_inputs};
use crate::engine::client::EngineEvent;
use crate::meters::{METER_PERIOD_MS, MeterMerge, max_watched_peak};
use crate::site_view::{Page, SiteView};
use crate::{AppState, RunMode, To};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How often solos and talk locks are checked.
pub const JANITOR_PERIOD: Duration = Duration::from_millis(250);

/// The console's data (F29).
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

/// The banner state for engineer pages.
pub fn activity_msg(state: &AppState) -> ServerMsg {
    ServerMsg::BandActivity {
        active: state.activity.load(std::sync::atomic::Ordering::Acquire),
        can_switch: !state.site_config.back_to_reaper.is_empty(),
    }
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

/// The band-activity alarm fed with the engine's meter frames (§4.2): only
/// the stage inputs count ([`watched_inputs`]), resolved again whenever the
/// engine announces another topology.
pub struct ActivityWatch {
    activity: BandActivity,
    inputs: Vec<String>,
    /// The site view the watched inputs were resolved for, and those inputs.
    resolved: Option<(Arc<SiteView>, Vec<usize>)>,
}

impl ActivityWatch {
    pub fn new(cfg: &ActivityConfig, start: Instant) -> Self {
        Self {
            activity: BandActivity::new(cfg, start),
            inputs: cfg.inputs.clone(),
            resolved: None,
        }
    }

    /// One meter frame at `now`, with the site view of the engine's
    /// topology (none yet: the frame is ignored); `Some` when the alarm
    /// turned on (`true`) or off (`false`).
    pub fn observe(
        &mut self,
        site: Option<&Arc<SiteView>>,
        now: Instant,
        m: &Meters,
    ) -> Option<bool> {
        let site = site?;
        let seen = self
            .resolved
            .as_ref()
            .is_some_and(|(view, _)| Arc::ptr_eq(view, site));
        if !seen {
            let (watched, unknown) = watched_inputs(&self.inputs, site);
            for id in &unknown {
                tracing::error!(input = %id, "[activity] inputs: the engine has no such input; left out");
            }
            if watched.is_empty() {
                tracing::error!("band activity watches no input: its alarm cannot turn on");
            } else {
                tracing::info!(
                    inputs = watched.len(),
                    "band activity watches the stage inputs"
                );
            }
            self.resolved = Some((Arc::clone(site), watched));
        }
        let (_, watched) = self.resolved.as_ref()?;
        self.activity.observe(now, max_watched_peak(m, watched))
    }
}

/// Starts the meter merger, the activity alarm and the janitor.
pub fn spawn_tasks(state: AppState) {
    tokio::spawn(meter_task(state.clone()));
    tokio::spawn(janitor_task(state));
}

async fn meter_task(state: AppState) {
    let mut rx = state.engine.subscribe();
    let mut merge = MeterMerge::default();
    let mut activity = ActivityWatch::new(&state.site_config.activity, Instant::now());
    let mut tick = tokio::time::interval(Duration::from_millis(METER_PERIOD_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(EngineEvent::Meters(m)) => {
                    merge.push(&m);
                    if state.mode == RunMode::Dev
                        && let Some(on) =
                            activity.observe(state.site().as_ref(), Instant::now(), &m)
                    {
                        activity_changed(&state, on);
                    }
                }
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

/// The alarm turned on or off: engineer pages get the banner state, and when
/// it turned on the engineer's devices get the band-activity notice (design
/// note §5.4). Returns the push task.
fn activity_changed(state: &AppState, on: bool) -> Option<tokio::task::JoinHandle<()>> {
    state
        .activity
        .store(on, std::sync::atomic::Ordering::Release);
    let push = if on {
        tracing::warn!("band activity while developing: engineer banner and notice");
        let payload = crate::notify::alarm_payload(
            "Kapela hrá",
            "iemmixer beží vo vývoji a na vstupoch je signál. Späť na REAPER?",
        );
        let s = state.clone();
        Some(tokio::spawn(async move {
            crate::notify::push_engineers(&s, &payload).await;
            tracing::info!("band-activity notice pushed to the engineer's devices");
        }))
    } else {
        tracing::info!("band activity ended");
        None
    };
    state.broadcast(To::Engineers, activity_msg(state));
    push
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

    /// One meter frame of the test site: `loud` at −1 dBFS, every other input
    /// silent.
    fn frame_with(v: &SiteView, loud: &str) -> Meters {
        let peak = 10f32.powf(-1.0 / 20.0);
        Meters {
            inputs: v
                .inputs
                .iter()
                .map(|i| {
                    if i.id.0 == loud {
                        [peak, peak]
                    } else {
                        [0.0, 0.0]
                    }
                })
                .collect(),
            ..Meters::default()
        }
    }

    const NONE: Vec<(u64, bool)> = Vec::new();
    /// The 120th loud second (second 119) turns the alarm on, once.
    const ON_AT_119: [(u64, bool); 1] = [(119, true)];

    /// Three frames a second for `secs` seconds; every change of the alarm
    /// with the second it happened in.
    fn play(cfg: &ActivityConfig, loud: &str, secs: u64) -> Vec<(u64, bool)> {
        let site = Arc::new(test_view());
        let m = frame_with(&site, loud);
        let t = Instant::now();
        let mut watch = ActivityWatch::new(cfg, t);
        let mut changes = Vec::new();
        for s in 0..secs {
            for ms in [0, 333, 666] {
                let now = t + Duration::from_secs(s) + Duration::from_millis(ms);
                if let Some(on) = watch.observe(Some(&site), now, &m) {
                    changes.push((s, on));
                }
            }
        }
        changes
    }

    #[test]
    fn program_input_signal_does_not_raise_band_activity() {
        // S1a: the program input (`content`, category tech) carries signal
        // while the band is silent; five minutes of it are no band.
        assert_eq!(play(&ActivityConfig::default(), "content", 300), NONE);
    }

    #[test]
    fn stage_input_activity_still_raises_it() {
        assert_eq!(play(&ActivityConfig::default(), "mic1", 300), ON_AT_119);
        // An input without a category is a mic too.
        assert_eq!(play(&ActivityConfig::default(), "keys", 300), ON_AT_119);
    }

    #[test]
    fn an_explicit_input_list_replaces_the_mics_default() {
        let cfg = ActivityConfig {
            inputs: vec!["content".into()],
            ..ActivityConfig::default()
        };
        assert_eq!(play(&cfg, "content", 300), ON_AT_119);
        assert_eq!(play(&cfg, "mic1", 300), NONE);
    }

    #[test]
    fn a_new_topology_resolves_the_watched_inputs_again() {
        // Only mic2 counts. The engine then announces a topology with the
        // inputs in reverse order: mic2 sits at another index of the frame.
        let cfg = ActivityConfig {
            inputs: vec!["mic2".into()],
            ..ActivityConfig::default()
        };
        let first = Arc::new(test_view());
        let mut reversed = test_view();
        reversed.inputs.reverse();
        let second = Arc::new(reversed);
        let t = Instant::now();
        let mut watch = ActivityWatch::new(&cfg, t);
        assert_eq!(
            watch.observe(Some(&first), t, &frame_with(&first, "mic2")),
            None
        );
        let loud = frame_with(&second, "mic2");
        let mut changes = Vec::new();
        for s in 1..=120 {
            let now = t + Duration::from_secs(s);
            if let Some(on) = watch.observe(Some(&second), now, &loud) {
                changes.push((s, on));
            }
        }
        // Second 0 (the first topology) and seconds 1 to 119: 120 seconds.
        assert_eq!(changes, ON_AT_119);
    }

    #[tokio::test]
    async fn the_band_activity_notice_reaches_the_engineers_devices() {
        use crate::push::tests::{fake_push_service, subscription, vapid_private_key};
        let (base, seen) = fake_push_service().await;
        let dir = tempfile::tempdir().unwrap();
        let config = iem_core::Config {
            vapid_private_key: vapid_private_key(),
            ..iem_core::Config::default()
        };
        let s = AppState::new(config, dir.path());
        s.push_store
            .write()
            .await
            .add(subscription(format!("{base}/201")))
            .unwrap();
        let push = activity_changed(&s, true).expect("a notice when it turns on");
        tokio::time::timeout(Duration::from_secs(10), push)
            .await
            .expect("pushed within 10 s")
            .unwrap();
        let paths: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(path, _, _)| path.clone())
            .collect();
        assert_eq!(paths, ["/201"], "the engineer's device");
        assert!(s.activity.load(std::sync::atomic::Ordering::Acquire));
        assert!(
            activity_changed(&s, false).is_none(),
            "no notice when it ends"
        );
        assert!(!s.activity.load(std::sync::atomic::Ordering::Acquire));
    }

    #[tokio::test]
    async fn the_banner_reports_activity_and_the_switch() {
        let (_d, s) = state();
        assert_eq!(
            activity_msg(&s),
            ServerMsg::BandActivity {
                active: false,
                can_switch: true
            }
        );
        let mut rx = s.event_tx.subscribe();
        activity_changed(&s, false);
        assert_eq!(
            rx.try_recv().unwrap(),
            (
                To::Engineers,
                ServerMsg::BandActivity {
                    active: false,
                    can_switch: true
                }
            )
        );
        let dir = tempfile::tempdir().unwrap();
        let none = AppState::new(iem_core::Config::default(), dir.path());
        assert_eq!(
            activity_msg(&none),
            ServerMsg::BandActivity {
                active: false,
                can_switch: false
            }
        );
    }
}
