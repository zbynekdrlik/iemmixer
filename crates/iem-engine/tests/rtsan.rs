//! I7 under RealtimeSanitizer (design note §3.8). The `engine` CI job builds
//! this test with a pinned nightly, `-Zsanitizer=realtime`, `-Zbuild-std` and
//! `--cfg iem_rtsan`; `Processor::process` then carries
//! `#[sanitize(realtime = "nonblocking")]` and any allocation, lock or
//! blocking syscall inside it aborts the test binary. Built normally (every
//! other job), the same workload runs without the sanitizer.
#![cfg_attr(iem_rtsan, feature(sanitize))]

mod common;

use std::process::Command;

/// Sums in a real-time context: sanitizer-clean.
#[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
fn nonblocking_sum(x: &[f64]) -> f64 {
    x.iter().sum()
}

/// Allocates in a real-time context: a violation under the sanitizer.
#[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
fn nonblocking_alloc(n: usize) -> usize {
    vec![0u8; n].len()
}

const PROBE: &str = "IEM_RTSAN_PROBE";

#[test]
fn process_is_realtime_safe() {
    let mut s = common::scenario();
    let mut b = common::buffers(&s.graph);
    common::drive(&mut s, &mut b, 3_000);
    assert_eq!(s.processor.time(), 3_000 * common::BLOCK as u64);
    assert!(b.output.iter().all(|y| y.is_finite() && y.abs() <= 1.0));
}

/// Run by `rtsan_detects_a_violation` in a child process; on its own it only
/// checks the clean real-time function.
#[test]
fn probe() {
    assert_eq!(nonblocking_sum(&[1.0, 2.0, 3.5]), 6.5);
    if std::env::var_os(PROBE).is_some() {
        assert_eq!(nonblocking_alloc(64), 64);
    }
}

#[test]
fn rtsan_detects_a_violation() {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "probe", "--nocapture", "--test-threads", "1"])
        .env(PROBE, "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if cfg!(iem_rtsan) {
        assert!(!out.status.success(), "the allocation went unnoticed");
        assert!(stderr.contains("RealtimeSanitizer"), "{stderr}");
        println!("rtsan: the self-test violation was reported");
    } else {
        assert!(out.status.success(), "{stderr}");
    }
}
