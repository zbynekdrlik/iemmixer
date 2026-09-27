//! Our scheduled tasks (S6 design note §5.1) through `schtasks.exe`, with
//! arguments only (no shell). The `/Query /V /FO CSV` columns keep their
//! order in every Windows language while their headers and status words do
//! not, so the task's row is found by its name (2nd column) and read by its
//! numeric `Last Result` (7th column).

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

/// The task's `Last Result` from `schtasks /Query /FO CSV /V` output.
pub fn last_result(csv: &str, task: &str) -> Option<i64> {
    csv.lines()
        .map(csv_fields)
        .find(|f| f.get(1).is_some_and(|name| name.eq_ignore_ascii_case(task)))
        .and_then(|f| f.get(6).and_then(|v| v.trim().parse().ok()))
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

    #[test]
    fn the_last_result_is_read_from_the_tasks_row() {
        assert_eq!(last_result(&query(PROBE, "0"), PROBE), Some(0));
        assert_eq!(last_result(&query(PROBE, "267011"), PROBE), Some(NOT_RUN));
        assert_eq!(
            last_result(&query(PROBE, " -2147024894 "), PROBE),
            Some(-2_147_024_894)
        );
        assert_eq!(
            last_result(&query("\\IEMMIXER\\iemmixer-PROBE", "1"), PROBE),
            Some(1)
        );
        assert_eq!(last_result(&query(TUNING, "0"), PROBE), None);
        assert_eq!(last_result(&query(PROBE, "n/a"), PROBE), None);
        assert_eq!(last_result("", PROBE), None);
        assert_eq!(
            last_result("\"PC\",\"\\iemmixer\\iemmixer-probe\"", PROBE),
            None
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
