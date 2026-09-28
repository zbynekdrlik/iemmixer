//! The elevated tasks' files (S6 design note §5.1; `IemPc.psm1`
//! `Invoke-IemTaskRequest`, `.claude/rules/guard.md`) and the drift check.
//!
//! The guard runs Limited; S1c's tuning module and Defender's exclusions
//! need elevation, so they run only as `\iemmixer\iemmixer-tuning` and
//! `\iemmixer\iemmixer-exclude` (RunLevel Highest). Each reads one request
//! the guard writes in the user's root and answers in the elevated root,
//! which the user may only read (an elevated process never writes into the
//! user's root):
//!
//! - `<root>\guard\tasks\<kind>.request.json`: tuning `{"id", "verb"}`,
//!   exclude `{"id", "sha", "keep"}`;
//! - `<elevated root>\tasks\out\<kind>.result.json`: `{"kind", "id", "ok",
//!   "at", "result", "error"}`; the logon task (`\iemmixer\iemmixer-logon`,
//!   G1) needs no request and writes `logon.result.json` at every logon,
//!   which the guard reads ([`logon_result`]);
//! - `<elevated root>\tuning\expect.json` (S1c's module): what it applied for
//!   the current mode, `{"plan": "<power plan GUID>", "services": {"<name>":
//!   <start>}}`, which the guard's drift check compares with native reads
//!   (P10: no PowerShell poll, no elevation).
//!
//! Windows PowerShell writes a UTF-8 byte-order mark: it is skipped.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::pc::{CardHolders, PrefHeld};

/// The tasks' kinds: the request and result files' names.
pub const TUNING: &str = "tuning";
pub const EXCLUDE: &str = "exclude";
/// The logon task (G1): no request, its result only (`logon_result`).
pub const LOGON: &str = "logon";
pub const EXPECT: &str = "expect.json";

/// What `tuning` answers when S1c's module is not installed: reported,
/// never fatal.
pub const ABSENT: &str = "absent";

/// The task's verbs (design §5.1).
pub const VERBS: [&str; 4] = ["enter", "exit", "state", "apply-tier2"];

pub fn valid_verb(verb: &str) -> bool {
    VERBS.contains(&verb)
}

/// `<kind>.request.json`.
pub fn request_name(kind: &str) -> String {
    format!("{kind}.request.json")
}

/// `<kind>.result.json`.
pub fn result_name(kind: &str) -> String {
    format!("{kind}.result.json")
}

pub fn tuning_request(id: &str, verb: &str) -> String {
    json!({"id": id, "verb": verb}).to_string()
}

/// The exclusions of bundle `sha`; those of `keep` (the other pin) stay,
/// every other bundle's go (`Set-IemDefenderExclusion`).
pub fn exclude_request(id: &str, sha: &str, keep: &[String]) -> String {
    json!({"id": id, "sha": sha, "keep": keep}).to_string()
}

fn without_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// The task's answer to request `id` of `kind`: `None` while there is none
/// (no file yet, another request's answer, or a file being written). A
/// `result` that is not text is given as its JSON.
pub fn result_for(text: &str, kind: &str, id: &str) -> Option<Result<String, String>> {
    let v: Value = serde_json::from_str(without_bom(text)).ok()?;
    if v.get("kind").and_then(Value::as_str) != Some(kind)
        || v.get("id").and_then(Value::as_str) != Some(id)
    {
        return None;
    }
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        Some(Ok(match v.get("result") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
        }))
    } else {
        let error = v.get("error").and_then(Value::as_str).unwrap_or_default();
        Some(Err(if error.is_empty() {
            "the task did not succeed".to_owned()
        } else {
            error.to_owned()
        }))
    }
}

/// What the logon task (G1) found of the preference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogonPref {
    /// REAPER's original was there, or the task restored it.
    Original,
    /// Not the original while the driver module was held: not written
    /// (`Restore-IemPref`, the guard's `PrefCheck` rule; #9 2026-09-28).
    Held(PrefHeld),
    /// The task failed, or its restore did not read back.
    Failed(String),
}

/// One run of the logon task; `at` (its UTC time as the task wrote it)
/// tells one run from the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logon {
    pub at: String,
    pub pref: LogonPref,
}

/// A JSON string or number as text.
fn scalar_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// An image name without a trailing ".exe", in any case.
fn bare_image(image: &str) -> &str {
    match image
        .len()
        .checked_sub(4)
        .and_then(|cut| image.split_at_checked(cut))
    {
        Some((stem, ext)) if ext.eq_ignore_ascii_case(".exe") => stem,
        _ => image,
    }
}

/// A holder as `Get-IemModuleHolders` names it (`image:pid`), in the
/// guard's form (`image (pid)`), and whether it is REAPER's image.
fn holder(entry: &str, reaper: &str) -> (String, bool) {
    let (image, name) = match entry.rsplit_once(':') {
        Some((image, pid)) if !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()) => {
            (image, format!("{image} ({pid})"))
        }
        _ => (entry, entry.to_owned()),
    };
    (
        name,
        bare_image(image).eq_ignore_ascii_case(bare_image(reaper)),
    )
}

/// The held preference of a `pref` object whose action is `held`.
fn held_of(pref: &Value, reaper: &str) -> PrefHeld {
    let holders: Vec<(String, bool)> = pref
        .get("holders")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(|entry| holder(entry, reaper))
                .collect()
        })
        .unwrap_or_default();
    PrefHeld {
        value: pref.get("before").and_then(scalar_text),
        by: CardHolders {
            reaper: holders.iter().any(|(_, is_reaper)| *is_reaper),
            names: holders
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        },
    }
}

/// `logon.result.json` (`Invoke-IemTaskRequest -Kind logon`, G1; #9
/// 2026-09-28): `None` unless it is the logon task's, with its `at`.
/// `reaper` is REAPER's image (`pc.toml` `reaper_exe`): a holder with it,
/// in any case and with or without ".exe" (an unreadable holder list names a
/// running REAPER by its process name), is REAPER.
pub fn logon_result(text: &str, reaper: &str) -> Option<Logon> {
    let v: Value = serde_json::from_str(without_bom(text)).ok()?;
    if v.get("kind").and_then(Value::as_str) != Some(LOGON) {
        return None;
    }
    let at = v.get("at").and_then(Value::as_str)?.to_owned();
    let pref = v.get("result").and_then(|r| r.get("pref"));
    let action = pref.and_then(|p| p.get("action")).and_then(Value::as_str);
    let restored = pref.and_then(|p| p.get("ok")).and_then(Value::as_bool) == Some(true);
    let error = v.get("error").and_then(Value::as_str).unwrap_or_default();
    let pref = match (pref, action) {
        (Some(p), Some("held")) => LogonPref::Held(held_of(p, reaper)),
        (Some(_), Some("none" | "restore")) if restored => LogonPref::Original,
        _ if !error.is_empty() => LogonPref::Failed(error.to_owned()),
        (Some(p), Some(_)) => LogonPref::Failed(format!(
            "the preference was not restored: it reads {}",
            p.get("after")
                .and_then(scalar_text)
                .unwrap_or_else(|| "nothing readable".to_owned())
        )),
        _ => LogonPref::Failed("the logon task did not check the preference".to_owned()),
    };
    Some(Logon { at, pref })
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
    fn requests_name_their_id_and_what_they_ask() {
        assert_eq!(tuning_request("r1", "exit"), r#"{"id":"r1","verb":"exit"}"#);
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let keep = vec!["89abcdef0123456789abcdef0123456789abcdef".to_owned()];
        let parsed = |text: String| serde_json::from_str::<Value>(&text).unwrap();
        assert_eq!(
            parsed(exclude_request("r2", sha, &keep)),
            json!({"id": "r2", "sha": sha, "keep": keep})
        );
        assert_eq!(
            parsed(exclude_request("r3", sha, &[])),
            json!({"id": "r3", "sha": sha, "keep": []})
        );
        assert_eq!(request_name(TUNING), "tuning.request.json");
        assert_eq!(result_name(EXCLUDE), "exclude.result.json");
        assert_eq!(
            (TUNING, EXCLUDE, EXPECT),
            ("tuning", "exclude", "expect.json")
        );
    }

    #[test]
    fn only_the_answer_to_our_request_counts() {
        let ok = r#"{"kind":"tuning","id":"r1","ok":true,"at":"x","result":"entered","error":""}"#;
        assert_eq!(result_for(ok, TUNING, "r1"), Some(Ok("entered".into())));
        assert_eq!(result_for(ok, EXCLUDE, "r1"), None, "another kind's");
        assert_eq!(result_for(ok, TUNING, "r0"), None, "another request's");
        assert_eq!(
            result_for(
                "\u{feff}{\"kind\":\"tuning\",\"id\":\"r1\",\"ok\":true,\"result\":null}",
                TUNING,
                "r1"
            ),
            Some(Ok(String::new()))
        );
        let object = result_for(
            r#"{"kind":"exclude","id":"r1","ok":true,"result":{"sha":"a","add":[]}}"#,
            EXCLUDE,
            "r1",
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&object).unwrap(),
            json!({"sha": "a", "add": []})
        );
        assert_eq!(
            result_for(
                r#"{"kind":"tuning","id":"r1","ok":false,"error":"exit failed"}"#,
                TUNING,
                "r1"
            ),
            Some(Err("exit failed".into()))
        );
        assert_eq!(
            result_for(r#"{"kind":"tuning","id":"r1","result":"x"}"#, TUNING, "r1"),
            Some(Err("the task did not succeed".into()))
        );
        assert_eq!(result_for(r#"{"id":"r1","ok":true}"#, TUNING, "r1"), None);
        assert_eq!(
            result_for(r#"{"kind":"tuning","ok":true}"#, TUNING, "r1"),
            None
        );
        assert_eq!(
            result_for(r#"{"kind":"tuning","id":"r1","ok":tr"#, TUNING, "r1"),
            None
        );
        assert_eq!(result_for("", TUNING, "r1"), None);
    }

    const AT: &str = "2026-09-28T06:00:00.1234567Z";

    /// `logon.result.json` as `Invoke-IemTaskRequest -Kind logon` writes it.
    fn logon_json(pref: &str) -> String {
        format!(
            r#"{{"kind":"logon","id":"logon","ok":true,"at":"{AT}","result":{{"tuning":"absent","pref":{pref}}},"error":""}}"#
        )
    }

    fn logon(pref: LogonPref) -> Option<Logon> {
        Some(Logon {
            at: AT.into(),
            pref,
        })
    }

    fn held(value: Option<&str>, reaper: bool, names: &str) -> Option<Logon> {
        logon(LogonPref::Held(PrefHeld {
            value: value.map(str::to_owned),
            by: CardHolders {
                reaper,
                names: names.into(),
            },
        }))
    }

    /// The logon task (G1) follows the guard's PrefCheck rule (#9
    /// 2026-09-28): a preference it did not write under a holder of the
    /// driver module is named, and REAPER among the holders is known by
    /// `pc.toml`'s image (any case, ".exe" or not: an unreadable holder list
    /// names a running REAPER by its process name).
    #[test]
    fn the_logon_result_names_a_preference_left_under_a_holder() {
        assert_eq!(LOGON, "logon");
        assert_eq!(result_name(LOGON), "logon.result.json");
        let both = logon_json(
            r#"{"before":"32","after":"32","kind":"DWord","attempts":0,"action":"held","holders":["reaper.exe:11","spike.exe:99"],"ok":false}"#,
        );
        assert_eq!(
            logon_result(&both, "reaper.exe"),
            held(Some("32"), true, "reaper.exe (11), spike.exe (99)")
        );
        let assumed = logon_json(r#"{"before":"32","action":"held","holders":["REAPER:11"]}"#);
        for image in ["reaper.exe", "Reaper.EXE", "reaper"] {
            assert_eq!(
                logon_result(&assumed, image),
                held(Some("32"), true, "REAPER (11)"),
                "{image}"
            );
        }
        let spike = logon_json(r#"{"before":32,"action":"held","holders":["spike.exe:99"]}"#);
        assert_eq!(
            logon_result(&spike, "reaper.exe"),
            held(Some("32"), false, "spike.exe (99)")
        );
        // A holder without a pid is named as read; an unreadable value is none.
        let odd = logon_json(r#"{"before":null,"action":"held","holders":["odd","odd:x","x:"]}"#);
        assert_eq!(
            logon_result(&odd, "reaper.exe"),
            held(None, false, "odd, odd:x, x:")
        );
        // "reaperx.exe" is not REAPER.
        let near = logon_json(r#"{"before":"32","action":"held","holders":["reaperx.exe:5"]}"#);
        assert_eq!(
            logon_result(&near, "reaper.exe"),
            held(Some("32"), false, "reaperx.exe (5)")
        );
        // Windows PowerShell's byte-order mark is skipped.
        assert_eq!(
            logon_result(&format!("\u{feff}{both}"), "reaper.exe"),
            held(Some("32"), true, "reaper.exe (11), spike.exe (99)")
        );
    }

    #[test]
    fn the_logon_result_at_the_original_or_failed() {
        for action in ["none", "restore"] {
            let text = logon_json(&format!(
                r#"{{"before":"32","after":"64","attempts":1,"action":"{action}","holders":[],"ok":true}}"#
            ));
            assert_eq!(
                logon_result(&text, "reaper.exe"),
                logon(LogonPref::Original),
                "{action}"
            );
        }
        let failed = |why: &str| logon(LogonPref::Failed(why.into()));
        let not_restored = logon_json(
            r#"{"before":"32","after":"32","attempts":3,"action":"restore","holders":[],"ok":false}"#,
        );
        assert_eq!(
            logon_result(&not_restored, "reaper.exe"),
            failed("the preference was not restored: it reads 32")
        );
        let error = format!(
            r#"{{"kind":"logon","id":"logon","ok":false,"at":"{AT}","result":null,"error":"the logon task needs -PrefKey, -PrefName, -PrefOriginal and -Module"}}"#
        );
        assert_eq!(
            logon_result(&error, "reaper.exe"),
            failed("the logon task needs -PrefKey, -PrefName, -PrefOriginal and -Module")
        );
        let silent = format!(r#"{{"kind":"logon","ok":false,"at":"{AT}"}}"#);
        assert_eq!(
            logon_result(&silent, "reaper.exe"),
            failed("the logon task did not check the preference")
        );
        // Not the logon task's, without its time, or unparsable: none.
        let other = format!(
            r#"{{"kind":"tuning","id":"t","ok":true,"at":"{AT}","result":{{"pref":{{"action":"held","holders":["reaper.exe:11"]}}}}}}"#
        );
        assert_eq!(logon_result(&other, "reaper.exe"), None);
        assert_eq!(
            logon_result(r#"{"kind":"logon","ok":true,"result":null}"#, "reaper.exe"),
            None
        );
        assert_eq!(logon_result(r#"{"kind":"logon","at":"#, "reaper.exe"), None);
        assert_eq!(logon_result("", "reaper.exe"), None);
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
