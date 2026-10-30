//! `iem-server cloudflare set-token` end to end (#91): the token is read from
//! stdin and stored sealed for this user next to the site file (Windows,
//! DPAPI); a platform without a sealed store refuses and stores nothing.

use std::io::Write;
use std::process::{Command, Output, Stdio};

fn site_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("iemmixer.toml"), "").unwrap();
    dir
}

fn run(dir: &std::path::Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_iem-server"))
        .args(args)
        .env("IEMMIXER_CONFIG", dir.join("iemmixer.toml"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn iem-server");
    // The child may exit before reading stdin; the exit code is what counts.
    let _ = child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes());
    child.wait_with_output().expect("wait for iem-server")
}

/// A low-entropy token-shaped value (the secret scan stays quiet).
fn token() -> String {
    "t".repeat(24)
}

#[test]
fn a_cloudflare_command_without_set_token_is_a_usage_error() {
    let dir = site_dir();
    for args in [&["cloudflare"][..], &["cloudflare", "set-token", "x"]] {
        let out = run(dir.path(), args, "");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("cloudflare set-token"), "{err}");
    }
    assert!(!dir.path().join("secrets").exists());
}

#[cfg(not(windows))]
#[test]
fn without_a_sealed_store_set_token_refuses_and_stores_nothing() {
    let dir = site_dir();
    let out = run(
        dir.path(),
        &["cloudflare", "set-token"],
        &format!("{}\n", token()),
    );
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no sealed store on this platform"), "{err}");
    assert!(!err.contains(&token()), "never the token");
    assert!(!dir.path().join("secrets").exists());
}

#[cfg(windows)]
#[test]
fn set_token_stores_the_token_sealed() {
    let dir = site_dir();
    let out = run(
        dir.path(),
        &["cloudflare", "set-token"],
        &format!("{}\n", token()),
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stored = std::fs::read(dir.path().join("secrets").join("cloudflare_token.dpapi")).unwrap();
    assert_ne!(stored, token().into_bytes(), "sealed, not plain");
    let bad = run(dir.path(), &["cloudflare", "set-token"], "not a token\n");
    assert_eq!(bad.status.code(), Some(2));
}
