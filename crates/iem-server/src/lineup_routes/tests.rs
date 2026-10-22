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
    // Legacy mode: no band to tell of, and no error status (a browser
    // logs every 4xx answer in its console, on every page load).
    let (status, json) = get(&api, Some(&token("member1", false))).await;
    assert_eq!(
        (status, json),
        (StatusCode::NO_CONTENT, serde_json::Value::Null),
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

async fn get_page(api: &Router, bearer: &str, page: &str) -> (StatusCode, serde_json::Value) {
    let uri = format!("/api/lineup?page={page}");
    call(api, Method::GET, &uri, Some(bearer), None).await
}

/// #61 review I2: `?page=<id>` asks whether that page's person plays now,
/// so the engineer on a person's page (a dimmed tile, or `/member1` kept
/// open across a switch) gets the notice, not a socket the server refuses.
/// `/engineer` and a page without a person always play; a person's own
/// page answers as without `page`. Only the engineer may ask about another
/// page, as only the engineer may open it (#61 second review m1; anyone
/// else: 403); a page the site lacks is 404.
#[tokio::test]
async fn the_lineup_of_a_page_says_whether_its_person_plays() {
    let dir = tempfile::tempdir().unwrap();
    let (mut state, legacy) = app(dir.path());
    let eng = token("engineer", true);
    assert_eq!(
        get_page(&legacy, &eng, "member2").await,
        (StatusCode::NO_CONTENT, serde_json::Value::Null),
        "legacy mode: no band, whatever the page"
    );
    state.set_roster(Some(test_roster("youth")));
    let (engine, mut peer) = fake::announced(test_topology()).await;
    state.engine = engine;
    fake::sync(&state.engine, &mut peer, 1, MixState::default()).await;
    let api = router(state.clone());
    let playing = |json: &serde_json::Value| json["playing"].as_bool().unwrap();
    for (page, plays) in [
        ("member2", false),
        ("member1", false),
        ("member10", true),
        ("member3", true),
        ("engineer", true),
        ("translator", true),
    ] {
        let (status, json) = get_page(&api, &eng, page).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert_eq!(playing(&json), plays, "{page}");
        // What the bearer may do stays the bearer's.
        assert!(json["can_switch"].as_bool().unwrap(), "{page}");
        assert!(!json["leader"].as_bool().unwrap(), "{page}");
        assert_eq!(json["active"]["id"], "youth", "{page}");
    }
    // A person's own page: as without `page`.
    let (status, json) = get_page(&api, &token("member2", false), "member2").await;
    assert_eq!((status, playing(&json)), (StatusCode::OK, false));
    let (status, json) = get_page(&api, &token("member10", false), "member10").await;
    assert_eq!((status, playing(&json)), (StatusCode::OK, true));
    // Anyone but the engineer asking about another page: 403, as opening
    // that page (#61 second review m1), leaders too (member1 leads main,
    // member3 leads youth).
    for (who, page) in [
        ("member1", "member10"),
        ("member3", "member2"),
        ("member10", "member2"),
        ("member2", "engineer"),
    ] {
        let (status, json) = get_page(&api, &token(who, false), page).await;
        assert_eq!(
            (status, json["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("FORBIDDEN")),
            "{who} on {page}"
        );
    }
    // A page the site does not have.
    let (status, _) = get_page(&api, &eng, "ghost").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Without `page`: the bearer's, as before.
    let (status, json) = get(&api, Some(&eng)).await;
    assert_eq!((status, playing(&json)), (StatusCode::OK, true));
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
        (
            SwitchError::Pending("the engine is not connected".into()),
            StatusCode::ACCEPTED,
            "SWITCH_PENDING",
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

/// #58 re-review (decision): a switch the engine dropped in, whose old
/// band could not go back in either, answers 202 SWITCH_PENDING; once the
/// engine is back the replay finishes it, and the pages and the
/// engineer's devices hear of it as of any switch.
#[tokio::test]
async fn a_pending_switch_answers_202_and_is_announced_once_done() {
    let dir = tempfile::tempdir().unwrap();
    let (mut state, _) = app(dir.path());
    state.set_roster(Some(test_roster("main")));
    let (base, seen) = crate::push::tests::fake_push_service().await;
    state.config.write().await.vapid_private_key = crate::push::tests::vapid_private_key();
    state
        .push_store
        .write()
        .await
        .add(crate::push::tests::subscription(format!("{base}/201")))
        .unwrap();
    let (connector, mut peers) = fake::connector();
    state.engine = crate::engine::EngineClient::spawn_with(connector, "test".into());
    state.retry = std::time::Duration::from_millis(50);
    let mut peer = fake::accept(&mut peers, test_topology()).await;
    fake::sync(&state.engine, &mut peer, 1, MixState::default()).await;
    let mut rx = state.lineup_tx.subscribe();
    let api = router(state.clone());
    let leader = token("member3", false);
    let request = tokio::spawn(async move { switch(&api, &leader, "youth").await });
    fake::next_import(&mut peer).await;
    drop(peer);
    let (status, json) = tokio::time::timeout(std::time::Duration::from_secs(5), request)
        .await
        .expect("within 5 s")
        .unwrap();
    assert_eq!(
        (status, json["code"].as_str()),
        (StatusCode::ACCEPTED, Some("SWITCH_PENDING")),
        "{json}"
    );
    assert!(state.frozen_now());
    assert!(seen.lock().unwrap().is_empty(), "not switched yet");
    let mut peer = fake::accept(&mut peers, test_topology()).await;
    fake::sync(&state.engine, &mut peer, 2, MixState::default()).await;
    fake::imports(&mut peer, 3).await;
    until("the switch finished", || {
        !state.frozen_now() && state.roster().unwrap().active == "youth"
    })
    .await;
    assert_eq!(rx.try_recv().unwrap().id, "youth");
    until("the engineer's devices heard of it", || {
        seen.lock().unwrap().len() == 1
    })
    .await;
}
