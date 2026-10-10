//! The S6 stream's real-time side: the callbacks azo hands to the driver
//! (`BACKEND_CALLBACKS`), which reach the open [`Backend`] through the
//! `BACKEND` slot while `BACKEND_IN_FLIGHT` counts them, and the stream's
//! work in each callback. I7: the callbacks allocate, lock, log and make
//! syscalls never, apart from the driver's own `getSamplePosition` and
//! `outputReady`; the message handlers, which may run on any thread, only
//! count.

use core::ffi::{c_long, c_void};
use core::sync::atomic::Ordering;
use std::panic::{AssertUnwindSafe, catch_unwind};

use azo::sys::{Bool, Callbacks, MessageSelector, SampleRate, Time, TimeInfoFlags};

use super::{
    BACKEND, BACKEND_IN_FLIGHT, Backend, Carry, DEPTH, MESSAGES, NO_POSITION, clock_ns, half,
    nanos, read, zero,
};
use crate::{Block, rtpanic, telemetry};

pub(super) static BACKEND_CALLBACKS: Callbacks = Callbacks {
    buffer_switch: backend_buffer_switch,
    sample_rate_did_change: backend_rate_change,
    asio_message: backend_message,
    buffer_switch_time_info: backend_buffer_switch_time_info,
};

/// Runs `f` on the backend's live stream, if any, keeping it alive meanwhile.
fn with_backend<R>(f: impl FnOnce(&Backend) -> R) -> Option<R> {
    BACKEND_IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
    DEPTH.with(|d| d.set(d.get() + 1));
    let raw = BACKEND.load(Ordering::SeqCst);
    // SAFETY: a non-null slot points to a live stream: the owner thread
    // clears the slot and waits for BACKEND_IN_FLIGHT == 0 before it frees
    // the stream, and this callback counted itself before it read the slot.
    let out = unsafe { raw.as_ref() }.map(f);
    DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    BACKEND_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    out
}

unsafe extern "system" fn backend_buffer_switch(index: c_long, _direct: Bool) {
    with_backend(|b| b.on_buffer(index != 0, None));
}

unsafe extern "system" fn backend_buffer_switch_time_info(
    params: *mut Time,
    index: c_long,
    _direct: Bool,
) -> *mut Time {
    // SAFETY: the driver passes a valid Time for this call, or null.
    let position = unsafe { params.as_ref() }
        .filter(|t| {
            t.time_info
                .flags
                .contains(TimeInfoFlags::SAMPLE_POSITION_VALID)
        })
        .map(|t| i64::from(t.time_info.sample_position));
    with_backend(|b| b.on_buffer(index != 0, position));
    params
}

unsafe extern "system" fn backend_message(
    sel: MessageSelector,
    value: c_long,
    _message: *const c_void,
    _opt: *const f64,
) -> c_long {
    // Any thread (azo passes our callbacks to the driver as they are):
    // counted in atomics for the owner thread's log, also while no stream
    // exists (asked while createBuffers runs, or between a stream's finish
    // and the next open), where the stream's telemetry cannot count.
    MESSAGES.message(sel.0, value, clock_ns());
    with_backend(|b| b.telemetry.driver_message(sel.0, value))
        .unwrap_or_else(|| telemetry::reply(sel.0, value))
}

unsafe extern "system" fn backend_rate_change(rate: SampleRate) {
    MESSAGES.rate_change(rate, clock_ns());
    with_backend(|b| b.telemetry.on_rate_change());
}

impl Backend {
    fn on_buffer(&self, second: bool, position: Option<i64>) {
        // Every entry: after a reopen the driver may call back on a new thread.
        rtpanic::mark_rt_thread();
        let entry = self.base.elapsed();
        let position = position.or_else(|| {
            // SAFETY: the driver outlives the stream: the owner thread frees
            // the stream before it drops the host.
            unsafe { self.driver.as_ref() }
                .and_then(|d| d.sample_position().ok())
                .map(|p| p.position)
        });
        // The interval telemetry judged: never the warm-up after an open, so
        // a reopen's gap stays out of the histogram.
        if let Some(dt) = self.telemetry.on_callback(nanos(entry).max(1), position) {
            self.hists.interval.record(dt);
        }
        self.record(position);
        for ch in &self.outputs {
            zero(half(ch, second), self.bytes);
        }
        let panicked = !self.faulted.load(Ordering::Acquire) && self.render(second);
        if self.output_ready.load(Ordering::Relaxed) {
            // SAFETY: as above.
            let ok = unsafe { self.driver.as_ref() }.is_some_and(|d| d.output_ready().is_ok());
            if !ok {
                // ASE_NotPresent: the driver does not need the signal.
                self.output_ready.store(false, Ordering::Relaxed);
            }
        }
        let took = nanos(self.base.elapsed().saturating_sub(entry));
        self.telemetry.on_done(took);
        self.hists.process.record(took);
        if panicked {
            // The faulting callback's own time, then the fault: two stores on
            // the fault path only (I7). The outputs stayed zero.
            self.fault_ns.store(took.max(1), Ordering::Relaxed);
            self.faulted.store(true, Ordering::Release);
        }
    }

    /// The first callbacks' positions, for the owner thread (single writer:
    /// callbacks never overlap).
    fn record(&self, position: Option<i64>) {
        let n = self.ring_len.load(Ordering::Relaxed);
        if let Some(slot) = self.ring.get(n) {
            slot.store(position.unwrap_or(NO_POSITION), Ordering::Relaxed);
            self.ring_len.store(n + 1, Ordering::Release);
        }
    }

    /// Decode the engine's inputs, process, encode its outputs (the card's
    /// outputs are already zero). True when the processor panicked: the
    /// outputs stay zero and the caller marks the stream faulted.
    fn render(&self, second: bool) -> bool {
        // SAFETY: see `carry`: only this callback touches it now.
        let Carry {
            processor,
            inbuf,
            outbuf,
        } = unsafe { &mut *self.carry.get() };
        for (dst, &card) in inbuf.chunks_exact_mut(self.frames).zip(&self.rx) {
            match self.inputs.get(card) {
                Some(ch) => {
                    self.format.decode(read(half(ch, second), self.bytes), dst);
                }
                None => dst.fill(0.0),
            }
        }
        outbuf.fill(0.0);
        let discontinuity = self.discontinuity.swap(false, Ordering::AcqRel);
        let caught = catch_unwind(AssertUnwindSafe(|| {
            if discontinuity {
                processor.discontinuity();
            }
            let mut block = Block::new(self.frames, inbuf.as_slice(), outbuf.as_mut_slice());
            processor.process(&mut block);
        }));
        match caught {
            Ok(()) => {
                for (src, &card) in outbuf.chunks_exact(self.frames).zip(&self.tx) {
                    if let Some(ch) = self.outputs.get(card) {
                        self.format
                            .encode(src, writable(half(ch, second), self.bytes));
                    }
                }
                false
            }
            Err(payload) => {
                // The outputs stay zero. The panic hook recorded the place in
                // atomics; the payload is never freed on this thread.
                core::mem::forget(payload);
                true
            }
        }
    }

    /// The positions recorded so far, for the owner thread.
    pub(super) fn positions(&self) -> Vec<Option<i64>> {
        let n = self.ring_len.load(Ordering::Acquire);
        self.ring
            .iter()
            .take(n)
            .map(|p| Some(p.load(Ordering::Relaxed)).filter(|v| *v != NO_POSITION))
            .collect()
    }
}

fn writable<'a>(p: *mut c_void, bytes: usize) -> &'a mut [u8] {
    if p.is_null() {
        return Default::default();
    }
    // SAFETY: as in `zero`; the driver does not touch this half while the
    // host is inside the callback for it, and no other reference to it lives.
    unsafe { core::slice::from_raw_parts_mut(p.cast::<u8>(), bytes) }
}
