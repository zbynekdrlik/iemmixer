//! `iem-server roster check <file>` end to end (#58): a roster against the
//! site file `IEMMIXER_CONFIG` names, before it is installed on the PC. One
//! JSON line `{"ok","problems","sha256"}`; exit 0 when it can be installed,
//! 1 when not, 2 for bad arguments.

use std::path::Path;
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

const SITE: &str = include_str!("../../../config/test-site.toml");
const ROSTER: &str = include_str!("../../../config/test-roster.toml");

fn run(site: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_iem-server"))
        .args(args)
        .env("IEMMIXER_CONFIG", site)
        .output()
        .expect("run iem-server")
}

/// The one line `out` printed, as JSON.
fn line(out: &Output) -> serde_json::Value {
    let stdout = String::from_utf8(out.stdout.clone()).unwrap();
    assert_eq!(stdout.lines().count(), 1, "one line: {stdout}");
    serde_json::from_str(stdout.trim_end()).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `text` with `from` (found exactly once) replaced by `to`.
fn edit(text: &str, from: &str, to: &str) -> String {
    assert_eq!(text.matches(from).count(), 1, "{from}");
    text.replacen(from, to, 1)
}

#[test]
fn check_prints_one_json_line_and_exits_by_result() {
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("iemmixer.toml");
    std::fs::write(&site, SITE).unwrap();
    let roster = dir.path().join("roster.new.toml");
    let check = |text: &str| {
        std::fs::write(&roster, text).unwrap();
        let out = run(&site, &["roster", "check", roster.to_str().unwrap()]);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        (out.status.code(), line(&out), stderr)
    };

    let (code, json, stderr) = check(ROSTER);
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert_eq!(
        json,
        serde_json::json!({"ok": true, "problems": [], "sha256": hex(ROSTER.as_bytes())})
    );

    // Two main lineups: refused, the problem named.
    let two = edit(
        ROSTER,
        "name = \"Youth band\"\n",
        "name = \"Youth band\"\nmain = true\n",
    );
    let (code, json, _) = check(&two);
    assert_eq!(code, Some(1));
    assert_eq!(
        json,
        serde_json::json!({
            "ok": false,
            "problems": ["exactly one lineup must be main, not 2"],
            "sha256": hex(two.as_bytes())
        })
    );

    // Against the site: a leader whose set cannot hear another set.
    std::fs::write(
        &site,
        edit(
            SITE,
            "peers = [\"member2\", \"member3\", \"member4\"",
            "peers = [\"member2\", \"member4\"",
        ),
    )
    .unwrap();
    let (code, json, _) = check(ROSTER);
    assert_eq!(code, Some(1));
    assert_eq!(
        json["problems"],
        serde_json::json!([
            "leader member1 in main: set member1 does not hear member3; the site needs peers (F30)"
        ])
    );
    std::fs::write(&site, SITE).unwrap();

    // Before the site's directory is migrated, the migration's own rule: a
    // takes_over that names a person.
    let taker = edit(
        ROSTER,
        "name = \"Member 10\"\n",
        "name = \"Member 10\"\ntakes_over = \"member1\"\n",
    );
    let (code, json, _) = check(&taker);
    assert_eq!(code, Some(1));
    assert_eq!(
        json["problems"],
        serde_json::json!(["person #10: takes_over names a person, whose files are their own"])
    );
    std::fs::create_dir_all(dir.path().join("lineups")).unwrap();
    std::fs::write(dir.path().join("lineups").join("active.json"), "{}").unwrap();
    let (code, json, _) = check(&taker);
    assert_eq!(code, Some(0), "migrated: takes_over means nothing: {json}");
}

#[test]
fn a_file_that_cannot_be_read_is_a_problem_and_bad_arguments_are_usage() {
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("iemmixer.toml");
    std::fs::write(&site, SITE).unwrap();
    let missing = dir.path().join("nowhere.toml");
    let out = run(&site, &["roster", "check", missing.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        line(&out),
        serde_json::json!({"ok": false, "problems": ["roster file: entity not found"], "sha256": ""})
    );
    // A roster beside no site file.
    let roster = dir.path().join("roster.toml");
    std::fs::write(&roster, ROSTER).unwrap();
    let out = run(
        &dir.path().join("gone.toml"),
        &["roster", "check", roster.to_str().unwrap()],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        line(&out)["problems"],
        serde_json::json!(["site file: entity not found"])
    );
    for args in [&["roster", "check"][..], &["roster"][..]] {
        let out = run(&site, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}
