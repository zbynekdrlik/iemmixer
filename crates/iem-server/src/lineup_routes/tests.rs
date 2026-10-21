//! `/api/lineup` as the page sees it (#58).

use super::*;

use axum::http::Method;
use iem_engine_proto::MixState;

use crate::engine::client::EngineError;
use crate::engine::client::fake;
use crate::roster_state::tests::test_roster;
use crate::routes::api_tests::{app, call, router, token};
use crate::site_view::tests::test_topology;

async fn get(api: &Router, bearer: Option<&str>) -> (StatusCode, serde_json::Value) {
    call(api, Method::GET, "/api/lineup", bearer, None).await
}

async fn switch(api: &Router, bearer: &str, to: &str) -> (StatusCode, serde_json::Value) {
    let body = serde_json::json!({ "to": to }).to_string();
    call(
        api,
        Method::POST,
        "/api/lineup/switch",
        Some(bearer),
        Some(&body),
    )
    .await
}

fn lineups() -> serde_json::Value {
    serde_json::json!([
        {"id": "main", "name": "Main band"},
        {"id": "youth", "name": "Youth band"}
    ])
}

/// While youth plays: who plays, who leads it, who may switch.
#[tokio::test]
async fn the_lineup_tells_the_bearer_what_they_may_do() {
    let dir = tempfile::tempdir().unwrap();
    let (state, api) = app(dir.path());
    assert_eq!(get(&api, None).await.0, StatusCode::UNAUTHORIZED);
    let (status, json) = get(&api, Some(&token("member1", false))).await;
    assert_eq!(
        (status, json["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("NOT_CONFIGURED")),
        "legacy mode"
    );
    state.set_roster(Some(test_roster("youth")));
    let (status, json) = get(&api, Some(&token("member2", false))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        serde_json::json!({
            "active": {"id": "youth", "name": "Youth band"},
            "lineups": lineups(),
            "playing": false,
            "leader": false,
            "can_switch": false
        })
    );
    let flags = |json: &serde_json::Value| {
        (
            json["playing"].as_bool().unwrap(),
            json["leader"].as_bool().unwrap(),
            json["can_switch"].as_bool().unwrap(),
        )
    };
    for (who, engineer, want) in [
        ("member3", false, (true, true, true)),
        ("member1", false, (false, false, true)),
        ("member10", false, (true, false, false)),
        ("engineer", true, (true, false, true)),
    ] {
        let (status, json) = get(&api, Some(&token(who, engineer))).await;
        assert_eq!(status, StatusCode::OK, "{who}");
        assert_eq!(flags(&json), want, "{who}");
        assert_eq!(json["lineups"], lineups(), "{who}");
    }
}

#[test]
fn a_refused_switch_answers_with_its_reason() {
    let answer = |e: SwitchError| {
        let (status, Json(body)) = reject(&e);
        (status, body.code)
    };
    let engine = EngineError::Timeout;
    for (e, status, code) in [
        (
            SwitchError::NotConfigured,
            StatusCode::NOT_FOUND,
            "NOT_CONFIGURED",
        ),
        (
            SwitchError::Unknown("x".into()),
            StatusCode::NOT_FOUND,
            "UNKNOWN_LINEUP",
        ),
        (SwitchError::Forbidden, StatusCode::FORBIDDEN, "FORBIDDEN"),
        (SwitchError::Busy, StatusCode::CONFLICT, "BUSY"),
        (SwitchError::Same, StatusCode::CONFLICT, "ALREADY_PLAYING"),
        (
            SwitchError::NotSynced,
            StatusCode::SERVICE_UNAVAILABLE,
            "ENGINE_UNAVAILABLE",
        ),
        (
            SwitchError::Io("disk".into()),
            StatusCode::INTERNAL_SERVER_ERROR,
            "IO_ERROR",
        ),
        (
            SwitchError::Engine(engine),
            StatusCode::BAD_GATEWAY,
            "SWITCH_FAILED",
        ),
    ] {
        assert_eq!(answer(e), (status, code.to_string()));
    }
    // The id asked for is never echoed.
    let (_, Json(body)) = reject(&SwitchError::Unknown("member7".into()));
    assert!(!body.message.contains("member7"), "{}", body.message);
}

#[tokio::test]
async fn the_switch_answers_by_its_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let (mut state, legacy) = app(dir.path());
    let eng = token("engineer", true);
    let (status, json) = switch(&legacy, &eng, "youth").await;
    assert_eq!(
        (status, json["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("NOT_CONFIGURED"))
    );
    state.set_roster(Some(test_roster("main")));
    let (engine, mut peer) = fake::announced(test_topology()).await;
    state.engine = engine;
    fake::sync(&state.engine, &mut peer, 1, MixState::default()).await;
    let api = router(state.clone());
    let code = |json: serde_json::Value| json["code"].as_str().map(str::to_string);
    let (status, json) = switch(&api, &token("member2", false), "youth").await;
    assert_eq!(
        (status, code(json)),
        (StatusCode::FORBIDDEN, Some("FORBIDDEN".into()))
    );
    let (status, json) = switch(&api, &eng, "nowhere").await;
    assert_eq!(
        (status, code(json)),
        (StatusCode::NOT_FOUND, Some("UNKNOWN_LINEUP".into()))
    );
    let (status, json) = switch(&api, &eng, "main").await;
    assert_eq!(
        (status, code(json)),
        (StatusCode::CONFLICT, Some("ALREADY_PLAYING".into()))
    );
    let (status, _) = call(
        &api,
        Method::POST,
        "/api/lineup/switch",
        None,
        Some(r#"{"to":"youth"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no token");
    let leader = token("member3", false);
    let task = {
        let api = api.clone();
        tokio::spawn(async move { switch(&api, &leader, "youth").await })
    };
    fake::imports(&mut peer, 2).await;
    let (status, _) = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("within 5 s")
        .unwrap();
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(state.roster().unwrap().active, "youth");
    let _held = state.switching.lock().await;
    let (status, json) = switch(&api, &eng, "main").await;
    assert_eq!(
        (status, code(json)),
        (StatusCode::CONFLICT, Some("BUSY".into()))
    );
}

/// #58 review: a switch whose request is dropped (the phone went away)
/// after the engine got the band's state still finishes: the new band is
/// recorded, the intent is gone and the pages are free again.
#[tokio::test]
async fn a_switch_finishes_when_its_request_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let (mut state, _) = app(dir.path());
    state.set_roster(Some(test_roster("main")));
    let (engine, mut peer) = fake::announced(test_topology()).await;
    state.engine = engine;
    fake::sync(&state.engine, &mut peer, 1, MixState::default()).await;
    let api = router(state.clone());
    let leader = token("member3", false);
    let request = tokio::spawn(async move { switch(&api, &leader, "youth").await });
    let (id, imported) = fake::next_import(&mut peer).await;
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    fake::answer_import(&mut peer, id, 2, imported).await;
    let store = crate::lineup_store::LineupStore::new(dir.path());
    let t0 = std::time::Instant::now();
    while state.roster().unwrap().active != "youth" || state.frozen_now() {
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(5),
            "the switch finished"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(store.active().unwrap().as_deref(), Some("youth"));
    assert_eq!(store.intent().unwrap(), None);
}
