//! The backend's owner thread (S6 design note §3): it makes every driver
//! call and pumps the thread's messages. It runs each open (I3 module
//! holders, the preference window, `createBuffers`, `start`, the measured
//! period), the watch and reopen through the reset budget, the release for
//! good, the parked-engine test's hold and the session end's release. Its
//! decisions live in the portable, mutation-tested `crate::owner`.

use core::cell::{RefCell, UnsafeCell};
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::{Duration, Instant};

use azo::dto::ChannelId;
use iem_win::window::SessionEndWindow;
use tracing::{error, info, warn};

use super::backend::BACKEND_CALLBACKS;
use super::{
    AsioError, BACKEND, BACKEND_IN_FLIGHT, BASE, BUSY, Backend, Card, Carry, Live, MESSAGES,
    NO_POSITION, Owner, PrefWindow, SEH, SEH_HOLD, SESSION_END_REASON, STOP_WAIT, Shared, Start,
    TICK, note, pump_messages, when, zero,
};
use crate::channels::ChannelMap;
use crate::format::{self, SampleFormat};
use crate::messages::TOPICS;
use crate::owner::{self, Asked, OpenPeriod, SehRelease, StopOutcome, Then, Watchdog};
use crate::reset::{ResetBudget, Verdict};
use crate::rtpanic;
use crate::telemetry::{Counters, Telemetry};

fn count(n: i32) -> usize {
    usize::try_from(n).unwrap_or(0)
}

/// `(address, bytes)` of a preallocated buffer, for `VirtualLock`.
fn range(buf: &[f64]) -> (usize, usize) {
    (buf.as_ptr().expose_provenance(), size_of_val(buf))
}

/// A driver with buffers, before `start()`.
struct Prepared {
    card: Card,
    rate: f64,
    /// The card's input count (the buffers list the inputs first).
    inputs: usize,
    map: ChannelMap,
    format: SampleFormat,
    buffers: Vec<[*mut c_void; 2]>,
}

fn panic_text() -> String {
    match rtpanic::latest() {
        Some(p) => format!(
            "panic in the audio callback at {}:{}:{}",
            p.file, p.line, p.col
        ),
        None => "panic in the audio callback".to_owned(),
    }
}

/// I3: a process with the driver module loaded refuses the first open (no
/// process names: whoever holds the card).
fn holders(module: &str) -> Result<(), AsioError> {
    let holders =
        iem_win::process::module_holders(module).map_err(|e| AsioError::Scan(e.to_string()))?;
    if holders.is_empty() {
        Ok(())
    } else {
        Err(AsioError::Held(holders))
    }
}

enum Next {
    Continue,
    Exit,
    Park,
}

impl Owner {
    /// Opens the card and starts a stream with `carry`. On failure the carry
    /// is back in `self.carry` (none when a stuck callback parked it); the
    /// preference window stays as it is, the caller closes it when no open
    /// follows.
    fn open(&mut self, carry: Carry, first: bool) -> Result<(), AsioError> {
        self.opens = self.opens.saturating_add(1);
        let began = Instant::now();
        info!(
            "[{}] open {} of the card{}",
            when(),
            self.opens,
            if first { "" } else { " (a reopen)" }
        );
        let opened = self.open_card(carry, first);
        // What the driver sent meanwhile (it asks questions while
        // createBuffers runs; a reset request may come any time).
        self.log_messages();
        match &opened {
            Ok(()) => info!(
                "[{}] open {} ready after {} ms: the driver delivers {} samples per callback",
                when(),
                self.opens,
                began.elapsed().as_millis(),
                self.shared.frames.load(Ordering::SeqCst)
            ),
            Err(e) => warn!(
                "[{}] open {} failed after {} ms: {e}",
                when(),
                self.opens,
                began.elapsed().as_millis()
            ),
        }
        opened
    }

    fn open_card(&mut self, carry: Carry, first: bool) -> Result<(), AsioError> {
        // I3, before the preference window and never after our own open.
        if first && let Err(e) = holders(&self.card.module) {
            self.carry = Some(carry);
            return Err(e);
        }
        match self.prepare() {
            Ok(prepared) => self.start_stream(prepared, carry, first),
            Err(e) => {
                self.carry = Some(carry);
                Err(e)
            }
        }
    }

    /// The preference window (32 written on the first open, kept on a
    /// reopen), then the driver's open and `createBuffers`. The window stays
    /// held after `createBuffers` (#9 2026-09-28): a write while the driver
    /// is open makes it ask for a reset.
    fn prepare(&mut self) -> Result<Prepared, AsioError> {
        self.enter_pref().map_err(AsioError::Pref)?;
        self.prepare_card()
    }

    /// Logs the driver's messages since the last look: the handlers only
    /// count them. Those that ask for a reopen are warnings.
    fn log_messages(&mut self) {
        for arrived in MESSAGES.since(&mut self.seen) {
            if arrived.topic.asks_reopen() {
                warn!("[{}] {arrived}", when());
            } else {
                info!("[{}] {arrived}", when());
            }
        }
    }

    fn prepare_card(&self) -> Result<Prepared, AsioError> {
        let card = Card::open(&self.card.driver)?;
        let info = card.host.info()?;
        let inputs = count(info.inputs);
        let map = ChannelMap::new(&self.rx, &self.tx, inputs, count(info.outputs))
            .and_then(|m| m.with_hil_return(&self.hil_rx, inputs))
            .map_err(AsioError::Channels)?;
        let format = format::admit(
            info.rate,
            info.buffer_preferred,
            self.card.frames,
            &info.sample_types,
        )
        .map_err(AsioError::Refused)?;
        let channels: Vec<ChannelId> = (0..info.inputs)
            .map(|index| ChannelId { input: true, index })
            .chain((0..info.outputs).map(|index| ChannelId {
                input: false,
                index,
            }))
            .collect();
        // SAFETY: BACKEND_CALLBACKS is a static, so it outlives the buffers.
        // The pointers are dereferenced only by callbacks of this stream,
        // which end before `finish` disposes the buffers.
        let created = unsafe {
            card.driver().create_buffers(
                channels.iter().copied(),
                self.card.frames,
                &raw const BACKEND_CALLBACKS,
            )
        }
        .map_err(card.host.call("createBuffers"))?;
        let buffers: Vec<[*mut c_void; 2]> = created.collect();
        Ok(Prepared {
            card,
            rate: info.rate,
            inputs,
            map,
            format,
            buffers,
        })
    }

    /// Zeroed outputs, `start()`, then the period from the first callbacks.
    /// A failure here ends the open with the card released for good: no open
    /// follows (a first open is refused, a reopen faults).
    fn start_stream(
        &mut self,
        prepared: Prepared,
        mut carry: Carry,
        first: bool,
    ) -> Result<(), AsioError> {
        let Prepared {
            card,
            rate,
            inputs,
            map,
            format,
            buffers,
        } = prepared;
        let frames = self.frames as usize;
        // The same sizes at every reopen: no reallocation, the pages stay.
        carry.inbuf.clear();
        let all_rx = map.all_rx();
        carry.inbuf.resize(all_rx.len().saturating_mul(frames), 0.0);
        carry.outbuf.clear();
        carry
            .outbuf
            .resize(map.tx().len().saturating_mul(frames), 0.0);
        let ranges = vec![range(&carry.inbuf), range(&carry.outbuf)];
        let split = inputs.min(buffers.len());
        let (card_inputs, card_outputs) = buffers.split_at(split);
        let backend = Box::new(Backend {
            format,
            frames,
            bytes: frames.saturating_mul(format.bytes()),
            inputs: card_inputs.to_vec(),
            outputs: card_outputs.to_vec(),
            rx: all_rx,
            tx: map.tx().to_vec(),
            carry: UnsafeCell::new(carry),
            telemetry: Telemetry::new(self.frames, rate),
            hists: Arc::clone(&self.hists),
            base: Instant::now(),
            ring: [const { AtomicI64::new(NO_POSITION) }; owner::RING],
            ring_len: AtomicUsize::new(0),
            discontinuity: AtomicBool::new(!first),
            faulted: AtomicBool::new(false),
            fault_ns: AtomicU64::new(0),
            output_ready: AtomicBool::new(true),
            driver: ptr::from_ref(card.driver()),
        });
        for ch in &backend.outputs {
            let [a, b] = *ch;
            zero(a, backend.bytes);
            zero(b, backend.bytes);
        }
        let raw = Box::into_raw(backend);
        BACKEND.store(raw, Ordering::SeqCst);
        let mut live = Live {
            card,
            backend: raw,
            watchdog: Watchdog::new(Instant::now()),
            rate_changes: 0,
        };
        if let Err(e) = live.card.driver().start() {
            let err = live.card.host.call("start")(e);
            self.carry = self.finish(live, Then::Release);
            return Err(err);
        }
        let started = Instant::now();
        loop {
            // A driver may need this thread's messages to call back.
            pump_messages();
            if self.shared.release_pending.load(Ordering::SeqCst) {
                // The session ended meanwhile (`session_end`): no stream.
                self.carry = self.finish(live, Then::Release);
                return Err(AsioError::SessionEnd);
            }
            // SAFETY: the stream is live: only `finish` frees it.
            let ring = unsafe { &*raw }.positions();
            match owner::open_period(&ring, self.frames, started.elapsed()) {
                OpenPeriod::Wait => thread::sleep(Duration::from_millis(1)),
                OpenPeriod::Ok(n) => {
                    self.shared.frames.store(n, Ordering::SeqCst);
                    break;
                }
                OpenPeriod::Refuse(verdict) => {
                    self.carry = self.finish(live, Then::Release);
                    return Err(AsioError::Period(verdict));
                }
            }
        }
        if let Ok(mut r) = self.shared.ranges.lock() {
            *r = ranges;
        }
        live.watchdog = Watchdog::new(Instant::now());
        self.live = Some(live);
        self.shared.running.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// [`Owner::stop_stream`] for a stream that ends for good: the carry.
    fn finish(&mut self, live: Live, then: Then) -> Option<Carry> {
        self.stop_stream(live, then).map(|(carry, _)| carry)
    }

    /// Stops the stream, waits until no callback is inside it (bounded by
    /// [`STOP_WAIT`], pumping), disposes the buffers and releases the driver
    /// ([`Owner::release_card`]: with `Then::Release` the preference window
    /// closes before `RELEASED` is set); returns the carry and what followed
    /// the release. `None` when a callback stayed inside (R6): the stream and
    /// the driver are left alone, never freed under a callback, the
    /// preference window stays held (the card may be), and the owner is done
    /// (parked).
    ///
    /// The stream's `faulted` is read again once no callback is inside it
    /// (S7, #10): a callback marks its panic at its end, after `watch` may
    /// have decided on a reopen. A faulted stream is noted as `watch` notes
    /// it and released for good (`owner::then_after_stop`): the panicked
    /// processor is never opened again.
    fn stop_stream(&mut self, live: Live, then: Then) -> Option<(Carry, Then)> {
        let Live { card, backend, .. } = live;
        let _ = card.driver().stop();
        BACKEND.store(ptr::null_mut(), Ordering::SeqCst);
        self.shared.running.store(false, Ordering::SeqCst);
        let wait = Instant::now();
        while BACKEND_IN_FLIGHT.load(Ordering::SeqCst) != 0 {
            if wait.elapsed() >= STOP_WAIT {
                // SAFETY: a parked stream is never freed, so its counters
                // stay readable; `stats` keeps counting its callbacks.
                self.base = self.base.plus(unsafe { &*backend }.telemetry.counters());
                core::mem::forget(card);
                self.shared.parked.store(true, Ordering::SeqCst);
                self.done = Some(StopOutcome::Parked);
                error!(
                    "[{}] a callback is still in the stream {} s after stop(): the stream is parked \
                     and may hold the card; the preferred buffer keeps the engine's value until \
                     the guard restores it",
                    when(),
                    STOP_WAIT.as_secs()
                );
                return None;
            }
            pump_messages();
            thread::yield_now();
        }
        // SAFETY: the slot no longer points to the stream and no callback is
        // inside it, so this is the only reference; it came from Box::into_raw.
        let stream = unsafe { Box::from_raw(backend) };
        self.base = self.base.plus(stream.telemetry.counters());
        let faulted = self.note_fault(&stream);
        let then_now = owner::then_after_stop(then, faulted);
        if then_now != then {
            error!(
                "[{}] the audio callback faulted while the card was stopped for a reopen: no \
                 reopen, the card is released for good",
                when()
            );
        }
        let _ = card.driver().dispose_all_buffers();
        let Backend { carry, .. } = *stream;
        self.release_card(card, then_now);
        Some((carry.into_inner(), then_now))
    }

    /// Drops the driver instance (the driver is released), then, when no open
    /// follows, closes the preference window, and only then sets `RELEASED`.
    /// After the driver's release, so no open driver of ours sees the write;
    /// before `RELEASED`, since the SEH filter lets the process end once it
    /// is set: a structured exception still writes REAPER's value back first
    /// (one registry write and read, well inside `owner::SEH_WAIT`).
    fn release_card(&mut self, card: Card, then: Then) {
        let Card {
            host,
            _released: mark,
        } = card;
        drop(host);
        if then == Then::Release {
            // A failure is logged and noted in `pref_failure`; the release
            // goes on.
            let _ = self.leave_pref();
        }
        drop(mark);
    }

    /// Ends the stream for good (a stop, the session end, a structured
    /// exception): the card released and the preference window closed,
    /// unless a stuck callback parked the stream.
    fn release_for_good(&mut self) -> StopOutcome {
        if let Some(outcome) = self.done {
            return outcome;
        }
        if let Some(live) = self.live.take() {
            self.carry = self.finish(live, Then::Release);
        }
        let outcome = self.done.unwrap_or(StopOutcome::Released);
        if outcome == StopOutcome::Released {
            // Without a live stream (a failed reopen, or one the session end
            // stopped) the window may still be held; after a failed close in
            // `finish` this is the second try.
            let _ = self.leave_pref();
        }
        self.log_messages();
        self.done = Some(outcome);
        self.publish();
        outcome
    }

    /// The parked-engine test's hold (S6 design §10 test #2, #35; set only
    /// by `raise_test_park`): after its structured exception the driver is
    /// kept as a driver that hangs in `dispose` keeps it. The stream is
    /// stopped (as `finish` stops it first; the faulting callback no longer
    /// counts in `BACKEND_IN_FLIGHT`), then neither disposed nor released:
    /// `RELEASED` stays clear, so the SEH filter's wait runs out and it parks
    /// the faulting thread. The owner is done (parked) like after a stuck
    /// callback (R6): the stream is never freed and its counters stay in
    /// `stats`, the preference window stays held (the card is), a stop or
    /// the session end releases nothing, and the thread keeps pumping.
    fn hold_card(&mut self) {
        if let Some(live) = self.live.take() {
            let Live { card, backend, .. } = live;
            // Logged before the call: a driver whose `stop()` waited for the
            // callback thread the filter holds would hang the owner thread
            // here, and the log tells that from a park. The SEH release
            // (test #4, PASS on #9 2026-09-28) made the same call under the
            // same exception, and it returned.
            warn!(
                "[{}] the parked-engine test's hold: stopping the driver while the faulting \
                 callback waits in the SEH filter",
                when()
            );
            let _ = card.driver().stop();
            info!(
                "[{}] the parked-engine test's hold: stop() returned",
                when()
            );
            BACKEND.store(ptr::null_mut(), Ordering::SeqCst);
            self.shared.running.store(false, Ordering::SeqCst);
            // SAFETY: a held stream is never freed, so its counters stay
            // readable.
            self.base = self.base.plus(unsafe { &*backend }.telemetry.counters());
            core::mem::forget(card);
        }
        self.shared.parked.store(true, Ordering::SeqCst);
        self.done = Some(StopOutcome::Parked);
        error!(
            "[{}] the parked-engine test's hold: the driver is kept, never disposed or released, \
             so the SEH filter parks the faulting thread; the stream stays parked with the card \
             held and the preferred buffer at the engine's value until the engine ends",
            when()
        );
        self.log_messages();
        self.publish();
    }

    fn fault(&mut self, why: String) {
        if let Some(live) = &self.live {
            // SAFETY: the stream is live.
            unsafe { &*live.backend }
                .faulted
                .store(true, Ordering::Release);
        }
        self.shared.faulted.store(true, Ordering::SeqCst);
        note(&self.shared.fault, why);
    }

    /// Whether the stream faulted (a panic in the callback, or the owner's
    /// own fault); a fault is noted in `shared` once: the faulting callback's
    /// time first, since the control thread reads it once it sees `faulted`
    /// (0 for the owner's own fault), then `faulted` and the panic's place.
    fn note_fault(&self, b: &Backend) -> bool {
        if !b.faulted.load(Ordering::Acquire) {
            return false;
        }
        self.shared
            .fault_ns
            .store(b.fault_ns.load(Ordering::Relaxed), Ordering::Release);
        if !self.shared.faulted.swap(true, Ordering::SeqCst) {
            note(&self.shared.fault, panic_text());
        }
        true
    }

    /// One look at the live stream: a panic in the callback, then the reopen
    /// question and its reasons.
    fn watch(&mut self, live: &mut Live, now: Instant) -> Option<(Asked, Verdict)> {
        // SAFETY: the stream is live.
        let b = unsafe { &*live.backend };
        if self.note_fault(b) {
            return None;
        }
        let requested = b.telemetry.take_requests();
        let rate_changes = b.telemetry.rate_changes();
        let rate = rate_changes != live.rate_changes;
        live.rate_changes = rate_changes;
        let asked = Asked {
            reset: requested.reset,
            buffer_size: requested.buffer_size,
            rate,
            forced: self.shared.reopen.swap(false, Ordering::SeqCst),
            stalled: live.watchdog.stalled(b.telemetry.callbacks(), now),
        };
        owner::reset_step(asked, &mut self.budget, now).map(|verdict| (asked, verdict))
    }

    /// Finish, then open again with the same processor; its first block
    /// after the reopen follows `Process::discontinuity`. The preference
    /// window stays held across it (nothing is written); when the reopen
    /// fails no open follows, and it closes.
    ///
    /// Its time (S7 HIL v2) runs from here, before `finish` stops the old
    /// stream, to the new stream's measured period: `last_reopen_us`.
    fn reopen(&mut self, live: Live) {
        let began = Instant::now();
        let Some((carry, then)) = self.stop_stream(live, Then::Reopen) else {
            return;
        };
        if then == Then::Release {
            // The processor panicked while the reopen was decided (S7, #10):
            // the fault is noted and the card released for good (the window
            // closed); the control thread ends the engine.
            self.carry = Some(carry);
            return;
        }
        if self.shared.release_pending.load(Ordering::SeqCst) {
            // The session ended during the finish: no new open (the next
            // tick releases for good and closes the window).
            self.carry = Some(carry);
            return;
        }
        match self.open(carry, false) {
            Ok(()) => {
                let us = owner::reopen_us(began.elapsed());
                self.shared.reopen_us.store(us, Ordering::SeqCst);
                info!(
                    "[{}] the reopen took {us} microseconds, from the old stream's stop to \
                     the new one's measured period",
                    when()
                );
            }
            Err(e) => {
                // The card is released and no open follows: REAPER's value
                // goes back now (a failure is noted in `pref_failure`).
                let _ = self.leave_pref();
                if !matches!(e, AsioError::SessionEnd) {
                    self.fault(format!("the reopen failed: {e}"));
                }
            }
        }
    }

    fn tick(&mut self, now: Instant) -> Next {
        self.log_messages();
        // `SEH` first: `raise_test_park` sets the hold before its exception,
        // so a tick that sees the exception sees the hold too.
        let seh = SEH.load(Ordering::SeqCst);
        match owner::seh_release(seh, SEH_HOLD.load(Ordering::SeqCst), self.done.is_some()) {
            SehRelease::Nothing => {}
            SehRelease::Release => {
                self.fault("a structured exception reached the filter".to_owned());
                self.release_for_good();
            }
            SehRelease::Hold => self.hold_card(),
        }
        if self.shared.release_pending.load(Ordering::SeqCst) && self.done.is_none() {
            // The session-end handler could not reach the owner.
            self.release_for_good();
        }
        if self.shared.stop.load(Ordering::SeqCst) {
            let outcome = self.release_for_good();
            self.shared.set_outcome(outcome);
            return match outcome {
                StopOutcome::Released => Next::Exit,
                StopOutcome::Parked => Next::Park,
            };
        }
        if self.done.is_none()
            && let Some(mut live) = self.live.take()
        {
            match self.watch(&mut live, now) {
                None => self.live = Some(live),
                Some((asked, Verdict::Reopen)) => {
                    warn!(
                        "[{}] reopening the card for {asked} ({})",
                        when(),
                        self.budget.state()
                    );
                    self.reopen(live);
                }
                Some((asked, Verdict::Fault)) => {
                    self.live = Some(live);
                    let why = format!(
                        "the driver needed more reopens than the budget allows (program spec §4.4): \
                         {asked}; {}",
                        self.budget.state()
                    );
                    error!("[{}] {why}", when());
                    self.fault(why);
                }
            }
        }
        self.publish();
        Next::Continue
    }

    fn publish(&self) {
        let live = self.live.as_ref().map_or_else(Counters::default, |l| {
            // SAFETY: the stream is live.
            unsafe { &*l.backend }.telemetry.counters()
        });
        let c = self.base.plus(live);
        let s = &self.shared;
        s.callbacks.store(c.callbacks, Ordering::Release);
        s.late.store(c.late, Ordering::Release);
        s.missed.store(c.missed, Ordering::Release);
        s.overruns.store(c.overruns, Ordering::Release);
        s.max_ns.store(c.max_ns, Ordering::Release);
        s.resets
            .store(u64::from(self.budget.used()), Ordering::Release);
    }

    /// The owner thread ends with the driver released: the processor and
    /// its buffers go.
    fn close(&mut self) {
        if let Ok(mut r) = self.shared.ranges.lock() {
            r.clear();
        }
        drop(self.carry.take());
    }
}

pub(super) fn owner_main(start: Start, ready: SyncSender<Result<(), AsioError>>) {
    let Start {
        card,
        frames,
        rx,
        tx,
        hil_rx,
        processor,
        shared,
        hists,
    } = start;
    // The clock of the messages' times and the owner's log lines, before
    // any driver exists.
    let _ = BASE.get_or_init(Instant::now);
    info!(
        "[{}] the ASIO owner thread starts: {} at {frames} samples",
        when(),
        card.driver
    );
    let pref = card
        .pref
        .as_ref()
        .map(|p| PrefWindow::new(p, frames, &shared));
    let state = Rc::new(RefCell::new(Owner {
        card,
        frames,
        rx,
        tx,
        hil_rx,
        shared: Arc::clone(&shared),
        hists,
        budget: ResetBudget::default(),
        base: Counters::default(),
        live: None,
        carry: None,
        done: None,
        opens: 0,
        seen: [0; TOPICS],
        pref,
    }));
    // A hidden top-level window (never message-only: those never see the
    // session end) on this thread, which pumps its messages.
    let window = {
        let state = Rc::clone(&state);
        let at_end = Arc::clone(&shared);
        SessionEndWindow::create(
            SESSION_END_REASON,
            Arc::clone(&shared.session_end),
            move || session_end(&state, &at_end),
        )
    };
    let window = match window {
        Ok(w) => w,
        Err(e) => {
            shared.set_outcome(StopOutcome::Released);
            BUSY.store(false, Ordering::SeqCst);
            let _ = ready.send(Err(AsioError::Call(
                "the session-end window",
                e.to_string(),
            )));
            return;
        }
    };
    let carry = Carry {
        processor,
        inbuf: Vec::new(),
        outbuf: Vec::new(),
    };
    let opened = state.borrow_mut().open(carry, true);
    if let Err(e) = opened {
        if state.borrow().done == Some(StopOutcome::Parked) {
            // The card may be held: the preference window stays held too.
            shared.set_outcome(StopOutcome::Parked);
            let _ = ready.send(Err(e));
            park(&window);
        }
        // The card is released and no open follows: REAPER's value goes
        // back before the refusal (usually done already by the failed
        // open's release; this is then a no-op, or the second try).
        let left = state.borrow_mut().leave_pref();
        state.borrow_mut().close();
        shared.set_outcome(StopOutcome::Released);
        BUSY.store(false, Ordering::SeqCst);
        let refusal = match left {
            Ok(()) => e,
            Err(error) => AsioError::PrefLeave {
                error,
                after: Some(Box::new(e)),
            },
        };
        let _ = ready.send(Err(refusal));
        return;
    }
    state.borrow().publish();
    let _ = ready.send(Ok(()));
    loop {
        pump_messages();
        let next = state.borrow_mut().tick(Instant::now());
        match next {
            Next::Continue => thread::sleep(TICK),
            Next::Exit => break,
            Next::Park => park(&window),
        }
    }
    state.borrow_mut().close();
    drop(window);
    BUSY.store(false, Ordering::SeqCst);
}

/// A parked stream: the thread keeps its window and pumps messages for good
/// (a driver may need them) and never frees the stream.
fn park(_window: &SessionEndWindow) -> ! {
    loop {
        pump_messages();
        thread::sleep(TICK);
    }
}

/// `WM_ENDSESSION` on the owner thread's window: the engine sees
/// `session_ending`, saves, fades out and asks the stream to stop; the
/// driver is released here, before the handler returns and Windows may end
/// the process (after [`owner::SESSION_END_WAIT`] at the latest).
///
/// Dispatched from the pump inside an open or a finish (the owner is
/// borrowed), it cannot reach the driver: it sets `release_pending` and
/// returns, and the owner thread stops that open and releases for good right
/// after. Windows may end the process in between (a short residual window,
/// only when the session ends during an open or a reopen).
fn session_end(state: &RefCell<Owner>, shared: &Shared) {
    let started = Instant::now();
    while !owner::session_end_release(shared.stop.load(Ordering::SeqCst), started.elapsed()) {
        pump_messages();
        thread::sleep(TICK);
    }
    match state.try_borrow_mut() {
        Ok(mut o) => {
            o.release_for_good();
        }
        Err(_) => shared.release_pending.store(true, Ordering::SeqCst),
    }
}
