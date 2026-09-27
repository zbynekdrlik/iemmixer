//! The predecessor app's exit (S6 design note §5.3): exactly one process,
//! its log line after the tray command (corroboration only), and no temp
//! file written after the command (a write the exit cut). The verdict is
//! `handover::app_exit`.

/// Exactly one pid of `what`, or why not.
pub fn one_pid(pids: &[u32], what: &str) -> Result<u32, String> {
    match pids {
        [pid] => Ok(*pid),
        [] => Err(format!("{what} does not run")),
        more => Err(format!("{what} runs {} times", more.len())),
    }
}

/// Whether every running app process was started from the configured exe
/// (design §5.3: the exe hash identifies the binary that runs). `images`:
/// each process's full image path, or why it could not be read. None
/// running is fine: the configured exe is the one our task starts.
pub fn running_from(configured: &str, images: &[Result<String, String>]) -> Result<(), String> {
    for image in images {
        match image {
            Ok(path) if same_path(path, configured) => {}
            Ok(path) => {
                return Err(format!(
                    "the predecessor app runs from {path}, not from pc.toml app_exe"
                ));
            }
            Err(e) => return Err(format!("the predecessor app's image: {e}")),
        }
    }
    Ok(())
}

/// Two Windows paths name the same file: case and separator style aside.
fn same_path(a: &str, b: &str) -> bool {
    let plain = |p: &str| p.replace('/', "\\").to_lowercase();
    plain(a) == plain(b)
}

/// `pid`s and images for a message: `a.exe (12), b.exe (13)`.
pub fn holders_text(holders: &[(u32, String)]) -> String {
    holders
        .iter()
        .map(|(pid, name)| format!("{name} ({pid})"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn digits<T: std::str::FromStr>(s: &str, len: usize) -> Option<T> {
    if s.len() == len && s.bytes().all(|b| b.is_ascii_digit()) {
        s.parse().ok()
    } else {
        None
    }
}

/// Days from 1970-01-01 to a date of the proleptic Gregorian calendar
/// (year ≥ 1970), after Howard Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = i64::from((month + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Milliseconds since the Unix epoch of the RFC 3339 UTC time a log line
/// starts with (`2026-09-27T12:34:56.123456Z`, the `tracing` default);
/// `None` for any other line.
pub fn line_time_ms(line: &str) -> Option<u64> {
    let stamp = line.split_whitespace().next()?.strip_suffix('Z')?;
    let (date, time) = stamp.split_once('T')?;
    let mut d = date.split('-');
    let year: i64 = digits(d.next()?, 4)?;
    let month: u32 = digits(d.next()?, 2)?;
    let day: u32 = digits(d.next()?, 2)?;
    let (clock, frac) = time.split_once('.').unwrap_or((time, ""));
    let mut t = clock.split(':');
    let hour: u64 = digits(t.next()?, 2)?;
    let minute: u64 = digits(t.next()?, 2)?;
    let second: u64 = digits(t.next()?, 2)?;
    let valid = d.next().is_none()
        && t.next().is_none()
        && year >= 1970
        && (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && hour < 24
        && minute < 60
        && second < 60
        && frac.bytes().all(|b| b.is_ascii_digit());
    if !valid {
        return None;
    }
    let ms = frac
        .bytes()
        .chain(std::iter::repeat(b'0'))
        .take(3)
        .fold(0u64, |acc, b| acc * 10 + u64::from(b - b'0'));
    let days = u64::try_from(days_from_civil(year, month, day)).ok()?;
    Some(((days * 24 + hour) * 60 + minute) * 60_000 + second * 1000 + ms)
}

/// Whether the log `text` has a line containing `wanted` stamped at or
/// after `after_ms`.
pub fn logged_after(text: &str, wanted: &str, after_ms: u64) -> bool {
    text.lines()
        .any(|l| l.contains(wanted) && line_time_ms(l).is_some_and(|t| t >= after_ms))
}

/// The newest of `(name, modified ms)`; a tie goes to the greater name (a
/// daily log's date suffix).
pub fn newest(files: &[(String, u64)]) -> Option<&str> {
    files
        .iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
        .map(|(name, _)| name.as_str())
}

/// Whether a temp file (`*.tmp`, any case) was modified at or after
/// `after_ms`.
pub fn newer_temp(files: &[(String, u64)], after_ms: u64) -> bool {
    files
        .iter()
        .any(|(name, t)| name.to_ascii_lowercase().ends_with(".tmp") && *t >= after_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_running_app_must_be_the_configured_exe() {
        let exe = "C:\\Programs\\App\\app.exe";
        assert_eq!(running_from(exe, &[]), Ok(()));
        assert_eq!(running_from(exe, &[Ok(exe.to_owned())]), Ok(()));
        assert_eq!(
            running_from(exe, &[Ok(exe.to_owned()), Ok(exe.to_owned())]),
            Ok(())
        );
        assert_eq!(
            running_from(exe, &[Ok("C:\\Other\\app.exe".to_owned())]),
            Err(
                "the predecessor app runs from C:\\Other\\app.exe, not from pc.toml app_exe".into()
            )
        );
        assert_eq!(
            running_from(
                exe,
                &[Ok(exe.to_owned()), Ok("C:\\Programs\\App\\app2.exe".to_owned())]
            ),
            Err(
                "the predecessor app runs from C:\\Programs\\App\\app2.exe, not from pc.toml app_exe"
                    .into()
            )
        );
        assert_eq!(
            running_from(exe, &[Err("access denied".to_owned())]),
            Err("the predecessor app's image: access denied".into())
        );
    }

    /// Windows compares paths without case and takes either separator.
    #[test]
    fn the_image_compares_without_case_or_separator_style() {
        let exe = "C:\\Programy\\Aplikácia\\app.exe";
        assert_eq!(
            running_from(exe, &[Ok("c:/PROGRAMY/APLIKÁCIA/App.EXE".to_owned())]),
            Ok(())
        );
        assert_eq!(
            running_from("C:/Programy/Aplikácia/app.exe", &[Ok(exe.to_owned())]),
            Ok(())
        );
        assert!(running_from(exe, &[Ok("C:\\Programy\\Aplikacia\\app.exe".to_owned())]).is_err());
    }

    #[test]
    fn exactly_one_process() {
        assert_eq!(one_pid(&[12], "the app"), Ok(12));
        assert_eq!(one_pid(&[], "the app"), Err("the app does not run".into()));
        assert_eq!(
            one_pid(&[12, 17], "the app"),
            Err("the app runs 2 times".into())
        );
        assert_eq!(
            one_pid(&[1, 2, 3], "REAPER"),
            Err("REAPER runs 3 times".into())
        );
    }

    #[test]
    fn holders_read_as_a_list() {
        assert_eq!(holders_text(&[]), "");
        assert_eq!(holders_text(&[(12, "a.exe".to_owned())]), "a.exe (12)");
        assert_eq!(
            holders_text(&[(12, "a.exe".to_owned()), (13, "b.exe".to_owned())]),
            "a.exe (12), b.exe (13)"
        );
    }

    #[test]
    fn log_times_are_utc_milliseconds() {
        let t = |s: &str| line_time_ms(&format!("{s}  INFO app::tray: tray exit"));
        assert_eq!(t("1970-01-01T00:00:00.000000Z"), Some(0));
        assert_eq!(t("1970-03-01T00:00:00Z"), Some(5_097_600_000));
        assert_eq!(t("2000-02-29T12:34:56.789Z"), Some(951_827_696_789));
        assert_eq!(t("2000-03-01T00:00:00.0Z"), Some(951_868_800_000));
        assert_eq!(t("1999-12-31T23:59:59.999999Z"), Some(946_684_799_999));
        assert_eq!(t("2024-01-31T01:02:03.004Z"), Some(1_706_662_923_004));
        assert_eq!(t("2026-09-27T12:00:00.5Z"), Some(1_790_510_400_500));
        assert_eq!(t("2100-02-28T23:59:59Z"), Some(4_107_542_399_000));
        assert_eq!(t("2100-03-01T00:00:00Z"), Some(4_107_542_400_000));
        assert_eq!(t("2026-12-31T00:00:00Z"), Some(1_798_675_200_000));
    }

    #[test]
    fn other_lines_have_no_time() {
        for bad in [
            "",
            "tray exit",
            "2026-09-27T12:00:00",
            "2026-09-27 12:00:00Z",
            "2026-09-27T12:00:00+02:00",
            "1969-12-31T23:59:59Z",
            "2026-00-10T00:00:00Z",
            "2026-13-10T00:00:00Z",
            "2026-01-00T00:00:00Z",
            "2026-01-32T00:00:00Z",
            "2026-01-01T24:00:00Z",
            "2026-01-01T00:60:00Z",
            "2026-01-01T00:00:60Z",
            "2026-01-01T00:00:00.1x2Z",
            "2026-01-01T00:00Z",
            "2026-01-01T00:00:00:00Z",
            "2026-01-01-01T00:00:00Z",
            "2026-01T00:00:00Z",
            "26-01-01T00:00:00Z",
            "2026-1-01T00:00:00Z",
            "2026-01-01T0:00:00Z",
            "+026-01-01T00:00:00Z",
        ] {
            assert_eq!(line_time_ms(bad), None, "{bad:?}");
        }
        // The upper edges are accepted.
        assert!(line_time_ms("2026-12-31T23:59:59.999Z").is_some());
        assert!(line_time_ms("1970-01-01T00:00:00Z").is_some());
    }

    #[test]
    fn the_line_counts_only_at_or_after_the_command() {
        let log = "2026-09-27T12:00:00.100Z  INFO app: started\n\
                   2026-09-27T12:00:05.000Z  INFO app::tray: tray exit\n";
        let at = 1_790_510_405_000;
        assert!(logged_after(log, "tray exit", at));
        assert!(logged_after(log, "tray exit", at - 1));
        assert!(!logged_after(log, "tray exit", at + 1));
        assert!(!logged_after(log, "other line", at - 10_000));
        // The wanted text on an unstamped line does not count.
        assert!(!logged_after("tray exit\n", "tray exit", 0));
        // A stamped line without the text does not count either.
        assert!(!logged_after(
            "2026-09-27T12:00:06.000Z  INFO app: started\n",
            "tray exit",
            at
        ));
    }

    #[test]
    fn the_newest_file_wins_and_a_tie_goes_to_the_later_name() {
        let f = |v: &[(&str, u64)]| -> Vec<(String, u64)> {
            v.iter().map(|(n, t)| ((*n).to_owned(), *t)).collect()
        };
        assert_eq!(newest(&[]), None);
        assert_eq!(
            newest(&f(&[("a.log.1", 10), ("a.log.3", 30), ("a.log.2", 20)])),
            Some("a.log.3")
        );
        assert_eq!(
            newest(&f(&[("a.log.2", 30), ("a.log.1", 30)])),
            Some("a.log.2")
        );
        assert_eq!(newest(&f(&[("b", 31), ("a", 30)])), Some("b"));
    }

    #[test]
    fn a_temp_file_at_or_after_the_command_is_a_cut_write() {
        let files = vec![
            ("members.json".to_owned(), 500),
            ("state.TMP".to_owned(), 100),
        ];
        assert!(newer_temp(&files, 100));
        assert!(newer_temp(&files, 99));
        assert!(!newer_temp(&files, 101));
        // Only temp files count, however new the others are.
        assert!(!newer_temp(&[("members.json".to_owned(), 500)], 100));
        assert!(!newer_temp(&[], 0));
        assert!(newer_temp(&[("x.tmp".to_owned(), 0)], 0));
    }
}
