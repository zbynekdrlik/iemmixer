//! The server against the real engine (S5 design note §7): `iem-engine` on
//! NullRt with the test site, driven through the server's own client, view
//! and routes. Every wait is bounded (5 s).

use std::time::Duration;

use iem_core::ClientMsg;
use iem_engine_proto::{Cmd, InputId, MixId, Source};

use crate::engine::client::EngineEvent;
use crate::engine::testkit::{EngineHarness, wait_until};
use crate::view::{self, Viewer};

fn member(sub: &str) -> Viewer {
    Viewer {
        sub: sub.into(),
        engineer: false,
    }
}

async fn changed(rx: &mut tokio::sync::broadcast::Receiver<EngineEvent>) -> (Option<u64>, usize) {
    let t0 = std::time::Instant::now();
    loop {
        assert!(t0.elapsed() < Duration::from_secs(5), "a change within 5 s");
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Ok(EngineEvent::Changed { origin, changes })) => return (origin, changes.len()),
            Ok(Ok(_)) => {}
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn the_engine_announces_the_test_site() {
    let h = EngineHarness::start();
    let (_d, s) = h.state().await;
    let site = s.site().expect("the topology");
    assert_eq!(site.inputs.len(), 24);
    assert_eq!(site.mixes.len(), 11);
    assert_eq!(site.members.len(), 10);
    let m = s.engine.mirror();
    assert!(m.synced);
    assert_eq!(m.hello.as_ref().map(|h| h.sample_rate), Some(96_000));
    assert_eq!(m.state.mixes.len(), 11);
    drop(m);
    assert!(s.page("translator").is_some(), "a member-less mix page");
}

#[tokio::test]
async fn a_ui_command_changes_the_engine_and_tells_the_other_sessions() {
    let h = EngineHarness::start();
    let (_d, s) = h.state().await;
    let site = s.site().unwrap();
    let page = s.page("member2").unwrap();
    let mut rx = s.engine.subscribe();
    let cmds = view::command(
        &site,
        &s.engine.mirror(),
        &page,
        &member("member2"),
        &ClientMsg::SetLevel {
            id: "keys".into(),
            level_db: -6.0,
        },
    )
    .unwrap();
    s.apply(&page, cmds, Some(42)).await.unwrap();
    let lvl = s
        .engine
        .mirror()
        .level(&MixId::new("member2"), &Source::Input(InputId::new("keys")));
    assert_eq!(lvl.gain_db, -6.0);
    assert_eq!(changed(&mut rx).await, (Some(42), 1));
    // The same page seen by another session gets the update; another page not.
    let ups = view::updates_for(
        &site,
        &s.engine.mirror(),
        &page,
        &[iem_engine_proto::Change::Level {
            mix: MixId::new("member2"),
            source: Source::Input(InputId::new("keys")),
            level: lvl,
        }],
    );
    assert_eq!(
        ups,
        vec![iem_core::ServerMsg::ChannelUpdate {
            id: "keys".into(),
            level_db: -6.0,
            muted: false,
            pan: 0.5
        }]
    );
    // The daily auto-snapshot was taken before the change.
    let snaps = s.band.snapshots("member2").unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].label, "auto");
    let keys = snaps[0]
        .sends
        .iter()
        .find(|x| x.src == Source::Input(InputId::new("keys")))
        .unwrap();
    assert_eq!(
        keys.gain_db,
        iem_engine_proto::DB_OFF,
        "the state before the change"
    );
}

#[tokio::test]
async fn the_engine_refuses_what_it_does_not_know() {
    let h = EngineHarness::start();
    let (_d, s) = h.state().await;
    let err = s
        .engine
        .request(
            Cmd::SetMix {
                mix: MixId::new("nobody"),
                volume_db: Some(0.0),
                muted: None,
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, crate::engine::client::EngineError::Refused(_)),
        "{err:?}"
    );
}

#[tokio::test]
async fn an_engine_restart_disconnects_and_resyncs() {
    let mut h = EngineHarness::start();
    let (_d, s) = h.state().await;
    let mut rx = s.engine.subscribe();
    s.engine
        .request_applied(
            Cmd::SetMix {
                mix: MixId::new("member3"),
                volume_db: Some(-9.0),
                muted: Some(false),
            },
            None,
        )
        .await
        .unwrap();
    let _ = s.engine.request(Cmd::Shutdown, None).await;
    let engine = s.engine.clone();
    wait_until("the disconnect", || !engine.connected()).await;
    h.start_again();
    wait_until("the reconnect", || engine.connected()).await;
    let out = s.engine.mirror().out(&MixId::new("member3"));
    assert_eq!(
        (out.volume_db, out.muted),
        (-9.0, false),
        "persisted and resynced"
    );
    let mut saw = (false, false);
    while let Ok(ev) = rx.try_recv() {
        match ev {
            EngineEvent::Disconnected => saw.0 = true,
            EngineEvent::Connected => saw.1 = true,
            _ => {}
        }
    }
    assert_eq!(saw, (true, true));
}

#[tokio::test]
async fn a_second_controller_supersedes_the_first() {
    let h = EngineHarness::start();
    let (_d1, first) = h.state().await;
    let (_d2, second) = h.state().await;
    let engine = first.engine.clone();
    wait_until("the first controller to be superseded", || {
        !engine.connected()
    })
    .await;
    assert!(second.engine.connected());
}

#[tokio::test]
async fn mute_all_mutes_every_level_of_the_engineers_mix() {
    let h = EngineHarness::start();
    let (_d, s) = h.state().await;
    let site = s.site().unwrap();
    let page = s.page("engineer").unwrap();
    s.apply(&page, vec![view::mute_all(&site, &page)], None)
        .await
        .unwrap();
    let m = s.engine.mirror();
    let mix = &m.state.mixes[&MixId::new("engineer")];
    assert_eq!(mix.inputs.len(), 24);
    assert!(mix.inputs.values().all(|l| l.muted));
    assert_eq!(mix.mixes.len(), 9);
    assert!(mix.mixes.values().all(|l| l.muted));
    assert!(
        m.state.mixes[&MixId::new("member1")]
            .inputs
            .values()
            .all(|l| !l.muted),
        "other mixes untouched"
    );
}

#[tokio::test]
async fn a_preset_ramp_lands_exactly_on_its_target() {
    let h = EngineHarness::start();
    let (_d, s) = h.state().await;
    let site = s.site().unwrap();
    let page = s.page("member1").unwrap();
    let mut target = crate::band_store::capture(&site, &s.engine.mirror(), &page);
    for send in &mut target.sends {
        send.gain_db = -3.0;
        send.pan = 0.5;
    }
    target.sends[2].muted = true;
    target
        .groups
        .insert(iem_engine_proto::GroupId::new("stems"), -1.5);
    let (sent, skipped) =
        crate::preset_routes::apply_ramp(&s, &page, &target.sends, &target.groups)
            .await
            .unwrap();
    assert_eq!((sent, skipped), (32 + 1, 0));
    let now = crate::band_store::capture(&site, &s.engine.mirror(), &page);
    assert_eq!(now.sends, target.sends);
    assert_eq!(now.groups, target.groups);
}

#[tokio::test]
async fn a_solo_masks_the_other_channels_and_clears() {
    let h = EngineHarness::start();
    let (_d, s) = h.state().await;
    let site = s.site().unwrap();
    let page = s.page("member4").unwrap();
    let v = member("member4");
    let solo = |ids: &[&str]| ClientMsg::SetSolo {
        soloed: ids.iter().map(|x| x.to_string()).collect(),
    };
    let cmds = view::command(&site, &s.engine.mirror(), &page, &v, &solo(&["mic4"])).unwrap();
    s.apply(&page, cmds, None).await.unwrap();
    let chs = view::channels(&site, &s.engine.mirror(), &page, &v);
    assert!(!chs.iter().find(|c| c.id == "mic4").unwrap().muted);
    assert!(chs.iter().filter(|c| c.id != "mic4").all(|c| c.muted));
    let cmds = view::command(&site, &s.engine.mirror(), &page, &v, &solo(&[])).unwrap();
    s.apply(&page, cmds, None).await.unwrap();
    assert!(
        view::channels(&site, &s.engine.mirror(), &page, &v)
            .iter()
            .all(|c| !c.muted)
    );
}

#[tokio::test]
async fn a_backup_restores_through_import_state() {
    let h = EngineHarness::start();
    let (_d, s) = h.state().await;
    let site = s.site().unwrap();
    s.engine
        .request_applied(
            Cmd::SetInput {
                input: InputId::new("keys"),
                trim_db: Some(4.0),
                muted: None,
                processing: None,
            },
            None,
        )
        .await
        .unwrap();
    let backup = crate::backup::capture(&site, &s.engine.mirror(), &s.band, "t".into());
    s.engine
        .request_applied(
            Cmd::SetInput {
                input: InputId::new("keys"),
                trim_db: Some(-2.0),
                muted: Some(true),
                processing: None,
            },
            None,
        )
        .await
        .unwrap();
    let preview =
        crate::backup::preview(&site, &s.engine.mirror().state.clone(), &|_| None, &backup);
    let what: Vec<&str> = preview
        .changes
        .iter()
        .map(|c| c.description.as_str())
        .collect();
    assert_eq!(what, ["KEYS: trim", "KEYS: mute"]);
    s.engine
        .request_applied(
            Cmd::ImportState {
                state: backup.state.clone(),
                baseline: false,
            },
            None,
        )
        .await
        .unwrap();
    let keys = s.engine.mirror().input(&InputId::new("keys"));
    assert_eq!((keys.trim_db, keys.muted), (4.0, false));
}

#[cfg(feature = "audio")]
#[tokio::test]
async fn listen_frames_flow_as_opus_from_the_engineers_tap() {
    use iem_audio_io::InputSignal;
    let h = EngineHarness::start_with(|cfg| {
        cfg.signal = InputSignal::Sine {
            hz: 1000.0,
            amp: 0.1,
        };
    });
    let (_d, s) = h.state().await;
    s.engine
        .request_applied(
            Cmd::SetLevel {
                mix: MixId::new("engineer"),
                source: Source::Input(InputId::new("mic1")),
                gain_db: Some(0.0),
                pan: None,
                muted: Some(false),
            },
            None,
        )
        .await
        .unwrap();
    let media = s.media.clone();
    wait_until("the media pipe", || media.connected()).await;
    let mut rx = s.media.subscribe(0);
    s.engine
        .request(
            Cmd::StartListen {
                mix: MixId::new("engineer"),
            },
            None,
        )
        .await
        .unwrap();
    let mut dec = opus::Decoder::new(48_000, opus::Channels::Stereo).unwrap();
    let mut out = vec![0f32; 1920];
    let mut loudest = 0f32;
    for _ in 0..25 {
        let packet = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("a listen frame within 5 s")
            .expect("a frame");
        let n = dec.decode_float(&packet, &mut out, false).unwrap();
        assert_eq!(n, 960);
        loudest = out.iter().fold(loudest, |m, x| m.max(x.abs()));
    }
    assert!(
        loudest > 0.02,
        "the sine through the engineer's tap: {loudest}"
    );
    assert!(s.media.diagnostics().receiving_oiem);
}

#[cfg(feature = "audio")]
#[tokio::test]
async fn talkback_frames_reach_the_talkback_input() {
    let h = EngineHarness::start();
    let (_d, s) = h.state().await;
    let media = s.media.clone();
    wait_until("the media pipe", || media.connected()).await;
    let topo = s.engine.mirror().topology.clone().unwrap();
    let talk = topo.inputs.iter().position(|i| i.talkback).unwrap();
    let mut rx = s.engine.subscribe();
    let tone: Vec<f32> = (0..960)
        .map(|i| 0.5 * (std::f32::consts::TAU * 500.0 * i as f32 / 48_000.0).sin())
        .collect();
    let mut peak = 0f32;
    let t0 = std::time::Instant::now();
    while peak < 0.01 {
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "talkback on the input meter"
        );
        s.media.send_talkback(tone.clone());
        tokio::time::sleep(Duration::from_millis(20)).await;
        while let Ok(ev) = rx.try_recv() {
            if let EngineEvent::Meters(m) = ev
                && let Some(p) = m.inputs.get(talk)
            {
                peak = peak.max(p[0]);
            }
        }
    }
}
