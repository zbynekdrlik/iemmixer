use std::process::Command;

fn run_gen(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_iem-rpp-gen"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn writes_a_calibration_bundle_and_refuses_a_used_folder_or_bad_family() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("b");
    let first = run_gen(&["--out", out.to_str().unwrap(), "--only", "cal"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        String::from_utf8_lossy(&first.stdout).starts_with("2 projects, 10 cases, 13 stimuli, ")
    );
    assert!(out.join("bundle.json").is_file());
    assert!(out.join("projects/cal-96000-f64.rpp").is_file());
    let again = run_gen(&["--out", out.to_str().unwrap(), "--only", "cal"]);
    assert_eq!(again.status.code(), Some(2));
    let bad = run_gen(&[
        "--out",
        dir.path().join("c").to_str().unwrap(),
        "--only",
        "nope",
    ]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unknown family: nope"));
    assert_eq!(run_gen(&["--bogus"]).status.code(), Some(2));
}
