//! The backend's tests that run on the hosted runner (no ASIO driver there).

use super::*;
use crate::Block;

struct Silent;

impl Process for Silent {
    fn process(&mut self, block: &mut Block<'_>) {
        block.zero_outputs();
    }
}

fn card(driver: &str, frames: i32) -> CardConfig {
    CardConfig {
        driver: driver.to_owned(),
        module: "testcard.dll".to_owned(),
        frames,
        pref: None,
    }
}

// The hosted runner has no ASIO driver: the first open fails at the
// driver list (or at the name, where one exists), after the module check
// and before any buffer, and releases everything, so the next start is
// not `Busy`.
#[test]
fn a_missing_driver_refuses_the_stream_and_frees_the_slot() {
    for _ in 0..2 {
        let r = AsioStream::start(
            card("No Such Card", 32),
            (101..=132).collect(),
            (71..=93).collect(),
            Vec::new(),
            Silent,
        );
        assert!(
            matches!(r, Err(AsioError::NoDrivers(_) | AsioError::NotFound { .. })),
            "{:?}",
            r.as_ref().err()
        );
        assert!(RELEASED.load(Ordering::SeqCst));
    }
    assert!(!BUSY.load(Ordering::SeqCst));
}

#[test]
fn a_buffer_that_is_no_sample_count_is_refused_first() {
    for frames in [0, -32] {
        let r = AsioStream::start(
            card("No Such Card", frames),
            vec![101],
            vec![71],
            Vec::new(),
            Silent,
        );
        assert!(
            matches!(r, Err(AsioError::Frames(f)) if f == frames),
            "{:?}",
            r.as_ref().err()
        );
    }
}

#[test]
fn backend_errors_read_as_sentences() {
    assert_eq!(
        AsioError::Frames(0).to_string(),
        "the configured buffer of 0 samples is not a positive size"
    );
    assert_eq!(
        AsioError::Held(vec![(4242, "test.exe".into()), (7, "other.exe".into())]).to_string(),
        "the driver module is loaded by test.exe (pid 4242), other.exe (pid 7) (I3)"
    );
    assert_eq!(
        AsioError::Period(PeriodVerdict::Wrong {
            expected: 32,
            measured: 64
        })
        .to_string(),
        "the driver delivers 64 samples per callback, expected 32"
    );
    assert_eq!(
        AsioError::Period(PeriodVerdict::Undecided).to_string(),
        "the driver's period could not be measured from its first callbacks"
    );
    let unrestored = AsioError::PrefLeave {
        error: PrefError::Write("denied".into()),
        after: Some(Box::new(AsioError::NoDrivers("none".into()))),
    };
    assert_eq!(
        unrestored.to_string(),
        "the preferred buffer was not restored: writing the preferred buffer failed: \
         denied (after the open failed: no ASIO drivers registered: none)"
    );
    assert_eq!(
        AsioError::Channels(MapError::Zero { side: "rx" }).to_string(),
        "the topology does not fit the card: rx channel 0: card channels count from 1"
    );
    assert_eq!(
        AsioError::SessionEnd.to_string(),
        "the Windows session ended while the card opened"
    );
}
