//! The band's address (design §5.2 step 8, §6): `/api/version` naming the
//! bundle, cloudflared's `/ready`, the app's member list; `iem-server`'s
//! CLI (`notify`, the PIN freeze of its config); and HTTPS checks through
//! Windows' own `curl.exe` (schannel with the system's roots), so the guard
//! carries no TLS stack of its own. Plain HTTP on this PC goes through
//! `ureq`.

use serde_json::Value;

use crate::plan::Mode;

/// A local URL on port 80 (an IP host passes the servers' HTTPS redirect).
pub fn local_url(path: &str) -> String {
    format!("http://127.0.0.1{path}")
}

pub fn https_url(host: &str, path: &str) -> String {
    format!("https://{host}{path}")
}

pub fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// `iem-server`'s `/api/version` names the bundle: its `git_hash` (a short
/// commit, at least 7 hex digits) starts `sha`.
pub fn version_matches(body: &str, sha: &str) -> Result<(), String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("/api/version: {e}"))?;
    let hash = v
        .get("git_hash")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let named = hash.len() >= 7
        && hash.bytes().all(|b| b.is_ascii_hexdigit())
        && sha
            .get(..hash.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(hash));
    if named {
        Ok(())
    } else {
        Err(format!("/api/version names {hash:?}, not {sha}"))
    }
}

/// cloudflared's `/ready` (`{"status":200,"readyConnections":N}`): N; 0 for
/// anything else (a 503 body says 0 too).
pub fn ready_connections(body: &str) -> u64 {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("readyConnections").and_then(Value::as_u64))
        .unwrap_or(0)
}

/// The length of `/api/members` (a JSON array).
pub fn member_count(body: &str) -> Option<usize> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .as_array()
        .map(Vec::len)
}

/// Whether `/api/members` lists the expected count; `Some` says why not.
pub fn members_problem(body: &str, expected: u32) -> Option<String> {
    match member_count(body) {
        Some(n) if u32::try_from(n) == Ok(expected) => None,
        Some(n) => Some(format!("/api/members lists {n}, expected {expected}")),
        None => Some("/api/members is not a list".to_owned()),
    }
}

/// Windows' curl for an HTTPS GET: silent but for errors, failing on an
/// HTTP error status, bounded in time. `local` resolves the public host to
/// this PC, so the LAN's port 443 is checked with the public certificate.
pub fn curl_args(host: &str, path: &str, local: bool, max_s: u32) -> Vec<String> {
    let mut args = vec![
        "--silent".to_owned(),
        "--show-error".to_owned(),
        "--fail".to_owned(),
        "--max-time".to_owned(),
        max_s.to_string(),
    ];
    if local {
        args.push("--resolve".to_owned());
        args.push(format!("{host}:443:127.0.0.1"));
    }
    args.push(https_url(host, path));
    args
}

/// `IEMMIXER_MODE` of the server.
pub fn server_mode(mode: Mode) -> Result<&'static str, String> {
    match mode {
        Mode::Dev => Ok("dev"),
        Mode::Live => Ok("live"),
        Mode::Event => Err("the server does not run in event mode".into()),
    }
}

/// Before cutover PINs change only in the predecessor (design §5.4, P9):
/// the server's config must say `pin_changes = false`, or the server is not
/// started.
pub fn pins_frozen(config: &str) -> Result<(), String> {
    let t: toml::Table = toml::from_str(config).map_err(|e| format!("server config: {e}"))?;
    match t.get("pin_changes") {
        Some(toml::Value::Boolean(false)) => Ok(()),
        _ => Err("the server config does not set pin_changes = false".into()),
    }
}

/// `iem-server notify --count alarm`: the number it printed (exit 0).
pub fn recipients(code: Option<i32>, stdout: &str) -> Option<u32> {
    if code == Some(0) {
        stdout.trim().parse().ok()
    } else {
        None
    }
}

/// `iem-server notify --to …`: 0 sent, 3 no device took it, else an error.
pub fn notify_result(code: Option<i32>, stderr: &str) -> Result<(), String> {
    match code {
        Some(0) => Ok(()),
        Some(3) => Err(format!("no device took the notice: {}", stderr.trim())),
        other => Err(format!(
            "iem-server notify ended with {other:?}: {}",
            stderr.trim()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn urls_are_local_or_https() {
        assert_eq!(local_url("/api/version"), "http://127.0.0.1/api/version");
        assert_eq!(
            https_url("mixer.example.org", "/api/version"),
            "https://mixer.example.org/api/version"
        );
    }

    #[test]
    fn success_is_2xx() {
        assert!(is_success(200));
        assert!(is_success(299));
        assert!(!is_success(199));
        assert!(!is_success(300));
        assert!(!is_success(503));
    }

    #[test]
    fn the_version_names_the_bundle_by_a_short_hash() {
        let body = |hash: &str| format!(r#"{{"version":"2.0.0","git_hash":"{hash}"}}"#);
        assert_eq!(version_matches(&body("0123456"), SHA), Ok(()));
        assert_eq!(version_matches(&body("0123456789AB"), SHA), Ok(()));
        assert_eq!(version_matches(&body(SHA), SHA), Ok(()));
        for bad in [
            "012345",
            "1234567",
            "unknown",
            "",
            "0123456789abcdef0123456789abcdef012345670",
        ] {
            assert_eq!(
                version_matches(&body(bad), SHA),
                Err(format!("/api/version names {bad:?}, not {SHA}")),
                "{bad}"
            );
        }
        // Hex only: a non-hex prefix of equal text never matches.
        assert!(version_matches(&body("zzzzzzz"), "zzzzzzzz").is_err());
        assert!(
            version_matches("{not json", SHA)
                .unwrap_err()
                .starts_with("/api/version: ")
        );
        assert!(version_matches(r#"{"version":"2.0.0"}"#, SHA).is_err());
    }

    #[test]
    fn the_tunnel_is_ready_with_a_connection() {
        assert_eq!(
            ready_connections(r#"{"status":200,"readyConnections":4}"#),
            4
        );
        assert_eq!(
            ready_connections(r#"{"status":503,"readyConnections":0}"#),
            0
        );
        assert_eq!(ready_connections(r#"{"status":200}"#), 0);
        assert_eq!(ready_connections("garbage"), 0);
    }

    #[test]
    fn members_are_counted() {
        assert_eq!(
            member_count(r#"[{"id":"member1"},{"id":"member2"}]"#),
            Some(2)
        );
        assert_eq!(member_count("[]"), Some(0));
        assert_eq!(member_count(r#"{"members":[]}"#), None);
        assert_eq!(member_count("<html>"), None);
    }

    #[test]
    fn the_member_list_has_the_expected_count() {
        let two = r#"[{"id":"member1"},{"id":"member2"}]"#;
        assert_eq!(members_problem(two, 2), None);
        assert_eq!(
            members_problem(two, 3),
            Some("/api/members lists 2, expected 3".into())
        );
        assert_eq!(
            members_problem(two, 1),
            Some("/api/members lists 2, expected 1".into())
        );
        assert_eq!(
            members_problem("{}", 2),
            Some("/api/members is not a list".into())
        );
    }

    #[test]
    fn curl_checks_https_with_the_public_certificate() {
        assert_eq!(
            curl_args("mixer.example.org", "/api/version", false, 10),
            [
                "--silent",
                "--show-error",
                "--fail",
                "--max-time",
                "10",
                "https://mixer.example.org/api/version"
            ]
        );
        assert_eq!(
            curl_args("mixer.example.org", "/api/version", true, 5),
            [
                "--silent",
                "--show-error",
                "--fail",
                "--max-time",
                "5",
                "--resolve",
                "mixer.example.org:443:127.0.0.1",
                "https://mixer.example.org/api/version"
            ]
        );
    }

    #[test]
    fn the_server_runs_in_dev_or_live() {
        assert_eq!(server_mode(Mode::Dev), Ok("dev"));
        assert_eq!(server_mode(Mode::Live), Ok("live"));
        assert!(server_mode(Mode::Event).is_err());
    }

    #[test]
    fn the_server_starts_only_with_pins_frozen() {
        assert_eq!(pins_frozen("port = 80\npin_changes = false\n"), Ok(()));
        let refused = Err("the server config does not set pin_changes = false".to_owned());
        assert_eq!(pins_frozen("pin_changes = true\n"), refused);
        assert_eq!(pins_frozen("port = 80\n"), refused);
        assert_eq!(pins_frozen("pin_changes = \"false\"\n"), refused);
        // A table of that name is not the setting.
        assert_eq!(pins_frozen("[x]\npin_changes = false\n"), refused);
        assert!(
            pins_frozen("= broken")
                .unwrap_err()
                .starts_with("server config: ")
        );
    }

    #[test]
    fn the_cli_answers_are_read() {
        assert_eq!(recipients(Some(0), "2\n"), Some(2));
        assert_eq!(recipients(Some(0), "0"), Some(0));
        assert_eq!(recipients(Some(1), "2"), None);
        assert_eq!(recipients(None, "2"), None);
        assert_eq!(recipients(Some(0), "two"), None);
        assert_eq!(notify_result(Some(0), ""), Ok(()));
        assert_eq!(
            notify_result(Some(3), "no device took the notice (Alarm)\n"),
            Err("no device took the notice: no device took the notice (Alarm)".into())
        );
        assert_eq!(
            notify_result(Some(1), " boom "),
            Err("iem-server notify ended with Some(1): boom".into())
        );
        assert_eq!(
            notify_result(None, ""),
            Err("iem-server notify ended with None: ".into())
        );
    }
}
