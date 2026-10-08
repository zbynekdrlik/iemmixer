//! The command lines of `iemmode` and `iemmixer-guard` (S6 plan Task 10
//! Step 4), their exit codes and output. `iemmode` prints one JSON reply
//! (the alarms included, spec §4.2) and exits 0 ok, 1 refused or failed,
//! 2 usage, 4 guard unreachable (`iempc event` then runs `event --direct`).

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::bundle;
use crate::pipe::CallError;
use crate::proto::{Reply, Request};

pub const EXIT_OK: u8 = 0;
pub const EXIT_REFUSED: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_UNREACHABLE: u8 = 4;

/// After starting the guard's task, `iemmode` tries the pipe this long…
pub const START_WAIT: Duration = Duration::from_secs(15);
/// …this often.
pub const START_POLL: Duration = Duration::from_millis(500);

pub const IEMMODE_USAGE: &str = "usage: iemmode status | event [--dry-run] [--direct]
  | dev [--build SHA] [--dry-run] | live --build SHA [--trial] [--dry-run]
  | install <zip> | activate <sha> | test-signal <input> <dbfs> <ttl>
  | report <sha> <green|red> <detail> | job-begin <run> | job-end <run>
  | install-site <file> | force-reopen | inject-fault | inject-seh | inject-park | runner-stop
  | probe-task | rehearse-teardown | alarm-test | alarm-ack <id> | quit";

pub const GUARD_USAGE: &str =
    "usage: iemmixer-guard run | install <zip> [--verify-only] | activate <sha>";

/// What `iemmode` does.
#[derive(Debug, Clone, PartialEq)]
pub enum Cli {
    /// One request to the guard over its pipe.
    Ask(Request),
    /// `event --direct`: the event plan in this process, when no guard runs.
    Direct { dry_run: bool },
}

fn sha(s: &str) -> Result<String, String> {
    if bundle::valid_sha(s) {
        Ok(s.to_owned())
    } else {
        Err(format!("{s:?} is not a 40-digit lowercase commit SHA"))
    }
}

fn number(s: &str) -> Result<f64, String> {
    s.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("{s:?} is not a number"))
}

fn whole(s: &str) -> Result<u64, String> {
    s.parse::<u64>()
        .map_err(|_| format!("{s:?} is not a whole number"))
}

/// A path as the guard reads it: absolute (`iemmode` and the guard run in
/// different directories and sessions).
fn absolute(s: &str) -> Result<String, String> {
    std::path::absolute(Path::new(s))
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| format!("{s:?}: {e}"))
}

fn exactly<'a, const N: usize>(rest: &[&'a str]) -> Result<[&'a str; N], String> {
    let n = N;
    <[&'a str; N]>::try_from(rest)
        .map_err(|_| format!("{n} arguments expected, {} given", rest.len()))
}

/// The switches and the options with a value that `rest` names.
#[derive(Debug, Default)]
struct Flags<'a> {
    set: Vec<&'a str>,
    values: Vec<(&'a str, &'a str)>,
}

impl<'a> Flags<'a> {
    fn read(rest: &[&'a str], switches: &[&str], valued: &[&str]) -> Result<Self, String> {
        let mut f = Self::default();
        let mut i = 0;
        while let Some(arg) = rest.get(i).copied() {
            i += 1;
            let seen = f.set.contains(&arg) || f.values.iter().any(|(k, _)| *k == arg);
            if seen {
                return Err(format!("{arg} given twice"));
            }
            if switches.contains(&arg) {
                f.set.push(arg);
            } else if valued.contains(&arg) {
                let value = rest
                    .get(i)
                    .copied()
                    .ok_or_else(|| format!("{arg} needs a value"))?;
                i += 1;
                f.values.push((arg, value));
            } else {
                return Err(format!("unknown argument {arg:?}"));
            }
        }
        Ok(f)
    }

    fn has(&self, flag: &str) -> bool {
        self.set.contains(&flag)
    }

    fn value(&self, key: &str) -> Option<&'a str> {
        self.values.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }
}

/// `iemmode`'s arguments (the program name excluded).
pub fn parse(args: &[String]) -> Result<Cli, String> {
    let (cmd, rest) = args.split_first().ok_or("no command")?;
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    let bare = |req: Request| -> Result<Cli, String> {
        exactly::<0>(&rest)?;
        Ok(Cli::Ask(req))
    };
    let ask = |req: Request| -> Result<Cli, String> { Ok(Cli::Ask(req)) };
    match cmd.as_str() {
        "status" => bare(Request::Status),
        "event" => {
            let f = Flags::read(&rest, &["--dry-run", "--direct"], &[])?;
            let dry_run = f.has("--dry-run");
            if f.has("--direct") {
                Ok(Cli::Direct { dry_run })
            } else {
                ask(Request::Event { dry_run })
            }
        }
        "dev" => {
            let f = Flags::read(&rest, &["--dry-run"], &["--build"])?;
            ask(Request::Dev {
                build: f.value("--build").map(sha).transpose()?,
                dry_run: f.has("--dry-run"),
            })
        }
        "live" => {
            let f = Flags::read(&rest, &["--trial", "--dry-run"], &["--build"])?;
            let build = f.value("--build").ok_or("live needs --build SHA")?;
            ask(Request::Live {
                build: sha(build)?,
                trial: f.has("--trial"),
                dry_run: f.has("--dry-run"),
            })
        }
        "install" => {
            let [zip] = exactly(&rest)?;
            ask(Request::Install {
                zip: absolute(zip)?,
            })
        }
        "activate" => {
            let [s] = exactly(&rest)?;
            ask(Request::Activate { sha: sha(s)? })
        }
        "test-signal" => {
            let [input, dbfs, ttl] = exactly(&rest)?;
            ask(Request::TestSignal {
                input: input.to_owned(),
                dbfs: number(dbfs)?,
                ttl_s: number(ttl)?,
            })
        }
        "report" => {
            let [s, hil, detail] = exactly(&rest)?;
            if hil != "green" && hil != "red" {
                return Err(format!("{hil:?}: green or red"));
            }
            ask(Request::Report {
                sha: sha(s)?,
                hil: hil.to_owned(),
                detail: detail.to_owned(),
            })
        }
        "job-begin" => {
            let [run] = exactly(&rest)?;
            ask(Request::JobBegin { run: whole(run)? })
        }
        "job-end" => {
            let [run] = exactly(&rest)?;
            ask(Request::JobEnd { run: whole(run)? })
        }
        "install-site" => {
            let [file] = exactly(&rest)?;
            ask(Request::InstallSite {
                path: absolute(file)?,
            })
        }
        "alarm-ack" => {
            let [id] = exactly(&rest)?;
            ask(Request::AlarmAck { id: whole(id)? })
        }
        "force-reopen" => bare(Request::ForceReopen),
        "inject-fault" => bare(Request::InjectFault),
        "inject-seh" => bare(Request::InjectSeh),
        "inject-park" => bare(Request::InjectPark),
        "runner-stop" => bare(Request::RunnerStop),
        "probe-task" => bare(Request::ProbeTask),
        "rehearse-teardown" => bare(Request::RehearseTeardown),
        "alarm-test" => bare(Request::AlarmTest),
        "quit" => bare(Request::Quit),
        other => Err(format!("unknown command {other:?}")),
    }
}

/// What `iemmixer-guard` does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardCli {
    Run,
    Install {
        zip: String,
        verify_only: bool,
    },
    /// While no guard runs, from a bundle's own exe: `daemon::activate_offline`.
    Activate {
        sha: String,
    },
}

/// `iemmixer-guard`'s arguments (the program name excluded).
pub fn parse_guard(args: &[String]) -> Result<GuardCli, String> {
    let (cmd, rest) = args.split_first().ok_or("no command")?;
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    match cmd.as_str() {
        "run" => {
            exactly::<0>(&rest)?;
            Ok(GuardCli::Run)
        }
        "install" => {
            let verify_only = rest.contains(&"--verify-only");
            let paths: Vec<&str> = rest
                .iter()
                .copied()
                .filter(|a| *a != "--verify-only")
                .collect();
            let given = rest.len() - paths.len();
            if given > 1 {
                return Err("--verify-only given twice".to_owned());
            }
            let [zip] = exactly(&paths)?;
            Ok(GuardCli::Install {
                zip: zip.to_owned(),
                verify_only,
            })
        }
        "activate" => {
            let [s] = exactly(&rest)?;
            Ok(GuardCli::Activate { sha: sha(s)? })
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

pub fn exit_code(reply: &Reply) -> u8 {
    if reply.ok { EXIT_OK } else { EXIT_REFUSED }
}

/// The reply as `iemmode` prints it: one line of JSON.
pub fn reply_json(reply: &Reply) -> String {
    serde_json::to_string(reply)
        .unwrap_or_else(|e| json!({"ok": false, "detail": e.to_string()}).to_string())
}

/// What `iemmode` prints when no guard answers (exit 4).
pub fn unreachable_json(why: &str) -> String {
    json!({"ok": false, "detail": format!("the guard is unreachable: {why}")}).to_string()
}

/// Calls the guard; when nothing answers on its pipe, `start` runs once
/// (`schtasks /Run` of the guard's task) and the call is tried again every
/// `every` for `limit`. A request is sent at most once: after a broken pipe
/// it is never repeated.
pub fn call_starting(
    mut call: impl FnMut() -> Result<Reply, CallError>,
    start: impl FnOnce() -> Result<(), String>,
    limit: Duration,
    every: Duration,
) -> Result<Reply, String> {
    let first = match call() {
        Ok(reply) => return Ok(reply),
        Err(CallError::Pipe(e)) => return Err(CallError::Pipe(e).to_string()),
        Err(CallError::Connect(e)) => e,
    };
    let started = start().err();
    let begin = Instant::now();
    let mut last = first.to_string();
    while begin.elapsed() < limit {
        thread::sleep(every);
        match call() {
            Ok(reply) => return Ok(reply),
            Err(CallError::Pipe(e)) => return Err(CallError::Pipe(e).to_string()),
            Err(CallError::Connect(e)) => last = e.to_string(),
        }
    }
    Err(match started {
        Some(why) => format!("{last}; the guard's task did not start: {why}"),
        None => format!("{last}; no guard answered within {limit:?}"),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io;

    use super::*;
    use crate::plan::Mode;
    use crate::proto::FrameError;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    fn ask(a: &[&str]) -> Request {
        match parse(&args(a)) {
            Ok(Cli::Ask(r)) => r,
            other => panic!("{a:?}: {other:?}"),
        }
    }

    fn err(a: &[&str]) -> String {
        parse(&args(a)).unwrap_err()
    }

    #[test]
    fn every_command_parses() {
        assert_eq!(ask(&["status"]), Request::Status);
        assert_eq!(ask(&["event"]), Request::Event { dry_run: false });
        assert_eq!(
            ask(&["event", "--dry-run"]),
            Request::Event { dry_run: true }
        );
        assert_eq!(
            ask(&["dev"]),
            Request::Dev {
                build: None,
                dry_run: false
            }
        );
        assert_eq!(
            ask(&["dev", "--build", SHA, "--dry-run"]),
            Request::Dev {
                build: Some(SHA.into()),
                dry_run: true
            }
        );
        assert_eq!(
            ask(&["live", "--trial", "--build", SHA]),
            Request::Live {
                build: SHA.into(),
                trial: true,
                dry_run: false
            }
        );
        assert_eq!(
            ask(&["live", "--build", SHA, "--dry-run"]),
            Request::Live {
                build: SHA.into(),
                trial: false,
                dry_run: true
            }
        );
        assert_eq!(
            ask(&["activate", SHA]),
            Request::Activate { sha: SHA.into() }
        );
        assert_eq!(
            ask(&["test-signal", "mic1", "-30.5", "20"]),
            Request::TestSignal {
                input: "mic1".into(),
                dbfs: -30.5,
                ttl_s: 20.0,
                listen: false
            }
        );
        assert_eq!(
            ask(&["report", SHA, "green", "120 s at 32"]),
            Request::Report {
                sha: SHA.into(),
                hil: "green".into(),
                detail: "120 s at 32".into()
            }
        );
        assert_eq!(
            ask(&["report", SHA, "red", "x"]),
            Request::Report {
                sha: SHA.into(),
                hil: "red".into(),
                detail: "x".into()
            }
        );
        assert_eq!(ask(&["job-begin", "4242"]), Request::JobBegin { run: 4242 });
        assert_eq!(ask(&["job-end", "4242"]), Request::JobEnd { run: 4242 });
        assert_eq!(ask(&["alarm-ack", "7"]), Request::AlarmAck { id: 7 });
        for (word, req) in [
            ("force-reopen", Request::ForceReopen),
            ("inject-fault", Request::InjectFault),
            ("inject-seh", Request::InjectSeh),
            ("inject-park", Request::InjectPark),
            ("runner-stop", Request::RunnerStop),
            ("probe-task", Request::ProbeTask),
            ("rehearse-teardown", Request::RehearseTeardown),
            ("alarm-test", Request::AlarmTest),
            ("quit", Request::Quit),
        ] {
            assert_eq!(ask(&[word]), req, "{word}");
            assert_eq!(err(&[word, "x"]), "0 arguments expected, 1 given", "{word}");
        }
    }

    #[test]
    fn direct_is_the_event_without_the_pipe() {
        assert_eq!(
            parse(&args(&["event", "--direct"])),
            Ok(Cli::Direct { dry_run: false })
        );
        // iempc appends --direct to the arguments that got exit 4.
        assert_eq!(
            parse(&args(&["event", "--dry-run", "--direct"])),
            Ok(Cli::Direct { dry_run: true })
        );
    }

    #[test]
    fn paths_go_to_the_guard_absolute() {
        let Request::Install { zip } = ask(&["install", "bundle.zip"]) else {
            panic!("not an install");
        };
        assert!(Path::new(&zip).is_absolute(), "{zip}");
        assert!(zip.ends_with("bundle.zip"), "{zip}");
        let here = std::env::current_dir().unwrap().join("site.toml");
        assert_eq!(
            ask(&["install-site", "site.toml"]),
            Request::InstallSite {
                path: here.to_string_lossy().into_owned()
            }
        );
        let abs = std::env::temp_dir().join("b.zip");
        let abs = abs.to_string_lossy().into_owned();
        assert_eq!(
            ask(&["install", &abs]),
            Request::Install { zip: abs.clone() }
        );
        assert!(err(&["install", ""]).starts_with("\"\": "));
    }

    #[test]
    fn bad_arguments_are_named() {
        assert_eq!(parse(&[]), Err("no command".to_owned()));
        assert_eq!(err(&["reboot"]), "unknown command \"reboot\"");
        assert_eq!(err(&["status", "now"]), "0 arguments expected, 1 given");
        assert_eq!(err(&["event", "--now"]), "unknown argument \"--now\"");
        assert_eq!(
            err(&["event", "--dry-run", "--dry-run"]),
            "--dry-run given twice"
        );
        assert_eq!(err(&["dev", "--build"]), "--build needs a value");
        // Nothing waits for a quiet stage any more, so nothing skips it (#38).
        assert_eq!(err(&["dev", "--force"]), "unknown argument \"--force\"");
        assert_eq!(
            err(&["dev", "--build", SHA, "--build", SHA]),
            "--build given twice"
        );
        assert_eq!(
            err(&["dev", "--build", "abc"]),
            "\"abc\" is not a 40-digit lowercase commit SHA"
        );
        assert_eq!(err(&["live"]), "live needs --build SHA");
        assert_eq!(err(&["live", "--trial"]), "live needs --build SHA");
        assert_eq!(
            err(&["live", "--build", &SHA.to_uppercase()]),
            format!(
                "{:?} is not a 40-digit lowercase commit SHA",
                SHA.to_uppercase()
            )
        );
        assert_eq!(err(&["activate"]), "1 arguments expected, 0 given");
        assert_eq!(
            err(&["activate", SHA, SHA]),
            "1 arguments expected, 2 given"
        );
        assert_eq!(
            err(&["test-signal", "mic1", "loud", "20"]),
            "\"loud\" is not a number"
        );
        assert_eq!(
            err(&["test-signal", "mic1", "-30", "inf"]),
            "\"inf\" is not a number"
        );
        assert_eq!(
            err(&["test-signal", "mic1", "NaN", "20"]),
            "\"NaN\" is not a number"
        );
        assert_eq!(
            err(&["test-signal", "mic1"]),
            "3 arguments expected, 1 given"
        );
        assert_eq!(
            err(&["report", SHA, "yellow", "x"]),
            "\"yellow\": green or red"
        );
        assert_eq!(err(&["job-begin", "-1"]), "\"-1\" is not a whole number");
        assert_eq!(err(&["job-end", "x"]), "\"x\" is not a whole number");
        assert_eq!(err(&["alarm-ack", "1.5"]), "\"1.5\" is not a whole number");
    }

    /// The listen probe (S7, #10): `--listen` only after the three values,
    /// so a negative dB value never meets a flag parser.
    #[test]
    fn test_signal_takes_a_trailing_listen_only() {
        let signal = |listen| Request::TestSignal {
            input: "mic1".into(),
            dbfs: -20.0,
            ttl_s: 30.0,
            listen,
        };
        assert_eq!(
            ask(&["test-signal", "mic1", "-20", "30", "--listen"]),
            signal(true)
        );
        assert_eq!(ask(&["test-signal", "mic1", "-20", "30"]), signal(false));
        for wrong in [
            ["test-signal", "--listen", "mic1", "-20", "30"],
            ["test-signal", "mic1", "-20", "30", "--loud"],
            ["test-signal", "mic1", "-20", "--listen", "30"],
        ] {
            assert_eq!(err(&wrong), "3 arguments expected, 4 given", "{wrong:?}");
        }
        assert_eq!(
            err(&["test-signal", "mic1", "-20", "30", "--listen", "--listen"]),
            "3 arguments expected, 4 given"
        );
        assert_eq!(
            err(&["test-signal", "mic1", "-20", "--listen"]),
            "3 arguments expected, 2 given"
        );
        assert!(IEMMODE_USAGE.contains("| test-signal <input> <dbfs> <ttl> [--listen]\n"));
    }

    #[test]
    fn the_guard_runs_or_installs() {
        assert_eq!(parse_guard(&args(&["run"])), Ok(GuardCli::Run));
        assert_eq!(
            parse_guard(&args(&["install", "b.zip"])),
            Ok(GuardCli::Install {
                zip: "b.zip".into(),
                verify_only: false
            })
        );
        for a in [
            ["install", "b.zip", "--verify-only"],
            ["install", "--verify-only", "b.zip"],
        ] {
            assert_eq!(
                parse_guard(&args(&a)),
                Ok(GuardCli::Install {
                    zip: "b.zip".into(),
                    verify_only: true
                })
            );
        }
        assert_eq!(parse_guard(&[]), Err("no command".to_owned()));
        assert_eq!(
            parse_guard(&args(&["run", "now"])),
            Err("0 arguments expected, 1 given".to_owned())
        );
        assert_eq!(
            parse_guard(&args(&["install"])),
            Err("1 arguments expected, 0 given".to_owned())
        );
        assert_eq!(
            parse_guard(&args(&[
                "install",
                "--verify-only",
                "--verify-only",
                "b.zip"
            ])),
            Err("--verify-only given twice".to_owned())
        );
        assert_eq!(
            parse_guard(&args(&["install", "a.zip", "b.zip"])),
            Err("1 arguments expected, 2 given".to_owned())
        );
        assert_eq!(
            parse_guard(&args(&["stop"])),
            Err("unknown command \"stop\"".to_owned())
        );
    }

    /// `activate <sha>` without a guard (#9 2026-09-28), from a bundle's
    /// own exe: the way to a guard too old to activate in event.
    #[test]
    fn the_guard_activates_a_bundle_while_no_guard_runs() {
        assert_eq!(
            parse_guard(&args(&["activate", SHA])),
            Ok(GuardCli::Activate { sha: SHA.into() })
        );
        assert_eq!(
            parse_guard(&args(&["activate"])),
            Err("1 arguments expected, 0 given".to_owned())
        );
        assert_eq!(
            parse_guard(&args(&["activate", SHA, SHA])),
            Err("1 arguments expected, 2 given".to_owned())
        );
        let upper = SHA.to_uppercase();
        assert_eq!(
            parse_guard(&args(&["activate", upper.as_str()])),
            Err(format!("{upper:?} is not a 40-digit lowercase commit SHA"))
        );
        assert_eq!(
            GUARD_USAGE,
            "usage: iemmixer-guard run | install <zip> [--verify-only] | activate <sha>"
        );
    }

    fn reply(ok: bool) -> Reply {
        Reply {
            ok,
            mode: Mode::Dev,
            switching: None,
            alarms: Vec::new(),
            detail: "d".into(),
            engine: None,
            guard_build: None,
            last_switch: None,
        }
    }

    #[test]
    fn exit_codes_and_output_follow_the_reply() {
        assert_eq!(exit_code(&reply(true)), 0);
        assert_eq!(exit_code(&reply(false)), 1);
        assert_eq!(
            (EXIT_OK, EXIT_REFUSED, EXIT_USAGE, EXIT_UNREACHABLE),
            (0, 1, 2, 4)
        );
        assert_eq!(
            reply_json(&reply(true)),
            r#"{"ok":true,"mode":"dev","switching":null,"alarms":[],"detail":"d"}"#
        );
        let v: serde_json::Value = serde_json::from_str(&unreachable_json("x")).unwrap();
        assert_eq!(
            v,
            json!({"ok": false, "detail": "the guard is unreachable: x"})
        );
        assert!(IEMMODE_USAGE.starts_with("usage: iemmode status"));
        assert!(IEMMODE_USAGE.contains("| inject-seh | inject-park |"));
        assert!(GUARD_USAGE.contains("--verify-only"));
    }

    fn refused() -> CallError {
        CallError::Connect(io::Error::new(io::ErrorKind::NotFound, "no pipe"))
    }

    #[test]
    fn an_answer_needs_no_start() {
        let starts = Cell::new(0);
        let got = call_starting(
            || Ok(reply(true)),
            || {
                starts.set(starts.get() + 1);
                Ok(())
            },
            Duration::from_secs(1),
            Duration::from_millis(10),
        );
        assert_eq!(got, Ok(reply(true)));
        assert_eq!(starts.get(), 0);
    }

    #[test]
    fn a_missing_guard_is_started_and_asked_again() {
        let calls = Cell::new(0);
        let starts = Cell::new(0);
        let got = call_starting(
            || {
                calls.set(calls.get() + 1);
                if calls.get() < 3 {
                    Err(refused())
                } else {
                    Ok(reply(true))
                }
            },
            || {
                starts.set(starts.get() + 1);
                Ok(())
            },
            Duration::from_secs(2),
            Duration::from_millis(10),
        );
        assert_eq!(got, Ok(reply(true)));
        assert_eq!((calls.get(), starts.get()), (3, 1));
    }

    #[test]
    fn a_guard_that_never_answers_is_unreachable_after_the_limit() {
        let calls = Cell::new(0);
        let t = Instant::now();
        let got = call_starting(
            || {
                calls.set(calls.get() + 1);
                Err(refused())
            },
            || Ok(()),
            Duration::from_millis(300),
            Duration::from_millis(50),
        );
        assert!(t.elapsed() >= Duration::from_millis(300));
        assert!(t.elapsed() < Duration::from_secs(2));
        assert_eq!(
            got,
            Err("no pipe; no guard answered within 300ms".to_owned())
        );
        assert!(calls.get() >= 2, "{}", calls.get());
        let failed_start = call_starting(
            || Err(refused()),
            || Err("access denied".to_owned()),
            Duration::from_millis(50),
            Duration::from_millis(10),
        );
        assert_eq!(
            failed_start,
            Err("no pipe; the guard's task did not start: access denied".to_owned())
        );
    }

    #[test]
    fn a_broken_pipe_is_never_asked_again() {
        let calls = Cell::new(0);
        let got = call_starting(
            || {
                calls.set(calls.get() + 1);
                Err(CallError::Pipe(FrameError::Closed))
            },
            || Ok(()),
            Duration::from_secs(1),
            Duration::from_millis(10),
        );
        assert_eq!(
            got,
            Err("the guard pipe broke: guard pipe closed".to_owned())
        );
        assert_eq!(calls.get(), 1);
        // Also after a start.
        let calls = Cell::new(0);
        let got = call_starting(
            || {
                calls.set(calls.get() + 1);
                if calls.get() == 1 {
                    Err(refused())
                } else {
                    Err(CallError::Pipe(FrameError::Closed))
                }
            },
            || Ok(()),
            Duration::from_secs(1),
            Duration::from_millis(10),
        );
        assert_eq!(
            got,
            Err("the guard pipe broke: guard pipe closed".to_owned())
        );
        assert_eq!(calls.get(), 2);
    }
}
