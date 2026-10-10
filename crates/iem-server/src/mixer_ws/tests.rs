//! The mixer socket's tests: tokens, pages, echoes, the catch-up and, live, its handler.

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
    assert!(addressed(&To::All, "member1", 1));
    assert!(addressed(&To::Page("member1".into()), "member1", 1));
    assert!(!addressed(&To::Page("member1".into()), "member2", 1));
    assert!(addressed(&To::Session(7), "member2", 7));
    assert!(!addressed(&To::Session(7), "member2", 8));
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
