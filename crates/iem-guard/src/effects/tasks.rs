//! Our scheduled tasks (S6 design note §5.1) through `schtasks.exe`, with
//! arguments only (no shell). The `/Query /V /FO CSV` columns keep their
//! order in every Windows language while their headers and status words do
//! not, so the task's row is found by its name (2nd column) and read by its
//! `Last Run Time` (6th column, compared as text) and numeric `Last Result`
//! (7th column).

/// Starts REAPER (its exe directly, with its project).
pub const REAPER: &str = r"\iemmixer\iemmixer-StartREAPER";
/// Starts the predecessor app's exe directly (never its launcher script).
pub const APP: &str = r"\iemmixer\iemmixer-StartApp";
/// The elevated S1c tuning task (one verb from a request file).
pub const TUNING: &str = r"\iemmixer\iemmixer-tuning";
/// Proves a Limited guard may start our tasks (`cmd /c exit 0`).
pub const PROBE: &str = r"\iemmixer\iemmixer-probe";

/// `SCHED_S_TASK_RUNNING` as a `Last Result`.
pub const RUNNING: i64 = 0x41301;
/// `SCHED_S_TASK_HAS_NOT_RUN` as a `Last Result`.
pub const NOT_RUN: i64 = 0x41303;

pub fn run_args(task: &str) -> [&str; 3] {
    ["/Run", "/TN", task]
}

pub fn query_args(task: &str) -> [&str; 6] {
    ["/Query", "/TN", task, "/FO", "CSV", "/V"]
}

/// The fields of one CSV line: quoted fields may hold commas and doubled
/// quotes.
pub fn csv_fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut field)),
            other => field.push(other),
        }
    }
    out.push(field);
    out
}

/// The task's row of `schtasks /Query /FO CSV /V`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// `Last Run Time` as schtasks prints it (its format is the locale's).
    pub last_run: String,
    /// `Last Result`; `None` when it is not a number.
    pub last_result: Option<i64>,
}

/// The task's row from `schtasks /Query /FO CSV /V` output, found by its
/// name.
pub fn row(csv: &str, task: &str) -> Option<Row> {
    let fields = csv
        .lines()
        .map(csv_fields)
        .find(|f| f.get(1).is_some_and(|name| name.eq_ignore_ascii_case(task)))?;
    Some(Row {
        last_run: fields.get(5)?.trim().to_owned(),
        last_result: fields.get(6).and_then(|v| v.trim().parse().ok()),
    })
}

/// Where the probe task stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Wait,
    Passed,
    Failed(String),
}

pub fn probe(last: Option<i64>) -> Probe {
    match last {
        Some(0) => Probe::Passed,
        None | Some(RUNNING | NOT_RUN) => Probe::Wait,
        Some(code) => Probe::Failed(format!("the probe task ended with {code}")),
    }
}

/// The probe task's run after our `/Run` (design §5.1). Right after `/Run`
/// schtasks may still show the previous run's result, so a result counts
/// only for a newer run: a `Last Run Time` other than the one read before
/// `/Run`, or once the task was seen running (a run time has whole seconds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeWatch {
    before: Option<String>,
    running_seen: bool,
}

impl ProbeWatch {
    /// `before`: the task's row read just before `/Run` (`None`: no row).
    pub fn new(before: Option<&Row>) -> Self {
        Self {
            before: before.map(|r| r.last_run.clone()),
            running_seen: false,
        }
    }

    /// Where the probe stands after one `/Query` (`None`: no row).
    pub fn observe(&mut self, now: Option<&Row>) -> Probe {
        let Some(row) = now else {
            return Probe::Wait;
        };
        if row.last_result == Some(RUNNING) {
            self.running_seen = true;
        }
        let newer = self.running_seen || self.before.as_deref() != Some(row.last_run.as_str());
        if newer {
            probe(row.last_result)
        } else {
            Probe::Wait
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_tasks_are_run_and_queried_by_name() {
        assert_eq!(
            run_args(PROBE),
            ["/Run", "/TN", "\\iemmixer\\iemmixer-probe"]
        );
        assert_eq!(
            query_args(TUNING),
            [
                "/Query",
                "/TN",
                "\\iemmixer\\iemmixer-tuning",
                "/FO",
                "CSV",
                "/V"
            ]
        );
        assert_eq!(RUNNING, 267_009);
        assert_eq!(NOT_RUN, 267_011);
    }

    #[test]
    fn csv_fields_keep_quoted_commas_and_quotes() {
        assert_eq!(csv_fields(r#""a","b,c","d""e""#), ["a", "b,c", "d\"e"]);
        assert_eq!(csv_fields("a,,b"), ["a", "", "b"]);
        assert_eq!(csv_fields(""), [""]);
        assert_eq!(csv_fields(r#""","x""#), ["", "x"]);
        assert_eq!(csv_fields(r#"a"b,c"#), ["ab,c"]);
    }

    fn query(task: &str, last: &str) -> String {
        format!(
            "\"HostName\",\"TaskName\",\"Next Run Time\",\"Status\",\"Logon Mode\",\"Last Run Time\",\"Last Result\",\"Author\"\r\n\
             \"PC\",\"{task}\",\"N/A\",\"Ready\",\"Interactive only\",\"27.09.2026 12:00:00\",\"{last}\",\"x, y\"\r\n"
        )
    }

    fn row_of(last_run: &str, last_result: Option<i64>) -> Row {
        Row {
            last_run: last_run.into(),
            last_result,
        }
    }

    #[test]
    fn the_row_is_found_by_the_tasks_name() {
        let at = "27.09.2026 12:00:00";
        assert_eq!(row(&query(PROBE, "0"), PROBE), Some(row_of(at, Some(0))));
        assert_eq!(
            row(&query(PROBE, "267011"), PROBE),
            Some(row_of(at, Some(NOT_RUN)))
        );
        assert_eq!(
            row(&query(PROBE, " -2147024894 "), PROBE),
            Some(row_of(at, Some(-2_147_024_894)))
        );
        assert_eq!(
            row(&query("\\IEMMIXER\\iemmixer-PROBE", "1"), PROBE),
            Some(row_of(at, Some(1)))
        );
        assert_eq!(row(&query(TUNING, "0"), PROBE), None);
        assert_eq!(row(&query(PROBE, "n/a"), PROBE), Some(row_of(at, None)));
        assert_eq!(row("", PROBE), None);
        assert_eq!(row("\"PC\",\"\\iemmixer\\iemmixer-probe\"", PROBE), None);
        // The run time is trimmed; a missing result reads as none.
        assert_eq!(
            row(
                "\"PC\",\"\\iemmixer\\iemmixer-probe\",\"\",\"\",\"\",\" N/A \",\"0\"",
                PROBE
            ),
            Some(row_of("N/A", Some(0)))
        );
        assert_eq!(
            row(
                "\"PC\",\"\\iemmixer\\iemmixer-probe\",\"\",\"\",\"\",\"N/A\"",
                PROBE
            ),
            Some(row_of("N/A", None))
        );
    }

    /// Right after `/Run` schtasks may still show the previous run: its
    /// result, success or failure, is not this run's.
    #[test]
    fn the_probe_counts_only_a_run_after_ours() {
        let before = row_of("27.09.2026 12:00:00", Some(0));
        let mut w = ProbeWatch::new(Some(&before));
        assert_eq!(w.observe(Some(&before)), Probe::Wait);
        assert_eq!(w.observe(None), Probe::Wait);
        assert_eq!(
            w.observe(Some(&row_of("27.09.2026 12:05:00", Some(0)))),
            Probe::Passed
        );
        let mut w = ProbeWatch::new(Some(&before));
        assert_eq!(
            w.observe(Some(&row_of("27.09.2026 12:05:00", Some(1)))),
            Probe::Failed("the probe task ended with 1".into())
        );
        let failed = row_of("27.09.2026 12:00:00", Some(1));
        let mut w = ProbeWatch::new(Some(&failed));
        assert_eq!(w.observe(Some(&failed)), Probe::Wait);
        assert_eq!(
            w.observe(Some(&row_of("27.09.2026 12:05:00", Some(0)))),
            Probe::Passed
        );
    }

    /// A run seen running is ours even when it started within the second
    /// of the previous one (the run time has whole seconds).
    #[test]
    fn a_run_seen_running_counts_within_the_same_second() {
        let before = row_of("27.09.2026 12:00:00", Some(0));
        let mut w = ProbeWatch::new(Some(&before));
        assert_eq!(
            w.observe(Some(&row_of("27.09.2026 12:00:00", Some(RUNNING)))),
            Probe::Wait
        );
        assert_eq!(w.observe(Some(&before)), Probe::Passed);
    }

    #[test]
    fn a_task_that_never_ran_counts_its_first_run() {
        let never = row_of("N/A", Some(NOT_RUN));
        let mut w = ProbeWatch::new(Some(&never));
        assert_eq!(w.observe(Some(&never)), Probe::Wait);
        assert_eq!(
            w.observe(Some(&row_of("27.09.2026 12:05:00", Some(0)))),
            Probe::Passed
        );
        // No row before /Run: any row after it is the new run's.
        let mut w = ProbeWatch::new(None);
        assert_eq!(
            w.observe(Some(&row_of("27.09.2026 12:05:00", Some(0)))),
            Probe::Passed
        );
    }

    #[test]
    fn the_probe_passes_on_zero_and_waits_while_it_runs() {
        assert_eq!(probe(Some(0)), Probe::Passed);
        assert_eq!(probe(None), Probe::Wait);
        assert_eq!(probe(Some(RUNNING)), Probe::Wait);
        assert_eq!(probe(Some(NOT_RUN)), Probe::Wait);
        assert_eq!(
            probe(Some(1)),
            Probe::Failed("the probe task ended with 1".into())
        );
        assert_eq!(
            probe(Some(-2_147_024_891)),
            Probe::Failed("the probe task ended with -2147024891".into())
        );
    }
}
