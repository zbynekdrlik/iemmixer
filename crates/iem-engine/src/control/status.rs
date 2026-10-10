//! What the control loop reports: meter frames in the protocol's shape,
//! and the once-a-second `Status` with HIL's peaks since the last one.

use std::sync::atomic::Ordering;

use iem_audio_io::StreamStats;
use iem_engine_proto::{HilOut, Meters, Status};

use super::Control;
use crate::SAMPLE_RATE;
use crate::rt::MeterFrame;

pub(super) fn meters_msg(f: &MeterFrame) -> Meters {
    let pair = |&[l, r]: &[f64; 2]| [l as f32, r as f32];
    Meters {
        seq: f.seq,
        inputs: f.inputs.iter().map(pair).collect(),
        mixes: f.mixes.iter().map(pair).collect(),
        groups: f.groups.iter().map(pair).collect(),
        gr_db: f.gr_db.iter().map(|g| *g as f32).collect(),
        limiter_active_s: f
            .active
            .iter()
            .map(|a| *a as f64 / f64::from(SAMPLE_RATE))
            .collect(),
        trips: f.trips,
    }
}

impl Control {
    pub(super) fn status_msg(&self, st: &StreamStats) -> Status {
        let h = self
            .driver
            .as_ref()
            .and_then(|d| d.histograms())
            .unwrap_or_default();
        Status {
            callbacks: st.callbacks,
            late: st.late,
            faulted: st.faulted,
            process_max_us: st.max_process_ns as f64 / 1000.0,
            trips: self.status.trips.load(Ordering::Relaxed),
            tap_overruns: self.status.tap_overruns.load(Ordering::Relaxed),
            talkback_dropped: self.talkback_dropped.load(Ordering::Relaxed),
            cmd_backlog: self.pending.iter().map(|g| g.len() as u64).sum(),
            frames: st.frames,
            missed: st.missed,
            overruns: st.overruns,
            resets: st.resets,
            parked: st.parked,
            held: self.held,
            lock_failed: self.driver.as_ref().is_some_and(|d| d.lock_failed()),
            hil: self
                .core
                .hil()
                .iter()
                .zip(&self.hil_peaks)
                .map(|(&tx, &peak)| HilOut {
                    tx,
                    peak: peak as f32,
                })
                .collect(),
            loopback_samples: self.status.loopback_samples.load(Ordering::Relaxed),
            interval_hist: h.interval,
            process_hist: h.process,
            hist_top_us: h.top_us,
            last_reopen_us: st.last_reopen_us,
            fault_callback_us: st.fault_callback_ns as f64 / 1000.0,
        }
    }

    /// A meter frame's peaks of HIL's spare outputs join those since the
    /// last `Status` (S6).
    pub(super) fn note_hil(&mut self, peaks: &[f64]) {
        for (held, &p) in self.hil_peaks.iter_mut().zip(peaks) {
            *held = held.max(p);
        }
    }

    /// The `Status` to broadcast now; HIL's peaks start again after it.
    pub(super) fn next_status(&mut self, st: &StreamStats) -> Status {
        let status = self.status_msg(st);
        self.hil_peaks.fill(0.0);
        status
    }
}
