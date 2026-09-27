//! The RT panic hook (S6 design note §3). Its own test binary with a single
//! test, because the hook and the record are process-global: a panic on a
//! thread marked real-time is only recorded (the previous hook, which prints,
//! never runs), a panic on any other thread goes to the previous hook.

use std::panic::catch_unwind;
use std::sync::atomic::{AtomicU32, Ordering};

use iem_audio_io::rtpanic::{self, FILE_MAX};

/// Calls of the hook that was installed before `rtpanic::install`.
static PREVIOUS: AtomicU32 = AtomicU32::new(0);

/// Panics at its caller's line.
#[track_caller]
fn boom(on: &str) {
    panic!("a panic on the {on} thread");
}

/// The recorded form of this file's path (its last `FILE_MAX` bytes).
fn this_file() -> String {
    let f = file!();
    f[f.len().saturating_sub(FILE_MAX)..].to_owned()
}

/// Panics on a new thread marked real-time; returns the panic's line.
fn rt_panic() -> u32 {
    std::thread::spawn(|| {
        rtpanic::mark_rt_thread();
        let (line, caught) = (line!(), catch_unwind(|| boom("RT")));
        assert!(caught.is_err());
        line
    })
    .join()
    .unwrap()
}

#[test]
fn rt_panics_are_recorded_and_others_reach_the_previous_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        PREVIOUS.fetch_add(1, Ordering::SeqCst);
        default(info);
    }));
    rtpanic::install();
    assert_eq!(rtpanic::latest(), None, "nothing recorded yet");

    let line = rt_panic();
    let first = rtpanic::latest().unwrap();
    assert_eq!(
        (first.count, first.file, first.line),
        (1, this_file(), line)
    );
    assert!(first.col > 0, "{}", first.col);
    assert_eq!(PREVIOUS.load(Ordering::SeqCst), 0, "the RT panic printed");

    // An ordinary thread: the previous hook runs, nothing is recorded.
    let caught = std::thread::spawn(|| catch_unwind(|| boom("control")).is_err())
        .join()
        .unwrap();
    assert!(caught);
    assert_eq!(PREVIOUS.load(Ordering::SeqCst), 1);
    assert_eq!(rtpanic::latest().map(|p| p.count), Some(1));

    // A second RT panic counts on.
    let line = rt_panic();
    let second = rtpanic::latest().unwrap();
    assert_eq!(
        (second.count, second.file, second.line),
        (2, this_file(), line)
    );
    assert_eq!(PREVIOUS.load(Ordering::SeqCst), 1);
}
