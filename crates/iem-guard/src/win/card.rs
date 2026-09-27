//! The card (design §3, §5.2): the preferred buffer back at REAPER's
//! original before REAPER starts, and the driver module's other holders.

use std::time::Duration;

use iem_win::prefwin;
use iem_win::process;
use iem_win::registry::HkcuPref;

use super::WinPc;
use super::procs;
use crate::cancel::Cancel;
use crate::effects::app::holders_text;
use crate::pc::{R, StepError, foreign_holders};

/// Restores the original preferred buffer: up to three writes, each read
/// back; the writes it took.
pub(super) fn pref_check(pc: &WinPc) -> R<u32> {
    let card = &pc.s.card;
    let mut store = HkcuPref {
        key: card.pref_key.clone(),
        name: card.pref_name.clone(),
    };
    prefwin::restore(&mut store, &card.pref_original.pref(), 3)
        .map_err(|e| StepError::Failed(e.to_string()))
}

/// ≤ 30 s for every holder of the driver module but REAPER to leave (e.g.
/// a spike window; `iempc event` pre-empts those first).
pub(super) fn holder_gone(pc: &WinPc, c: &Cancel) -> R<()> {
    let mut left = Vec::new();
    let gone = procs::poll(Duration::from_secs(30), c, || {
        let all = process::module_holders(&pc.s.card.module)
            .map_err(|e| procs::failed("the driver module's holders", e))?;
        left = foreign_holders(&all, &procs::list(pc).reaper);
        Ok(left.is_empty())
    })?;
    if gone {
        Ok(())
    } else {
        Err(StepError::failed(format!(
            "the driver module is still held by {} after 30 s",
            holders_text(&left)
        )))
    }
}
