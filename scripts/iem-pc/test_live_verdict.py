"""Tests for scripts/iem-pc/live_verdict.py (S7 design note §6, plan Task 22):
the live verdict on synthetic records, the ops report job's mapping and the
reading of the expected titles from the spec files. Every value is synthetic:
the titles are invented and mixer.example.org is the placeholder host that no
output may ever hold."""
from __future__ import annotations

import contextlib
import copy
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.append(str(Path(__file__).resolve().parents[1]))
import check_parity_manifest as cpm  # noqa: E402
import live_verdict as lv  # noqa: E402

T0 = 1_790_000_000
T1 = "the probe plays audio within 3 s"
T2 = "the probe reads 1 kHz at the burst level"
T3 = "every listen frame is CELT Opus"
TITLES = [T1, T2, T3]
SITE = "https://mixer.example.org/ws/audio?token=x"
NUMBERS = {"listen_hz": 1000.02, "listen_dbfs": -20.08, "first_audio_ms": 640, "talkback_db": -8.39,
           "limiter_active_s": 2.4, "meter_fps": 10.1, "burst_input_dbfs": -20.03, "opus_frames": 1500}
JOBS = {"begin": "success", "pc": "success", "browser": "success", "end": "success"}
GREEN = ("green: 3 live specs, 7 bursts, listen 1000.02 Hz -20.08 dBFS, first audio 640 ms, talkback -8.39 dB, "
         "limiter 2.4 s, meters 10.1 fps, burst input -20.03 dBFS, opus frames 1500, client log found, push 2 -> 2")


def number(key: str, value: object) -> dict:
    return {"type": "live_number", "description": f"{key}={value}"}


def case(title: str, status: str = "expected", expected: str = "passed", annotations: list | None = None) -> dict:
    """One spec of Playwright 1.58's JSON report with its one test (one project, no retry)."""
    passed = status == "expected" and expected == "passed"
    result = {"workerIndex": 0, "parallelIndex": 0, "status": "passed" if passed else "failed", "duration": 1200,
              "errors": [] if passed else [{"message": f"Error: socket {SITE} closed"}], "stdout": [], "stderr": [],
              "retry": 0, "startTime": "2026-10-08T00:00:00.000Z", "annotations": list(annotations or []),
              "attachments": []}
    if not passed:
        result["error"] = {"message": f"Error: socket {SITE} closed", "stack": f"Error: at {SITE}"}
    test = {"timeout": 240_000, "annotations": list(annotations or []), "expectedStatus": expected,
            "projectId": "chromium", "projectName": "chromium", "results": [result], "status": status}
    return {"title": title, "ok": status in ("expected", "flaky", "skipped"), "tags": [], "tests": [test],
            "id": "0" * 20, "file": "probe.spec.ts", "line": 9, "column": 5}


def results(first: list[dict], nested: list[dict], other: list[dict]) -> dict:
    """A report of two spec files; the first file's second list sits in a describe block."""
    return {"config": {"rootDir": "/runner/e2e/tests/live"},
            "suites": [{"title": "probe.spec.ts", "file": "probe.spec.ts", "line": 0, "column": 0, "specs": first,
                        "suites": [{"title": "a burst", "file": "probe.spec.ts", "line": 20, "column": 6,
                                    "specs": nested}]},
                       {"title": "frames.spec.ts", "file": "frames.spec.ts", "line": 0, "column": 0,
                        "specs": other}],
            "errors": [], "stats": {"startTime": "2026-10-08T00:00:00.000Z", "duration": 60_000, "expected": 3,
                                    "skipped": 0, "unexpected": 0, "flaky": 0}}


def green_results(**status: str) -> dict:
    """Every title passed, the numbers spread over the three tests; `status` maps t1..t3 to another outcome."""
    keys = list(NUMBERS)
    cases = [case(t, status.get(f"t{i}", "expected"), annotations=[number(k, NUMBERS[k]) for k in keys[i - 1::3]])
             for i, t in enumerate(TITLES, 1)]
    return results([cases[0]], [cases[1]], [cases[2]])


class Record:
    """A whole live run inside every check; tests change one part of it."""

    def __init__(self) -> None:
        self.results: object = green_results()
        self.begin: object = {"reason": "ready", "push_before": 2}
        self.pc: object = {"reason": "browser-done"}
        self.evidence: object = {"job_end": "ok", "client_log": "found", "push_after": 2}
        self.bursts: list | None = [{"t": T0 + 60 * i, "exit": 0} for i in range(7)]
        self.titles: list[str] | None = list(TITLES)
        self.jobs: dict = dict(JOBS)

    def verdict(self) -> dict:
        return lv.report(self.results, self.begin, self.pc, self.evidence, self.bursts, self.titles, self.jobs)


class Verdict(unittest.TestCase):
    def red(self, r: Record, first: str) -> dict:
        v = r.verdict()
        self.assertEqual((v["conclusion"], v["first_failure"]), ("failure", first), v["summary"])
        self.assertTrue(v["summary"].startswith(f"red: {first}"), v["summary"])
        return v

    def green(self, r: Record) -> dict:
        v = r.verdict()
        self.assertEqual((v["conclusion"], v["first_failure"]), ("success", None), v["summary"])
        return v

    def cancelled(self, r: Record, summary: str) -> None:
        v = r.verdict()
        self.assertEqual(v, {"conclusion": "cancelled", "summary": summary, "first_failure": None, "numbers": {}})

    def test_a_full_run_with_every_spec_passed_is_green(self):
        v = self.green(Record())
        self.assertEqual(v["summary"], GREEN)
        self.assertEqual(v["numbers"], {"live_specs": 3, "bursts": 7, "push_before": 2, "push_after": 2, **NUMBERS})
        # One burst is enough, and push subscriptions may go down.
        r = Record()
        r.bursts = r.bursts[:1]
        r.evidence["push_after"] = 1
        self.assertIn("1 bursts, ", self.green(r)["summary"])
        self.assertTrue(self.green(r)["summary"].endswith(", push 2 -> 1"))
        # A test outside the read titles that passed changes nothing.
        r = Record()
        r.results["suites"][1]["specs"].append(case("a helper test that passed"))
        self.assertEqual(self.green(r)["summary"], GREEN)

    def test_a_failed_skipped_or_missing_spec_is_red_naming_the_first_title(self):
        for status, word in (("unexpected", "failed"), ("skipped", "skipped"), ("flaky", "flaky"),
                             ("interrupted", "not passed")):
            r = Record()
            r.results = green_results(t2=status)
            self.red(r, f"live test {word}: {T2}")
        # Expected to fail (test.fail) and failed: not a pass.
        r = Record()
        r.results["suites"][1]["specs"] = [case(T3, "expected", expected="failed")]
        self.red(r, f"live test not passed: {T3}")
        # The first title in the specs' order is named, whatever the report's order.
        r = Record()
        r.results = green_results(t1="skipped", t3="unexpected")
        r.results["suites"].reverse()
        self.red(r, f"live test skipped: {T1}")
        # A title the report lacks, also in a describe block.
        r = Record()
        r.results["suites"][0]["suites"][0]["specs"] = []
        self.red(r, f"live test missing: {T2}")
        # A title two spec files share needs two tests, all passed.
        r = Record()
        r.titles = [T1, T2, T3, T2]
        self.red(r, f"live test missing: {T2}")
        r.results["suites"][1]["specs"].append(case(T2))
        self.green(r)
        r.results["suites"][1]["specs"][-1] = case(T2, "unexpected")
        self.red(r, f"live test failed: {T2}")
        # More tests of a title than the specs hold (another project, a repeat): green when all passed.
        r = Record()
        r.results["suites"][1]["specs"].append(case(T1))
        self.green(r)
        r.results["suites"][1]["specs"][-1] = case(T1, "flaky")
        self.red(r, f"live test flaky: {T1}")
        # A test outside the read titles that did not pass (a title the regex cannot read).
        r = Record()
        r.results["suites"][1]["specs"].append(case("a title built at run time", "unexpected"))
        self.red(r, "a live test outside the read titles did not pass")
        # The specs and the report themselves.
        r = Record()
        r.titles = None
        self.red(r, "the live specs are unreadable")
        r.titles = []
        self.red(r, "no live spec titles")
        r = Record()
        r.results = None
        self.red(r, "no browser results")
        for bad in (lv.UNREADABLE, [], {"suites": {}}, {"suites": [[]]}, {"suites": [{"specs": [[]]}]},
                    {"suites": [{"specs": [{"title": 1, "tests": []}]}]},
                    {"suites": [{"specs": [{"title": T1, "tests": [None]}]}]},
                    {"suites": [{"specs": [{"title": T1, "tests": {}}]}]},
                    {"suites": [{"specs": [], "suites": {}}]}):
            r.results = bad
            self.red(r, "the browser results are unreadable")
        # Every test passed, but the browser job did not end well.
        for job in ("failure", "skipped"):
            r = Record()
            r.jobs["browser"] = job
            self.red(r, f"browser job {job}")
        # A browser job cancelled alone is no cut of the PC side: red.
        r = Record()
        r.jobs["browser"] = "cancelled"
        self.red(r, "browser job cancelled")

    def test_no_burst_or_a_refused_burst_is_red(self):
        for bursts, count in ((None, None), ([], 0)):
            r = Record()
            r.bursts = bursts
            v = self.red(r, "no bursts")
            self.assertEqual(v["numbers"].get("bursts"), count)
        for change, first in ((lambda b: b.update(exit=1), "burst 3 exited 1"),
                              (lambda b: b.update(exit=-1), "burst 3 exited -1"),
                              (lambda b: b.update(exit=True), "burst 3 has no exit code"),
                              (lambda b: b.update(exit="0"), "burst 3 has no exit code"),
                              (lambda b: b.pop("exit"), "burst 3 has no exit code")):
            r = Record()
            change(r.bursts[2])
            self.red(r, first)
        for bad in (None, [], 0):
            r = Record()
            r.bursts[2] = bad
            self.red(r, "burst 3 is unreadable")
        # The last burst counts as much as the first.
        r = Record()
        r.bursts[-1]["exit"] = 2
        self.red(r, "burst 7 exited 2")
        r = Record()
        r.pc["reason"] = "burst-refused"
        self.red(r, "pc burst-refused")
        for job in ("failure", "skipped"):
            r = Record()
            r.jobs["pc"] = job
            self.red(r, f"pc job {job}")

    def test_a_missing_or_unreadable_client_log_marker_is_red(self):
        for value in ("missing", "unreadable"):
            r = Record()
            r.evidence["client_log"] = value
            v = self.red(r, f"client log marker {value}")
            self.assertIn(f", client log {value}, ", v["summary"])
        for value in (None, SITE, True):
            r = Record()
            r.evidence["client_log"] = value
            v = self.red(r, "client log marker unknown")
            self.assertNotIn("client log ", v["summary"].split("; ", 1)[1])
        r = Record()
        r.evidence = None
        self.red(r, "no end record")
        for bad in (lv.UNREADABLE, [], "ok"):
            r.evidence = bad
            self.red(r, "the end record is unreadable")

    def test_a_push_subscription_left_behind_is_red(self):
        r = Record()
        r.evidence["push_after"] = 3
        v = self.red(r, "push subscriptions left behind (2 -> 3)")
        self.assertTrue(v["summary"].endswith(", push 2 -> 3"), v["summary"])
        r = Record()
        r.begin["push_before"], r.evidence["push_after"] = 0, 1
        self.red(r, "push subscriptions left behind (0 -> 1)")
        r.evidence["push_after"] = 0
        self.green(r)
        for where, key in (("begin", "push_before"), ("evidence", "push_after")):
            for bad in (None, -1, True, 2.0, "2"):
                r = Record()
                getattr(r, where)[key] = bad
                v = self.red(r, "push counts unreadable")
                self.assertNotIn(key, v["numbers"])
                self.assertNotIn("push ", v["summary"].split("; ", 1)[1])

    def test_a_failed_job_end_is_red(self):
        r = Record()
        r.evidence["job_end"] = "failed"
        self.red(r, "job end failed")
        for value in (None, SITE, 0):
            r.evidence["job_end"] = value
            self.red(r, "job end unknown")
        for job in ("failure", "skipped"):
            r = Record()
            r.jobs["end"] = job
            self.red(r, f"pc-end job {job}")

    def test_a_cancelled_pc_job_left_dev_or_not_free_is_cancelled(self):
        def broken() -> Record:
            """Every check fails: the cancelled mapping must come before all of them."""
            r = Record()
            r.results, r.bursts, r.titles = green_results(t1="unexpected"), [], []
            r.pc["reason"] = "not-finished"
            r.evidence.update(client_log="missing", push_after=9, job_end="failed")
            r.jobs = {k: "failure" for k in JOBS}
            return r

        causes = (
            (lambda r: r.jobs.update(begin="cancelled"), "cancelled: the pc-begin job was cancelled"),
            (lambda r: r.jobs.update(pc="cancelled"), "cancelled: the pc job was cancelled"),
            (lambda r: r.jobs.update(end="cancelled"), "cancelled: the pc-end job was cancelled"),
            (lambda r: r.begin.update(reason="left-dev"), "cancelled: the pc left dev"),
            (lambda r: r.begin.update(reason="not-free"), "cancelled: the pc was not free"),
            (lambda r: r.pc.update(reason="left-dev"), "cancelled: the pc left dev"),
            (lambda r: r.evidence.update(job_end="left-dev"), "cancelled: the pc left dev"),
        )
        for cause, summary in causes:
            for make in (Record, broken):
                r = make()
                cause(r)
                self.cancelled(r, summary)
        # The records may be gone: a cancelled job still reads cancelled.
        r = Record()
        r.begin = r.pc = r.evidence = r.results = None
        r.jobs["pc"] = "cancelled"
        self.cancelled(r, "cancelled: the pc job was cancelled")
        # not-free is the begin record's code only, and a cancelled browser job alone is red.
        r = Record()
        r.pc["reason"] = "not-free"
        self.red(r, "pc unknown")
        r = Record()
        r.jobs["browser"] = "cancelled"
        self.assertEqual(r.verdict()["conclusion"], "failure")

    def test_a_failed_pc_job_that_left_no_record_is_cancelled(self):
        # The guard stops the runner at "ide event": GitHub's conclusion for that job, and whether its
        # always() upload still runs, are unverified (plan Task 10). A failed PC job without its record is
        # a cut, never red; one that succeeded or was skipped and left none is red.
        for key, field, job, missing in (("begin", "begin", "pc-begin", "no begin record"),
                                         ("pc", "pc", "pc", "no pc record"),
                                         ("end", "evidence", "pc-end", "no end record")):
            r = Record()
            setattr(r, field, None)
            r.jobs[key] = "failure"
            self.cancelled(r, f"cancelled: no record of the {job} job")
            # Before every check: the titles, the bursts and the browser job broken too.
            r.titles, r.bursts, r.jobs["browser"] = [], [], "failure"
            self.cancelled(r, f"cancelled: no record of the {job} job")
            for result in ("success", "skipped"):
                r = Record()
                setattr(r, field, None)
                r.jobs[key] = result
                self.red(r, missing)
            r = Record()
            setattr(r, field, lv.UNREADABLE)
            r.jobs[key] = "failure"
            self.assertEqual(r.verdict()["conclusion"], "failure")
        # The browser's report is no PC record.
        r = Record()
        r.results, r.jobs["browser"] = None, "failure"
        self.red(r, "no browser results")

    def test_a_bursts_exit_code_is_printed_only_within_32_bits(self):
        for code, first in ((2 ** 32 - 1, "burst 3 exited 4294967295"), (-(2 ** 31), "burst 3 exited -2147483648"),
                            (2 ** 32, "burst 3 exited abnormally"), (-(2 ** 31) - 1, "burst 3 exited abnormally"),
                            (10 ** 40, "burst 3 exited abnormally")):
            r = Record()
            r.bursts[2]["exit"] = code
            self.red(r, first)

    def test_another_pc_failure_names_its_reason_code(self):
        for code in ("bundle-not-active", "engine-not-up", "no-client", "token-failed", "push-count-failed"):
            r = Record()
            r.begin["reason"] = code
            self.red(r, f"begin {code}")
        for code in ("browser-never-started", "burst-refused", "jobs-unreadable", "not-finished"):
            r = Record()
            r.pc["reason"] = code
            self.red(r, f"pc {code}")
        for bad in (SITE, None, 1):
            r = Record()
            r.begin["reason"] = bad
            self.red(r, "begin unknown")
            r = Record()
            r.pc["reason"] = bad
            self.red(r, "pc unknown")
        r = Record()
        r.begin = None
        self.red(r, "no begin record")
        for bad in (lv.UNREADABLE, [], "ready"):
            r.begin = bad
            self.red(r, "the begin record is unreadable")
        r = Record()
        r.pc = None
        self.red(r, "no pc record")
        r.pc = lv.UNREADABLE
        self.red(r, "the pc record is unreadable")
        # A record that reads ready from a job that did not succeed.
        for job in ("failure", "skipped", SITE, None):
            r = Record()
            r.jobs["begin"] = job
            self.red(r, f"pc-begin job {job if job in ('failure', 'skipped') else 'unknown'}")
        r = Record()
        del r.jobs["begin"]
        self.red(r, "pc-begin job unknown")

    def test_red_names_the_first_failing_check_in_order(self):
        r = Record()
        lacking = "a title the report lacks"
        # The checks in order, and the steps inside each, all broken here; they are broken from
        # the last to the first, so each one is the first failure once broken.
        breaks = [
            (lambda: r.begin.update(reason="engine-not-up"), "begin engine-not-up"),
            (lambda: r.jobs.update(begin="failure"), "pc-begin job failure"),
            (lambda: r.pc.update(reason="not-finished"), "pc not-finished"),
            (lambda: r.bursts[1].update(exit=1), "burst 2 exited 1"),
            (lambda: r.jobs.update(pc="failure"), "pc job failure"),
            (lambda: setattr(r, "titles", None), "the live specs are unreadable"),
            (lambda: setattr(r, "results", lv.UNREADABLE), "the browser results are unreadable"),
            (lambda: r.titles.insert(0, lacking), f"live test missing: {lacking}"),
            (lambda: r.results["suites"][0]["specs"].__setitem__(0, case(T1, "unexpected")), f"live test failed: {T1}"),
            (lambda: r.results["suites"][1]["specs"].__setitem__(0, case(T3, "unexpected")), f"live test failed: {T3}"),
            (lambda: r.results["suites"][1]["specs"].append(case("a title built at run time", "unexpected")),
             "a live test outside the read titles did not pass"),
            (lambda: r.jobs.update(browser="failure"), "browser job failure"),
            (lambda: r.evidence.update(client_log="missing"), "client log marker missing"),
            (lambda: r.evidence.update(push_after=3), "push subscriptions left behind (2 -> 3)"),
            (lambda: r.evidence.update(job_end="failed"), "job end failed"),
            (lambda: r.jobs.update(end="failure"), "pc-end job failure"),
        ]
        for brk, first in reversed(breaks):
            brk()
            self.red(r, first)


class Numbers(unittest.TestCase):
    def numbers(self, *annotations: dict) -> dict:
        r = Record()
        r.results = results([case(T1, annotations=list(annotations))], [case(T2)], [case(T3)])
        return {k: v for k, v in r.verdict()["numbers"].items() if k in lv.NUMBER_KEYS}

    def test_numbers_come_only_from_known_keys_and_finite_values(self):
        self.assertEqual(lv.NUMBER_KEYS, tuple(NUMBERS))
        self.assertEqual(self.numbers(*(number(k, v) for k, v in NUMBERS.items())), NUMBERS)
        # Unknown keys, other annotation types, other shapes.
        self.assertEqual(self.numbers(number("url", 1), number("LISTEN_HZ", 1), number("listen_hz ", 1),
                                      {"type": "info", "description": "listen_hz=1"},
                                      {"type": "live_number", "description": 5}, {"type": "live_number"},
                                      "listen_hz=1", {"description": "listen_hz=1"}), {})
        # Only finite decimal numbers.
        for text in ("nan", "inf", "-inf", "Infinity", "1e999", "-1e999", "1_000", " 5", "5 ", "0x10", "", "+5",
                     "5.", ".5", "1e", "\u0661", f"1 {SITE}", "true", "null"):
            self.assertEqual(self.numbers({"type": "live_number", "description": f"listen_hz={text}"}), {}, text)
        self.assertEqual(self.numbers(number("listen_dbfs", "-1e308")), {"listen_dbfs": -1e308})
        self.assertIs(type(self.numbers(number("listen_dbfs", "-1e308"))["listen_dbfs"]), float)
        # A whole number becomes an integer only below 2**53, where its float is still exact.
        self.assertIs(type(self.numbers(number("opus_frames", "9007199254740992"))["opus_frames"]), float)
        self.assertIs(type(self.numbers(number("opus_frames", "9007199254740991"))["opus_frames"]), int)
        self.assertEqual(self.numbers(number("opus_frames", "1500"), number("meter_fps", "9.75"),
                                      number("talkback_db", "-8.4e0")),
                         {"opus_frames": 1500, "meter_fps": 9.75, "talkback_db": -8.4})
        self.assertIs(type(self.numbers(number("opus_frames", "1500"))["opus_frames"]), int)
        # The first value of a key stays; a failed test's numbers count too.
        self.assertEqual(self.numbers(number("listen_hz", "1000.5"), number("listen_hz", "999")),
                         {"listen_hz": 1000.5})
        r = Record()
        r.results = results([case(T1, "unexpected", annotations=[number("listen_hz", 1003)])],
                            [case(T2, annotations=[number("listen_hz", 1000)])], [case(T3)])
        v = r.verdict()
        self.assertEqual((v["numbers"]["listen_hz"], v["first_failure"]), (1003, f"live test failed: {T1}"))
        self.assertIn("; 3 live specs, 7 bursts, listen 1003 Hz, client log found", v["summary"])
        # Without readable results there are no live numbers, and the counts stay.
        r.results = lv.UNREADABLE
        self.assertEqual(r.verdict()["numbers"], {"live_specs": 3, "bursts": 7, "push_before": 2, "push_after": 2})

    def test_the_summary_shows_two_decimals_and_whole_numbers_bare(self):
        r = Record()
        r.results = results([case(T1, annotations=[number("listen_hz", "1000.0249"), number("listen_dbfs", "-0.001"),
                                                    number("limiter_active_s", "2.50")])], [case(T2)], [case(T3)])
        self.assertEqual(r.verdict()["summary"], "green: 3 live specs, 7 bursts, listen 1000.02 Hz 0 dBFS, limiter 2.5 s, "
                                                 "client log found, push 2 -> 2")


class Privacy(unittest.TestCase):
    def test_the_summary_never_holds_a_tests_error_text(self):
        def outputs() -> list[dict]:
            out = []
            r = Record()
            r.results = green_results(t2="unexpected")  # its error and stack name SITE
            r.results["errors"] = [{"message": f"Error: {SITE}"}]
            r.results["suites"][1]["specs"].append(case(f"a run-time title {SITE}", "unexpected"))
            r.results["suites"][1]["specs"][0]["tests"][0]["annotations"] += [
                {"type": "live_number", "description": f"{SITE}=1"}, {"type": "live_number", "description": SITE},
                {"type": SITE, "description": "listen_hz=1"}]
            out.append(r.verdict())
            r.results["suites"][0]["suites"][0]["specs"][0] = case(T2)
            out.append(r.verdict())  # first: the test outside the titles, named by a fixed text
            for where, key in (("begin", "reason"), ("pc", "reason"), ("evidence", "client_log"),
                               ("evidence", "job_end"), ("evidence", "push_after"), ("begin", "push_before")):
                r = Record()
                getattr(r, where)[key] = SITE
                out.append(r.verdict())
            r = Record()
            r.bursts[0] = {"t": SITE, "exit": SITE}
            out.append(r.verdict())
            r = Record()
            r.jobs = {k: SITE for k in JOBS}
            out.append(r.verdict())
            return out

        firsts = [v["first_failure"] for v in outputs()]
        self.assertEqual(firsts, [f"live test failed: {T2}", "a live test outside the read titles did not pass",
                                  "begin unknown", "pc unknown", "client log marker unknown", "job end unknown",
                                  "push counts unreadable", "push counts unreadable", "burst 1 has no exit code",
                                  "pc-begin job unknown"])
        for v in outputs():
            text = json.dumps(v)
            for site in ("mixer.example.org", "token", "a run-time title", "Error"):
                self.assertNotIn(site, text)


class Titles(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name)

    def spec(self, rel: str, text: str) -> None:
        path = self.dir / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def test_expected_titles_are_read_from_the_spec_files(self):
        a = (r'''import { test, expect } from "../support/fixtures";
test.describe("a burst", () => {
  test("first \"quoted\" title", async ({ page }) => {
    await test.step("a step is no test", async () => { run(); });
  });
});
mytest("an identifier ending in test is no test", () => { run(); });
test('second; with a semicolon', async () => { run(); });
''')
        self.spec("probe.spec.ts", a)
        self.spec("nested/frames.spec.ts", "test(`third title`, async () => { run(); });\n")
        self.spec("zz.test.ts", 'test("fourth, from a .test.ts file", async () => { run(); });\n')
        self.spec("support/relay.ts", 'test("a support file is no spec", async () => { run(); });\n')
        self.spec("notes.md", 'test("a note is no spec")\n')
        self.spec("probe.spec.ts.orig", 'test("a copy is no spec")\n')
        want = ['first "quoted" title', "second; with a semicolon", "third title", "fourth, from a .test.ts file"]
        # Files in path order (nested/ before probe, zz last), titles in source order.
        self.assertEqual(lv.titles(self.dir), [want[2], want[0], want[1], want[3]])
        # The parity checker's own `test(` rule: the same titles from the same text.
        self.assertIs(lv.PW_TEST, cpm.PW_TEST)
        self.assertEqual(set(lv.titles(self.dir)) - {want[2], want[3]}, cpm.playwright_titles(a))
        # A title twice stays twice: each needs its own test.
        self.spec("nested/again.spec.ts", "test(`third title`, async () => { run(); });\n")
        self.assertEqual(lv.titles(self.dir).count("third title"), 2)

    def test_each_files_titles_are_the_parity_checkers_on_every_e2e_spec(self):
        # One rule for both: on the repository's own spec files, a file's titles in source order hold
        # exactly the titles check_parity_manifest reads, one per `test(` it finds.
        specs = sorted((Path(__file__).resolve().parents[2] / "e2e" / "tests").rglob("*.spec.ts"))
        self.assertGreater(len(specs), 10)
        for path in specs:
            text = path.read_text(encoding="utf-8")
            got = lv.file_titles(text)
            self.assertEqual(set(got), cpm.playwright_titles(text), path.name)
            self.assertEqual(len(got), len(cpm.PW_TEST.findall(text)), path.name)
        self.assertEqual(lv.file_titles('test("b", f);\ntest(`a \\` tick`, f);\ntest(\'c\', f);\n'),
                         ["b", "a ` tick", "c"])

    def test_a_missing_folder_or_an_unreadable_spec_cannot_be_read(self):
        self.assertEqual(lv.titles(self.dir), [])
        with self.assertRaises(lv.Bad):
            lv.titles(self.dir / "absent")
        (self.dir / "bad.spec.ts").write_bytes(b'test("\xff", () => { run(); });\n')
        with self.assertRaises(lv.Bad):
            lv.titles(self.dir)


class Main(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name) / "record"
        self.specs = Path(tmp.name) / "live"
        self.dir.mkdir()
        self.specs.mkdir()
        (self.specs / "probe.spec.ts").write_text(
            "".join(f'test("{t}", async () => {{}});\n' for t in TITLES), encoding="utf-8")

    def run_main(self, *argv: str) -> tuple[int, str]:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = lv.main(list(argv))
        return code, out.getvalue()

    def report(self, **jobs: str) -> dict:
        j = {**JOBS, **jobs}
        code, out = self.run_main("report", "--dir", str(self.dir), "--specs", str(self.specs), "--begin", j["begin"],
                                  "--pc", j["pc"], "--browser", j["browser"], "--end", j["end"])
        self.assertEqual(code, 0)
        self.assertEqual(out.count("\n"), 1, out)
        return json.loads(out, parse_constant=lambda c: self.fail(f"{c} is not JSON (jq refuses it)"))

    def write(self, name: str, text: str) -> None:
        (self.dir / name).write_text(text, encoding="utf-8")

    def write_all(self, record: Record) -> None:
        # PowerShell's files: a BOM, CRLF; one burst per line.
        self.write("begin.json", "\ufeff" + json.dumps(record.begin) + "\r\n")
        self.write("pc.json", "\ufeff" + json.dumps(record.pc) + "\r\n")
        self.write("evidence.json", "\ufeff" + json.dumps(record.evidence) + "\r\n")
        self.write("bursts.jsonl", "\ufeff" + "".join(json.dumps(b) + "\r\n" for b in record.bursts))
        self.write("results.json", json.dumps(record.results, indent=2))

    def test_main_report_reads_the_record_and_prints_one_json(self):
        self.assertEqual(self.report()["first_failure"], "no begin record")
        self.assertEqual(self.report(pc="cancelled")["summary"], "cancelled: the pc job was cancelled")
        r = Record()
        self.write_all(r)
        self.assertEqual(self.report(), r.verdict())
        self.assertEqual(self.report()["summary"], GREEN)
        # The titles come from the --specs folder.
        (self.specs / "more.spec.ts").write_text('test("a fourth title", async () => { run(); });\n', encoding="utf-8")
        self.assertEqual(self.report()["first_failure"], "live test missing: a fourth title")
        (self.specs / "more.spec.ts").write_bytes(b'test("\xff", () => { run(); });\n')
        self.assertEqual(self.report()["first_failure"], "the live specs are unreadable")
        (self.specs / "more.spec.ts").unlink()
        # A burst line iemmode could not answer, and a line cut short.
        with (self.dir / "bursts.jsonl").open("ab") as f:
            f.write(b'{"t":1,"exit":}\n{"t":2,"ex\xc3')
        self.assertEqual(self.report()["first_failure"], "burst 8 is unreadable")
        for name, first in (("begin.json", "the begin record is unreadable"), ("pc.json", "the pc record is unreadable"),
                            ("results.json", "the browser results are unreadable"),
                            ("evidence.json", "the end record is unreadable")):
            self.write_all(r)
            self.write(name, "{")
            self.assertEqual(self.report()["first_failure"], first)
        # A cancelled job of the PC side stays cancelled whatever the files hold.
        self.assertEqual(self.report(begin="cancelled")["conclusion"], "cancelled")
        # A left-dev record read from the file.
        self.write_all(r)
        self.write("pc.json", '{"reason":"left-dev"}')
        self.assertEqual(self.report(pc="failure")["summary"], "cancelled: the pc left dev")

    def test_main_report_never_prints_a_non_json_number(self):
        r = Record()
        self.write_all(r)
        self.write("begin.json", '{"reason":"ready","push_before":NaN}')
        self.write("evidence.json", '{"job_end":"ok","client_log":"found","push_after":Infinity}')
        res = copy.deepcopy(r.results)
        res["suites"][1]["specs"][0]["tests"][0]["annotations"].append(number("listen_hz", "1e999"))
        self.write("results.json", json.dumps(res))
        v = self.report()
        self.assertEqual(v["first_failure"], "push counts unreadable")

    def test_main_report_takes_only_the_job_results(self):
        for bad in ("succeeded", SITE, ""):
            argv = ("report", "--dir", str(self.dir), "--specs", str(self.specs), "--begin", bad, "--pc", "success",
                    "--browser", "success", "--end", "success")
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                self.run_main(*argv)
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            self.run_main("report", "--dir", str(self.dir), "--specs", str(self.specs))


if __name__ == "__main__":
    unittest.main()
