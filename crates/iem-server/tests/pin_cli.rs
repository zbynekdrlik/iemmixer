//! The `iem-server` command line end to end: `pin …` reads the PIN from stdin
//! and stores only an argon2id hash next to the site config; no arguments run
//! the server.

use std::io::Write;
use std::process::{Command, Output, Stdio};

const SITE: &str = "[[members]]\nid = \"member1\"\nname = \"Member1\"\nmix = \"member1\"\n\n\
[[members]]\nid = \"engineer\"\nname = \"Engineer\"\nmix = \"engineer\"\n";

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

/// The engineer's push store as the server leaves it (its one-time cleanup
/// marker and the list), holding `endpoints`.
fn engineer_subscriptions(dir: &std::path::Path, endpoints: &[&str]) {
    std::fs::write(dir.join("push_subs_v2_migrated"), "").unwrap();
    let subs: Vec<serde_json::Value> = endpoints
        .iter()
        .map(|e| serde_json::json!({"endpoint": e, "p256dh": "k", "auth": "a"}))
        .collect();
    std::fs::write(
        dir.join("push_subscriptions.json"),
        serde_json::to_string(&subs).unwrap(),
    )
    .unwrap();
}

#[test]
fn notify_without_a_subscription_exits_3() {
    let dir = site_dir();
    let out = run_pin(
        dir.path(),
        &["notify", "--to", "alarm", "Strážca", "test"],
        "",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "stderr: {stderr}");
    assert!(
        stderr.contains("no device took the notice (Alarm)"),
        "stderr: {stderr}"
    );
}

/// The guard's alarms go to the engineer's subscriptions (#9 2026-09-28):
/// with one, the notice gets as far as the VAPID key, which a notice never
/// creates (exit 1, no secret made).
#[test]
fn an_alarm_goes_to_the_engineers_subscriptions() {
    let dir = site_dir();
    engineer_subscriptions(dir.path(), &["https://push.example.org/engineer"]);
    let out = run_pin(
        dir.path(),
        &["notify", "--to", "alarm", "Strážca", "test"],
        "",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("vapid"), "stderr: {stderr}");
    assert!(!dir.path().join("secrets").exists());
}

#[test]
fn notify_knows_only_alarms() {
    let dir = site_dir();
    for args in [
        &["notify", "only a title"][..],
        &["notify", "Kapela hrá", "test"][..],
        &["notify", "--to", "engineer", "T", "B"][..],
        &["notify", "--to", "band-activity", "T", "B"][..],
        &["notify", "--to", "alarm", "T"][..],
        &["notify", "--count", "band-activity"][..],
        &["notify", "--count"][..],
        &["alarm-link"][..],
    ] {
        assert_eq!(
            run_pin(dir.path(), args, "").status.code(),
            Some(2),
            "{args:?}"
        );
    }
}

#[test]
fn notify_count_prints_the_engineers_subscriptions() {
    let dir = site_dir();
    let count = |dir: &std::path::Path| {
        let out = run_pin(dir, &["notify", "--count", "alarm"], "");
        let stdout = String::from_utf8(out.stdout).unwrap();
        (out.status.code(), stdout)
    };
    assert_eq!(count(dir.path()), (Some(0), "0\n".to_string()));
    engineer_subscriptions(
        dir.path(),
        &["https://push.example.org/1", "https://push.example.org/2"],
    );
    assert_eq!(count(dir.path()), (Some(0), "2\n".to_string()));
    std::fs::write(dir.path().join("push_subscriptions.json"), "[oops").unwrap();
    assert_eq!(count(dir.path()), (Some(1), String::new()));
}
