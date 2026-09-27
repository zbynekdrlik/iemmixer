//! Backups, presets and history over HTTP against the real engine (S5 design
//! note §6, §7): `iem-engine` on NullRt with the test site, the server's own
//! routes, and what they leave behind — status codes, JSON bodies, the files
//! written and the engine's state. Every wait is bounded (5 s).

use axum::Router;
use axum::http::{Method, StatusCode};
use iem_core::backup::MixerBackup;
use iem_engine_proto::{Cmd, InputId, MixId, Source};

use crate::AppState;
use crate::engine::testkit::EngineHarness;
use crate::routes::api_tests::{SECRET, call, router, token};

/// A state connected to `h`'s engine and its API (tokens from [`token`]).
async fn live(h: &EngineHarness) -> (tempfile::TempDir, AppState, Router) {
    let (dir, state) = h.state().await;
    state.config.write().await.jwt_secret = SECRET.into();
    let app = router(state.clone());
    (dir, state, app)
}

fn set_input(input: &str, trim_db: f64, muted: bool) -> Cmd {
    Cmd::SetInput {
        input: InputId::new(input),
        trim_db: Some(trim_db),
        muted: Some(muted),
        processing: None,
    }
}

fn set_level(mix: &str, input: &str, gain_db: f64) -> Cmd {
    Cmd::SetLevel {
        mix: MixId::new(mix),
        source: Source::Input(InputId::new(input)),
        gain_db: Some(gain_db),
        pan: None,
        muted: None,
    }
}

/// A level's gain and its pan in the engine's domain (−1…1).
fn set_send(mix: &str, input: &str, gain_db: f64, pan: f64) -> Cmd {
    Cmd::SetLevel {
        mix: MixId::new(mix),
        source: Source::Input(InputId::new(input)),
        gain_db: Some(gain_db),
        pan: Some(pan),
        muted: None,
    }
}

#[tokio::test]
async fn a_backup_is_captured_listed_previewed_and_restored() {
    let h = EngineHarness::start();
    let (dir, s, app) = live(&h).await;
    let eng = token("engineer", true);
    s.engine
        .request_applied(set_input("keys", 4.0, false), None)
        .await
        .unwrap();

    // Capture: the file is written and described.
    let (status, info) = call(&app, Method::POST, "/api/backups/capture", Some(&eng), None).await;
    assert_eq!(status, StatusCode::OK, "{info}");
    let name = info["filename"].as_str().unwrap().to_string();
    let file = dir.path().join("backups").join(&name);
    let size = std::fs::metadata(&file).unwrap().len();
    assert_eq!(info["size_bytes"].as_u64(), Some(size));
    assert_eq!(info["track_count"].as_u64(), Some(11));
    let saved = s.backup_store.load(&name).unwrap();
    assert_eq!(saved.state, s.engine.mirror().state, "the running state");
    assert_eq!(saved.customizations.len(), 10, "every member's pins");

    // Listed, and served whole.
    let (status, list) = call(&app, Method::GET, "/api/backups", Some(&eng), None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["filename"].as_str().unwrap())
        .collect();
    assert_eq!(names, [name.as_str()]);
    let one = format!("/api/backups/{name}");
    let (status, got) = call(&app, Method::GET, &one, Some(&eng), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(serde_json::from_value::<MixerBackup>(got).unwrap(), saved);

    // After a change: the preview names it, the restore undoes it.
    s.engine
        .request_applied(set_input("keys", -2.0, true), None)
        .await
        .unwrap();
    let preview = format!("/api/backups/{name}/preview");
    let (status, p) = call(&app, Method::POST, &preview, Some(&eng), None).await;
    assert_eq!(status, StatusCode::OK, "{p}");
    let what: Vec<&str> = p["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["description"].as_str().unwrap())
        .collect();
    assert_eq!(what, ["KEYS: trim", "KEYS: mute"]);
    let restore = format!("/api/backups/{name}/restore");
    let (status, r) = call(&app, Method::POST, &restore, Some(&eng), None).await;
    assert_eq!(
        (status, r["restored_count"].as_u64()),
        (StatusCode::OK, Some(2))
    );
    let keys = s.engine.mirror().input(&InputId::new("keys"));
    assert_eq!((keys.trim_db, keys.muted), (4.0, false));
}

#[tokio::test]
async fn a_capture_answers_with_its_backup_and_never_overwrites_one() {
    let h = EngineHarness::start();
    let (_dir, s, app) = live(&h).await;
    let eng = token("engineer", true);
    let now = || {
        chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    };
    let t0 = now();
    let capture = "/api/backups/capture";
    // Two captures in a row (a double click, the daemon's slot) are two
    // files, listed newest first. The names carry the milliseconds; two
    // backups of one moment are told apart by a number (forced below).
    let (status, a) = call(&app, Method::POST, capture, Some(&eng), None).await;
    assert_eq!(status, StatusCode::OK, "{a}");
    let (status, b) = call(&app, Method::POST, capture, Some(&eng), None).await;
    assert_eq!(status, StatusCode::OK, "{b}");
    let t1 = now();
    assert_ne!(a["filename"], b["filename"], "two files");
    for info in [&a, &b] {
        let name = info["filename"].as_str().unwrap();
        let saved = s.backup_store.load(name).unwrap();
        let ts = saved.timestamp.as_str();
        assert_eq!(info["timestamp"].as_str(), Some(ts));
        assert!(ts.len() == 24 && ts.ends_with('Z'), "{ts}");
        assert!(t0.as_str() <= ts && ts <= t1.as_str(), "{t0} ≤ {ts} ≤ {t1}");
        // Every level of every mix: 11 mixes × 24 inputs, and the mixes
        // member1 (8) and the engineer (9) hear.
        assert_eq!(info["send_count"].as_u64(), Some(11 * 24 + 8 + 9));
        assert_eq!(saved.level_count(), 11 * 24 + 8 + 9);
        assert_eq!(info["track_count"].as_u64(), Some(11));
    }
    let (status, list) = call(&app, Method::GET, "/api/backups", Some(&eng), None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["filename"].as_str().unwrap())
        .collect();
    let (a_name, b_name) = (
        a["filename"].as_str().unwrap(),
        b["filename"].as_str().unwrap(),
    );
    assert_eq!(names, [b_name, a_name], "newest first");

    // A backup of the first one's very moment (the daemon's slot and a
    // manual capture in one millisecond) takes the next free number; the
    // backup that has the name stays as it was.
    let first = s.backup_store.load(a_name).unwrap();
    let mut same_moment = first.clone();
    same_moment.rev += 1;
    let numbered = s.backup_store.save(&same_moment).unwrap();
    let stem = a_name.strip_suffix(".json").unwrap();
    // `_2`, unless the second capture fell in the same millisecond and took it.
    let free = (2..)
        .map(|n| format!("{stem}_{n}.json"))
        .find(|name| name.as_str() != b_name)
        .unwrap();
    assert_eq!(numbered, free);
    assert_eq!(
        s.backup_store.load(a_name).unwrap(),
        first,
        "not overwritten"
    );
    assert_eq!(s.backup_store.load(&numbered).unwrap(), same_moment);
    let (status, list) = call(&app, Method::GET, "/api/backups", Some(&eng), None).await;
    assert_eq!(status, StatusCode::OK);
    let listed: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["filename"].as_str().unwrap())
        .collect();
    let at = |name: &str| listed.iter().position(|l| *l == name);
    assert_eq!(listed.len(), 3, "{listed:?}");
    assert!(
        at(numbered.as_str()).unwrap() < at(a_name).unwrap(),
        "{listed:?}: the numbered one is listed before the first"
    );
}

#[tokio::test]
async fn a_stale_backups_restore_names_what_it_skipped() {
    let h = EngineHarness::start();
    let (_dir, s, app) = live(&h).await;
    let eng = token("engineer", true);
    let (status, info) = call(&app, Method::POST, "/api/backups/capture", Some(&eng), None).await;
    assert_eq!(status, StatusCode::OK, "{info}");
    // The backup of an older site: an input, a mix and a member that are
    // gone now, and one value that still applies.
    let mut old = s
        .backup_store
        .load(info["filename"].as_str().unwrap())
        .unwrap();
    let gone = InputId::new("gone");
    old.state.inputs.insert(gone.clone(), Default::default());
    old.state
        .mixes
        .get_mut(&MixId::new("member1"))
        .unwrap()
        .inputs
        .insert(gone.clone(), Default::default());
    old.state
        .mixes
        .insert(MixId::new("oldmix"), Default::default());
    old.customizations.insert(
        "ghost".into(),
        iem_core::band::CustomizationFile::new("ghost", vec![], vec![]),
    );
    old.state
        .inputs
        .get_mut(&InputId::new("keys"))
        .unwrap()
        .trim_db = 2.5;
    old.timestamp = "2026-01-01T00:00:00.000Z".into();
    let name = s.backup_store.save(&old).unwrap();

    let restore = format!("/api/backups/{name}/restore");
    let (status, r) = call(&app, Method::POST, &restore, Some(&eng), None).await;
    assert_eq!(status, StatusCode::OK, "{r}");
    let why = "not in the running topology";
    assert_eq!(
        r,
        serde_json::json!({
            "restored_count": 1,
            "skipped": [
                {"category": "Input", "description": "gone", "reason": why},
                {"category": "Level", "description": "gone → Member1", "reason": why},
                {"category": "Output", "description": "oldmix", "reason": why},
                {"category": "Customization", "description": "ghost", "reason": why},
            ]
        })
    );
    let m = s.engine.mirror();
    assert_eq!(m.input(&InputId::new("keys")).trim_db, 2.5, "applied");
    assert!(!m.state.inputs.contains_key(&gone));
    assert!(!m.state.mixes.contains_key(&MixId::new("oldmix")));
}

#[tokio::test]
async fn a_restore_keeps_what_the_backup_does_not_have() {
    let h = EngineHarness::start();
    let (_dir, s, app) = live(&h).await;
    let eng = token("engineer", true);
    let keys = InputId::new("keys");
    // keys away from its defaults: its strip and its level in two mixes.
    for cmd in [
        set_input("keys", 3.0, false),
        set_level("member2", "keys", -6.0),
        set_level("engineer", "keys", -9.0),
    ] {
        s.engine.request_applied(cmd, None).await.unwrap();
    }
    let (status, info) = call(&app, Method::POST, "/api/backups/capture", Some(&eng), None).await;
    assert_eq!(status, StatusCode::OK, "{info}");
    let name = info["filename"].as_str().unwrap().to_string();
    let fresh = format!("/api/backups/{name}/preview");
    let (status, p) = call(&app, Method::POST, &fresh, Some(&eng), None).await;
    assert_eq!(status, StatusCode::OK, "{p}");
    assert_eq!(p["not_in_backup"], serde_json::json!([]), "a fresh backup");

    // The same backup as if taken before keys was added to the site: no
    // strip and no level of it anywhere (a topology change after capture).
    let mut old = s.backup_store.load(&name).unwrap();
    old.state.inputs.remove(&keys);
    for mix in old.state.mixes.values_mut() {
        mix.inputs.remove(&keys);
    }
    old.timestamp = "2026-01-01T00:00:00.000Z".into();
    let old_name = s.backup_store.save(&old).unwrap();
    // One value the old backup does put back.
    s.engine
        .request_applied(set_input("mic1", 5.0, false), None)
        .await
        .unwrap();

    let preview = format!("/api/backups/{old_name}/preview");
    let (status, p) = call(&app, Method::POST, &preview, Some(&eng), None).await;
    assert_eq!(status, StatusCode::OK, "{p}");
    assert_eq!(
        p["not_in_backup"],
        serde_json::json!([{
            "category": "Input",
            "description": "KEYS",
            "reason": "not in the backup; stays as it is"
        }])
    );
    let before = s.engine.mirror().state.clone();
    let restore = format!("/api/backups/{old_name}/restore");
    let (status, r) = call(&app, Method::POST, &restore, Some(&eng), None).await;
    assert_eq!(
        (status, r["restored_count"].as_u64()),
        (StatusCode::OK, Some(1)),
        "{r}"
    );
    let after = s.engine.mirror().state.clone();
    assert_eq!(after.inputs[&InputId::new("mic1")].trim_db, 0.0, "restored");
    // keys is not in the backup: its strip and every level of it stay.
    assert_eq!(after.inputs[&keys], before.inputs[&keys]);
    assert_eq!(after.inputs[&keys].trim_db, 3.0);
    for (id, mix) in &after.mixes {
        assert_eq!(
            mix.inputs.get(&keys),
            before.mixes[id].inputs.get(&keys),
            "keys → {id}"
        );
    }
    assert_eq!(
        after.mixes[&MixId::new("member2")].inputs[&keys].gain_db,
        -6.0
    );
    assert_eq!(
        after.mixes[&MixId::new("engineer")].inputs[&keys].gain_db,
        -9.0
    );
}

#[tokio::test]
async fn a_backup_restores_a_muted_input_as_muted() {
    let h = EngineHarness::start();
    let (_dir, s, app) = live(&h).await;
    let eng = token("engineer", true);
    let (m3, mic1) = (MixId::new("member3"), Source::Input(InputId::new("mic1")));
    let level_mute = |muted: bool| Cmd::SetLevel {
        mix: m3.clone(),
        source: mic1.clone(),
        gain_db: Some(-6.0),
        pan: None,
        muted: Some(muted),
    };
    // Muted when the backup is taken: the program input everywhere, and one
    // level in one mix.
    for cmd in [set_input("content", 0.0, true), level_mute(true)] {
        s.engine.request_applied(cmd, None).await.unwrap();
    }
    let (status, info) = call(&app, Method::POST, "/api/backups/capture", Some(&eng), None).await;
    assert_eq!(status, StatusCode::OK, "{info}");
    let name = info["filename"].as_str().unwrap().to_string();

    // Both unmuted afterwards.
    for cmd in [set_input("content", 0.0, false), level_mute(false)] {
        s.engine.request_applied(cmd, None).await.unwrap();
    }
    let preview = format!("/api/backups/{name}/preview");
    let (status, p) = call(&app, Method::POST, &preview, Some(&eng), None).await;
    assert_eq!(status, StatusCode::OK, "{p}");
    let changes: Vec<(&str, &str, &str)> = p["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["description"].as_str().unwrap(),
                c["current_value"].as_str().unwrap(),
                c["backup_value"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        changes,
        [
            ("CONTENT: mute", "off", "on"),
            ("MEMBER1 mic → Member3", "-6.0 dB C", "-6.0 dB C muted"),
        ]
    );

    let restore = format!("/api/backups/{name}/restore");
    let (status, r) = call(&app, Method::POST, &restore, Some(&eng), None).await;
    assert_eq!(
        (status, r["restored_count"].as_u64()),
        (StatusCode::OK, Some(2))
    );
    let m = s.engine.mirror();
    assert!(m.input(&InputId::new("content")).muted, "muted again");
    let l = m.level(&m3, &mic1);
    assert_eq!((l.gain_db, l.muted), (-6.0, true));
}

/// Member `a` restores a history entry: `a`'s mix goes back to it, gains and
/// pans as they were — reaperiem#203: a pan written in the UI's 0…1 instead
/// of the engine's −1…1 moved every send right — while `b`'s mix, and every
/// other mix and input, stays exactly as it was.
async fn restore_is_isolated(a: &str, b: &str) {
    let h = EngineHarness::start();
    let (_d, s, app) = live(&h).await;
    let ta = token(a, false);
    let (keys, mic2) = (
        Source::Input(InputId::new("keys")),
        Source::Input(InputId::new("mic2")),
    );
    // Off-centre pans on both sides, so a pan in the wrong domain shows.
    for (mix, gain, pan) in [(a, -6.0, 0.25), (b, -12.0, -0.25)] {
        for cmd in [
            set_send(mix, "keys", gain, pan),
            set_send(mix, "mic2", gain - 1.0, -2.0 * pan),
        ] {
            s.engine.request_applied(cmd, None).await.unwrap();
        }
    }
    let history = format!("/api/snapshots/{a}");
    let label = Some(r#"{"label":"isolation"}"#);
    let (status, created) = call(&app, Method::POST, &history, Some(&ta), label).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let ts = created["timestamp"].as_i64().unwrap();
    let (ma, mb) = (MixId::new(a), MixId::new(b));
    let entry = s.engine.mirror().state.mixes[&ma].clone();

    // Both mixes change after the entry was taken: gain and pan.
    for (mix, gain, pan) in [(a, -20.0, -0.75), (b, -3.0, 0.75)] {
        s.engine
            .request_applied(set_send(mix, "keys", gain, pan), None)
            .await
            .unwrap();
    }
    let before = s.engine.mirror().state.clone();
    let restore = format!("/api/snapshots/{a}/{ts}/restore");
    let (status, r) = call(&app, Method::POST, &restore, Some(&ta), None).await;
    assert_eq!(status, StatusCode::OK, "{r}");
    let after = s.engine.mirror().state.clone();

    // `a`'s own mix: the changed level back at its entry, the unchanged one
    // (a round trip through the history) where it was, pans included.
    let send = |mix: &MixId, src: &Source| {
        let l = s.engine.mirror().level(mix, src);
        (l.gain_db, l.pan)
    };
    assert_eq!(send(&ma, &keys), (-6.0, 0.25), "{a}'s keys at its entry");
    assert_eq!(send(&ma, &mic2), (-7.0, -0.5), "{a}'s mic2 as it was");
    assert_eq!(after.mixes[&ma], entry, "{a}'s whole mix is its entry");
    assert_eq!(
        after.inputs, before.inputs,
        "{a} restored: inputs untouched"
    );
    assert_eq!(after.mixes.len(), before.mixes.len());
    for (id, mix) in &before.mixes {
        if id != &ma {
            assert_eq!(
                after.mixes.get(id),
                Some(mix),
                "{a} restored: {id} untouched"
            );
        }
    }
    assert_eq!(
        send(&mb, &keys),
        (-3.0, 0.75),
        "{b} keeps its own later change"
    );
    assert_eq!(send(&mb, &mic2), (-13.0, 0.5), "{b}'s mic2 untouched");
}

#[tokio::test]
async fn a_restore_leaves_other_mixes_untouched() {
    // member1 hears member2's mix (the Mixes tab): both directions.
    restore_is_isolated("member1", "member2").await;
    restore_is_isolated("member2", "member1").await;
}

#[tokio::test]
async fn a_preset_is_saved_listed_loaded_and_deleted() {
    let h = EngineHarness::start();
    let (_d, s, app) = live(&h).await;
    let m6 = token("member6", false);
    let (mix, keys) = (MixId::new("member6"), Source::Input(InputId::new("keys")));
    s.engine
        .request_applied(set_level("member6", "keys", -6.0), None)
        .await
        .unwrap();

    let body = Some(r#"{"name":" rehearsal "}"#);
    let (status, p) = call(&app, Method::POST, "/api/presets/member6", Some(&m6), body).await;
    assert_eq!(status, StatusCode::CREATED, "{p}");
    assert_eq!(p["name"], "rehearsal", "the name is trimmed");
    let stored = s.band.preset("member6", "rehearsal").unwrap().unwrap();
    let k = stored.sends.iter().find(|x| x.src == keys).unwrap();
    assert_eq!(k.gain_db, -6.0, "the page mix as it was");
    let (status, list) = call(&app, Method::GET, "/api/presets/member6", Some(&m6), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().map(Vec::len), Some(1));
    assert_eq!(list[0]["name"], "rehearsal");

    // Loading ramps the page mix back to the preset.
    s.engine
        .request_applied(set_level("member6", "keys", -20.0), None)
        .await
        .unwrap();
    let load = "/api/presets/member6/rehearsal/restore";
    let (status, r) = call(&app, Method::POST, load, Some(&m6), None).await;
    assert_eq!(
        (status, r["restored"].as_u64(), r["skipped"].as_u64()),
        (StatusCode::OK, Some(1), Some(0))
    );
    assert_eq!(s.engine.mirror().level(&mix, &keys).gain_db, -6.0);

    // Deleted once; a second time there is nothing to delete.
    let one = "/api/presets/member6/rehearsal";
    assert_eq!(
        call(&app, Method::DELETE, one, Some(&m6), None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(s.band.preset("member6", "rehearsal").unwrap(), None);
    assert_eq!(
        call(&app, Method::DELETE, one, Some(&m6), None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_history_entry_is_taken_pinned_unpinned_and_deleted() {
    let h = EngineHarness::start();
    let (_d, s, app) = live(&h).await;
    let m6 = token("member6", false);
    let label = Some(r#"{"label":"soundcheck"}"#);
    let (status, created) = call(
        &app,
        Method::POST,
        "/api/snapshots/member6",
        Some(&m6),
        label,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let ts = created["timestamp"].as_i64().unwrap();
    let (status, list) = call(&app, Method::GET, "/api/snapshots/member6", Some(&m6), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().map(Vec::len), Some(1));
    assert_eq!(
        (list[0]["timestamp"].as_i64(), &list[0]["label"]),
        (Some(ts), &serde_json::json!("soundcheck"))
    );

    // Pinned with a label, then unpinned.
    let pin = format!("/api/snapshots/member6/{ts}/pin");
    let gig = Some(r#"{"label":"gig"}"#);
    assert_eq!(
        call(&app, Method::POST, &pin, Some(&m6), gig).await.0,
        StatusCode::OK
    );
    let entry = s.band.snapshot("member6", ts).unwrap().unwrap();
    assert_eq!((entry.pinned, entry.label.as_str()), (true, "gig"));
    let unpin = format!("/api/snapshots/member6/{ts}/unpin");
    assert_eq!(
        call(&app, Method::POST, &unpin, Some(&m6), None).await.0,
        StatusCode::OK
    );
    let entry = s.band.snapshot("member6", ts).unwrap().unwrap();
    assert_eq!((entry.pinned, entry.label.as_str()), (false, "gig"));

    // An unknown entry is not found.
    let other = ts + 1;
    let pin_other = format!("/api/snapshots/member6/{other}/pin");
    assert_eq!(
        call(&app, Method::POST, &pin_other, Some(&m6), gig).await.0,
        StatusCode::NOT_FOUND
    );
    let unpin_other = format!("/api/snapshots/member6/{other}/unpin");
    assert_eq!(
        call(&app, Method::POST, &unpin_other, Some(&m6), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    // Deleted once; a second time there is nothing to delete.
    let one = format!("/api/snapshots/member6/{ts}");
    assert_eq!(
        call(&app, Method::DELETE, &one, Some(&m6), None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(s.band.snapshot("member6", ts).unwrap(), None);
    assert_eq!(
        call(&app, Method::DELETE, &one, Some(&m6), None).await.0,
        StatusCode::NOT_FOUND
    );
}
