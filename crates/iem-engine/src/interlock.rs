//! The interlock (S6 design note §4, §5.2 step 2): before the guard takes the
//! card for iemmixer while the band's system could be in use, the engine
//! opens the card, listens to the stage inputs for up to 60 s and writes
//! nothing. Activity — −50 dBFS for 3 s in a row
//! (`telemetry::ActivityGuard`) — refuses the switch.
//!
//! Portable and mutation-tested: which card channels are the stage, the
//! processor that only records their peaks, the verdict, the report and the
//! listening loop. Opening the card is Windows-only (`crate::asio`).

use std::sync::Arc;
use std::time::Duration;

use iem_audio_io::telemetry::{ActivityGuard, InputPeaks, Loudest, dbfs};
use iem_audio_io::{Block, Process, StreamStats};
use serde::Serialize;

use crate::site::{SiteError, Stage};
use crate::topology::Topology;

/// One-second peaks above −50 dBFS in a row that mean the band plays.
pub const ACTIVE_SECONDS: u32 = 3;
/// The interlock's length unless `--seconds` says otherwise.
pub const DEFAULT_SECONDS: u32 = 60;
/// The longest `--seconds`.
pub const MAX_SECONDS: u32 = 600;
/// How often the loop looks at the stop request.
pub const SLICE: Duration = Duration::from_millis(100);
/// Slices per second of listening.
pub const SLICES: u32 = 10;
/// Inputs named in the report.
pub const LOUDEST: usize = 5;

/// The card RX channels of the stage inputs, in topology order: the
/// site's `[activity] inputs`, or (an empty list) every input of category
/// `mics` — the server's band-activity rule (grouped inputs are stems, an
/// input without a category is `mics`). The program input carries signal
/// while the band is silent, so it is never listened to. An unknown id or
/// an empty stage is a site error: an interlock that hears nothing would
/// always be quiet.
pub fn stage_channels(topo: &Topology, stage: &Stage) -> Result<Vec<u16>, SiteError> {
    if let Some(unknown) = stage
        .inputs
        .iter()
        .find(|id| topo.inputs.iter().all(|n| n.id.0 != **id))
    {
        return Err(SiteError::StageInput(unknown.clone()));
    }
    let mut channels = Vec::new();
    for node in &topo.inputs {
        let watched = if stage.inputs.is_empty() {
            let category = stage
                .categories
                .iter()
                .find(|(id, _)| *id == node.id.0)
                .and_then(|(_, c)| c.as_deref())
                .unwrap_or("mics");
            node.group.is_none() && category == "mics"
        } else {
            stage.inputs.contains(&node.id.0)
        };
        if !watched {
            continue;
        }
        // A mono input has the same RX slot twice: listen to it once.
        let count = if node.stereo { 2 } else { 1 };
        channels.extend(
            node.rx
                .iter()
                .take(count)
                .filter_map(|k| topo.rx.get(*k).copied()),
        );
    }
    if channels.is_empty() {
        return Err(SiteError::NoStage);
    }
    Ok(channels)
}

/// The interlock's processor: the peak of every input it gets (the stage
/// channels), nothing written — the backend zeroes every card output on
/// every callback (A1) and gets no TX channel. Allocates, locks, logs and
/// makes syscalls never (I7).
pub struct PeakTap {
    peaks: Arc<InputPeaks>,
}

impl PeakTap {
    pub fn new(peaks: Arc<InputPeaks>) -> Self {
        Self { peaks }
    }
}

impl Process for PeakTap {
    fn process(&mut self, block: &mut Block<'_>) {
        for ch in 0..block.inputs() {
            let peak = block.input(ch).iter().fold(0.0f64, |m, x| m.max(x.abs()));
            self.peaks.record(ch, peak);
        }
    }
}

/// How an interlock ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// No activity for the whole time: the switch may go on.
    Quiet,
    /// The band plays: the switch is refused.
    Activity,
    /// A stop was requested (the guard's pre-emption) before a verdict.
    Stopped,
}

impl Verdict {
    /// The process exit code: 0 quiet, 5 activity, 6 stopped.
    pub fn code(self) -> u8 {
        match self {
            Self::Quiet => 0,
            Self::Activity => 5,
            Self::Stopped => 6,
        }
    }
}

/// The JSON line the guard reads: `{"quiet", "stopped", "loudest"}`, the
/// loudest stage channels as `[card channel, dBFS]`, loudest first.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub quiet: bool,
    pub stopped: bool,
    pub loudest: Vec<(u16, f64)>,
}

/// dBFS with one decimal, for the report.
fn tenths(db: f64) -> f64 {
    (db * 10.0).round() / 10.0
}

/// The listening state: one-second peaks per stage channel.
#[derive(Debug, Clone)]
pub struct Interlock {
    channels: Vec<u16>,
    seconds: u32,
    heard: u32,
    guard: ActivityGuard,
    loudest: Loudest,
}

impl Interlock {
    /// Listens to `channels` (card RX numbers, in the order the peaks come)
    /// for `seconds`.
    pub fn new(channels: Vec<u16>, seconds: u32) -> Self {
        Self {
            channels,
            seconds,
            heard: 0,
            guard: ActivityGuard::new(ACTIVE_SECONDS),
            loudest: Loudest::default(),
        }
    }

    /// One second's peaks, one per channel: the verdict once it is decided.
    pub fn second(&mut self, peaks: &[f64]) -> Option<Verdict> {
        self.loudest.observe(peaks);
        self.heard += 1;
        let loudest = peaks.iter().copied().fold(0.0f64, f64::max);
        if self.guard.observe(loudest) {
            Some(Verdict::Activity)
        } else if self.heard >= self.seconds {
            Some(Verdict::Quiet)
        } else {
            None
        }
    }

    pub fn report(&self, verdict: Verdict) -> Report {
        Report {
            quiet: verdict == Verdict::Quiet,
            stopped: verdict == Verdict::Stopped,
            loudest: self
                .loudest
                .top(LOUDEST)
                .into_iter()
                .filter_map(|(k, peak)| Some((*self.channels.get(k)?, tenths(dbfs(peak)))))
                .collect(),
        }
    }
}

/// Listens until the verdict. Every second `take` gives the peaks since the
/// last call, or why the card stopped delivering (an error); `stopped` is
/// looked at every [`SLICE`], so a stop request ends the wait within one.
pub fn watch(
    lock: &mut Interlock,
    mut take: impl FnMut() -> Result<Vec<f64>, String>,
    mut stopped: impl FnMut() -> bool,
    mut sleep: impl FnMut(Duration),
) -> Result<Verdict, String> {
    loop {
        for _ in 0..SLICES {
            if stopped() {
                return Ok(Verdict::Stopped);
            }
            sleep(SLICE);
        }
        if let Some(verdict) = lock.second(&take()?) {
            return Ok(verdict);
        }
    }
}

/// Whether the stream still delivers: not faulted, not parked, and more
/// callbacks than `before`. Returns the new callback count.
pub fn health(before: u64, st: &StreamStats) -> Result<u64, String> {
    if st.faulted {
        return Err(format!(
            "the card's stream faulted: {}",
            st.fault.as_deref().unwrap_or("unknown")
        ));
    }
    if st.parked {
        return Err("the card's stream is parked".to_owned());
    }
    if st.callbacks <= before {
        return Err(format!(
            "the card stopped calling back ({} callbacks)",
            st.callbacks
        ));
    }
    Ok(st.callbacks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::parse_stage;
    use crate::test_support::{test_site, test_site_text};
    use iem_audio_io::{Offline, Planar};

    fn stage(inputs: &[&str]) -> Stage {
        Stage {
            inputs: inputs.iter().map(|s| (*s).to_owned()).collect(),
            categories: Vec::new(),
        }
    }

    #[test]
    fn the_stage_is_the_listed_inputs_in_topology_order() {
        let topo = test_site();
        // hand2 is mono (RX 112), keys stereo (RX 115, 116).
        assert_eq!(
            stage_channels(&topo, &stage(&["keys", "hand2", "mic1"])),
            Ok(vec![101, 112, 115, 116])
        );
        assert_eq!(
            stage_channels(&topo, &stage(&["mic1", "ghost"])),
            Err(SiteError::StageInput("ghost".into()))
        );
    }

    #[test]
    fn without_a_list_the_stage_is_every_ungrouped_mics_input() {
        let topo = test_site();
        let from_site = parse_stage(&test_site_text()).unwrap();
        // mic1…mic10 and keys: no category is mics; hand, eng_mic and content
        // are tech, iemonly is stems, the grouped inputs are stems.
        let mut want: Vec<u16> = (101..=110).collect();
        want.extend([115, 116]);
        assert_eq!(stage_channels(&topo, &from_site), Ok(want));
        // An input without an [[inputs]] entry is mics too; an explicit
        // `mics` counts, grouped inputs never do.
        let named = Stage {
            inputs: Vec::new(),
            categories: vec![
                ("mic1".into(), Some("tech".into())),
                ("hand1".into(), Some("mics".into())),
                ("drums".into(), Some("mics".into())),
            ],
        };
        let got = stage_channels(&topo, &named).unwrap();
        assert!(!got.contains(&101) && got.contains(&102) && got.contains(&111));
        assert!(!got.contains(&123), "a grouped input is a stem");
        assert!(got.contains(&119), "content without a category is mics");
        // Nothing to listen to is an error, never a quiet stage.
        let deaf = Stage {
            inputs: Vec::new(),
            categories: topo
                .inputs
                .iter()
                .map(|n| (n.id.0.clone(), Some("tech".into())))
                .collect(),
        };
        assert_eq!(stage_channels(&topo, &deaf), Err(SiteError::NoStage));
    }

    #[test]
    fn the_tap_records_each_input_peak_and_writes_nothing() {
        let peaks = Arc::new(InputPeaks::new(3));
        let mut tap = PeakTap::new(Arc::clone(&peaks));
        let mut input = Planar::new(3, 64);
        input.channel_mut(0)[5] = -0.5;
        input.channel_mut(0)[6] = 0.25;
        input.channel_mut(2)[63] = 0.125;
        let run = Offline { block: 32 }.run(&mut tap, &input, 2);
        assert!(run.fault.is_none());
        assert_eq!(peaks.take(), vec![0.5, 0.0, 0.125]);
        assert!(run.output.channel(0).iter().all(|y| *y == 0.0));
        assert!(run.output.channel(1).iter().all(|y| *y == 0.0));
    }

    /// −40 dBFS and −60 dBFS: either side of the −50 dBFS threshold.
    const LOUD: f64 = 0.01;
    const SOFT: f64 = 0.001;

    #[test]
    fn three_active_seconds_in_a_row_refuse_and_quiet_needs_every_second() {
        let mut lock = Interlock::new(vec![101, 102], 4);
        assert_eq!(lock.second(&[LOUD, 0.0]), None);
        assert_eq!(lock.second(&[0.0, LOUD]), None);
        assert_eq!(lock.second(&[SOFT, SOFT]), None, "the run was broken");
        assert_eq!(lock.second(&[SOFT, SOFT]), Some(Verdict::Quiet));
        let mut lock = Interlock::new(vec![101, 102], 60);
        for _ in 0..2 {
            assert_eq!(lock.second(&[0.0, LOUD]), None);
        }
        assert_eq!(lock.second(&[LOUD, 0.0]), Some(Verdict::Activity));
        // No seconds at all: the first one decides.
        let mut lock = Interlock::new(vec![101], 0);
        assert_eq!(lock.second(&[SOFT]), Some(Verdict::Quiet));
        // Activity on the last second still refuses.
        let mut lock = Interlock::new(vec![101], 3);
        lock.second(&[LOUD]);
        lock.second(&[LOUD]);
        assert_eq!(lock.second(&[LOUD]), Some(Verdict::Activity));
    }

    #[test]
    fn the_report_names_the_loudest_channels_in_tenths_of_a_db() {
        let mut lock = Interlock::new(vec![101, 102, 103, 104, 105, 106, 107], 60);
        lock.second(&[0.5, 0.0, 0.25, 0.001, 0.1, 0.002, 0.003]);
        lock.second(&[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.004]);
        let r = lock.report(Verdict::Activity);
        assert_eq!((r.quiet, r.stopped), (false, false));
        assert_eq!(
            r.loudest,
            vec![
                (101, -6.0),
                (103, -12.0),
                (105, -20.0),
                (107, -48.0),
                (106, -54.0)
            ]
        );
        let json = serde_json::to_string(&lock.report(Verdict::Quiet)).unwrap();
        assert!(
            json.starts_with(r#"{"quiet":true,"stopped":false,"loudest":[[101,-6.0],"#),
            "{json}"
        );
        let stopped = lock.report(Verdict::Stopped);
        assert_eq!((stopped.quiet, stopped.stopped), (false, true));
        assert_eq!(tenths(-12.04), -12.0);
        assert_eq!(tenths(-12.06), -12.1);
        assert!(
            Interlock::new(vec![101], 60)
                .report(Verdict::Quiet)
                .loudest
                .is_empty()
        );
    }

    #[test]
    fn verdicts_have_their_exit_codes() {
        assert_eq!(Verdict::Quiet.code(), 0);
        assert_eq!(Verdict::Activity.code(), 5);
        assert_eq!(Verdict::Stopped.code(), 6);
    }

    /// `watch` with scripted peaks, on a thread: its result, the number of
    /// takes and sleeps; fails after 5 s instead of hanging.
    fn watch_bounded(
        seconds: u32,
        peaks: Vec<Result<Vec<f64>, String>>,
        stop_at: Option<u32>,
    ) -> (Result<Verdict, String>, usize, u32) {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = std::thread::spawn(move || {
            let mut lock = Interlock::new(vec![101], seconds);
            let mut script = peaks.into_iter();
            let (mut takes, mut looks, mut sleeps) = (0, 0, 0);
            let out = watch(
                &mut lock,
                || {
                    takes += 1;
                    script.next().unwrap_or(Ok(vec![0.0]))
                },
                || {
                    looks += 1;
                    stop_at == Some(looks)
                },
                |d| {
                    assert_eq!(d, SLICE);
                    sleeps += 1;
                },
            );
            let _ = tx.send((out, takes, sleeps));
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("watch returns")
    }

    #[test]
    fn watch_ends_at_its_verdict_or_at_a_stop_request() {
        let soft = || Ok(vec![SOFT]);
        let loud = || Ok(vec![LOUD]);
        // Quiet after `seconds` takes, ten slices each.
        assert_eq!(
            watch_bounded(3, vec![soft(), soft(), soft()], None),
            (Ok(Verdict::Quiet), 3, 30)
        );
        assert_eq!(
            watch_bounded(
                60,
                vec![loud(), loud(), soft(), loud(), loud(), loud()],
                None
            ),
            (Ok(Verdict::Activity), 6, 60)
        );
        // A stop request is seen before the next slice's sleep.
        assert_eq!(
            watch_bounded(60, Vec::new(), Some(15)),
            (Ok(Verdict::Stopped), 1, 14)
        );
        assert_eq!(
            watch_bounded(60, Vec::new(), Some(1)),
            (Ok(Verdict::Stopped), 0, 0)
        );
        // A card that stops delivering ends it with the reason.
        assert_eq!(
            watch_bounded(60, vec![soft(), Err("gone".into())], None),
            (Err("gone".into()), 2, 20)
        );
    }

    #[test]
    fn a_stream_is_healthy_while_its_callbacks_advance() {
        let st = |callbacks: u64| StreamStats {
            callbacks,
            running: true,
            ..StreamStats::default()
        };
        assert_eq!(health(10, &st(11)), Ok(11));
        assert_eq!(
            health(10, &st(10)),
            Err("the card stopped calling back (10 callbacks)".into())
        );
        assert!(health(10, &st(9)).is_err());
        let faulted = StreamStats {
            faulted: true,
            fault: Some("boom".into()),
            ..st(20)
        };
        assert_eq!(
            health(10, &faulted),
            Err("the card's stream faulted: boom".into())
        );
        let unknown = StreamStats {
            fault: None,
            ..faulted
        };
        assert_eq!(
            health(10, &unknown),
            Err("the card's stream faulted: unknown".into())
        );
        let parked = StreamStats {
            parked: true,
            ..st(20)
        };
        assert_eq!(
            health(10, &parked),
            Err("the card's stream is parked".into())
        );
    }
}
