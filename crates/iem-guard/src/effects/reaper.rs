//! REAPER's web control (S6 design note §5.2): its replies are
//! tab-separated lines that start with a verb (`NTRACK`, `TRACK`,
//! `EXTSTATE`). A port of the S1a spike's `ConvertFrom-SpikeReaperLine`,
//! plus the stage tracks' meters. And REAPER's crash on quit (#10): Windows
//! Error Reporting's report of it, its exit codes, the hold, and the
//! handover's load poll that ends when REAPER's process has ended.

use std::time::Duration;

/// A `TRACK` line carries its meters only with at least this many fields,
/// the verb included; its 7th field is then the last meter peak in dB × 10
/// (REAPER's web-interface description, as the predecessor's poller reads
/// it). Without them the 7th field is something else.
pub const METER_FIELDS: usize = 14;

/// The URL of one web-control command (`NTRACK`, `TRACK`, an action id,
/// `GET/EXTSTATE/<section>/<key>`).
pub fn url(base: &str, command: &str) -> String {
    format!("{}/_/{command}", base.trim_end_matches('/'))
}

/// The command that reads one extended state value.
pub fn extstate_command(section_key: &str) -> String {
    format!("GET/EXTSTATE/{section_key}")
}

/// The fields after `verb` on the first line that starts with it; `None`
/// when no line does.
pub fn fields<'a>(text: &'a str, verb: &str) -> Option<Vec<&'a str>> {
    text.lines().find_map(|line| {
        let mut parts = line.trim_end_matches('\r').split('\t');
        if parts.next() == Some(verb) {
            Some(parts.collect())
        } else {
            None
        }
    })
}

/// `NTRACK`'s count.
pub fn ntrack(text: &str) -> Option<u32> {
    fields(text, "NTRACK")?.first()?.trim().parse().ok()
}

/// An extended state's value (`EXTSTATE`, section, key, value); empty when
/// the reply has none.
pub fn extstate(text: &str) -> String {
    fields(text, "EXTSTATE")
        .and_then(|f| f.get(2).map(|v| (*v).to_owned()))
        .unwrap_or_default()
}

/// Every `TRACK` line's last meter peak in dBFS, by track number.
pub fn track_peaks(text: &str) -> Vec<(u32, f64)> {
    text.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.trim_end_matches('\r').split('\t').collect();
            if parts.len() < METER_FIELDS || parts.first() != Some(&"TRACK") {
                return None;
            }
            let track = parts.get(1)?.parse().ok()?;
            let db10: f64 = parts.get(6)?.parse().ok()?;
            Some((track, db10 / 10.0))
        })
        .collect()
}

/// The stage tracks' peaks from one `TRACK` reply, in the order of `stage`;
/// `None` for a track the reply has no meters for.
pub fn stage_peaks(text: &str, stage: &[u32]) -> Vec<Option<f64>> {
    let all = track_peaks(text);
    stage
        .iter()
        .map(|s| all.iter().find(|(t, _)| t == s).map(|(_, db)| *db))
        .collect()
}

/// Windows Error Reporting's process, started for a crashed process; it
/// holds that process until its report is done.
pub const WER_IMAGE: &str = "WerFault.exe";

/// How long a REAPER that crashed on quit may still be held by Windows Error
/// Reporting before the guard gives up on it being gone (#10, 2026-10-08):
/// REAPER crashes on quit routinely (`reaper_csurf.dll`, 37 times in 30
/// days on the PC, long before iemmixer) and is usually gone within ~3 s,
/// but once WER held it past the 30 s quit bound. The project's save was
/// verified before the quit, so only the wait is longer. "Ide event" ends
/// the wait at once.
pub const CRASH_HOLD: Duration = Duration::from_secs(90);

/// Whether a WerFault command line (`WerFault.exe -u -p <pid> -s <n>`, or
/// a process snapshot's `-pss -s <n> -p <pid> -ip <pid>`) reports a crash
/// of `pid`: the word right after a `-p` (any case) is that pid.
pub fn wer_reports(command_line: &str, pid: u32) -> bool {
    let words: Vec<&str> = command_line.split_whitespace().collect();
    words.windows(2).any(|w| {
        matches!(w, [flag, value]
            if flag.eq_ignore_ascii_case("-p") && value.parse::<u32>().ok() == Some(pid))
    })
}

/// REAPER's exit code is a crash's: an NTSTATUS error (`0xC…`, e.g.
/// 0xC0000005, an access violation) or STATUS_FATAL_APP_EXIT (0x40000015),
/// the two codes of its crashes on quit in the PC's Application log (#10).
pub fn crashed(code: u32) -> bool {
    code >= 0xC000_0000 || code == 0x4000_0015
}

/// One look of the handover's load poll (#10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Load {
    /// REAPER reports the expected track count.
    Loaded,
    /// Not yet: look again until the bound.
    Waiting,
    /// REAPER's process ended (its exit code): the wait ends at once.
    Ended(u32),
}

/// `loaded`: the project reports its tracks; `ended`: the watched REAPER's
/// exit code once its process has ended. An ended process loads nothing
/// more, so its end ends the poll whatever the tracks read (#10: the
/// 2026-10-08 handover waited the full bound on a REAPER that had crashed).
pub fn load_look(loaded: bool, ended: Option<u32>) -> Load {
    match ended {
        Some(code) => Load::Ended(code),
        None if loaded => Load::Loaded,
        None => Load::Waiting,
    }
}

/// Keeps the loudest reading of each stage track; start from
/// `f64::NEG_INFINITY` (no reading yet).
pub fn keep_max(loudest: &mut [f64], now: &[Option<f64>]) {
    for (l, n) in loudest.iter_mut().zip(now) {
        if let Some(v) = n {
            *l = l.max(*v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TRACK line with meters: number, name, flags, volume, pan, peak and
    /// position (dB × 10), then the rest.
    fn track(n: u32, peak_db10: i32) -> String {
        format!("TRACK\t{n}\tIn {n}\t64\t1.0\t0.0\t{peak_db10}\t{peak_db10}\t1.0\t0\t0\t0\t1\t0")
    }

    #[test]
    fn urls_join_the_base_and_the_command() {
        assert_eq!(
            url("http://127.0.0.1:8080", "NTRACK"),
            "http://127.0.0.1:8080/_/NTRACK"
        );
        assert_eq!(
            url("http://127.0.0.1:8080/", "40026"),
            "http://127.0.0.1:8080/_/40026"
        );
        assert_eq!(url("http://h//", "TRACK"), "http://h/_/TRACK");
        assert_eq!(
            extstate_command("bridge/state"),
            "GET/EXTSTATE/bridge/state"
        );
    }

    #[test]
    fn fields_follow_the_verb_of_the_first_matching_line() {
        assert_eq!(fields("NTRACK\t45\n", "NTRACK"), Some(vec!["45"]));
        assert_eq!(
            fields("TRACK\t1\n\r\nEXTSTATE\tsec\tkey\t1\r\n", "EXTSTATE"),
            Some(vec!["sec", "key", "1"])
        );
        assert_eq!(fields("", "NTRACK"), None);
        assert_eq!(fields("NTRACKS\t1\n", "NTRACK"), None);
        assert_eq!(fields("X\tNTRACK\t1", "NTRACK"), None);
        assert_eq!(fields("NTRACK", "NTRACK"), Some(vec![]));
        assert_eq!(fields("A\t1\nA\t2", "A"), Some(vec!["1"]));
        // A bare CR at the end of the last line is trimmed too.
        assert_eq!(fields("A\t1\r", "A"), Some(vec!["1"]));
    }

    #[test]
    fn ntrack_and_extstate_read_their_values() {
        assert_eq!(ntrack("NTRACK\t45\n"), Some(45));
        assert_eq!(ntrack("NTRACK\t 7 \r\n"), Some(7));
        assert_eq!(ntrack("NTRACK\t\n"), None);
        assert_eq!(ntrack("NTRACK\n"), None);
        assert_eq!(ntrack("NTRACK\tx\n"), None);
        assert_eq!(ntrack(""), None);
        assert_eq!(extstate("EXTSTATE\tbridge\tstate\t1\n"), "1");
        assert_eq!(extstate("EXTSTATE\tbridge\tstate\t\n"), "");
        assert_eq!(extstate("EXTSTATE\tbridge\tstate\n"), "");
        assert_eq!(extstate("NTRACK\t1\n"), "");
        assert_eq!(extstate("EXTSTATE\ts\tk\t12345\tmore\n"), "12345");
    }

    #[test]
    fn track_peaks_need_the_meter_fields() {
        let text = format!(
            "NTRACK\t3\n{}\n{}\nTRACK\t3\tIn 3\t64\t1.0\t0.0\t-200\t-200\t1.0\t0\t0\t0\n",
            track(1, -123),
            track(2, -1500)
        );
        assert_eq!(track_peaks(&text), [(1, -12.3), (2, -150.0)]);
        // Exactly the meter fields: 14 with the verb.
        let exact = track(4, -35);
        assert_eq!(exact.split('\t').count(), METER_FIELDS);
        assert_eq!(track_peaks(&exact), [(4, -3.5)]);
        let short: Vec<&str> = exact.split('\t').take(METER_FIELDS - 1).collect();
        assert!(track_peaks(&short.join("\t")).is_empty());
        // Another verb with as many fields, or unreadable numbers: nothing.
        assert!(track_peaks(&exact.replacen("TRACK", "SEND", 1)).is_empty());
        assert!(track_peaks(&exact.replacen("\t4\t", "\tx\t", 1)).is_empty());
        assert!(track_peaks(&exact.replacen("\t-35\t", "\tloud\t", 1)).is_empty());
        assert_eq!(track_peaks(&format!("{exact}\r\n")), [(4, -3.5)]);
    }

    #[test]
    fn stage_peaks_follow_the_stage_order() {
        let text = format!(
            "{}\n{}\n{}\n",
            track(1, -100),
            track(2, -200),
            track(3, -300)
        );
        assert_eq!(
            stage_peaks(&text, &[3, 1, 9]),
            [Some(-30.0), Some(-10.0), None]
        );
        assert!(stage_peaks(&text, &[]).is_empty());
    }

    /// A WerFault command line as Windows Error Reporting starts it for a
    /// crashed process (`-u -p <pid> -s <n>`; a process snapshot's report
    /// is `-pss -s <n> -p <pid> -ip <pid>`).
    #[test]
    fn a_wer_command_line_reports_the_pid_after_its_p() {
        let line = r#""C:\Windows\system32\WerFault.exe" -u -p 4242 -s 552"#;
        assert!(wer_reports(line, 4242));
        assert!(!wer_reports(line, 424));
        assert!(!wer_reports(line, 42420));
        assert!(!wer_reports(line, 552));
        assert!(wer_reports("WerFault.exe -pss -s 516 -p 77 -ip 77", 77));
        assert!(wer_reports(
            r"C:\Windows\SysWOW64\WerFault.exe -u -P 9 -s 1",
            9
        ));
        // Any whitespace separates the words.
        assert!(wer_reports("WerFault.exe\t-u  -p\t13 -s 1", 13));
        // Only the value right after `-p` counts: `-ip`, `-s`, a value
        // before the flag, a glued or a non-numeric value never do.
        assert!(!wer_reports("WerFault.exe -u -ip 77 -s 1", 77));
        assert!(!wer_reports("WerFault.exe -u -s 77", 77));
        assert!(!wer_reports("WerFault.exe 77 -p", 77));
        assert!(!wer_reports("WerFault.exe -p77 -s 1", 77));
        assert!(!wer_reports("WerFault.exe -p x77", 77));
        assert!(!wer_reports("WerFault.exe -p", 0));
        assert!(!wer_reports("", 0));
        assert_eq!(WER_IMAGE, "WerFault.exe");
    }

    /// REAPER's two exit codes of a crash on quit (#10, 2026-10-08, the
    /// PC's Application log: 0xc0000005 and 0x40000015 in
    /// `reaper_csurf.dll`); a normal quit is 0.
    #[test]
    fn a_crash_is_an_ntstatus_error_or_a_fatal_app_exit() {
        for code in [0xC000_0005, 0x4000_0015, 0xC000_0000, 0xC000_0409, u32::MAX] {
            assert!(crashed(code), "{code:#x}");
        }
        for code in [0, 1, 0x4000_0014, 0x4000_0016, 0xBFFF_FFFF, 0x8000_0003] {
            assert!(!crashed(code), "{code:#x}");
        }
    }

    /// The handover's load poll (#10, 2026-10-08: it waited the full 120 s
    /// on a REAPER that had crashed): an ended process ends the wait at
    /// once, whatever the tracks read.
    #[test]
    fn the_load_poll_ends_at_once_when_reapers_process_ended() {
        assert_eq!(load_look(false, None), Load::Waiting);
        assert_eq!(load_look(true, None), Load::Loaded);
        assert_eq!(
            load_look(false, Some(0xC000_0005)),
            Load::Ended(0xC000_0005)
        );
        assert_eq!(load_look(true, Some(0)), Load::Ended(0));
    }

    /// The crash hold (#10): Windows Error Reporting held the crashed
    /// REAPER past the 30 s quit bound; 90 s more bounds the wait.
    #[test]
    fn the_crash_hold_is_ninety_seconds() {
        assert_eq!(CRASH_HOLD, std::time::Duration::from_secs(90));
    }

    #[test]
    fn the_loudest_reading_of_each_track_is_kept() {
        let mut loudest = vec![f64::NEG_INFINITY, -20.0, -30.0];
        keep_max(&mut loudest, &[Some(-40.0), Some(-25.0), None]);
        assert_eq!(loudest, [-40.0, -20.0, -30.0]);
        keep_max(&mut loudest, &[Some(-50.0), Some(-10.0), Some(-29.9)]);
        assert_eq!(loudest, [-40.0, -10.0, -29.9]);
        keep_max(&mut loudest, &[]);
        assert_eq!(loudest, [-40.0, -10.0, -29.9]);
    }
}
