//! Defensive hardening regression test: auto-snapshot cache flag must be set
//! AFTER a successful save, not before. A disk-write failure should leave the
//! day un-claimed so the next channel-change tick can retry.
//!
//! Currently (pre-T8): `try_persist_auto_snapshot` marks the date in
//! `mixer_cache.snapshot_last_date` **before** calling `save_snapshot`.
//! If the save fails the day stays "claimed" → this test FAILS RED.
//!
//! After T8: ordering is reversed → test passes GREEN.

// The failure path is driven by Unix file permissions (mode 000); the Linux
// `test` job runs this file, the Windows clippy job must not compile it.
#![cfg(unix)]

use std::collections::HashMap;

use iem_server::poller::try_persist_auto_snapshot;
use iem_server::test_helpers::make_test_state_with_bad_reaper;

/// Helper: make the snapshots subdirectory read-only so `save_snapshot` will
/// fail with a permission error, exercising the failure path.
fn lock_snapshots_dir(data_dir: &std::path::Path) {
    let snapshots_dir = data_dir.join("snapshots");
    // Create the dir first so `create_dir_all` in save() doesn't short-circuit
    // the error by silently succeeding before hitting the write step.
    std::fs::create_dir_all(&snapshots_dir).expect("create snapshots dir");

    // Remove all permissions so any write attempt returns EACCES.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&snapshots_dir, std::fs::Permissions::from_mode(0o000))
        .expect("set snapshots dir permissions to 000");
}

/// Restore permissions so TempDir cleanup can remove the directory.
fn unlock_snapshots_dir(data_dir: &std::path::Path) {
    let snapshots_dir = data_dir.join("snapshots");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&snapshots_dir, std::fs::Permissions::from_mode(0o755));
}

#[tokio::test]
async fn snapshot_cache_not_marked_on_save_failure() {
    // Skip if running as root (root ignores filesystem permissions).
    // This test is about filesystem permission enforcement, not root bypass.
    // Detect root by checking if we can write to /etc/shadow (root-only).
    let running_as_root = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false);
    if running_as_root {
        eprintln!("SKIP: running as root, filesystem permission test is meaningless");
        return;
    }

    let (state, tmp) = make_test_state_with_bad_reaper().await;
    let data_dir = tmp.path().to_path_buf();

    // Make the snapshots directory unwritable so save_snapshot returns an error.
    lock_snapshots_dir(&data_dir);

    let member_id = "member5";
    let today = "2026-04-19";
    let channels: HashMap<usize, iem_core::ChannelSnapshot> = HashMap::new();
    // No track indices → no EQ reads attempted; failure will come from save_snapshot.
    let track_indices: Vec<usize> = vec![];

    let result = try_persist_auto_snapshot(&state, member_id, today, channels, track_indices).await;

    // The save should have failed (permission denied).
    assert!(
        result.is_err(),
        "expected save_snapshot to fail on read-only directory, got Ok"
    );

    // Restore permissions before assertions so TempDir cleanup works even on panic.
    unlock_snapshots_dir(&data_dir);

    // Critical assertion: cache flag must NOT be set when save failed.
    //
    // This assertion FAILS (RED) with the current (pre-T8) code because
    // `try_persist_auto_snapshot` inserts the date into the cache BEFORE
    // attempting the save.
    //
    // After T8 reverses the ordering, the flag is only set on success → GREEN.
    let cache = state.mixer_cache.read().await;
    assert_eq!(
        cache.snapshot_last_date.get(member_id),
        None,
        "DEFENSIVE HARDENING REGRESSION: cache flag was set despite save failure. \
         T8 must reverse the ordering so the flag is only set after a successful save."
    );
}
