//! [`AsioStream`], the engine's handle on the S6 backend: it starts the
//! owner thread and waits for the first open, reads the stream's
//! statistics and histograms, asks for a reopen, locks the buffers and
//! stops the stream.

use core::marker::PhantomData;
use core::ptr;
use core::sync::atomic::Ordering;
use std::io;
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use super::owner_thread::owner_main;
use super::{
    AsioError, BUSY, CardConfig, OPEN_BOUND, SEH, SEH_HOLD, SEH_PARKED, Shared, Start, TICK,
};
use crate::hist::{HistSnapshot, StreamHists};
use crate::owner::{self, StopOutcome};
use crate::{Process, StreamStats, format, telemetry};

/// The S6 backend (S6 design note §3): the card at the configured buffer,
/// driven by an owner thread that makes every driver call, pumps its
/// messages and watches the stream. The engine's inputs are the topology's
/// RX card channels and its outputs the TX ones, then HIL's spare outputs
/// under the test-signal flag (S6); every other card output is zero on every
/// callback (A1).
pub struct AsioStream<P: Process + 'static> {
    thread: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
    /// The stream histograms (S7), counted since `start` across reopens.
    hists: Arc<StreamHists>,
    processor: PhantomData<fn(P)>,
}

impl<P: Process + 'static> AsioStream<P> {
    /// Opens the card and starts the owner thread; returns once the period
    /// was measured from the first callbacks, or with the open's refusal.
    /// `rx`/`tx` are the engine's card channels (numbered from 1): the
    /// topology's, and after its TX HIL's spare outputs.
    ///
    /// The first open checks the driver module's holders (I3), opens the
    /// preference window (32 from before the first open until the card is
    /// released for good, kept by every reopen, then REAPER's original: #9
    /// 2026-09-28), admits the driver (96 kHz, preferred buffer = `frames`,
    /// one sample type) and maps the channels.
    pub fn start(
        card: CardConfig,
        rx: Vec<u16>,
        tx: Vec<u16>,
        hil_rx: Vec<u16>,
        processor: P,
    ) -> Result<Self, AsioError> {
        let frames = owner::frames(card.frames).ok_or(AsioError::Frames(card.frames))?;
        // One stream per process (the spike host's claim too).
        if BUSY
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(AsioError::Busy);
        }
        let shared = Arc::new(Shared::default());
        // Allocated here, before any stream exists (I7); every open's backend
        // records into the same arrays.
        let hists = Arc::new(StreamHists::new(telemetry::period_ns(frames, format::RATE)));
        let (ready, answer) = mpsc::sync_channel(1);
        let start = Start {
            card,
            frames,
            rx,
            tx,
            hil_rx,
            processor: Box::new(processor),
            shared: Arc::clone(&shared),
            hists: Arc::clone(&hists),
        };
        let spawned = thread::Builder::new()
            .name("iem-asio-owner".into())
            .spawn(move || owner_main(start, ready));
        let thread = match spawned {
            Ok(t) => t,
            Err(e) => {
                BUSY.store(false, Ordering::SeqCst);
                return Err(AsioError::Thread(e.to_string()));
            }
        };
        match answer.recv_timeout(OPEN_BOUND) {
            Ok(Ok(())) => Ok(Self {
                thread: Some(thread),
                shared,
                hists,
                processor: PhantomData,
            }),
            Ok(Err(e)) => {
                // A parked open keeps its thread; a released one has ended.
                if shared.outcome() == Some(StopOutcome::Released) {
                    let _ = thread.join();
                }
                Err(e)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // A driver call hangs. The owner thread stops and releases
                // whatever it opened once the call returns (and frees the
                // slot); the engine meanwhile reports the refusal.
                shared.stop.store(true, Ordering::SeqCst);
                Err(AsioError::Thread(format!(
                    "the card did not open within {} s",
                    OPEN_BOUND.as_secs()
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
                let ended =
                    AsioError::Thread("the owner thread ended before the stream opened".to_owned());
                // A driver call panicked while the preference window was
                // held and its drop could not close it: the refusal says so.
                Err(
                    match shared.pref_leave.lock().ok().and_then(|p| p.clone()) {
                        Some(error) => AsioError::PrefLeave {
                            error,
                            after: Some(Box::new(ended)),
                        },
                        None => ended,
                    },
                )
            }
        }
    }

    /// The stream's statistics: the measured period, counters summed over
    /// reopens (`max_process_ns` is the longest callback: decode, process
    /// and encode), the reopens, a parked stream and a fault.
    pub fn stats(&self) -> StreamStats {
        let s = &self.shared;
        StreamStats {
            frames: s.frames.load(Ordering::Acquire),
            callbacks: s.callbacks.load(Ordering::Acquire),
            late: s.late.load(Ordering::Acquire),
            missed: s.missed.load(Ordering::Acquire),
            overruns: s.overruns.load(Ordering::Acquire),
            resets: s.resets.load(Ordering::Acquire),
            parked: s.parked.load(Ordering::Acquire) || SEH_PARKED.load(Ordering::SeqCst),
            // `SEH` first, as in the owner's tick: the hold precedes it.
            faulted: s.faulted.load(Ordering::Acquire)
                || owner::seh_faults(SEH.load(Ordering::SeqCst), SEH_HOLD.load(Ordering::SeqCst)),
            running: s.running.load(Ordering::Acquire),
            max_process_ns: s.max_ns.load(Ordering::Acquire),
            fault: s.fault.lock().ok().and_then(|f| f.clone()),
            last_reopen_us: s.reopen_us.load(Ordering::Acquire),
            // Read after `faulted`, which the owner sets after it.
            fault_callback_ns: s.fault_ns.load(Ordering::Acquire),
        }
    }

    /// The stream histograms (S7) since `start`, the reopens included: every
    /// interval telemetry judged (never an open's warm-up) and every
    /// callback's own time (decode, process and encode).
    pub fn histograms(&self) -> HistSnapshot {
        self.hists.snapshot()
    }

    /// `WM_ENDSESSION` reached the owner thread: the engine saves, fades out
    /// and stops the stream (the owner thread releases the driver then).
    pub fn session_ending(&self) -> bool {
        self.shared.session_end.load(Ordering::SeqCst)
    }

    /// Asks for one reopen (HIL, a dev-only flag); it goes through the
    /// reopen budget like a driver's request.
    pub fn force_reopen(&self) {
        self.shared.reopen.store(true, Ordering::SeqCst);
    }

    /// `VirtualLock`s the preallocated engine buffers (after the engine
    /// raised its minimum working set, S1c hand-off). A reopen keeps them.
    pub fn lock_buffers(&self) -> io::Result<()> {
        let ranges = self
            .shared
            .ranges
            .lock()
            .map_err(|_| io::Error::other("the buffer ranges are poisoned"))?;
        for &(addr, len) in ranges.iter() {
            if len > 0 {
                iem_win::power::virtual_lock(ptr::with_exposed_provenance::<u8>(addr), len)?;
            }
        }
        Ok(())
    }

    /// A release could not close the preference window (after a failed
    /// reopen, or the owner thread unwound): the preferred buffer may not
    /// hold REAPER's original (the engine alarms and exits 3 unless the
    /// stream faulted; the guard restores it before REAPER starts).
    pub fn pref_failure(&self) -> Option<String> {
        self.shared.pref_failure.lock().ok().and_then(|f| f.clone())
    }

    /// Stops the stream: the owner thread stops the driver, waits for the
    /// last callback (bounded), disposes and releases (`Released`), or leaves
    /// a stuck stream allocated (`Parked`).
    pub fn stop(mut self) -> StopOutcome {
        self.halt()
    }

    fn halt(&mut self) -> StopOutcome {
        self.shared.stop.store(true, Ordering::SeqCst);
        let started = Instant::now();
        let outcome = loop {
            let ended = self.thread.as_ref().is_none_or(JoinHandle::is_finished);
            if ended && let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            if let Some(o) = owner::stop_step(self.shared.outcome(), ended, started.elapsed()) {
                break o;
            }
            thread::sleep(TICK);
        };
        match outcome {
            StopOutcome::Released => {
                if let Some(t) = self.thread.take() {
                    let _ = t.join();
                }
            }
            StopOutcome::Parked => {
                // The owner thread keeps pumping for the stuck stream.
                self.shared.parked.store(true, Ordering::SeqCst);
                drop(self.thread.take());
            }
        }
        outcome
    }
}

impl<P: Process + 'static> Drop for AsioStream<P> {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.halt();
        }
    }
}
