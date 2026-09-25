//! The `iem-server` command line end to end: `pin …` reads the PIN from stdin
//! and stores only an argon2id hash next to the site config; no arguments run
//! the server.

use std::io::Write;
use std::process::{Command, Output, Stdio};

const SITE: &str = "[[members]]\nname = \"Member1\"\ndante_output_l = 71\ndante_output_r = 72\n\n\
[[members]]\nname = \"Engineer\"\ndante_output_l = 91\ndante_output_r = 92\n";

fn site_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("iemmixer.toml"), SITE).unwrap();
    dir
}

fn run_pin(dir: &std::path::Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_iem-server"))
        .args(args)
        .env("IEMMIXER_CONFIG", dir.join("iemmixer.toml"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn iem-server");
    // The child may exit before reading stdin (invalid target); a broken pipe
    // here is expected in that case and the exit code is what the test checks.
    let _ = child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes());
    child.wait_with_output().expect("wait for iem-server")
}

fn stored(dir: &std::path::Path) -> serde_json::Value {
    let text = std::fs::read_to_string(dir.join("secrets").join("pin_hashes.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn set_engineer_stores_only_a_hash() {
    let dir = site_dir();
    let out = run_pin(dir.path(), &["pin", "set-engineer"], "2468\n");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stored(dir.path())["engineer"]
            .as_str()
            .unwrap()
            .starts_with("$argon2id$")
    );
    let text = std::fs::read_to_string(dir.path().join("secrets").join("pin_hashes.json")).unwrap();
    assert!(!text.contains("\"2468\""));
}

#[test]
fn set_member_stores_the_member_hash() {
    let dir = site_dir();
    let out = run_pin(dir.path(), &["pin", "set-member", "member1"], "1357\n");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stored(dir.path())["members"]["member1"]
            .as_str()
            .unwrap()
            .starts_with("$argon2id$")
    );
}

#[test]
fn an_invalid_pin_is_rejected_with_exit_2() {
    let dir = site_dir();
    let out = run_pin(dir.path(), &["pin", "set-engineer"], "12a4\n");
    assert_eq!(out.status.code(), Some(2));
    assert!(!dir.path().join("secrets").join("pin_hashes.json").exists());
}

#[test]
fn the_engineer_cannot_be_set_as_a_member() {
    let dir = site_dir();
    assert_eq!(
        run_pin(dir.path(), &["pin", "set-member", "engineer"], "2468\n")
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn an_unknown_member_is_rejected() {
    let dir = site_dir();
    assert_eq!(
        run_pin(dir.path(), &["pin", "set-member", "member9"], "2468\n")
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn an_unknown_command_prints_usage_with_exit_2() {
    let dir = site_dir();
    let out = run_pin(dir.path(), &["pin"], "");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage"));
}

#[test]
fn a_missing_site_config_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        run_pin(dir.path(), &["pin", "set-engineer"], "2468\n")
            .status
            .code(),
        Some(1)
    );
}

#[test]
fn no_arguments_run_the_server_which_needs_the_site_config() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_pin(dir.path(), &[], "");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("loading site config"), "stderr: {stderr}");
    assert!(!stderr.contains("usage"), "stderr: {stderr}");
}
