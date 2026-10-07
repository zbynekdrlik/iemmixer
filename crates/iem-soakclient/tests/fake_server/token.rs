//! The PC's own engineer token (#10 decision, 2026-10-07): with
//! `--jwt-secret-file` the client reads the server's JWT secret, signs the
//! engineer's token itself and never logs in. The build check still comes
//! first; an unreadable secret sends nothing at all. The fake takes a token
//! only when it verifies with the script's secret, as the server's
//! `extract_claims` does.

use std::path::Path;

use iem_soakclient::TOKEN_MARGIN;

use super::*;

/// `content` written as the server writes its `jwt_secret`, one line, in a
/// new temporary directory (kept alive by the caller).
fn secret_file(content: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jwt_secret");
    std::fs::write(&path, format!("{content}\n")).unwrap();
    (dir, path)
}

fn with_secret() -> Fake {
    Fake::start(Script {
        secret: Some(SECRET),
        ..Script::default()
    })
}

#[test]
fn with_the_secret_file_the_client_signs_its_own_token_and_never_logs_in() {
    let fake = with_secret();
    let (_dir, file) = secret_file(SECRET);
    let ran = run_as(
        args(&fake.origin(), true, 1),
        Credential::SecretFile(file),
        Duration::from_secs(6),
        None,
    );
    assert!(ran.summary.complete, "{:?}", ran.summary);
    // The build check first, then the sockets: no login.
    let seen = fake.seen();
    assert_eq!(seen.first().map(String::as_str), Some("/api/version"));
    assert_eq!(times(&seen, "/api/auth"), 0, "{seen:?}");
    // Both sockets, once each, with one token: the engineer's, signed with
    // the secret, valid for the run's second and the margin.
    let mut sockets: Vec<(bool, &str)> = seen.iter().filter_map(|t| socket(t)).collect();
    sockets.sort_unstable();
    let [(false, mixer), (true, listen)] = sockets[..] else {
        panic!("one mixer and one listen socket: {seen:?}");
    };
    assert_eq!(mixer, listen);
    let claims = claims(mixer, SECRET).expect("the server's reading of it");
    assert_eq!((claims.sub.as_str(), claims.engineer), ("engineer", true));
    assert_eq!(claims.exp - claims.iat, 1 + TOKEN_MARGIN);
    assert_eq!(fake.heard(), [format!("listen {STOP}")]);
}

#[test]
fn the_build_check_comes_first_with_the_secret_file_too() {
    // The LAN URL names another build: the client goes nowhere, and the
    // token is never shown to it.
    let b = Fake::start(Script {
        version: Some("fedcba9"),
        secret: Some(SECRET),
        ..Script::default()
    });
    let a = Fake::start(Script {
        lan_url: Some(format!("{}/", b.origin())),
        ..Script::default()
    });
    let (_dir, file) = secret_file(SECRET);
    let ran = run_as(
        args(&a.origin(), false, 30),
        Credential::SecretFile(file),
        Duration::from_secs(5),
        None,
    );
    assert_eq!(ran.summary.error, Some(Reason::WrongServer));
    assert_eq!(a.seen(), ["/api/site"]);
    assert_eq!(b.seen(), ["/api/version"]);
}

#[test]
fn a_token_signed_with_another_secret_is_refused_at_the_sockets() {
    // The fake really checks the signature: a token from another secret
    // opens no socket, and the run ends `server-gone` without a login.
    let fake = with_secret();
    let (_dir, file) = secret_file("another-synthetic-secret");
    let ran = run_as(
        args(&fake.origin(), true, 30),
        Credential::SecretFile(file),
        Duration::from_secs(5),
        None,
    );
    assert_eq!(ran.summary.error, Some(Reason::ServerGone));
    assert_eq!(times(&fake.seen(), "/api/auth"), 0);
}

#[test]
fn an_unreadable_secret_file_ends_the_run_before_any_request() {
    let (dir, _file) = secret_file("");
    let blank = dir.path().join("jwt_secret");
    for file in [dir.path().join("missing"), blank] {
        let fake = with_secret();
        // Not --direct: not even /api/site is read.
        let ran = run_as(
            args(&fake.origin(), false, 30),
            Credential::SecretFile(file.clone()),
            Duration::from_secs(5),
            None,
        );
        assert_eq!(
            ran.summary.error,
            Some(Reason::SecretUnreadable),
            "{file:?}"
        );
        assert!(!ran.summary.complete);
        let written: Vec<&Summary> = ran.written.iter().map(|(_, s)| s).collect();
        assert_eq!(written, [&ran.summary], "one summary, the end's");
        assert!(fake.seen().is_empty(), "{file:?}");
    }
}

#[test]
fn the_binary_takes_the_secret_file_or_the_pin_never_both_nor_neither() {
    let fake = with_secret();
    let (dir, file) = secret_file(SECRET);
    let out = dir.path().join("soakclient.json");
    let argv = |file: Option<&Path>, seconds: &str| -> Vec<String> {
        let origin = fake.origin();
        let mut argv = [
            "--base",
            origin.as_str(),
            "--direct",
            "--member",
            "member9",
            "--expect-build",
            BUILD,
            "--seconds",
            seconds,
            "--out",
            out.to_str().unwrap(),
        ]
        .map(str::to_owned)
        .to_vec();
        if let Some(file) = file {
            argv.extend([
                "--jwt-secret-file".to_owned(),
                file.to_str().unwrap().to_owned(),
            ]);
        }
        argv
    };
    let path = file.to_str().unwrap();
    // Both, and neither: 2 before any request; the error line names neither
    // the PIN nor the path.
    let both = exe(argv(Some(&file), "30"), Some(PIN));
    let neither = exe(argv(None, "30"), None);
    let first = |o: &Output| {
        String::from_utf8_lossy(&o.stderr)
            .lines()
            .next()
            .map(str::to_owned)
    };
    for refused in [&both, &neither] {
        assert_eq!(refused.status.code(), Some(2));
        let line = first(refused).unwrap();
        assert!(!line.contains(PIN) && !line.contains(path), "{line}");
    }
    assert_ne!(first(&both), first(&neither));
    assert!(fake.seen().is_empty());
    // The secret file alone: a whole run, 0, no login.
    let done = exe(argv(Some(&file), "1"), None);
    let stderr = String::from_utf8_lossy(&done.stderr);
    assert_eq!(done.status.code(), Some(0), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    assert!(
        serde_json::from_slice::<Summary>(&done.stdout)
            .unwrap()
            .complete
    );
    assert_eq!(times(&fake.seen(), "/api/auth"), 0);
    // A missing secret file: 1, the code alone on stderr, its summary in the
    // file and on stdout, nothing requested, neither the path nor a secret
    // printed.
    let missing = dir.path().join("missing");
    let unread = exe(argv(Some(&missing), "30"), None);
    assert_eq!(unread.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&unread.stderr);
    assert_eq!(stderr.trim(), "iem-soakclient: secret-unreadable");
    let stdout = String::from_utf8_lossy(&unread.stdout);
    let summary: Summary = serde_json::from_str(&stdout).unwrap();
    assert_eq!(summary.error, Some(Reason::SecretUnreadable));
    let file_summary: Summary = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(file_summary, summary);
    for shown in [&stdout, &stderr] {
        assert!(
            !shown.contains("missing") && !shown.contains(SECRET),
            "{shown}"
        );
    }
    assert!(fake.seen().is_empty());
}
