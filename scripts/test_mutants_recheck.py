"""Tests for the mutation gate's timeout recheck (#23).

The logs below follow cargo-mutants 27.1.0 (`*** <argv>` / `*** result:`
markers, outcomes.json) and nextest 0.9.146 (`<STATUS> [<secs>s] <binary>
<test>` result lines; fail-fast `terminate = "immediate"` ends the tests still
running with SIGTERM).
"""
from __future__ import annotations

import contextlib
import io
import json
import re
import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path

import mutants_recheck as mr

ROOT = Path(__file__).resolve().parent.parent
CARGO = "/home/runner/.rustup/toolchains/1.98.1-x86_64-unknown-linux-gnu/bin/cargo"
PKG = "--package=iem-server@2.0.0-dev.7 --all-features --all-targets"
ESC = "\x1b"


def log(*results: str, mutant: str = "crates/iem-server/src/notify.rs:58:5: replace push_engineers with ()") -> str:
    """A cargo-mutants log whose test phase prints `results` (nextest lines)."""
    return "\n".join([
        "",
        f"*** {mutant}",
        "",
        "*** mutation diff:",
        "--- crates/iem-server/src/notify.rs",
        "+++ replace push_engineers with ()",
        # A diff context line shaped like a result must not count: it is not test output.
        "         FAIL [   0.001s] iem-server not::a::result",
        "",
        f"*** {CARGO} nextest run --no-run --cargo-profile=mutants --verbose {PKG}",
        "   Compiling iem-server v2.0.0-dev.7",
        "    Finished `mutants` profile [optimized] target(s) in 80.12s",
        "",
        "*** result: Success",
        "",
        f"*** {CARGO} nextest run --cargo-profile=mutants --verbose {PKG}",
        "    Finished `mutants` profile [optimized] target(s) in 0.31s",
        "────────────",
        " Nextest run ID 0b1f with nextest profile: mutants",
        "    Starting 412 tests across 3 binaries",
        *results,
        "────────────",
        "     Summary [  10.050s] 3/412 tests run: 1 passed, 2 failed, 409 not run",
        *[r for r in results if "PASS" not in r and "SLOW [>" not in r],
        "error: test run failed",
        "",
        "*** result: Failure(100)",
        "",
    ])


PASS = "        PASS [   0.004s] (  1/412) iem-server auth::tests::pin_is_checked"
SLOW = "        SLOW [>  5.000s] (─────────) iem-server engine_live_tests::slow"
TIMEOUT = "     TIMEOUT [  10.002s] (  2/412) iem-server engine_live_tests::slow"
FAIL = "        FAIL [   0.120s] (  3/412) iem-server notify::tests::engineers_are_pushed"
SIGTERM = "     SIGTERM [   3.001s] (  4/412) iem-server backup::tests::prune"


class FirstFailureTests(unittest.TestCase):
    def test_a_failing_test_proves_the_catch(self) -> None:
        f = mr.first_failure(log(PASS, FAIL, SIGTERM))
        self.assertEqual(f, mr.Failure("FAIL", "iem-server notify::tests::engineers_are_pushed", True))
        self.assertFalse(mr.needs_recheck(f))

    def test_a_timeout_does_not_and_fail_fast_collateral_is_not_a_failure(self) -> None:
        f = mr.first_failure(log(PASS, SLOW, TIMEOUT, SIGTERM))
        self.assertEqual(f, mr.Failure("TIMEOUT", "iem-server engine_live_tests::slow", False))
        self.assertTrue(mr.needs_recheck(f))

    def test_the_first_failure_decides(self) -> None:
        # A FAIL after the timeout may be a test that handled fail-fast's SIGTERM.
        self.assertEqual(mr.first_failure(log(TIMEOUT, FAIL)).status, "TIMEOUT")
        self.assertEqual(mr.first_failure(log(FAIL, TIMEOUT)).status, "FAIL")

    def test_nextest_termination_signals_are_not_failures_crashes_are(self) -> None:
        for status in ("SIGTERM", "SIGKILL", "TIMEOUT", "TRY 2 TMT"):
            f = mr.first_failure(log(f"{status:>12} [   3.001s] iem-server a::b"))
            self.assertEqual((f.status, f.by_test), (status, False), status)
        for status in ("SIGSEGV", "SIGABRT", "FAIL + LEAK", "LEAK-FAIL", "TRY 2 FAIL"):
            f = mr.first_failure(log(f"{status:>12} [   0.101s] iem-server a::b"))
            self.assertEqual((f.status, f.by_test), (status, True), status)

    def test_passing_and_progress_lines_are_not_failures(self) -> None:
        lines = [PASS, SLOW, "TIMEOUT-PASS [  60.000s] iem-server a::b", "        LEAK [   2.000s] iem-server a::c",
                 " TERMINATING [> 10.000s] (─────────) iem-server a::d", "   FLAKY 2/2 [   1.000s] iem-server a::e"]
        self.assertIsNone(mr.first_failure(log(*lines)))
        self.assertTrue(mr.needs_recheck(None))

    def test_colours_are_ignored(self) -> None:
        coloured = (f"{ESC}[31;1m     TIMEOUT{ESC}[0m [  10.002s] {ESC}[35;1miem-server{ESC}[0m "
                    f"{ESC}[36mengine_live_tests::{ESC}[0m{ESC}[1mslow{ESC}[0m")
        self.assertEqual(mr.first_failure(log(coloured)), mr.Failure("TIMEOUT", "iem-server engine_live_tests::slow", False))

    def test_only_the_test_phase_counts(self) -> None:
        build_only = log(FAIL).split(f"*** {CARGO} nextest run --cargo-profile")[0]
        self.assertIsNone(mr.first_failure(build_only))
        self.assertIsNone(mr.first_failure(""))


class RegexTests(unittest.TestCase):
    NAMES = [
        "crates/iem-server/src/mixer_ws.rs:525:9: delete match arm ClientMsg::ListenStart{..} | ClientMsg::ListenStop in handle",
        "crates/iem-engine/src/engine.rs:88:9: replace <impl Driver for NullRtDriver>::stop with ()",
        "crates/iem-server/src/lib.rs:607:5: replace detect_public_ip -> Option<String> with Some(\"xyzzy\".into())",
        "crates/iem-dsp/src/meter.rs:40:17: replace > with >= in to_db",
        "crates/a.rs:1:1: replace * with / in f; replace + with - ^$#&~[x]",
    ]

    def test_a_name_becomes_a_literal_anchored_pattern(self) -> None:
        for name in self.NAMES:
            pattern = f"^{mr.rust_regex_literal(name)}$"
            self.assertTrue(re.fullmatch(pattern, name), name)
            self.assertFalse(re.fullmatch(pattern, name + " "), name)
            # regex::escape leaves `<` and `>` alone: `\<` is a word boundary in the regex crate.
            self.assertNotIn("\\<", pattern)
            self.assertNotIn("\\>", pattern)
        self.assertEqual(mr.rust_regex_literal("a.b(c)|{d}-e"), r"a\.b\(c\)\|\{d\}\-e")

    def test_the_recheck_selects_every_name_and_keeps_the_main_options(self) -> None:
        argv = mr.recheck_argv(self.NAMES[:2], Path("mutants.out/recheck"),
                               ["--timeout", "120", "--package", "iem-server", "--", "--all-targets"])
        self.assertEqual(argv[:2], ["cargo", "mutants"])
        self.assertEqual(argv.count("--re"), 2)
        self.assertEqual(argv[argv.index("--output") + 1], "mutants.out/recheck")
        self.assertEqual(argv[-6:], ["--timeout", "120", "--package", "iem-server", "--", "--all-targets"])
        self.assertLess(argv.index("--re"), argv.index("--"))


def mutant(name: str, summary: str, log_path: str) -> dict:
    return {
        "scenario": {"Mutant": {"name": name, "package": "iem-server", "file": name.split(":")[0]}},
        "summary": summary,
        "log_path": log_path,
        "diff_path": log_path.replace("log/", "diff/").replace(".log", ".diff"),
        "phase_results": [
            {"phase": "Build", "duration": 80.1, "process_status": "Success", "argv": ["cargo"]},
            {"phase": "Test", "duration": 10.4, "process_status": {"Failure": 100}, "argv": ["cargo"]},
        ],
    }


def write_out(out: Path, entries: list[tuple[str, str, str]]) -> None:
    """A mutants.out directory holding `entries` (name, summary, log text)."""
    (out / "log").mkdir(parents=True, exist_ok=True)
    outcomes = []
    for i, (name, summary, text) in enumerate(entries):
        path = f"log/m{i}.log"
        (out / path).write_text(text, encoding="utf-8")
        outcomes.append(mutant(name, summary, path))
    doc = {"outcomes": outcomes, "total_mutants": len(outcomes), "caught": 0, "missed": 0,
           "timeout": 0, "unviable": 0, "success": 0, "cargo_mutants_version": "27.1.0"}
    (out / "outcomes.json").write_text(json.dumps(doc), encoding="utf-8")


A = "crates/iem-server/src/notify.rs:58:5: replace push_engineers with ()"
B = "crates/iem-server/src/mixer_ws.rs:353:5: replace cleanup with ()"
C = "crates/iem-server/src/engine/wire.rs:72:12: replace > with >= in read_frame"
D = "crates/iem-server/src/view.rs:45:11: replace > with >= in ui_db"


class MainTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.out = Path(self.tmp.name) / "mutants.out"
        self.calls: list[tuple[list[str], dict]] = []

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def fake_cargo(self, entries: list[tuple[str, str, str]], code: int = 0):
        def run(cmd: list[str], env: dict, check: bool) -> subprocess.CompletedProcess:
            self.calls.append((cmd, env))
            write_out(Path(cmd[cmd.index("--output") + 1]) / "mutants.out", entries)
            return subprocess.CompletedProcess(cmd, code)
        return run

    def main(self, run=None, extra: list[str] | None = None) -> tuple[int, str]:
        buf = io.StringIO()
        args = ["--out", str(self.out), "--", "--baseline=skip", "--timeout", "120", "--jobs", "1",
                *(extra or []), "--package", "iem-server", "--all-features", "--", "--all-targets"]
        with contextlib.redirect_stdout(buf):
            code = mr.main(args, run=run or self.fake_cargo([]))
        return code, buf.getvalue()

    def test_no_outcomes_nothing_to_recheck(self) -> None:
        code, text = self.main()
        self.assertEqual(code, 0)
        self.assertIn("nothing to recheck", text)
        self.assertEqual(self.calls, [])

    def test_catches_by_failing_tests_are_kept_without_a_recheck(self) -> None:
        write_out(self.out, [(A, "CaughtMutant", log(FAIL)), (B, "MissedMutant", log(PASS)),
                             (C, "Unviable", "")])
        code, text = self.main()
        self.assertEqual(code, 0)
        self.assertEqual(self.calls, [])
        self.assertIn(f"caught by FAIL iem-server notify::tests::engineers_are_pushed (a failing test): {A}", text)
        self.assertNotIn(B, text)

    def test_a_timeout_catch_is_tested_again_with_the_recheck_profile(self) -> None:
        write_out(self.out, [(A, "CaughtMutant", log(TIMEOUT, SIGTERM)), (B, "CaughtMutant", log(PASS)),
                             (C, "CaughtMutant", log(FAIL)), (D, "MissedMutant", log(PASS))])
        run = self.fake_cargo([(A, "CaughtMutant", log(FAIL, mutant=A)), (B, "CaughtMutant", log(FAIL, mutant=B))])
        code, text = self.main(run)
        self.assertEqual(code, 0, text)
        (cmd, env), = self.calls
        self.assertEqual(env["NEXTEST_PROFILE"], mr.RECHECK_PROFILE)
        self.assertEqual([cmd[i + 1] for i, a in enumerate(cmd) if a == "--re"],
                         [f"^{mr.rust_regex_literal(A)}$", f"^{mr.rust_regex_literal(B)}$"])
        self.assertEqual(cmd[cmd.index("--output") + 1], str(self.out / "recheck"))
        self.assertEqual(cmd[-4:], ["iem-server", "--all-features", "--", "--all-targets"])
        self.assertNotIn("--in-diff", cmd)
        self.assertIn(f"caught by TIMEOUT iem-server engine_live_tests::slow (nextest ended the test): {A} -> tested again", text)
        self.assertIn(f"caught by no failing test in the log: {B} -> tested again", text)
        self.assertIn(f"recheck: caught by FAIL iem-server notify::tests::engineers_are_pushed (a failing test): {A}", text)

    def test_a_mutant_that_survives_its_recheck_fails_the_gate(self) -> None:
        write_out(self.out, [(A, "CaughtMutant", log(TIMEOUT)), (B, "CaughtMutant", log(TIMEOUT))])
        run = self.fake_cargo([(A, "MissedMutant", log(PASS)), (B, "CaughtMutant", log(FAIL))], code=2)
        code, text = self.main(run)
        self.assertEqual(code, 2)
        self.assertIn(f"::error::recheck: MISSED (its first catch was not a failing test): {A}", text)

    def test_a_timeout_again_is_a_hang_and_stays_caught(self) -> None:
        write_out(self.out, [(A, "CaughtMutant", log(TIMEOUT))])
        code, text = self.main(self.fake_cargo([(A, "CaughtMutant", log(TIMEOUT))]))
        self.assertEqual(code, 0, text)
        self.assertIn(f"recheck: caught by TIMEOUT iem-server engine_live_tests::slow (nextest ended the test), a hang: {A}", text)

    def test_a_recheck_timeout_is_exit_3_and_wins_over_missed(self) -> None:
        write_out(self.out, [(A, "CaughtMutant", log(TIMEOUT)), (B, "CaughtMutant", log(TIMEOUT))])
        code, text = self.main(self.fake_cargo([(A, "Timeout", ""), (B, "MissedMutant", log(PASS))], code=3))
        self.assertEqual(code, 3)
        self.assertIn(f"::error::recheck: TIMEOUT: {A}", text)

    def test_a_recheck_that_cannot_show_what_caught_the_mutant_is_an_error(self) -> None:
        # e.g. nextest exiting "no tests to run": cargo-mutants counts it as caught.
        write_out(self.out, [(A, "CaughtMutant", log())])
        code, text = self.main(self.fake_cargo([(A, "CaughtMutant", log())]))
        self.assertEqual(code, 1)
        self.assertIn(f"::error::recheck: caught, but its log names no failing test: {A}", text)

    def test_every_mutant_must_be_tested_again(self) -> None:
        write_out(self.out, [(A, "CaughtMutant", log(TIMEOUT)), (B, "CaughtMutant", log(TIMEOUT))])
        code, text = self.main(self.fake_cargo([(A, "CaughtMutant", log(FAIL))]))
        self.assertEqual(code, 1)
        self.assertIn(f"::error::recheck: not tested again: {B}", text)
        write_out(self.out, [(A, "CaughtMutant", log(TIMEOUT))])
        code, text = self.main(self.fake_cargo([(A, "CaughtMutant", log(FAIL)), (C, "CaughtMutant", log(FAIL))]))
        self.assertEqual(code, 1)
        self.assertIn(f"::error::recheck: tested a mutant it did not select: {C}", text)

    def test_cargo_mutants_failing_to_run_is_an_error(self) -> None:
        write_out(self.out, [(A, "CaughtMutant", log(TIMEOUT))])
        code, text = self.main(self.fake_cargo([], code=1))
        self.assertEqual(code, 1)
        self.assertIn("failed to run (exit 1)", text)

    def test_options_that_would_select_other_mutants_are_refused(self) -> None:
        write_out(self.out, [(A, "CaughtMutant", log(TIMEOUT))])
        for extra in (["--in-diff", "pr.diff"], ["--shard=1/4"], ["--output", "x"], ["-o", "x"]):
            code, text = self.main(extra=extra)
            self.assertEqual(code, 1, extra)
            self.assertIn("::error::", text)
        self.assertEqual(self.calls, [])


def seconds(text: str) -> float:
    m = re.fullmatch(r"(\d+)(ms|s|m)", text)
    assert m, text
    return int(m[1]) * {"ms": 0.001, "s": 1, "m": 60}[m[2]]


def bound(slow: dict) -> float:
    return seconds(slow["period"]) * slow["terminate-after"]


class NextestConfigTests(unittest.TestCase):
    """.config/nextest.toml: the profiles the script and the mutation jobs use."""

    def setUp(self) -> None:
        self.profiles = tomllib.loads((ROOT / ".config" / "nextest.toml").read_text(encoding="utf-8"))["profile"]
        self.mutants = self.profiles["mutants"]

    def test_the_recheck_profile_ends_every_test_later_than_the_mutants_profile(self) -> None:
        recheck = self.profiles[mr.RECHECK_PROFILE]
        self.assertEqual(recheck["inherits"], "mutants")
        # An `all()` override: the mutants profile's per-package overrides would win over a base value.
        (every,) = [o for o in recheck["overrides"] if o["filter"] == "all()" and "slow-timeout" in o]
        mutants_bounds = [bound(self.mutants["slow-timeout"])]
        mutants_bounds += [bound(o["slow-timeout"]) for o in self.mutants.get("overrides", []) if "slow-timeout" in o]
        self.assertGreater(bound(every["slow-timeout"]), max(mutants_bounds))

    def test_every_prioritised_bounded_test_exists(self) -> None:
        names = [n for o in self.mutants.get("overrides", []) if "priority" in o
                 for n in re.findall(r"test\(=([\w:]+)\)", o["filter"])]
        self.assertTrue(names, "no test(=...) names in a priority override")
        sources = "\n".join(p.read_text(encoding="utf-8") for p in (ROOT / "crates").rglob("*.rs"))
        for name in names:
            self.assertRegex(sources, rf"\bfn {name.rsplit('::', 1)[-1]}\(", name)


if __name__ == "__main__":
    unittest.main()
