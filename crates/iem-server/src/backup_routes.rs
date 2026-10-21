//! Backups (F19) and restore with preview (F31), engineer only.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use iem_core::{
    ApiError,
    backup::{BackupInfo, MixerBackup, RestorePreview, RestoreResult},
};
use iem_engine_proto::{Cmd, Source};

use crate::AppState;
use crate::lineup_store::LineupStore;

type Reject = (StatusCode, Json<ApiError>);

pub fn backup_routes() -> Router<AppState> {
    Router::new()
        .route("/api/backups", get(list_backups))
        .route("/api/backups/{filename}", get(get_backup))
        .route("/api/backups/{filename}/preview", post(preview_backup))
        .route("/api/backups/{filename}/restore", post(restore_backup))
        .route("/api/backups/capture", post(trigger_capture))
}

async fn engineer(state: &AppState, headers: &axum::http::HeaderMap) -> Result<(), Reject> {
    crate::routes::require_engineer(state, headers)
        .await
        .map(|_| ())
        .map_err(|(code, body)| {
            if code == StatusCode::UNAUTHORIZED {
                (
                    StatusCode::FORBIDDEN,
                    Json(ApiError::new("FORBIDDEN", "engineer access required")),
                )
            } else {
                (code, body)
            }
        })
}

fn load(state: &AppState, filename: &str) -> Result<MixerBackup, Reject> {
    state.backup_store.load(filename).map_err(|e| {
        if e.starts_with("invalid filename") {
            (
                StatusCode::BAD_REQUEST,
                Json(ApiError::new("INVALID_FILENAME", &e)),
            )
        } else {
            (StatusCode::NOT_FOUND, Json(ApiError::new("NOT_FOUND", &e)))
        }
    })
}

fn unavailable() -> Reject {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiError::new(
            "ENGINE_UNAVAILABLE",
            "The engine is not connected",
        )),
    )
}

/// A backup of the running state, saved; `None` when the engine never spoke.
/// Never beside a switch, a roster reload or a restore (#58): it waits for
/// the one that runs (`AppState::switching`), so it backs up the band that
/// plays after it.
pub async fn capture_now(state: &AppState) -> Result<(String, MixerBackup), String> {
    let _running = state.switching.lock().await;
    let site = state
        .site()
        .ok_or("the engine has not announced its topology")?;
    if !state.engine.mirror().synced {
        return Err("the engine state is not synced".into());
    }
    // After a switch a restart cut short, until it is finished, the engine
    // may hold the next band's state while the roster still names the
    // last one.
    if state.frozen_now() {
        return Err("the band is being switched".into());
    }
    let mut b = crate::backup::capture(
        &site,
        &state.engine.mirror(),
        &state.band(),
        chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string(),
    )?;
    // Roster mode (#58 design §6): whose live state it is, and every other
    // lineup's stored state.
    if let Some(roster) = state.roster() {
        b.lineup = Some(roster.lineup().id.clone());
        b.lineups = crate::backup::inactive_lineups(&LineupStore::new(&state.config_dir), &roster)?;
    }
    let name = state.backup_store.save(&b).map_err(|e| e.to_string())?;
    Ok((name, b))
}

async fn list_backups(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<BackupInfo>>, Reject> {
    engineer(&state, &headers).await?;
    Ok(Json(state.backup_store.list()))
}

async fn get_backup(
    State(state): State<AppState>,
    Path(filename): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<MixerBackup>, Reject> {
    engineer(&state, &headers).await?;
    load(&state, &filename).map(Json)
}

fn preview_of(state: &AppState, b: &MixerBackup) -> Result<RestorePreview, Reject> {
    let roster = state.roster();
    crate::backup::live_lineup_fits(b, roster.as_ref()).map_err(|e| {
        (
            StatusCode::CONFLICT,
            Json(ApiError::new("LINEUP_MISMATCH", e)),
        )
    })?;
    let site = state.site().ok_or_else(unavailable)?;
    let current = state.engine.mirror().state.clone();
    let band = state.band();
    let mut p = crate::backup::preview(
        &site,
        &current,
        &move |m: &str| band.customization(m).ok(),
        b,
    );
    // Roster mode: the inactive lineups whose stored state it replaces.
    p.lineups = roster
        .map(|r| crate::backup::lineups_to_restore(b, &r))
        .unwrap_or_default();
    Ok(p)
}

async fn preview_backup(
    State(state): State<AppState>,
    Path(filename): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<RestorePreview>, Reject> {
    engineer(&state, &headers).await?;
    let b = load(&state, &filename)?;
    preview_of(&state, &b).map(Json)
}

async fn restore_backup(
    State(state): State<AppState>,
    Path(filename): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<RestoreResult>, Reject> {
    engineer(&state, &headers).await?;
    // On a task of its own: a client that goes away drops this handler,
    // never a restore half-way (#58 review).
    tokio::spawn(restore(state, filename)).await.map_err(|e| {
        tracing::error!(error = %e, "the restore's task failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("RESTORE_FAILED", "the restore's task failed")),
        )
    })?
}

/// The restore of [`restore_backup`], on a task of its own.
async fn restore(state: AppState, filename: String) -> Result<Json<RestoreResult>, Reject> {
    // Never beside a band switch (#58): one of them at a time, and none
    // while a switch a restart cut short is unfinished.
    let busy = || crate::routes::switching(crate::view::ViewError::Switching);
    let Ok(_running) = state.switching.try_lock() else {
        return Err(busy());
    };
    if state.frozen_now() || state.settling() {
        return Err(busy());
    }
    let b = load(&state, &filename)?;
    let preview = preview_of(&state, &b)?;
    // What the backup lacks (added to the topology after it) stays as it is.
    let mut import = crate::backup::keep_running(&b.state, &state.engine.mirror().state);
    // The active lineup's rules hold, as after a switch (#58 review); what
    // the restored state follows is not known (a backup's).
    let store = LineupStore::new(&state.config_dir);
    let running = state.roster().map(|r| {
        let id = r.lineup().id.clone();
        let (_, mut silenced) = crate::roster_watch::running_for(&store, &id);
        crate::roster_watch::fit_recorded(&mut import, &r.roster, r.lineup(), &mut silenced);
        crate::lineup_store::Running {
            lineup: id,
            follows: None,
            silenced,
        }
    });
    state
        .engine
        .request_applied(
            Cmd::ImportState {
                state: import,
                baseline: false,
            },
            None,
        )
        .await
        .map_err(|e| {
            tracing::error!(%filename, error = %e, "restore failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(ApiError::new("RESTORE_FAILED", e.to_string())),
            )
        })?;
    if let Some(running) = &running {
        crate::roster_watch::note_running(&store, running);
    }
    let site = state.site().ok_or_else(unavailable)?;
    let known = |s: &Source| match s {
        Source::Input(i) => site.input(&i.0).is_some(),
        Source::Mix(m) => site.mix(m).is_some(),
    };
    let keep =
        |list: &[Source]| -> Vec<Source> { list.iter().filter(|&s| known(s)).cloned().collect() };
    for (member, c) in &b.customizations {
        if site.member(member).is_none() {
            continue;
        }
        if let Err(e) = state
            .band()
            .save_customization(member, keep(&c.pinned), keep(&c.hidden))
        {
            tracing::error!(%member, error = %e, "restoring pins and hides failed");
        }
    }
    // Roster mode: every other lineup's stored state and its people's pins
    // and hides, the live state being in already; a failure is the caller's
    // to see.
    let lineups = match state.roster() {
        Some(roster) => crate::backup::restore_lineups(
            &LineupStore::new(&state.config_dir),
            &roster,
            &b,
            &|id: &str| state.band_of(Some(id)),
            &known,
        )
        .map_err(|e| {
            tracing::error!(%filename, error = %e, "the live state was restored, a lineup's was not");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new(
                    "RESTORE_INCOMPLETE",
                    format!("the live state was restored, but {e}"),
                )),
            )
        })?,
        None => Vec::new(),
    };
    tracing::warn!(%filename, changes = preview.changes.len(), ?lineups, "backup restored");
    Ok(Json(RestoreResult {
        restored_count: preview.changes.len(),
        skipped: preview.skipped,
    }))
}

async fn trigger_capture(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<BackupInfo>, Reject> {
    engineer(&state, &headers).await?;
    let (filename, b) = capture_now(&state).await.map_err(|e| {
        tracing::error!(error = %e, "backup capture failed");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError::new("CAPTURE_FAILED", e)),
        )
    })?;
    let size_bytes = state
        .backup_store
        .list()
        .into_iter()
        .find(|i| i.filename == filename)
        .map_or(0, |i| i.size_bytes);
    Ok(Json(BackupInfo {
        filename,
        timestamp: b.timestamp.clone(),
        size_bytes,
        send_count: b.level_count(),
        track_count: b.state.mixes.len(),
    }))
}

#[cfg(test)]
mod tests {
    use crate::engine::client::fake;
    use crate::routes::api_tests::{app, call, router, token};
    use axum::http::{Method, StatusCode};
    use iem_engine_proto::{InputId, MixState};

    #[tokio::test]
    async fn backups_are_the_engineers_and_need_the_engine() {
        let dir = tempfile::tempdir().unwrap();
        let (state, app) = app(dir.path());
        let eng = token("engineer", true);
        // No token or a member's: forbidden (never a login prompt).
        let (status, json) = call(&app, Method::GET, "/api/backups", None, None).await;
        assert_eq!(
            (status, json["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("FORBIDDEN"))
        );
        let member = token("member1", false);
        let (status, _) = call(&app, Method::GET, "/api/backups", Some(&member), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, json) = call(&app, Method::GET, "/api/backups", Some(&eng), None).await;
        assert_eq!((status, json), (StatusCode::OK, serde_json::json!([])));
        // A missing file, and a name that leaves the folder.
        let missing = "/api/backups/20260101_000000.json";
        let (status, json) = call(&app, Method::GET, missing, Some(&eng), None).await;
        assert_eq!(
            (status, json["code"].as_str()),
            (StatusCode::NOT_FOUND, Some("NOT_FOUND"))
        );
        let (status, json) =
            call(&app, Method::GET, "/api/backups/a..json", Some(&eng), None).await;
        assert_eq!(
            (status, json["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("INVALID_FILENAME"))
        );
        // Without the engine there is nothing to capture or compare with.
        let capture = "/api/backups/capture";
        let (status, json) = call(&app, Method::POST, capture, Some(&eng), None).await;
        assert_eq!(
            (status, json["code"].as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, Some("CAPTURE_FAILED"))
        );
        let b = iem_core::backup::MixerBackup::new(
            "2026-09-27T13:00:00Z".into(),
            1,
            Default::default(),
        );
        let name = state.backup_store.save(&b).unwrap();
        let preview = format!("/api/backups/{name}/preview");
        let (status, json) = call(&app, Method::POST, &preview, Some(&eng), None).await;
        assert_eq!(
            (status, json["code"].as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, Some("ENGINE_UNAVAILABLE"))
        );
    }

    #[tokio::test]
    async fn capture_is_refused_until_the_mirror_is_synced() {
        let dir = tempfile::tempdir().unwrap();
        let (mut state, _) = app(dir.path());
        // The engine said hello and announced its topology, but its state has
        // not arrived: a backup now would be empty or stale.
        let (engine, mut peer) = fake::announced(crate::site_view::tests::test_topology()).await;
        state.engine = engine;
        let app = router(state.clone());
        let eng = token("engineer", true);
        let capture = "/api/backups/capture";
        let (status, json) = call(&app, Method::POST, capture, Some(&eng), None).await;
        assert_eq!(
            (status, json["code"].as_str(), json["message"].as_str()),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Some("CAPTURE_FAILED"),
                Some("the engine state is not synced")
            )
        );
        assert!(state.backup_store.list().is_empty(), "nothing was written");
        assert!(!dir.path().join("backups").exists());

        // Once the state is there, the same request captures it.
        let mut running = MixState::default();
        running
            .inputs
            .entry(InputId::new("keys"))
            .or_default()
            .trim_db = 4.0;
        fake::sync(&state.engine, &mut peer, 7, running.clone()).await;
        let (status, info) = call(&app, Method::POST, capture, Some(&eng), None).await;
        assert_eq!(status, StatusCode::OK, "{info}");
        let saved = state
            .backup_store
            .load(info["filename"].as_str().unwrap())
            .unwrap();
        assert_eq!((saved.rev, saved.state), (7, running));
    }

    /// #58: in roster mode a backup names the lineup whose live state it
    /// holds and carries every other lineup's stored state; a backup of
    /// another lineup's live state is neither previewed nor restored.
    #[tokio::test]
    async fn roster_mode_backups_name_their_lineup_and_carry_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let (mut state, _) = app(dir.path());
        *state.roster.write().unwrap() = Some(crate::roster_state::tests::test_roster("main"));
        let (engine, mut peer) = fake::announced(crate::site_view::tests::test_topology()).await;
        state.engine = engine;
        fake::sync(&state.engine, &mut peer, 5, MixState::default()).await;
        let app = router(state.clone());
        let eng = token("engineer", true);
        let mut youth = MixState::default();
        youth
            .inputs
            .entry(InputId::new("keys"))
            .or_default()
            .trim_db = 3.0;
        let youth = iem_core::backup::MixerBackup::new("t".into(), 1, youth);
        crate::lineup_store::LineupStore::new(dir.path())
            .save_state("youth", &youth)
            .unwrap();
        let (status, info) =
            call(&app, Method::POST, "/api/backups/capture", Some(&eng), None).await;
        assert_eq!(status, StatusCode::OK, "{info}");
        let saved = state
            .backup_store
            .load(info["filename"].as_str().unwrap())
            .unwrap();
        assert_eq!(saved.lineup.as_deref(), Some("main"));
        assert_eq!(saved.lineups.keys().collect::<Vec<_>>(), ["youth"]);
        assert_eq!(saved.lineups["youth"], youth);
        // Its own lineup plays: it previews.
        let preview = format!(
            "/api/backups/{}/preview",
            info["filename"].as_str().unwrap()
        );
        let (status, json) = call(&app, Method::POST, &preview, Some(&eng), None).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        // It names the lineups whose stored state a restore replaces.
        assert_eq!(json["lineups"], serde_json::json!(["youth"]), "{json}");
        // A backup taken while youth played is not put on main's sets.
        let mut other = saved.clone();
        other.lineup = Some("youth".into());
        let name = state.backup_store.save(&other).unwrap();
        // Nor, once youth plays, one from before the roster (main's).
        let mut before = saved.clone();
        before.lineup = None;
        before.lineups.clear();
        let old = state.backup_store.save(&before).unwrap();
        let mismatch = |status: StatusCode, json: &serde_json::Value, what: &str| {
            assert_eq!(
                (status, json["code"].as_str()),
                (StatusCode::CONFLICT, Some("LINEUP_MISMATCH")),
                "{what}: {json}"
            );
        };
        for action in ["preview", "restore"] {
            let uri = format!("/api/backups/{name}/{action}");
            let (status, json) = call(&app, Method::POST, &uri, Some(&eng), None).await;
            mismatch(status, &json, action);
        }
        let uri = format!("/api/backups/{old}/preview");
        let (status, json) = call(&app, Method::POST, &uri, Some(&eng), None).await;
        assert_eq!(status, StatusCode::OK, "main plays: {json}");
        assert!(json.get("lineups").is_none(), "{json}");
        state.roster.write().unwrap().as_mut().unwrap().active = "youth".into();
        for action in ["preview", "restore"] {
            let uri = format!("/api/backups/{old}/{action}");
            let (status, json) = call(&app, Method::POST, &uri, Some(&eng), None).await;
            mismatch(status, &json, action);
        }
    }

    #[tokio::test]
    async fn a_capture_with_unreadable_pins_is_refused_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (mut state, _) = app(dir.path());
        let (engine, mut peer) = fake::announced(crate::site_view::tests::test_topology()).await;
        state.engine = engine;
        fake::sync(&state.engine, &mut peer, 3, MixState::default()).await;
        let app = router(state.clone());
        let eng = token("engineer", true);
        let broken = dir.path().join("customizations").join("member3.json");
        std::fs::create_dir_all(&broken).unwrap();
        let capture = "/api/backups/capture";
        let (status, json) = call(&app, Method::POST, capture, Some(&eng), None).await;
        assert_eq!(
            (status, json["code"].as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, Some("CAPTURE_FAILED")),
            "{json}"
        );
        assert!(
            json["message"].as_str().unwrap().contains("member3"),
            "{json}"
        );
        assert!(state.backup_store.list().is_empty(), "no partial backup");
        std::fs::remove_dir(&broken).unwrap();
        let (status, info) = call(&app, Method::POST, capture, Some(&eng), None).await;
        assert_eq!(status, StatusCode::OK, "{info}");
    }

    /// Waits (≤ 5 s) until `done` holds.
    async fn until(what: &str, mut done: impl FnMut() -> bool) {
        let t0 = std::time::Instant::now();
        while !done() {
            assert!(
                t0.elapsed() < std::time::Duration::from_secs(5),
                "{what} within 5 s"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// #58 review: a restore whose request is dropped after the engine got
    /// the backup's state still finishes: the pins and hides are written and
    /// the switch is free again.
    #[tokio::test]
    async fn a_restore_finishes_when_its_request_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let (mut state, _) = app(dir.path());
        let (engine, mut peer) = fake::announced(crate::site_view::tests::test_topology()).await;
        state.engine = engine;
        fake::sync(&state.engine, &mut peer, 1, MixState::default()).await;
        let mut b = iem_core::backup::MixerBackup::new(
            "2026-10-21T12:00:00Z".into(),
            1,
            MixState::default(),
        );
        let mic1 = iem_engine_proto::Source::Input(InputId::new("mic1"));
        b.customizations.insert(
            "member2".into(),
            iem_core::band::CustomizationFile::new("member2", vec![mic1.clone()], vec![]),
        );
        let name = state.backup_store.save(&b).unwrap();
        let api = router(state.clone());
        let eng = token("engineer", true);
        let uri = format!("/api/backups/{name}/restore");
        let request =
            tokio::spawn(async move { call(&api, Method::POST, &uri, Some(&eng), None).await });
        let (id, imported) = fake::next_import(&mut peer).await;
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        fake::answer_import(&mut peer, id, 2, imported).await;
        until("the pins and hides", || {
            state.band().customization("member2").unwrap().pinned == vec![mic1.clone()]
        })
        .await;
        until("the switch free", || state.switching.try_lock().is_ok()).await;
    }

    /// #58 review: a capture that comes while the band is being switched
    /// waits for the switch and backs up the band that plays then: no
    /// refusal, which the scheduled capture would turn into an alarm.
    #[tokio::test]
    async fn a_capture_waits_for_a_switch() {
        let dir = tempfile::tempdir().unwrap();
        let (mut state, _) = app(dir.path());
        state.set_roster(Some(crate::roster_state::tests::test_roster("main")));
        crate::lineup_store::LineupStore::new(dir.path())
            .set_active("main", 1)
            .unwrap();
        let (engine, mut peer) = fake::announced(crate::site_view::tests::test_topology()).await;
        state.engine = engine;
        fake::sync(&state.engine, &mut peer, 1, MixState::default()).await;
        let switch = {
            let s = state.clone();
            let by = iem_core::AuthClaims {
                sub: "engineer".into(),
                engineer: true,
                exp: u64::MAX,
                iat: 0,
            };
            tokio::spawn(async move { crate::lineup_switch::switch_lineup(&s, "youth", &by).await })
        };
        let (id, imported) = fake::next_import(&mut peer).await;
        let api = router(state.clone());
        let eng = token("engineer", true);
        let capture = tokio::spawn(async move {
            call(&api, Method::POST, "/api/backups/capture", Some(&eng), None).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!capture.is_finished(), "it waits for the switch");
        fake::answer_import(&mut peer, id, 2, imported).await;
        let within = std::time::Duration::from_secs(5);
        let switched = tokio::time::timeout(within, switch).await.unwrap().unwrap();
        assert_eq!(switched, Ok(()));
        let (status, info) = tokio::time::timeout(within, capture)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status, StatusCode::OK, "{info}");
        let saved = state
            .backup_store
            .load(info["filename"].as_str().unwrap())
            .unwrap();
        assert_eq!(saved.lineup.as_deref(), Some("youth"));
    }

    /// #58 review: a restore's import holds the active lineup's rules, as a
    /// switch's does: a set nobody wears in youth is silent.
    #[tokio::test]
    async fn a_restore_holds_the_lineups_rules() {
        let dir = tempfile::tempdir().unwrap();
        let (mut state, _) = app(dir.path());
        state.set_roster(Some(crate::roster_state::tests::test_roster("youth")));
        let topology = crate::site_view::tests::test_topology();
        let mut live = MixState::default();
        for m in &topology.mixes {
            live.mixes.insert(m.id.clone(), Default::default());
        }
        let (engine, mut peer) = fake::announced(topology).await;
        state.engine = engine;
        fake::sync(&state.engine, &mut peer, 1, MixState::default()).await;
        let mut b = iem_core::backup::MixerBackup::new("2026-10-21T12:00:00Z".into(), 1, live);
        b.lineup = Some("youth".into());
        let name = state.backup_store.save(&b).unwrap();
        let api = router(state.clone());
        let eng = token("engineer", true);
        let uri = format!("/api/backups/{name}/restore");
        let request =
            tokio::spawn(async move { call(&api, Method::POST, &uri, Some(&eng), None).await });
        let imported = fake::imports(&mut peer, 2).await;
        let (status, json) = request.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{json}");
        let mix = |id: &str| &imported.mixes[&iem_engine_proto::MixId::new(id)];
        assert!(
            mix("member2").out.muted,
            "nobody wears set member2 in youth"
        );
        assert!(!mix("member3").out.muted);
    }
}
