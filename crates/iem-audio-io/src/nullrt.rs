//! The `NullRt` backend: a thread that calls the processor every `block /
//! sample_rate` seconds against absolute deadlines, with synthetic inputs
//! (silence or a sine on every channel). For hosted E2E tests and soak runs;
//! pacing is best effort (the host scheduler decides), and a thread more than
//! eight periods behind resynchronises instead of bursting.

use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::{Block, Process, panic_message};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputSignal {
    Silence,
    /// The same sine on every input channel.
    Sine {
        hz: f64,
        amp: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NullRtConfig {
    pub sample_rate: u32,
    pub block: usize,
    pub inputs: usize,
    pub outputs: usize,
    pub signal: InputSignal,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamStats {
    pub callbacks: u64,
    /// Callbacks that finished more than one period after their deadline.
    pub late: u64,
    pub faulted: bool,
    pub running: bool,
    pub max_process_ns: u64,
    pub fault: Option<String>,
}

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    running: AtomicBool,
    faulted: AtomicBool,
    callbacks: AtomicU64,
    late: AtomicU64,
    max_ns: AtomicU64,
    fault: Mutex<Option<String>>,
}

pub struct NullRt<P: Process + 'static> {
    thread: JoinHandle<P>,
    shared: Arc<Shared>,
}

impl<P: Process + 'static> NullRt<P> {
    /// Starts the pacing thread; the processor comes back from [`NullRt::stop`].
    pub fn start(cfg: NullRtConfig, mut p: P) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        shared.running.store(true, Ordering::Release);
        let s = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("iem-nullrt".into())
            .spawn(move || {
                pace(&cfg, &mut p, &s);
                s.running.store(false, Ordering::Release);
                p
            })?;
        Ok(Self { thread, shared })
    }

    pub fn stats(&self) -> StreamStats {
        let s = &self.shared;
        StreamStats {
            callbacks: s.callbacks.load(Ordering::Acquire),
            late: s.late.load(Ordering::Acquire),
            faulted: s.faulted.load(Ordering::Acquire),
            running: s.running.load(Ordering::Acquire),
            max_process_ns: s.max_ns.load(Ordering::Acquire),
            fault: s.fault.lock().ok().and_then(|f| f.clone()),
        }
    }

    /// Stops the thread and returns the processor (`None` if the thread died
    /// outside the guarded callback).
    pub fn stop(self) -> Option<P> {
        self.shared.stop.store(true, Ordering::Release);
        self.thread.join().ok()
    }
}

fn pace<P: Process>(cfg: &NullRtConfig, p: &mut P, s: &Shared) {
    let block = cfg.block.max(1);
    let rate = f64::from(cfg.sample_rate.max(1));
    let period = Duration::from_secs_f64(block as f64 / rate);
    let mut input = vec![0.0; cfg.inputs.saturating_mul(block)];
    let mut output = vec![0.0; cfg.outputs.saturating_mul(block)];
    let (inc, amp) = match cfg.signal {
        InputSignal::Silence => (0.0, 0.0),
        InputSignal::Sine { hz, amp } => (hz / rate, amp),
    };
    let mut phase = 0.0f64;
    let mut deadline = Instant::now();
    while !s.stop.load(Ordering::Acquire) {
        if amp != 0.0 {
            for i in 0..block {
                let x = amp * (core::f64::consts::TAU * phase).sin();
                phase = (phase + inc).fract();
                for ch in 0..cfg.inputs {
                    if let Some(v) = input.get_mut(ch * block + i) {
                        *v = x;
                    }
                }
            }
        }
        output.fill(0.0);
        let started = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut b = Block::new(block, &input, &mut output);
            p.process(&mut b);
        }));
        let ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        s.max_ns.fetch_max(ns, Ordering::AcqRel);
        if let Err(payload) = result {
            output.fill(0.0);
            if let Ok(mut f) = s.fault.lock() {
                *f = Some(panic_message(&*payload));
            }
            s.faulted.store(true, Ordering::Release);
            return;
        }
        s.callbacks.fetch_add(1, Ordering::AcqRel);
        deadline += period;
        let now = Instant::now();
        if now > deadline + period {
            s.late.fetch_add(1, Ordering::AcqRel);
            if now > deadline + 8 * period {
                deadline = now;
            }
        } else if deadline > now {
            std::thread::sleep(deadline - now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 96_000;

    fn cfg(signal: InputSignal) -> NullRtConfig {
        NullRtConfig {
            sample_rate: SR,
            block: 32,
            inputs: 2,
            outputs: 1,
            signal,
        }
    }

    #[derive(Default)]
    struct Count {
        calls: u64,
        first: Vec<f64>,
        second: Vec<f64>,
        panic_at: Option<u64>,
    }

    impl Process for Count {
        fn process(&mut self, block: &mut Block<'_>) {
            self.calls += 1;
            if self.first.len() < 960 {
                self.first.extend_from_slice(block.input(0));
                self.second.extend_from_slice(block.input(1));
            }
            block.output(0).fill(1.0);
            if Some(self.calls) == self.panic_at {
                panic!("boom in the callback");
            }
        }
    }

    fn wait_for(rt: &NullRt<Count>, what: impl Fn(&StreamStats) -> bool) -> StreamStats {
        let start = Instant::now();
        loop {
            let s = rt.stats();
            if what(&s) || start.elapsed() > Duration::from_secs(5) {
                return s;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn nullrt_paces_close_to_real_time() {
        let rt = NullRt::start(cfg(InputSignal::Silence), Count::default()).unwrap();
        let t0 = Instant::now();
        std::thread::sleep(Duration::from_millis(300));
        let during = rt.stats();
        assert!(during.running);
        assert!(!during.faulted);
        let p = rt.stop().unwrap();
        let expected = t0.elapsed().as_secs_f64() * f64::from(SR) / 32.0;
        let calls = p.calls as f64;
        assert!(
            calls > 0.5 * expected && calls < 1.5 * expected,
            "{calls} vs {expected}"
        );
        assert!(p.first.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn nullrt_feeds_the_sine() {
        let rt = NullRt::start(
            cfg(InputSignal::Sine {
                hz: 1000.0,
                amp: 0.5,
            }),
            Count::default(),
        )
        .unwrap();
        wait_for(&rt, |s| s.callbacks >= 40);
        let p = rt.stop().unwrap();
        assert!(p.first.len() >= 960);
        let peak = p.first.iter().fold(0.0f64, |m, x| m.max(x.abs()));
        assert!((peak - 0.5).abs() < 1e-3, "{peak}");
        assert_eq!(p.first, p.second);
        // 1 kHz at 96 kHz: an upward zero crossing every 96 samples (±1 where
        // the accumulated phase lands a hair either side of zero).
        let ups: Vec<usize> = (1..960)
            .filter(|&i| p.first[i - 1] < 0.0 && p.first[i] >= 0.0)
            .collect();
        assert!(ups.len() >= 8, "{ups:?}");
        assert!(
            ups.windows(2).all(|w| (95..=97).contains(&(w[1] - w[0]))),
            "{ups:?}"
        );
        let span = (ups[ups.len() - 1] - ups[0]) as f64 / (ups.len() - 1) as f64;
        assert!((span - 96.0).abs() < 0.5, "{span}");
    }

    #[test]
    fn nullrt_panic_marks_the_stream_faulted_and_stops_calling() {
        let rt = NullRt::start(
            cfg(InputSignal::Silence),
            Count {
                panic_at: Some(3),
                ..Count::default()
            },
        )
        .unwrap();
        let s = wait_for(&rt, |s| s.faulted && !s.running);
        assert!(s.faulted);
        assert!(!s.running);
        assert_eq!(s.callbacks, 2);
        assert!(
            s.fault
                .as_deref()
                .unwrap_or("")
                .contains("boom in the callback")
        );
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(rt.stats().callbacks, 2);
        let p = rt.stop().unwrap();
        assert_eq!(p.calls, 3);
    }

    struct Slow {
        calls: u64,
    }

    impl Process for Slow {
        fn process(&mut self, _: &mut Block<'_>) {
            self.calls += 1;
            if self.calls == 3 {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    #[test]
    fn a_slow_callback_is_counted_late() {
        let rt = NullRt::start(cfg(InputSignal::Silence), Slow { calls: 0 }).unwrap();
        let start = Instant::now();
        while rt.stats().callbacks < 100 && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let s = rt.stats();
        assert!(s.late >= 1, "{s:?}");
        assert!(s.max_process_ns >= 20_000_000, "{s:?}");
        assert!(rt.stop().unwrap().calls >= 100);
    }

    #[test]
    fn stop_ends_a_running_stream() {
        let rt = NullRt::start(cfg(InputSignal::Silence), Count::default()).unwrap();
        let s = wait_for(&rt, |s| s.callbacks >= 5);
        assert!(s.max_process_ns > 0);
        let p = rt.stop().unwrap();
        assert!(p.calls >= 5);
    }
}
