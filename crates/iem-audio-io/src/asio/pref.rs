//! The preference window of the S6 backend (#9 2026-09-28): the driver's
//! preferred buffer holds the engine's 32 from before the first open until
//! the card is released for good, REAPER's original at every other moment.
//! `iem_win::prefwin::Window` decides; [`PrefWindow`] holds it with its
//! registry store, and the owner enters it before every open and leaves it
//! where the card is released and no open follows.

use std::sync::Arc;

use iem_win::prefwin::{self, Pref, PrefError};
use iem_win::registry::HkcuPref;
use tracing::{error, info, warn};

use super::{AsioError, Owner, Shared, note, when};

/// The preference window on the PC's registry (S6 design note §3; #9
/// 2026-09-28): 32 written before the first open, kept (read, never written)
/// by every reopen, REAPER's original written back when the card is released
/// and no open follows. `prefwin::Window` decides; dropping a window that
/// still holds the card (the owner thread unwinds) closes it too.
pub(super) struct PrefWindow {
    store: HkcuPref,
    window: prefwin::Window,
    /// Where a close that failed in `Drop` is noted.
    shared: Arc<Shared>,
}

impl PrefWindow {
    pub(super) fn new(
        (key, name, original): &(String, String, Pref),
        frames: u32,
        shared: &Arc<Shared>,
    ) -> Self {
        Self {
            store: HkcuPref {
                key: key.clone(),
                name: name.clone(),
            },
            window: prefwin::Window::new(original.clone(), frames),
            shared: Arc::clone(shared),
        }
    }
}

impl Drop for PrefWindow {
    fn drop(&mut self) {
        // Still held only when the owner thread unwinds (a panicking driver
        // call) before a release closed the window. A failed close is noted
        // where the engine finds it: `pref_failure` on a live stream (exit
        // 3), `start`'s refusal when the first open died.
        if self.window.state() != prefwin::State::Held {
            return;
        }
        match self.window.leave(&mut self.store) {
            Ok(_) => warn!(
                "[{}] the preferred buffer is back at REAPER's {} (the owner thread ended while it held the card)",
                when(),
                self.window.original().raw
            ),
            Err(error) => {
                let text = AsioError::PrefLeave {
                    error: error.clone(),
                    after: None,
                }
                .to_string();
                error!(
                    "[{}] {text} (the owner thread ended while it held the card)",
                    when()
                );
                note(&self.shared.pref_failure, text);
                note(&self.shared.pref_leave, error);
            }
        }
    }
}

impl Owner {
    /// Before every open: the window's `enter`, logged.
    pub(super) fn enter_pref(&mut self) -> Result<(), PrefError> {
        let Some(p) = self.pref.as_mut() else {
            return Ok(());
        };
        match p.window.enter(&mut p.store) {
            Ok(prefwin::Entered::Wrote) => {
                info!(
                    "[{}] the preferred buffer holds {} (written, read back) while iemmixer holds the card",
                    when(),
                    p.window.held().raw
                );
                Ok(())
            }
            Ok(prefwin::Entered::Kept) => {
                info!(
                    "[{}] the preferred buffer still holds {} (read, not written again)",
                    when(),
                    p.window.held().raw
                );
                Ok(())
            }
            Err(e) => {
                warn!("[{}] the preference window refuses the open: {e}", when());
                Err(e)
            }
        }
    }

    /// The card is released and no open follows: REAPER's original goes
    /// back (read back), logged. A failure is logged and noted in
    /// `pref_failure` (the window stays held, so a later release writes
    /// again; the guard's `PrefCheck` restores it before REAPER starts).
    /// Nothing happens when the window holds nothing.
    pub(super) fn leave_pref(&mut self) -> Result<(), PrefError> {
        let Some(p) = self.pref.as_mut() else {
            return Ok(());
        };
        match p.window.leave(&mut p.store) {
            Ok(prefwin::Left::Restored) => {
                info!(
                    "[{}] the preferred buffer is back at REAPER's {} (written, read back): the card is released",
                    when(),
                    p.window.original().raw
                );
                Ok(())
            }
            Ok(prefwin::Left::Untouched) => Ok(()),
            Err(error) => {
                let text = AsioError::PrefLeave {
                    error: error.clone(),
                    after: None,
                }
                .to_string();
                error!(
                    "[{}] {text} at the card's release (the guard restores it before REAPER starts)",
                    when()
                );
                note(&self.shared.pref_failure, text);
                Err(error)
            }
        }
    }
}
