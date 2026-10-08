//! The REAPER handover's first part (#10, 2026-10-08): before its checks a
//! REAPER must run. REAPER crashes on quit routinely (`reaper_csurf.dll`)
//! and Windows Error Reporting may hold the crashed process for a while;
//! the event plan reads its facts once, so a REAPER it saw may have ended,
//! or still be ending, by the time of the handover (the first live trial's
//! unwind planned no start and then waited for a process that no longer
//! ran). The decision is `handover::ensure_reaper`; this runs it against
//! the PC. Kept out of `daemon.rs`, which is over its size budget (#36).

use super::{Guard, pref_step};
use crate::cancel::Cancel;
use crate::effects::reaper::CRASH_HOLD;
use crate::handover::{self, Ensure};
use crate::pc::{Pc, R, StepError};
use crate::plan::{Mode, OnError, Step, on_error};

/// At most this many looks at REAPER's processes: an ending REAPER waited
/// for, a start, then the look that hands over to the checks.
const LOOKS: usize = 3;

/// Makes sure a REAPER runs before the handover's checks: a REAPER that is
/// still ending is waited for (bounded, "ide event" ends the wait); with
/// none running and none started by this plan, REAPER is started through
/// the plan's own path (the preference check, then `Pc::reaper_start`,
/// which refuses with an engine or another holder, I3). Never a second
/// REAPER, never one next to a REAPER that is still ending.
pub(super) fn ensure(pc: &mut dyn Pc, g: &mut Guard, c: &Cancel) -> R<()> {
    let mut started = g
        .state
        .switching
        .as_ref()
        .is_some_and(|s| s.done.contains(&Step::ReaperStart));
    let mut waited = false;
    for _ in 0..LOOKS {
        let seen = pc.reaper_procs()?;
        match handover::ensure_reaper(seen, started, waited) {
            Ensure::Check => return Ok(()),
            Ensure::AwaitEnd => {
                g.info(format!(
                    "REAPER is still ending ({} of its processes: a crash Windows Error \
                     Reporting reports, or a quit): the handover waits up to {} s for it",
                    seen.ending,
                    CRASH_HOLD.as_secs()
                ));
                pc.reaper_await_end(c)?;
                waited = true;
            }
            Ensure::Start => {
                g.info("REAPER does not run: the handover starts it");
                start(pc, g)?;
                started = true;
            }
            Ensure::StillEnding => {
                return Err(StepError::failed(format!(
                    "REAPER is still ending {} s after the handover began to wait for it: \
                     no REAPER is started next to it",
                    CRASH_HOLD.as_secs()
                )));
            }
        }
    }
    Err(StepError::failed(format!(
        "REAPER's processes did not settle within {LOOKS} looks"
    )))
}

/// The plan's own order for a start: `PrefCheck` right before REAPER (I2:
/// it never writes under a holder, and the REAPER that just ended no longer
/// holds the card), its failure as `[guard] on_pref_fail` says, then
/// `ReaperStart`'s path.
fn start(pc: &mut dyn Pc, g: &mut Guard) -> R<()> {
    if let Err(e) = pref_step(pc, g, Mode::Event) {
        match on_error(Mode::Event, Step::PrefCheck, None, g.site.on_pref_fail) {
            OnError::Continue => g.alarm(
                Step::PrefCheck,
                &format!("{e} (before the handover's REAPER start)"),
                false,
            ),
            _ => return Err(e),
        }
    }
    pc.reaper_start()
}
