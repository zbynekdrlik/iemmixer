//! The band switch (#58 design §7): rights, the order of its steps, a
//! refusal, a second switch, frozen pages, the replay after a restart, and
//! the target state's rules.

use super::*;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use iem_core::backup::MixerBackup;
use iem_core::band::CustomizationFile;
use iem_core::{ClientMsg, LineupRef};
use iem_engine_proto::{Change, EngineMsg, InputId, Level, Mix, MixId, Reply, Source};
use tokio::sync::mpsc;

use crate::engine::client::fake;
use crate::mixer_ws::{CLOSE_LINEUP, OnLineup, on_lineup};
use crate::roster_state::tests::test_roster;
use crate::routes::api_tests::{app, call, router, token};
use crate::site_view::tests::{test_config, test_topology};
use crate::view::{ViewError, Viewer};

/// The test roster's text (`config/test-roster.toml`).
const TEXT: &str = include_str!("../../../../config/test-roster.toml");

fn claims(sub: &str, engineer: bool) -> AuthClaims {
    AuthClaims {
        sub: sub.into(),
        engineer,
        exp: u64::MAX,
        iat: 0,
    }
}

fn mix_id(id: &str) -> MixId {
    MixId::new(id)
}

fn level(gain_db: f64) -> Level {
    Level {
        gain_db,
        pan: 0.0,
        muted: false,
    }
}

fn mix_mut<'a>(s: &'a mut MixState, id: &str) -> &'a mut Mix {
    s.mixes.get_mut(&mix_id(id)).unwrap()
}

/// The running state of the main band: every mix of the test site; keys
/// trimmed 1 dB; member1 hears every other set and member3 hears member1,
/// all open (both lead main).
fn running() -> MixState {
    let mut s = MixState::default();
    for m in test_topology().mixes {
        s.mixes.insert(m.id, Mix::default());
    }
    s.inputs.entry(InputId::new("keys")).or_default().trim_db = 1.0;
    for n in 2..=9 {
        mix_mut(&mut s, "member1")
            .mixes
            .insert(mix_id(&format!("member{n}")), level(0.0));
    }
    mix_mut(&mut s, "member3")
        .mixes
        .insert(mix_id("member1"), level(0.0));
    s
}

/// A state over `dir` with the test roster in use and `active` recorded
/// as playing (a migrated site).
fn with_roster(dir: &std::path::Path, active: &str) -> AppState {
    let state = AppState::new(test_config(), dir);
    state.set_roster(Some(test_roster(active)));
    LineupStore::new(dir).set_active(active, 1).unwrap();
    state
}

/// Connects `state` to a fake engine whose mirror holds `running` at
/// revision 1; the engine's end.
async fn synced(state: &mut AppState, running: MixState) -> fake::Peer {
    let (engine, mut peer) = fake::announced(test_topology()).await;
    state.engine = engine;
    fake::sync(&state.engine, &mut peer, 1, running).await;
    peer
}

/// [`switch_lineup`] to `to` by `by` on its own task.
fn spawn_switch(
    state: &AppState,
    to: &str,
    by: AuthClaims,
) -> tokio::task::JoinHandle<Result<(), SwitchError>> {
    let (s, to) = (state.clone(), to.to_string());
    tokio::spawn(async move { switch_lineup(&s, &to, &by).await })
}

async fn within<T>(task: tokio::task::JoinHandle<T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("within 5 s")
        .expect("the task")
}

fn youth_ref() -> LineupRef {
    LineupRef {
        id: "youth".into(),
        name: "Youth band".into(),
    }
}

fn pinned(input: &str) -> Vec<Source> {
    vec![Source::Input(InputId::new(input))]
}

/// main → youth by youth's leader: the engine gets youth's stored state
/// under youth's rules, youth is recorded, the intent is gone, main's live
/// state is stored, the change is announced (people's pages close with
/// 4003, the engineer's gets its state again) and the engineer's devices
/// hear of it. Pins and hides are youth's own files, never the copy inside
/// youth's stored state.
#[tokio::test]
async fn a_leader_switches_and_everyone_gets_their_lineup_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "main");
    let store = LineupStore::new(dir.path());
    let mut youth = MixState::default();
    youth
        .inputs
        .entry(InputId::new("keys"))
        .or_default()
        .trim_db = 3.0;
    let mut stored = MixerBackup::new("2026-10-21T12:00:00Z".into(), 1, youth);
    stored.customizations.insert(
        "member10".into(),
        CustomizationFile::new("member10", pinned("mic1"), vec![]),
    );
    store.save_state("youth", &stored).unwrap();
    state
        .band_of(Some("youth"))
        .save_customization("member10", pinned("mic4"), vec![])
        .unwrap();
    let (base, seen) = crate::push::tests::fake_push_service().await;
    state.config.write().await.vapid_private_key = crate::push::tests::vapid_private_key();
    state
        .push_store
        .write()
        .await
        .add(crate::push::tests::subscription(format!("{base}/201")))
        .unwrap();
    let mut peer = synced(&mut state, running()).await;
    let mut rx = state.lineup_tx.subscribe();

    let task = spawn_switch(&state, "youth", claims("member3", false));
    let imported = fake::imports(&mut peer, 2).await;
    assert_eq!(within(task).await, Ok(()));

    // Youth's stored values, the running ones it lacks, youth's rules.
    assert_eq!(imported.inputs[&InputId::new("keys")].trim_db, 3.0);
    let heard = |mix: &str, of: &str| imported.mixes[&mix_id(mix)].mixes[&mix_id(of)].muted;
    assert!(
        heard("member1", "member2"),
        "member10 leads no part of youth"
    );
    assert!(!heard("member3", "member1"), "member3 leads youth");
    assert!(
        imported.mixes[&mix_id("member2")].out.muted,
        "a set nobody wears"
    );
    assert!(!imported.mixes[&mix_id("member1")].out.muted);
    // Recorded; main's live state is main's stored state now.
    assert_eq!(store.active().unwrap().as_deref(), Some("youth"));
    assert_eq!(store.intent().unwrap(), None);
    let main = store.state("main").unwrap().unwrap();
    assert_eq!(main.lineup.as_deref(), Some("main"));
    assert_eq!(main.state, running());
    assert_eq!(state.roster().unwrap().active, "youth");
    assert!(!state.frozen_now());
    // The pins and hides are youth's file's, never the stored copy's.
    assert_eq!(
        state.band().customization("member10").unwrap().pinned,
        pinned("mic4")
    );
    // Announced: a person's page closes with 4003, the engineer's stays.
    assert_eq!(rx.try_recv().unwrap(), youth_ref());
    assert_eq!(CLOSE_LINEUP, 4003);
    let page10 = state.page("member10").unwrap();
    let member10 = Viewer {
        sub: "member10".into(),
        engineer: false,
    };
    assert!(matches!(on_lineup(&state, &page10, &member10), OnLineup::Close(f) if f.code == 4003));
    let eng_page = state.page("engineer").unwrap();
    let eng = Viewer {
        sub: "engineer".into(),
        engineer: true,
    };
    match on_lineup(&state, &eng_page, &eng) {
        OnLineup::Refresh(msgs) => {
            assert!(matches!(
                msgs.as_slice(),
                [
                    iem_core::ServerMsg::State { .. },
                    iem_core::ServerMsg::CustomizationUpdate { .. }
                ]
            ));
        }
        other => panic!("{other:?}"),
    }
    // The engineer's device heard of it.
    let t0 = std::time::Instant::now();
    while seen.lock().unwrap().is_empty() {
        assert!(t0.elapsed() < Duration::from_secs(5), "a push within 5 s");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_non_leader_cannot_switch() {
    let dir = tempfile::tempdir().unwrap();
    let state = with_roster(dir.path(), "main");
    for who in ["member2", "member10", "ghost"] {
        assert_eq!(
            switch_lineup(&state, "youth", &claims(who, false)).await,
            Err(SwitchError::Forbidden),
            "{who}"
        );
    }
    assert_eq!(state.roster().unwrap().active, "main");
    assert!(may_switch(
        &test_roster("main").roster,
        &claims("member1", false)
    ));
    assert!(may_switch(
        &test_roster("main").roster,
        &claims("engineer", true)
    ));
    assert!(!may_switch(
        &test_roster("main").roster,
        &claims("member2", false)
    ));
    // Legacy mode has no band to switch.
    let legacy = AppState::new(test_config(), dir.path());
    assert_eq!(
        switch_lineup(&legacy, "youth", &claims("engineer", true)).await,
        Err(SwitchError::NotConfigured)
    );
}

#[tokio::test]
async fn the_engineer_can() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "main");
    let mut peer = synced(&mut state, running()).await;
    let eng = claims("engineer", true);
    assert_eq!(
        switch_lineup(&state, "main", &eng).await,
        Err(SwitchError::Same)
    );
    assert_eq!(
        switch_lineup(&state, "gone", &eng).await,
        Err(SwitchError::Unknown("gone".into()))
    );
    let task = spawn_switch(&state, "youth", eng);
    fake::imports(&mut peer, 2).await;
    assert_eq!(within(task).await, Ok(()));
    assert_eq!(state.roster().unwrap().active, "youth");
}

#[tokio::test]
async fn a_youth_leader_can_switch_back_to_main_without_playing_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "youth");
    let mut r = test_roster("youth");
    let roster = Arc::make_mut(&mut r.roster);
    roster.lineups[1].leaders.push("member10".into());
    assert!(roster.lineup("main").unwrap().set_of("member10").is_none());
    state.set_roster(Some(r));
    let mut peer = synced(&mut state, running()).await;
    let task = spawn_switch(&state, "main", claims("member10", false));
    let imported = fake::imports(&mut peer, 2).await;
    assert_eq!(within(task).await, Ok(()));
    // Main had no stored state: the running one, under main's rules.
    assert_eq!(imported, running());
    assert_eq!(state.roster().unwrap().active, "main");
}

/// #58 review: a page change on its way to the engine when a switch begins
/// is in the band's stored state: the switch waits for every change queued
/// before its freeze before it takes the live state.
#[tokio::test]
async fn a_change_on_its_way_is_in_the_bands_stored_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "main");
    let store = LineupStore::new(dir.path());
    let mut peer = synced(&mut state, running()).await;
    let page = state.page("member3").unwrap();
    let change = {
        let s = state.clone();
        tokio::spawn(async move {
            let viewer = Viewer {
                sub: "member3".into(),
                engineer: false,
            };
            let (tx, _rx) = mpsc::unbounded_channel();
            let set = ClientMsg::SetLevel {
                id: "keys".into(),
                level_db: -6.0,
            };
            crate::mixer_ws::handle(&s, 7, &page, &viewer, set, &tx).await;
        })
    };
    let (change_id, cmd) = fake::next_request(&mut peer).await;
    assert!(matches!(cmd, Cmd::SetLevel { .. }), "{cmd:?}");
    // The switch begins while the engine holds the change.
    let switch = spawn_switch(&state, "youth", claims("engineer", true));
    let level = Level {
        gain_db: -6.0,
        pan: 0.0,
        muted: false,
    };
    peer.send(&EngineMsg::Reply(Reply {
        id: change_id,
        rev: 2,
        error: None,
    }))
    .await;
    peer.send(&EngineMsg::Delta {
        rev: 2,
        origin: Some(7),
        changes: vec![Change::Level {
            mix: mix_id("member3"),
            source: Source::Input(InputId::new("keys")),
            level,
        }],
    })
    .await;
    within(change).await;
    fake::imports(&mut peer, 3).await;
    assert_eq!(within(switch).await, Ok(()));
    let main = store.state("main").unwrap().unwrap();
    assert_eq!(
        main.state.mixes[&mix_id("member3")].inputs[&InputId::new("keys")].gain_db,
        -6.0,
        "the change is main's"
    );
}

#[tokio::test]
async fn a_second_switch_is_busy() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "main");
    let mut peer = synced(&mut state, running()).await;
    let first = spawn_switch(&state, "youth", claims("engineer", true));
    // The first switch's import is at the engine: it holds the switch.
    let (id, imported) = fake::next_import(&mut peer).await;
    assert_eq!(
        switch_lineup(&state, "youth", &claims("member3", false)).await,
        Err(SwitchError::Busy)
    );
    let page = state.page("member1").unwrap();
    assert_eq!(
        state.may_change(&page),
        Err(ViewError::Switching),
        "frozen meanwhile"
    );
    fake::answer_import(&mut peer, id, 2, imported).await;
    assert_eq!(within(first).await, Ok(()), "the first one went through");
    // While a reload or a restore holds the switch, a switch waits for none.
    let _held = state.switching.lock().await;
    assert_eq!(
        switch_lineup(&state, "main", &claims("member3", false)).await,
        Err(SwitchError::Busy)
    );
}

/// While a switch runs, and from a page of a band that no longer plays,
/// no change reaches the engine or the band's files.
#[tokio::test]
async fn mix_changes_are_refused_while_switching() {
    let dir = tempfile::tempdir().unwrap();
    let (mut state, _) = app(dir.path());
    state.set_roster(Some(test_roster("main")));
    let mut peer = synced(&mut state, running()).await;
    let page = state.page("member3").unwrap();
    let viewer = Viewer {
        sub: "member3".into(),
        engineer: false,
    };
    let (tx, _rx) = mpsc::unbounded_channel();
    state.frozen.store(true, Ordering::SeqCst);
    assert_eq!(state.may_change(&page), Err(ViewError::Switching));
    let set = ClientMsg::SetLevel {
        id: "keys".into(),
        level_db: -6.0,
    };
    crate::mixer_ws::handle(&state, 7, &page, &viewer, set.clone(), &tx).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), peer.recv())
            .await
            .is_err(),
        "nothing reached the engine"
    );
    // Over HTTP: Mute All, a ramp, a restore and a backup are refused.
    let api = router(state.clone());
    let m3 = token("member3", false);
    let (status, json) = call(
        &api,
        Method::POST,
        "/api/mixer/member3/batch",
        Some(&m3),
        Some(r#"{"operation":"mute_all"}"#),
    )
    .await;
    assert_eq!(
        (status, json["code"].as_str()),
        (StatusCode::CONFLICT, Some("SWITCHING"))
    );
    let (status, json) = call(
        &api,
        Method::PUT,
        "/api/mixer/member3/customization",
        Some(&m3),
        Some(r#"{"pinned":["mic1"],"hidden":[]}"#),
    )
    .await;
    assert_eq!(
        (status, json["code"].as_str()),
        (StatusCode::CONFLICT, Some("SWITCHING"))
    );
    assert!(
        state
            .band()
            .customization("member3")
            .unwrap()
            .pinned
            .is_empty()
    );
    let ramp = crate::preset_routes::apply_ramp(&state, &page, &[], &Default::default()).await;
    assert_eq!(ramp.unwrap_err().0, StatusCode::CONFLICT);
    let eng = token("engineer", true);
    let (status, json) = call(
        &api,
        Method::POST,
        "/api/backups/20260101_000000.json/restore",
        Some(&eng),
        None,
    )
    .await;
    assert_eq!(
        (status, json["code"].as_str()),
        (StatusCode::CONFLICT, Some("SWITCHING"))
    );
    let (status, json) = call(&api, Method::POST, "/api/backups/capture", Some(&eng), None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json["message"], "the band is being switched");
    // A restore waits for no switch either: one of them at a time.
    state.frozen.store(false, Ordering::SeqCst);
    {
        let _held = state.switching.lock().await;
        let (status, _) = call(
            &api,
            Method::POST,
            "/api/backups/20260101_000000.json/restore",
            Some(&eng),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }
    assert_eq!(state.may_change(&page), Ok(()));
    // Youth plays now: member1's page of main is no page of it.
    let main_page = state.page("member1").unwrap();
    state.set_roster(Some(test_roster("youth")));
    assert_eq!(state.may_change(&main_page), Err(ViewError::Switching));
    let pin = ClientMsg::UpdateCustomization {
        pinned: vec!["mic1".into()],
        hidden: vec![],
    };
    let member1 = Viewer {
        sub: "member1".into(),
        engineer: false,
    };
    crate::mixer_ws::handle(&state, 8, &main_page, &member1, pin, &tx).await;
    crate::mixer_ws::handle(&state, 8, &main_page, &member1, set, &tx).await;
    assert!(
        state
            .band_of(Some("youth"))
            .customization("member1")
            .unwrap()
            .pinned
            .is_empty(),
        "nothing written into youth's files"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), peer.recv())
            .await
            .is_err(),
        "nothing reached the engine"
    );
}

#[tokio::test]
async fn an_engine_refusal_keeps_the_old_lineup() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "main");
    let store = LineupStore::new(dir.path());
    let mut peer = synced(&mut state, running()).await;
    let mut rx = state.lineup_tx.subscribe();
    let task = spawn_switch(&state, "youth", claims("engineer", true));
    fake::refuses(&mut peer).await;
    // The band that played is imported again.
    assert_eq!(fake::imports(&mut peer, 2).await, running());
    let result = within(task).await;
    assert!(
        matches!(result, Err(SwitchError::Engine(EngineError::Refused(_)))),
        "{result:?}"
    );
    assert_eq!(store.active().unwrap().as_deref(), Some("main"));
    assert_eq!(store.intent().unwrap(), None);
    assert_eq!(state.roster().unwrap().active, "main");
    assert!(!state.frozen_now());
    assert!(rx.try_recv().is_err(), "nothing announced");
}

/// A site whose switch main → youth a restart cut short: the roster file,
/// main recorded active (or `active`), the intent written.
fn cut_short(dir: &std::path::Path, active: &str, to: &str) -> AppState {
    std::fs::write(dir.join(iem_core::roster::ROSTER_FILE), TEXT).unwrap();
    let store = LineupStore::new(dir);
    store.set_active(active, 1).unwrap();
    store
        .set_intent(&SwitchIntent {
            from: "main".into(),
            to: to.into(),
        })
        .unwrap();
    AppState::new(test_config(), dir)
}

#[tokio::test]
async fn a_restart_mid_switch_replays_the_intent() {
    // Cut before youth was recorded: the start finishes the switch.
    let dir = tempfile::tempdir().unwrap();
    let store = LineupStore::new(dir.path());
    let mut youth = MixState::default();
    youth
        .inputs
        .entry(InputId::new("keys"))
        .or_default()
        .trim_db = 3.0;
    store
        .save_state("youth", &MixerBackup::new("t".into(), 1, youth))
        .unwrap();
    let mut state = cut_short(dir.path(), "main", "youth");
    assert!(state.frozen_now(), "no change until it is finished");
    assert_eq!(state.roster().unwrap().active, "main");
    assert_eq!(
        switch_lineup(&state, "youth", &claims("engineer", true)).await,
        Err(SwitchError::Busy),
        "a switch waits for the cut one"
    );
    let mut peer = synced(&mut state, running()).await;
    let mut rx = state.lineup_tx.subscribe();
    let replay = tokio::spawn(replay_when_synced(state.clone()));
    let imported = fake::imports(&mut peer, 2).await;
    within(replay).await;
    assert_eq!(imported.inputs[&InputId::new("keys")].trim_db, 3.0);
    assert!(
        imported.mixes[&mix_id("member2")].out.muted,
        "youth's rules"
    );
    assert_eq!(store.active().unwrap().as_deref(), Some("youth"));
    assert_eq!(store.intent().unwrap(), None);
    assert_eq!(state.roster().unwrap().active, "youth");
    assert!(!state.frozen_now());
    assert_eq!(rx.try_recv().unwrap(), youth_ref());

    // Cut after youth was recorded: the start only clears the intent.
    let dir = tempfile::tempdir().unwrap();
    let state = cut_short(dir.path(), "youth", "youth");
    assert!(state.frozen_now());
    assert_eq!(replay_intent(&state).await, Ok(()), "no engine needed");
    assert_eq!(LineupStore::new(dir.path()).intent().unwrap(), None);
    assert!(!state.frozen_now());
    assert_eq!(state.roster().unwrap().active, "youth");

    // A target the roster no longer holds: the band that played again.
    let dir = tempfile::tempdir().unwrap();
    let mut state = cut_short(dir.path(), "main", "gone");
    let mut peer = synced(&mut state, running()).await;
    let replay = tokio::spawn(async move {
        let done = replay_intent(&state).await;
        (done, state)
    });
    assert_eq!(fake::imports(&mut peer, 2).await, running());
    let (done, state) = within(replay).await;
    assert_eq!(done, Ok(()));
    assert_eq!(state.roster().unwrap().active, "main");
    assert_eq!(LineupStore::new(dir.path()).intent().unwrap(), None);
}

/// Without a switch to finish, the start's replay ends at once.
#[tokio::test]
async fn nothing_to_replay_ends_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let state = with_roster(dir.path(), "main");
    assert!(!state.frozen_now());
    tokio::time::timeout(Duration::from_secs(1), replay_when_synced(state))
        .await
        .expect("no wait for an engine");
}

/// #58 design §7: on a set whose wearer leads no part of the band, every
/// heard mix goes off, though main had them open.
#[test]
fn heard_mixes_go_off_on_a_non_leaders_set() {
    let r = test_roster("youth");
    let youth = r.roster.lineup("youth").unwrap();
    let s = target_state(None, &running(), &r.roster, youth);
    let set1 = &s.mixes[&mix_id("member1")];
    assert_eq!(set1.mixes.len(), 8);
    assert!(set1.mixes.values().all(|l| l.muted), "member10's set");
    assert!(!s.mixes[&mix_id("member3")].mixes[&mix_id("member1")].muted);
    // A stored state's open levels are no exception.
    let s = target_state(Some(running()), &MixState::default(), &r.roster, youth);
    assert!(s.mixes[&mix_id("member1")].mixes.values().all(|l| l.muted));
}

#[test]
fn an_empty_set_is_muted() {
    let r = test_roster("youth");
    let s = target_state(
        None,
        &running(),
        &r.roster,
        r.roster.lineup("youth").unwrap(),
    );
    for vacant in [
        "member2", "member5", "member6", "member7", "member8", "member9",
    ] {
        assert!(s.mixes[&mix_id(vacant)].out.muted, "{vacant}");
    }
    for worn in ["member1", "member3", "member4", "engineer", "translator"] {
        assert!(!s.mixes[&mix_id(worn)].out.muted, "{worn}");
    }
}

#[test]
fn a_lineup_without_state_starts_from_the_current_one() {
    let r = test_roster("main");
    let main = r.roster.lineup("main").unwrap();
    assert_eq!(target_state(None, &running(), &r.roster, main), running());
    // A stored state keeps its own values and takes the running ones it
    // lacks.
    let mut stored = MixState::default();
    stored
        .inputs
        .entry(InputId::new("keys"))
        .or_default()
        .trim_db = 5.0;
    let s = target_state(Some(stored), &running(), &r.roster, main);
    assert_eq!(s.inputs[&InputId::new("keys")].trim_db, 5.0);
    assert_eq!(s.mixes.len(), running().mixes.len());
    assert_eq!(
        s.mixes[&mix_id("member1")],
        running().mixes[&mix_id("member1")]
    );
}
