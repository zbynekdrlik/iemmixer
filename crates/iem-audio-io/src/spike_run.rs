//! The S1a spike's run decisions (S1c design note §4.1, §4.3, §6.2 L5):
//! which argument combination it refuses, which levers a run applied as its
//! report shows them, when a lever that could not be applied fails the run,
//! the hwlat scanner's gate and end, and the exit code of every outcome.
//! Portable and mutated; `examples/asio_spike/` keeps only the thread and
//! Windows plumbing (`os::set_process_cpus`, `os::set_thread_cpus`,
//! `os::set_thread_time_critical`) and the JSON.

use std::time::Duration;

/// The exit code of a run's outcome (the example's header): 0 done or
/// stopped, 3 no driver, 4 refused, 5 band activity, 6 fault caught, 7 the
/// driver changed the rate, 8 a callback did not leave the stream; any other
/// outcome ("error") is 1. Exit 2 (usage) is the parser's, never an outcome.
pub fn exit_code(outcome: &str) -> u8 {
    match outcome {
        "done" | "stopped" => 0,
        "no-driver" => 3,
        "refused" => 4,
        "band-activity" => 5,
        "fault-caught" => 6,
        "rate-changed" => 7,
        "stop-hung" => 8,
        _ => 1,
    }
}

/// `--stress` with `--audio-cpus` needs `--stress-cpus`: a thread without
/// its own selection runs on the process default CPU Set, which
/// `--audio-cpus` sets, so the busy threads would share the audio CPUs (S1c
/// design note §4.3 puts them on the housekeeping CPUs).
pub fn check_stress_cpus(stress: u32, audio_cpus: &[u8], stress_cpus: &[u8]) -> Result<(), String> {
    if stress > 0 && !audio_cpus.is_empty() && stress_cpus.is_empty() {
        return Err(
            "--stress with --audio-cpus needs --stress-cpus (the housekeeping CPUs)".to_owned(),
        );
    }
    Ok(())
}

/// What a run applied of a requested placement, as its report shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// Nothing was placed: no processors (or no threads) asked for.
    Nothing,
    /// The CPU Set IDs applied.
    Ids(Vec<u32>),
    /// Why the placement failed; the run fails.
    Failed(String),
}

/// A placement on `cpus` through `place` (the CPU Set IDs applied, or why
/// not); without `cpus` nothing is placed and `place` is never called.
pub fn place_on(
    cpus: &[u8],
    place: impl FnOnce(&[u8]) -> Result<Vec<u32>, String>,
) -> Result<Vec<u32>, String> {
    if cpus.is_empty() {
        Ok(Vec::new())
    } else {
        place(cpus)
    }
}

/// The report's value of one placement on `cpus` ([`place_on`]'s result).
pub fn applied(cpus: &[u8], placed: Result<Vec<u32>, String>) -> Applied {
    match placed {
        Err(e) => Applied::Failed(e),
        Ok(_) if cpus.is_empty() => Applied::Nothing,
        Ok(ids) => Applied::Ids(ids),
    }
}

/// The busy threads' placement on `cpus` from each thread's result, in
/// arrival order (`None`: a thread ended before it reported): the first
/// failure fails them all (the run would load other processors than it
/// reports), and nothing was placed without threads or without `cpus`.
/// Stops reading `results` at the first failure.
pub fn stress_placement<I>(cpus: &[u8], results: I) -> Applied
where
    I: IntoIterator<Item = Option<Result<Vec<u32>, String>>>,
{
    let mut placed = None;
    for result in results {
        match result {
            Some(Ok(ids)) => placed = Some(ids),
            Some(Err(e)) => {
                return Applied::Failed(format!("placing a stress thread on {cpus:?}: {e}"));
            }
            None => {
                return Applied::Failed("a stress thread ended before it was placed".to_owned());
            }
        }
    }
    match placed {
        Some(ids) if !cpus.is_empty() => Applied::Ids(ids),
        _ => Applied::Nothing,
    }
}

/// A busy thread spins while it was placed and no stop was asked for; one
/// that could not be placed ends at once.
pub fn keeps_busy(placed: bool, stopped: bool) -> bool {
    placed && !stopped
}

/// The in-process levers a run applies before it starts (S1c design note
/// §6.2 L5): power throttling off (`throttling`) and the audio CPU Set as
/// the process default (`audio`). One that could not be applied fails the
/// run, since it would measure another process than the report names: the
/// outcome and why. A failed audio CPU Set refuses the run (checked first),
/// throttling left on is an error.
pub fn setup_failure(
    throttling: &Result<(), String>,
    audio: &Applied,
) -> Option<(&'static str, String)> {
    if let Applied::Failed(e) = audio {
        return Some(("refused", format!("audio CPU Set: {e}")));
    }
    throttling
        .as_ref()
        .err()
        .map(|e| ("error", format!("power throttling: {e}")))
}

/// The hwlat scanner's gate (S1c design note §4.1): placed on `cpu` through
/// `place`, then raised to TIME_CRITICAL through `raise`, only once placed.
/// Either failure means the scan would measure another processor or
/// priority than the report names: no scan, and why. Returns the CPU Set IDs
/// applied.
pub fn hwlat_ready(
    cpu: u8,
    place: impl FnOnce(&[u8]) -> Result<Vec<u32>, String>,
    raise: impl FnOnce() -> Result<(), String>,
) -> Result<Vec<u32>, String> {
    let ids = place(&[cpu]).map_err(|e| format!("placing the scanner on CPU {cpu}: {e}"))?;
    raise().map_err(|e| format!("raising the scanner to TIME_CRITICAL: {e}"))?;
    Ok(ids)
}

/// The hwlat scan ends once `elapsed` reaches `end` or a stop was asked for.
pub fn scan_ends(elapsed: Duration, end: Duration, stopped: bool) -> bool {
    elapsed >= end || stopped
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the CPU Set calls: processor n has the ID 0x100 + n.
    fn placed(cpus: &[u8]) -> Result<Vec<u32>, String> {
        Ok(cpus.iter().map(|&lp| 0x100 + u32::from(lp)).collect())
    }

    fn refused(cpus: &[u8]) -> Result<Vec<u32>, String> {
        Err(format!("processors {cpus:?} are not in group 0"))
    }

    #[test]
    fn every_outcome_has_its_exit_code() {
        assert_eq!(
            [
                "done",
                "stopped",
                "no-driver",
                "refused",
                "band-activity",
                "fault-caught",
                "rate-changed",
                "stop-hung",
                "error",
                "anything else",
            ]
            .map(exit_code),
            [0, 0, 3, 4, 5, 6, 7, 8, 1, 1]
        );
    }

    #[test]
    fn stress_on_the_audio_cpus_needs_its_own_cpus() {
        let e = check_stress_cpus(4, &[14], &[]).unwrap_err();
        assert!(e.contains("--stress-cpus"), "{e}");
        assert_eq!(check_stress_cpus(4, &[14], &[6, 7]), Ok(()));
        assert_eq!(check_stress_cpus(4, &[], &[]), Ok(()), "anywhere");
        assert_eq!(check_stress_cpus(0, &[14], &[]), Ok(()), "no threads");
        assert_eq!(check_stress_cpus(1, &[14], &[]), Err(e));
    }

    #[test]
    fn a_placement_happens_only_when_processors_are_asked_for() {
        assert_eq!(place_on(&[6, 7], placed), Ok(vec![0x106, 0x107]));
        assert_eq!(
            place_on(&[6], refused),
            Err("processors [6] are not in group 0".to_owned())
        );
        let mut called = false;
        let none = place_on(&[], |_| {
            called = true;
            Ok(vec![9])
        });
        assert_eq!((none, called), (Ok(vec![]), false));
    }

    #[test]
    fn the_report_shows_what_a_placement_applied() {
        assert_eq!(applied(&[14], Ok(vec![0x10e])), Applied::Ids(vec![0x10e]));
        assert_eq!(applied(&[], Ok(vec![])), Applied::Nothing);
        assert_eq!(
            applied(&[14], Err("no".to_owned())),
            Applied::Failed("no".to_owned())
        );
    }

    #[test]
    fn one_unplaced_stress_thread_fails_them_all() {
        let ok = |ids: &[u32]| Some(Ok(ids.to_vec()));
        assert_eq!(
            stress_placement(&[6, 7], [ok(&[0x106, 0x107]), ok(&[0x106, 0x107])]),
            Applied::Ids(vec![0x106, 0x107])
        );
        // Without processors or without threads nothing was placed.
        assert_eq!(stress_placement(&[], [ok(&[]), ok(&[])]), Applied::Nothing);
        assert_eq!(stress_placement(&[6], []), Applied::Nothing);
        // The first failure fails the start; later results are not read.
        let mut read = 0;
        let results = [ok(&[0x106]), Some(Err("denied".to_owned())), ok(&[0x106])]
            .into_iter()
            .inspect(|_| read += 1);
        assert_eq!(
            stress_placement(&[6], results),
            Applied::Failed("placing a stress thread on [6]: denied".to_owned())
        );
        assert_eq!(read, 2);
        assert_eq!(
            stress_placement(&[6], [ok(&[0x106]), None]),
            Applied::Failed("a stress thread ended before it was placed".to_owned())
        );
    }

    #[test]
    fn a_busy_thread_spins_only_while_placed_and_not_stopped() {
        assert!(keeps_busy(true, false));
        assert!(!keeps_busy(true, true));
        assert!(!keeps_busy(false, false));
        assert!(!keeps_busy(false, true));
    }

    #[test]
    fn a_failed_audio_cpu_set_refuses_the_run() {
        let off = Ok(());
        assert_eq!(setup_failure(&off, &Applied::Nothing), None);
        assert_eq!(setup_failure(&off, &Applied::Ids(vec![0x10e])), None);
        assert_eq!(
            setup_failure(&off, &Applied::Failed("denied".to_owned())),
            Some(("refused", "audio CPU Set: denied".to_owned()))
        );
    }

    /// Power throttling left on (EcoQoS, ignored timer requests) would
    /// measure another process than the report names: the run fails like a
    /// lever that could not be applied (#32 review).
    #[test]
    fn power_throttling_left_on_fails_the_run() {
        let on = Err("access denied".to_owned());
        assert_eq!(
            setup_failure(&on, &Applied::Nothing),
            Some(("error", "power throttling: access denied".to_owned()))
        );
        assert_eq!(
            setup_failure(&on, &Applied::Ids(vec![0x10e])),
            Some(("error", "power throttling: access denied".to_owned()))
        );
        // A refused audio CPU Set is named first (it refuses the run).
        assert_eq!(
            setup_failure(&on, &Applied::Failed("denied".to_owned())),
            Some(("refused", "audio CPU Set: denied".to_owned()))
        );
    }

    #[test]
    fn the_hwlat_scanner_runs_only_placed_and_raised() {
        assert_eq!(hwlat_ready(3, placed, || Ok(())), Ok(vec![0x103]));
        let mut raised = false;
        let e = hwlat_ready(3, refused, || {
            raised = true;
            Ok(())
        })
        .unwrap_err();
        assert_eq!(
            (e.as_str(), raised),
            (
                "placing the scanner on CPU 3: processors [3] are not in group 0",
                false
            )
        );
        assert_eq!(
            hwlat_ready(3, placed, || Err("access denied".to_owned())),
            Err("raising the scanner to TIME_CRITICAL: access denied".to_owned())
        );
    }

    #[test]
    fn the_hwlat_scan_ends_at_its_end_or_a_stop() {
        let end = Duration::from_secs(30);
        let ns = Duration::from_nanos(1);
        assert!(!scan_ends(end - ns, end, false));
        assert!(scan_ends(end, end, false));
        assert!(scan_ends(end + ns, end, false));
        assert!(scan_ends(Duration::ZERO, end, true));
        assert!(scan_ends(Duration::ZERO, Duration::ZERO, false));
    }
}
