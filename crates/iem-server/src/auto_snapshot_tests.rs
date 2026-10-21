//! The automatic history entry: a failed one is retried on the next change.

use super::*;
use crate::engine::client::fake;
use crate::site_view::tests::{test_config, test_topology};

/// #58: a person who plays in two bands the same day gets the day's
/// automatic history entry in each band's history.
#[tokio::test]
async fn a_person_in_two_bands_gets_a_daily_snapshot_in_each() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::new(test_config(), dir.path());
    let (engine, _peer) = fake::announced(test_topology()).await;
    state.engine = engine;
    for lineup in ["main", "youth"] {
        state.set_roster(Some(crate::roster_state::tests::test_roster(lineup)));
        let page = state.page("member3").unwrap();
        state.auto_snapshot(&page);
        state.auto_snapshot(&page);
    }
    for lineup in ["main", "youth"] {
        let snaps = state.band_of(Some(lineup)).snapshots("member3").unwrap();
        assert_eq!(snaps.len(), 1, "{lineup}");
        assert_eq!(snaps[0].label, band_store::AUTO_LABEL);
    }
    let mut keys: Vec<String> = lock(&state.auto_snapshots).keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["main/member3", "youth/member3"]);
    assert_eq!(snapshot_key(None, "member3"), "member3", "legacy mode");
}

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
    assert!(state.band().snapshots("member2").unwrap().is_empty());
    assert_eq!(taken(&state), None, "a failed save is not marked as done");

    // The next change of the day tries again, and succeeds.
    std::fs::remove_dir(&blocker).unwrap();
    state.auto_snapshot(&page);
    let snaps = state.band().snapshots("member2").unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].label, band_store::AUTO_LABEL);
    assert_eq!(
        taken(&state),
        Some(band_store::utc_day(snaps[0].timestamp)),
        "done for the snapshot's day"
    );
    assert!(
        state.band().snapshots("member1").unwrap().is_empty(),
        "only the changed member's"
    );
}
