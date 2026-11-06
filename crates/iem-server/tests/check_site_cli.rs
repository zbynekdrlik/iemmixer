//! `iem-server check-site` end to end: the site file `IEMMIXER_CONFIG` names,
//! loaded as the server's start loads it. A deploy runs it from the new slot
//! before it stops anything, so a key this build no longer knows stops the
//! deploy while the old build still serves.

use std::path::Path;
use std::process::Output;

const SITE: &str = include_str!("../../../config/test-site.toml");

fn check(dir: &Path, site: &str) -> Output {
    let path = dir.join("iemmixer.toml");
    std::fs::write(&path, site).unwrap();
    std::process::Command::new(env!("CARGO_BIN_EXE_iem-server"))
        .arg("check-site")
        .env("IEMMIXER_CONFIG", &path)
        .output()
        .expect("run iem-server")
}

#[test]
fn a_site_the_build_loads_exits_0_and_says_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let out = check(dir.path(), SITE);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");
    assert!(
        !dir.path().join("secrets").exists(),
        "the check makes no secrets"
    );
}

#[test]
fn a_key_the_build_no_longer_knows_exits_1_and_is_named() {
    let dir = tempfile::tempdir().unwrap();
    for (key, line) in [
        ("back_to_reaper", "back_to_reaper = [\"x\"]\n"),
        ("card", "[card]\ndriver = \"x\"\n"),
        ("activity", "[activity]\ninputs = [\"x\"]\n"),
    ] {
        let out = check(dir.path(), &format!("{line}\n{SITE}"));
        assert_eq!(out.status.code(), Some(1), "{key}: {out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(key), "{key}: {stderr}");
        assert!(stderr.contains("iemmixer.toml"), "{key}: {stderr}");
    }
}

#[test]
fn a_missing_site_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_iem-server"))
        .arg("check-site")
        .env("IEMMIXER_CONFIG", dir.path().join("absent.toml"))
        .output()
        .expect("run iem-server");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
}
