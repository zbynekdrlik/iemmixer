//! The automatic history entry: a failed one is retried on the next change.

use super::*;
use crate::engine::client::fake;
use crate::site_view::tests::{test_config, test_topology};

#[tokio::test]
async fn a_failed_auto_snapshot_is_retried_on_the_next_change() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::new(test_config(), dir.path());
    let (engine, _peer) = fake::announced(test_topology()).await;
    state.engine = engine;
    let page = state.page("member2").unwrap();
    let taken = |state: &AppState| lock(&state.auto_snapshots).get("member2").cloned();

    // A folder where the store writes its temporary file makes the save
    // fail for every user, root included (a permission would not stop
    // root, and the test would prove nothing there).
    let blocker = dir.path().join("snapshots").join("member2.tmp");
    std::fs::create_dir_all(&blocker).unwrap();
    state.auto_snapshot(&page);
    assert!(state.band.snapshots("member2").unwrap().is_empty());
    assert_eq!(taken(&state), None, "a failed save is not marked as done");

    // The next change of the day tries again, and succeeds.
    std::fs::remove_dir(&blocker).unwrap();
    state.auto_snapshot(&page);
    let snaps = state.band.snapshots("member2").unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].label, band_store::AUTO_LABEL);
    assert_eq!(
        taken(&state),
        Some(band_store::utc_day(snaps[0].timestamp)),
        "done for the snapshot's day"
    );
    assert!(
        state.band.snapshots("member1").unwrap().is_empty(),
        "only the changed member's"
    );
}
