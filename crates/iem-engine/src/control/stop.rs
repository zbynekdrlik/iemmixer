//! How a run ends (design note §3.7; S6 §3, §4): the shutdown with its
//! fade, the fault, the refused card, and the stream's last word.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use iem_audio_io::owner::StopOutcome;
use iem_engine_proto::{AlarmCode, EngineMsg};
use tracing::{error, info};

use super::{Control, Exit, FADE_WAIT};
use crate::cmd::RtOp;

/// What the engine says about its stream as it stops (#35): `DriverReleased`
/// once the card is free; a stream that stayed parked (a callback stuck in
/// it, or the parked-engine test's hold) released nothing, so
/// `DriverParked`: the card is free only once the process has ended.
fn stream_end(outcome: StopOutcome, reason: &str) -> EngineMsg {
    let reason = reason.to_owned();
    match outcome {
        StopOutcome::Released => {
            info!("driver released: {reason}");
            EngineMsg::DriverReleased { reason }
        }
        StopOutcome::Parked => {
            error!(
                "the stream stayed parked ({reason}): the driver is not released, \
                 the card is free once this process has ended"
            );
            EngineMsg::DriverParked { reason }
        }
    }
}

impl Control {
    /// Stops the backend and says how its stream ended, as the last word
    /// before every connection closes.
    fn release(&mut self, reason: &str) {
        let outcome = self
            .driver
            .take()
            .map_or(StopOutcome::Released, |d| d.stop());
        self.broadcast(&stream_end(outcome, reason));
        for (_, p) in std::mem::take(&mut self.peers) {
            p.conn.close();
        }
    }

    pub(super) fn shutdown_now(&mut self) -> Exit {
        info!("shutdown: saving, fading out, releasing the driver");
        self.save();
        let faded = self.fade_out();
        self.release("shutdown");
        Exit::Shutdown { faded }
    }

    /// Asks the callback to fade out and waits for it, at most `FADE_WAIT`;
    /// whether it faded.
    pub(super) fn fade_out(&mut self) -> bool {
        self.pending.push_back(vec![RtOp::FadeOut]);
        let t0 = Instant::now();
        while !self.status.faded_out.load(Ordering::Acquire) && t0.elapsed() < FADE_WAIT {
            self.flush_rt();
            std::thread::sleep(Duration::from_millis(5));
        }
        self.status.faded_out.load(Ordering::Acquire)
    }

    pub(super) fn fault(&mut self, why: String) -> Exit {
        error!("the RT callback faulted: {why}");
        self.save();
        self.alarm(AlarmCode::Fault, why.clone());
        self.release("fault");
        Exit::Fault(why)
    }

    /// The backend refused the card: save, stop without a fade (the card may
    /// be in a wrong state), exit 3 — the guard never respawns after it.
    pub(super) fn card(&mut self, why: String) -> Exit {
        error!("the card is refused: {why}");
        self.save();
        self.release("card refused");
        Exit::Card(why)
    }
}
