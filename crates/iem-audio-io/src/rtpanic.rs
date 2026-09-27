//! Panics on the real-time thread (S1a finding: the default hook formats and
//! locks stderr, 4.29 ms in the callback). On a thread marked real-time the
//! hook only stores the location and a count in atomics; the control thread
//! reads and logs them. Other threads keep the previous hook.

use core::cell::Cell;
use core::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};

/// The recorded path keeps its last `FILE_MAX` bytes.
pub const FILE_MAX: usize = 96;

thread_local! {
    static RT: Cell<bool> = const { Cell::new(false) };
}

static COUNT: AtomicU64 = AtomicU64::new(0);
static LINE: AtomicU32 = AtomicU32::new(0);
static COL: AtomicU32 = AtomicU32::new(0);
static FILE_LEN: AtomicUsize = AtomicUsize::new(0);
static FILE: [AtomicU8; FILE_MAX] = [const { AtomicU8::new(0) }; FILE_MAX];

/// Marks the calling thread real-time (the callback does this on entry; a
/// const thread-local, so no allocation on first use).
pub fn mark_rt_thread() {
    RT.with(|f| f.set(true));
}

pub fn is_rt_thread() -> bool {
    RT.with(Cell::get)
}

/// Records a location without allocating or locking.
pub fn record(file: &str, line: u32, col: u32) {
    let bytes = file.as_bytes();
    let tail = bytes.len().saturating_sub(FILE_MAX);
    let src = bytes.get(tail..).unwrap_or_default();
    for (slot, b) in FILE.iter().zip(src) {
        slot.store(*b, Ordering::Relaxed);
    }
    FILE_LEN.store(src.len(), Ordering::Relaxed);
    LINE.store(line, Ordering::Relaxed);
    COL.store(col, Ordering::Relaxed);
    COUNT.fetch_add(1, Ordering::Release);
}

/// Installs the hook once (engine start-up, not the RT thread).
pub fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if is_rt_thread() {
            if let Some(l) = info.location() {
                record(l.file(), l.line(), l.column());
            } else {
                record("", 0, 0);
            }
        } else {
            previous(info);
        }
    }));
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtPanic {
    /// RT panics since the process started.
    pub count: u64,
    pub file: String,
    pub line: u32,
    pub col: u32,
}

/// The latest RT panic, for the control thread (which may allocate).
pub fn latest() -> Option<RtPanic> {
    let count = COUNT.load(Ordering::Acquire);
    (count > 0).then(|| {
        let n = FILE_LEN.load(Ordering::Relaxed).min(FILE_MAX);
        let bytes: Vec<u8> = FILE
            .iter()
            .take(n)
            .map(|b| b.load(Ordering::Relaxed))
            .collect();
        RtPanic {
            count,
            file: String::from_utf8_lossy(&bytes).into_owned(),
            line: LINE.load(Ordering::Relaxed),
            col: COL.load(Ordering::Relaxed),
        }
    })
}

// `install` is tested in its own binary (`tests/rtpanic_hook.rs`): the hook
// is process-global.
#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard, PoisonError};

    use assert_no_alloc::{assert_no_alloc, reset_violation_count, violation_count};

    use super::*;

    /// The record is process-global: its tests take turns.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn count() -> u64 {
        latest().map_or(0, |p| p.count)
    }

    #[test]
    fn record_keeps_the_last_96_bytes_of_the_path() {
        let _turn = serial();
        let before = count();
        let long = format!("{}/src/rt.rs", "d".repeat(100));
        record(&long, 12, 34);
        assert_eq!(
            latest(),
            Some(RtPanic {
                count: before + 1,
                file: long[long.len() - FILE_MAX..].to_owned(),
                line: 12,
                col: 34,
            })
        );
        // Exactly 96 bytes are kept whole.
        let exact = "e".repeat(FILE_MAX);
        record(&exact, 1, 2);
        assert_eq!(
            latest().map(|p| (p.count, p.file)),
            Some((before + 2, exact))
        );
        // A shorter path after a longer one keeps only its own bytes.
        record("src/short.rs", 7, 9);
        assert_eq!(
            latest(),
            Some(RtPanic {
                count: before + 3,
                file: "src/short.rs".into(),
                line: 7,
                col: 9,
            })
        );
    }

    #[test]
    fn only_the_marked_thread_is_real_time() {
        let other = std::thread::spawn(is_rt_thread).join().unwrap();
        let marked = std::thread::spawn(|| {
            let before = is_rt_thread();
            mark_rt_thread();
            (before, is_rt_thread())
        })
        .join()
        .unwrap();
        let fresh = std::thread::spawn(is_rt_thread).join().unwrap();
        assert_eq!((other, marked, fresh), (false, (false, true), false));
    }

    #[test]
    fn the_detector_sees_an_allocation() {
        reset_violation_count();
        let v = assert_no_alloc(|| vec![1u8; 4]);
        assert!(violation_count() > 0);
        assert_eq!(v.len(), 4);
    }

    #[test]
    fn the_rt_hook_does_not_allocate() {
        let _turn = serial();
        let before = count();
        let (marked, violations) = std::thread::spawn(|| {
            reset_violation_count();
            let marked = assert_no_alloc(|| {
                mark_rt_thread();
                record(file!(), line!(), column!());
                is_rt_thread()
            });
            (marked, violation_count())
        })
        .join()
        .unwrap();
        assert_eq!((marked, violations), (true, 0), "the RT path allocated");
        assert_eq!(count(), before + 1);
    }
}
