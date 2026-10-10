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
