//! The elevated tuning task (S6 design note §5.1; S1c design §7) and the
//! drift check.
//!
//! The guard runs Limited; S1c's tuning module needs elevation, so it runs
//! only as `\iemmixer\iemmixer-tuning` (RunLevel Highest), which reads one
//! verb from a request file the guard writes and answers in a result file:
//!
//! - `guard\tuning\request.json`: `{"id": "<id>", "verb": "enter"}`;
//! - `guard\tuning\result.json`: `{"id": "<id>", "ok": true, "detail": "…"}`;
//! - `guard\tuning\expect.json`: what the module applied for the current
//!   mode, `{"plan": "<power plan GUID>", "services": {"<name>": <start>}}`,
//!   which the guard's drift check compares with native reads (P10: no
//!   PowerShell poll, no elevation).
//!
//! Each file is written whole (temp, then rename); a UTF-8 byte-order mark
//! (Windows PowerShell) is skipped.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

pub const REQUEST: &str = "request.json";
pub const RESULT: &str = "result.json";
pub const EXPECT: &str = "expect.json";

/// What `tuning` answers when the bundle carries no tuning module (S1c has
/// not shipped it): reported, never fatal.
pub const ABSENT: &str = "absent";

/// The task's verbs (design §5.1).
pub const VERBS: [&str; 4] = ["enter", "exit", "state", "apply-tier2"];

pub fn valid_verb(verb: &str) -> bool {
    VERBS.contains(&verb)
}

pub fn request(id: &str, verb: &str) -> String {
    json!({"id": id, "verb": verb}).to_string()
}

fn without_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// The task's answer to request `id`: `None` while there is none (no file
/// yet, another request's answer, or a file being written).
pub fn result_for(text: &str, id: &str) -> Option<Result<String, String>> {
    let v: Value = serde_json::from_str(without_bom(text)).ok()?;
    if v.get("id").and_then(Value::as_str) != Some(id) {
        return None;
    }
    let detail = v
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Some(if v.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(detail)
    } else {
        Err(detail)
    })
}

/// `expect.json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Expect {
    /// The power plan's GUID.
    pub plan: String,
    /// Service start types by service name (the registry's `Start`).
    #[serde(default)]
    pub services: BTreeMap<String, u32>,
}

pub fn parse_expect(text: &str) -> Result<Expect, String> {
    serde_json::from_str(without_bom(text)).map_err(|e| format!("{EXPECT}: {e}"))
}

/// The differences between the tuning module's record and the native
/// reads; `None` when there are none.
pub fn drift(
    expect: &Expect,
    plan: &str,
    services: &BTreeMap<String, Result<u32, String>>,
) -> Option<String> {
    let mut out = Vec::new();
    let want = expect.plan.trim_matches(['{', '}']);
    if !want.eq_ignore_ascii_case(plan) {
        out.push(format!("power plan {plan}, recorded {}", expect.plan));
    }
    for (name, want) in &expect.services {
        match services.get(name) {
            Some(Ok(got)) if got == want => {}
            Some(Ok(got)) => out.push(format!("service {name} start {got}, recorded {want}")),
            Some(Err(e)) => out.push(format!("service {name}: {e}")),
            None => out.push(format!("service {name} was not read")),
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = "01234567-89ab-cdef-0001-020304050607";

    #[test]
    fn the_four_verbs_and_nothing_else() {
        for v in VERBS {
            assert!(valid_verb(v), "{v}");
        }
        for v in ["", "Enter", "apply-tier3", "enter "] {
            assert!(!valid_verb(v), "{v:?}");
        }
    }

    #[test]
    fn a_request_names_its_id_and_verb() {
        assert_eq!(request("r1", "exit"), r#"{"id":"r1","verb":"exit"}"#);
    }

    #[test]
    fn only_the_answer_to_our_request_counts() {
        assert_eq!(
            result_for(r#"{"id":"r1","ok":true,"detail":"entered"}"#, "r1"),
            Some(Ok("entered".into()))
        );
        assert_eq!(
            result_for("\u{feff}{\"id\":\"r1\",\"ok\":true}", "r1"),
            Some(Ok(String::new()))
        );
        assert_eq!(
            result_for(r#"{"id":"r1","ok":false,"detail":"exit failed"}"#, "r1"),
            Some(Err("exit failed".into()))
        );
        assert_eq!(
            result_for(r#"{"id":"r1","detail":"no ok"}"#, "r1"),
            Some(Err("no ok".into()))
        );
        assert_eq!(result_for(r#"{"id":"r0","ok":true}"#, "r1"), None);
        assert_eq!(result_for(r#"{"ok":true}"#, "r1"), None);
        assert_eq!(result_for(r#"{"id":"r1","ok":tr"#, "r1"), None);
        assert_eq!(result_for("", "r1"), None);
    }

    fn expect() -> Expect {
        parse_expect(&format!(
            r#"{{"plan":"{PLAN}","services":{{"SvcA":2,"SvcB":4}}}}"#
        ))
        .unwrap()
    }

    fn read(
        a: Result<u32, String>,
        b: Result<u32, String>,
    ) -> BTreeMap<String, Result<u32, String>> {
        BTreeMap::from([("SvcA".to_owned(), a), ("SvcB".to_owned(), b)])
    }

    #[test]
    fn the_record_parses_with_and_without_services() {
        let e = expect();
        assert_eq!(e.plan, PLAN);
        assert_eq!(e.services.get("SvcB"), Some(&4));
        let bare = parse_expect(&format!("\u{feff}{{\"plan\":\"{PLAN}\"}}")).unwrap();
        assert!(bare.services.is_empty());
        assert!(parse_expect("{}").unwrap_err().starts_with("expect.json: "));
    }

    #[test]
    fn no_drift_when_the_reads_match_the_record() {
        assert_eq!(drift(&expect(), PLAN, &read(Ok(2), Ok(4))), None);
        assert_eq!(
            drift(&expect(), &PLAN.to_uppercase(), &read(Ok(2), Ok(4))),
            None
        );
        let braced = Expect {
            plan: format!("{{{PLAN}}}"),
            ..expect()
        };
        assert_eq!(drift(&braced, PLAN, &read(Ok(2), Ok(4))), None);
        // Services read but not recorded do not count.
        let mut more = read(Ok(2), Ok(4));
        more.insert("SvcC".into(), Ok(3));
        assert_eq!(drift(&expect(), PLAN, &more), None);
    }

    #[test]
    fn every_difference_is_named() {
        let other = "11111111-89ab-cdef-0001-020304050607";
        assert_eq!(
            drift(&expect(), other, &read(Ok(2), Ok(4))),
            Some(format!("power plan {other}, recorded {PLAN}"))
        );
        assert_eq!(
            drift(&expect(), PLAN, &read(Ok(3), Ok(4))),
            Some("service SvcA start 3, recorded 2".into())
        );
        assert_eq!(
            drift(&expect(), PLAN, &read(Ok(2), Err("access denied".into()))),
            Some("service SvcB: access denied".into())
        );
        let mut missing = read(Ok(2), Ok(4));
        missing.remove("SvcA");
        assert_eq!(
            drift(&expect(), PLAN, &missing),
            Some("service SvcA was not read".into())
        );
        assert_eq!(
            drift(&expect(), other, &read(Ok(1), Ok(1))),
            Some(format!(
                "power plan {other}, recorded {PLAN}; service SvcA start 1, recorded 2; \
                 service SvcB start 1, recorded 4"
            ))
        );
    }
}
