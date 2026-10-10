//! The process's SEH filter (S6 design note §3) and the owner-approved test
//! exceptions (design §10 tests #4 and #2): a structured exception asks the
//! owner thread to release the driver and waits for `RELEASED`, then ends
//! the process, or parks the faulting thread while the driver is held.

use core::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Diagnostics::Debug::{
    EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS, RaiseException, SetUnhandledExceptionFilter,
};

use super::{BACKEND_IN_FLIGHT, DEPTH, RELEASED, SEH, SEH_HOLD, SEH_PARKED};
use crate::owner::{self, SehStep};

/// Installs the process's SEH filter (S6 design note §3), once, at engine
/// start. On an unhandled structured exception it asks the owner thread to
/// release the driver and waits up to [`owner::SEH_WAIT`] for `RELEASED`
/// (set only after the buffers are disposed and the host is dropped). Then
/// it lets the exception end the process — with
/// `iem_win::errmode::quiet_crashes` in force, without a dialog that could
/// keep the card held in session 1. If the driver is still held, the
/// faulting thread sleeps for good (the stream is parked; the guard alarms).
/// Exercised by the owner-approved SEH test: `iemmode inject-seh` in a HIL
/// job drives `raise_test_seh` on the RT thread (design §10 test #4), and
/// `iemmode inject-park` drives `raise_test_park`, whose hold makes the
/// filter park (test #2, #35).
pub fn install_seh_filter() {
    // SAFETY: registers a function of the documented filter signature; the
    // engine installs this filter only, so the previous one is not chained.
    unsafe {
        SetUnhandledExceptionFilter(Some(seh_filter));
    }
}

/// Raises a non-continuable structured exception on the calling thread for
/// the owner-approved SEH test (design §10, `--fault-injection` only). Unlike
/// a Rust panic, `catch_unwind` cannot catch it, so the installed SEH filter
/// runs: it releases the driver within its bound or parks the stream. The
/// code is an application-defined value (top bit set, customer bit set).
pub fn raise_test_seh() {
    // Application-defined code (severity error bit 31, customer bit 29 set)
    // spelling "IEM"; non-continuable flag 0x1.
    const IEM_SEH_TEST: u32 = 0xE049_454D;
    const NON_CONTINUABLE: u32 = 0x1;
    // SAFETY: RaiseException with a private, non-continuable code; it does not
    // return (the SEH filter ends the process or parks the thread).
    unsafe {
        RaiseException(IEM_SEH_TEST, NON_CONTINUABLE, 0, std::ptr::null::<usize>());
    }
}

/// The parked-engine test (S6 design §10 test #2, #35; `--fault-injection`
/// only): sets the test hold, then raises the SEH test's exception on the
/// calling thread. The owner thread keeps the driver (`owner::seh_release`:
/// stopped, never disposed or released), so the SEH filter's wait runs out
/// and it parks this thread for good: the stream stays parked with the card
/// held, and the exception is no fault (`owner::seh_faults`), so the engine
/// keeps running and reports `parked` until it ends (the test ends it with an
/// OS restart; a `Shutdown` ends it too). Raised during a reopen, the old
/// card's release may set `RELEASED` before the new open clears it: the
/// filter then lets the exception end the process, as without the hold.
pub fn raise_test_park() {
    // Before the exception: a tick that sees `SEH` sees the hold too.
    SEH_HOLD.store(true, Ordering::SeqCst);
    raise_test_seh();
}

unsafe extern "system" fn seh_filter(_info: *const EXCEPTION_POINTERS) -> i32 {
    SEH.store(true, Ordering::SeqCst);
    // A callback that faulted never continues (the exception ends the
    // process, or the thread parks below), so its count leaves the stream
    // and the owner thread may release it.
    let depth = DEPTH.with(|d| d.replace(0));
    if depth > 0 {
        BACKEND_IN_FLIGHT.fetch_sub(depth, Ordering::SeqCst);
    }
    let started = Instant::now();
    loop {
        match owner::seh_step(RELEASED.load(Ordering::SeqCst), started.elapsed()) {
            SehStep::Wait => thread::sleep(Duration::from_millis(5)),
            SehStep::Continue => return EXCEPTION_CONTINUE_SEARCH,
            SehStep::Park => {
                SEH_PARKED.store(true, Ordering::SeqCst);
                loop {
                    thread::sleep(Duration::from_secs(3_600));
                }
            }
        }
    }
}
