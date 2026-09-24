//! Helpers for integration tests. NOT for production use.
//! Compiled only under `#[cfg(test)]` or `--features test-helpers`.

use std::sync::Arc;
use tempfile::TempDir;

use crate::AppState;

/// Build an `AppState` whose REAPER URL points to a closed local port (127.0.0.1:1).
///
/// Any HTTP call that reaches REAPER will fail immediately with a connection-refused
/// error. Useful for testing failure paths such as the auto-snapshot cache-ordering bug.
///
/// The returned `TempDir` must be kept alive for as long as `AppState` is used
/// (dropping it deletes the temp directory).
pub async fn make_test_state_with_bad_reaper() -> (Arc<AppState>, TempDir) {
    let tmp = TempDir::new().expect("create temp dir for test state");
    let state = AppState::new_for_test("http://127.0.0.1:1".to_string(), tmp.path().to_path_buf());
    (Arc::new(state), tmp)
}
