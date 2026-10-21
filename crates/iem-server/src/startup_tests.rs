//! The server's start: it refuses a bad PIN store or pepper, public IP detection, pages.

use super::*;

fn server_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        port: 0,
        config: Config::default(),
        config_dir: dir.to_path_buf(),
    }
}

#[tokio::test]
async fn start_refuses_a_plaintext_pin_store() {
    let dir = tempfile::tempdir().unwrap();
    let secrets_dir = dir.path().join(secrets::SECRETS_DIR);
    std::fs::create_dir_all(&secrets_dir).unwrap();
    std::fs::write(
        secrets_dir.join(pin_store::PIN_HASHES_FILE),
        r#"{"members":{"member1":"1357"}}"#,
    )
    .unwrap();
    let err = start_server(server_config(dir.path()), None)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("argon2id"), "{err:#}");
}

#[tokio::test]
async fn start_refuses_a_missing_pepper_next_to_pin_hashes() {
    // A new pepper would silently void every stored PIN (spec P9).
    let dir = tempfile::tempdir().unwrap();
    let secrets_dir = dir.path().join(secrets::SECRETS_DIR);
    std::fs::create_dir_all(&secrets_dir).unwrap();
    std::fs::write(
        secrets_dir.join(pin_store::PIN_HASHES_FILE),
        r#"{"engineer":"$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2hoYXNo"}"#,
    )
    .unwrap();
    let err = start_server(server_config(dir.path()), None)
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("pepper") && msg.contains("missing"), "{msg}");
    assert!(
        !secrets_dir.join(pepper::PEPPER_FILE).exists(),
        "no new pepper"
    );
}

// Linux only: on Windows the pepper file is DPAPI data and the error text differs.
#[cfg(not(windows))]
#[tokio::test]
async fn start_refuses_a_corrupt_pepper() {
    let dir = tempfile::tempdir().unwrap();
    let secrets_dir = dir.path().join(secrets::SECRETS_DIR);
    std::fs::create_dir_all(&secrets_dir).unwrap();
    std::fs::write(secrets_dir.join(pepper::PEPPER_FILE), b"short").unwrap();
    let err = start_server(server_config(dir.path()), None)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("pepper"), "{err:#}");
    assert_eq!(
        std::fs::read(secrets_dir.join(pepper::PEPPER_FILE)).unwrap(),
        b"short",
        "never replaced"
    );
}

const TEST_ROSTER: &str = include_str!("../../../config/test-roster.toml");

/// The status file the start wrote.
fn status(dir: &std::path::Path) -> roster_watch::Status {
    let text = std::fs::read_to_string(dir.join(roster_watch::STATUS_FILE)).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// #58: a roster with problems refuses the start, like an invalid site,
/// naming them, and says so in its status file.
#[tokio::test]
async fn a_bad_roster_fails_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let two_mains = TEST_ROSTER.replacen(
        "name = \"Youth band\"\n",
        "name = \"Youth band\"\nmain = true\n",
        1,
    );
    std::fs::write(dir.path().join(iem_core::roster::ROSTER_FILE), &two_mains).unwrap();
    let err = start_server(server_config(dir.path()), None)
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("roster.toml: exactly one lineup must be main, not 2"),
        "{msg}"
    );
    let s = status(dir.path());
    assert_eq!(
        (s.ok, s.applied, s.problems.as_slice()),
        (
            false,
            false,
            &["exactly one lineup must be main, not 2".to_string()][..]
        )
    );
    assert_eq!(s.sha256, roster_watch::sha256_hex(two_mains.as_bytes()));
    assert!(
        !dir.path().join(lineup_store::LINEUPS_DIR).exists(),
        "nothing migrated"
    );
    // One that does not parse refuses it too.
    std::fs::write(
        dir.path().join(iem_core::roster::ROSTER_FILE),
        "[[people]]\n",
    )
    .unwrap();
    assert!(AppState::try_new(crate::site_view::tests::test_config(), dir.path()).is_err());
}

/// #58: without `roster.toml` the server runs as it always did.
#[tokio::test]
async fn no_roster_is_legacy_mode() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::try_new(crate::site_view::tests::test_config(), dir.path()).unwrap();
    assert_eq!(state.roster(), None);
    assert!(!dir.path().join(roster_watch::STATUS_FILE).exists());
    assert!(!dir.path().join(lineup_store::LINEUPS_DIR).exists());
    assert_eq!(
        state.page("member1").unwrap().member.as_deref(),
        Some("member1")
    );
}

/// #58: a good roster starts roster mode on the main lineup, the legacy
/// layout migrated first; the next start keeps the recorded lineup.
#[tokio::test]
async fn a_roster_starts_on_the_recorded_lineup_after_the_migration() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::create_dir_all(d.join("presets")).unwrap();
    std::fs::write(d.join("presets").join("member2.json"), "member2's").unwrap();
    std::fs::write(d.join(iem_core::roster::ROSTER_FILE), TEST_ROSTER).unwrap();
    let state = AppState::try_new(crate::site_view::tests::test_config(), d).unwrap();
    let r = state.roster().expect("roster mode");
    assert!(!state.frozen_now(), "no switch to finish");
    assert_eq!(r.active, "main");
    assert_eq!(r.sha256, roster_watch::sha256_hex(TEST_ROSTER.as_bytes()));
    let moved = d
        .join("lineups")
        .join("main")
        .join("presets")
        .join("member2.json");
    assert_eq!(std::fs::read_to_string(moved).unwrap(), "member2's");
    assert!(!d.join("presets").join("member2.json").exists());
    let s = status(d);
    assert!(s.ok && s.applied, "{s:?}");
    drop(state);
    lineup_store::LineupStore::new(d)
        .set_active("youth", 1)
        .unwrap();
    let again = AppState::try_new(crate::site_view::tests::test_config(), d).unwrap();
    assert_eq!(again.roster().unwrap().active, "youth");
    // A recorded lineup the roster no longer holds refuses the start.
    lineup_store::LineupStore::new(d)
        .set_active("gone", 1)
        .unwrap();
    let err = AppState::try_new(crate::site_view::tests::test_config(), d)
        .err()
        .expect("refused");
    assert!(err.to_string().contains("no lineup 'gone'"), "{err}");
}

/// #58 review: the start checks the roster against the site's own
/// `[engine]` table, as `iem-server roster check` does: a roster the site's
/// topology cannot carry does not start, and nothing is migrated.
#[tokio::test]
async fn a_roster_the_sites_topology_cannot_carry_refuses_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join(iem_core::roster::ROSTER_FILE), TEST_ROSTER).unwrap();
    let site = include_str!("../../../config/test-site.toml").replacen(
        "peers = [\"member2\", \"member3\", \"member4\"",
        "peers = [\"member2\", \"member4\"",
        1,
    );
    let config: Config = toml::from_str(&site).unwrap();
    let err = AppState::try_new(config, d).err().expect("refused");
    assert!(
        err.to_string()
            .contains("leader member1 in main: set member1 does not hear member3"),
        "{err}"
    );
    assert!(
        !d.join(lineup_store::LINEUPS_DIR).exists(),
        "nothing migrated"
    );
    // The site as it is carries it.
    let config: Config = toml::from_str(include_str!("../../../config/test-site.toml")).unwrap();
    assert!(AppState::try_new(config, d).unwrap().roster().is_some());
}

/// #58 review: a `roster.toml` the start cannot use (or none) does not stop
/// a server that ran a roster before: it starts with the roster the states
/// follow, and the status file says why.
#[tokio::test]
async fn a_refused_roster_falls_back_to_the_one_applied_last() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let store = lineup_store::LineupStore::new(d);
    store.set_active("youth", 1).unwrap();
    store.set_applied_roster(TEST_ROSTER).unwrap();
    let two_mains = TEST_ROSTER.replacen(
        "name = \"Youth band\"\n",
        "name = \"Youth band\"\nmain = true\n",
        1,
    );
    std::fs::write(d.join(iem_core::roster::ROSTER_FILE), &two_mains).unwrap();
    let state = AppState::try_new(crate::site_view::tests::test_config(), d).unwrap();
    let r = state.roster().expect("roster mode");
    assert_eq!(
        (r.active.as_str(), r.sha256.as_str()),
        (
            "youth",
            roster_watch::sha256_hex(TEST_ROSTER.as_bytes()).as_str()
        )
    );
    let s = status(d);
    assert_eq!((s.ok, s.applied), (false, false));
    assert_eq!(s.sha256, roster_watch::sha256_hex(two_mains.as_bytes()));
    assert_eq!(
        s.problems,
        [
            "exactly one lineup must be main, not 2",
            "in use instead: the roster applied last"
        ]
    );
    drop(state);
    // No roster.toml at all: the same.
    std::fs::remove_file(d.join(iem_core::roster::ROSTER_FILE)).unwrap();
    let state = AppState::try_new(crate::site_view::tests::test_config(), d).unwrap();
    assert_eq!(
        state.roster().expect("roster mode").sha256,
        roster_watch::sha256_hex(TEST_ROSTER.as_bytes())
    );
    assert_eq!(
        status(d).problems,
        [
            "roster file: not found",
            "in use instead: the roster applied last"
        ]
    );
}

#[tokio::test]
async fn public_ip_detection_gives_up_when_no_service_answers() {
    // Every request goes through a proxy on a closed local port.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
        .build()
        .unwrap();
    assert_eq!(detect_public_ip(&client).await, None);
}

#[tokio::test]
async fn pages_resolve_from_the_config_until_the_engine_speaks() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(crate::site_view::tests::test_config(), dir.path());
    assert!(state.site().is_none());
    let p = state.page("member3").unwrap();
    assert_eq!(
        (p.mix.0.as_str(), p.member.as_deref()),
        ("member3", Some("member3"))
    );
    assert!(state.page("translator").is_none(), "needs the topology");
    assert!(state.page("ghost").is_none());
    assert_ne!(state.next_session(), state.next_session());
    assert_eq!(
        state.active_seconds(&iem_engine_proto::MixId::new("member1")),
        0.0
    );
}
