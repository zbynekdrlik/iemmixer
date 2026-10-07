"""Tests for scripts/iem-pc/soak_verdict.py (S7 design note §4, plan Task 7):
the soak verdict on synthetic polls, the ops report job's mapping and CI's
harness check. Every value is synthetic: the SHAs, member9 and the hosts are
placeholders that the summary must never hold."""
from __future__ import annotations

import contextlib
import io
import json
import re
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import soak_verdict as sv  # noqa: E402

SHA = "a" * 40
SHA2 = "b" * 40
T0 = 1_790_000_000
# The engine ran five minutes before the first poll: its counts start there.
BEFORE = 5
PER_MINUTE = 180_000
NUMBERS_ONLY = re.compile(r"^[a-z0-9 .,:;%+()/≥µ-]*$")
HARNESS = {"schema": 1, "build": SHA, "complete": True, "seconds": 28_980.0, "frames": 1_449_000,
           "expected_frames": 1_449_000, "decode_errors": 0, "gaps": 0, "max_gap_ms": 41, "first_frame_ms": 180,
           "meter_frames": 289_800, "reconnects": 0, "no_source": 0, "error": None}
GREEN = ("green: 8.00 h, 481 polls, missed +0, resets +0, late 0.003 % (≥ 347 µs), process p99.9 41 µs, "
         "gaps 0, reconnects 0, frames 1449000")


def pairs(h: dict[int, int]) -> list[list[int]]:
    return [[b, c] for b, c in sorted(h.items())]


def poll(i: int, every: int = 60) -> dict:
    """The poll i (from 0) of a soak inside every bound: 180 000 callbacks a
    minute, 4 a minute at 400 µs (late), every callback 40 µs long."""
    m = i + BEFORE
    return {"t": T0 + i * every, "exit": 0, "status": {
        "ok": True, "mode": "dev", "switching": None, "detail": f"mode dev; bundle {SHA}", "alarms": [],
        "engine": {"build": SHA, "pid": 4242, "callbacks": PER_MINUTE * m, "missed": 0, "resets": 0,
                   "late": 3 * m, "overruns": 0, "process_max_us": 52.5, "hist_top_us": 667,
                   "interval_hist": pairs({333: 179_986 * m, 340: 10 * m, 400: 4 * m}),
                   "process_hist": pairs({40: PER_MINUTE * m})}}}


def polls(n: int = 481, every: int = 60) -> list[dict]:
    return [poll(i, every) for i in range(n)]


def engine(p: dict) -> dict:
    return p["status"]["engine"]


def set_delta(ps: list[dict], key: str, delta: dict[int, int]) -> list[dict]:
    """The last poll's histogram `key` becomes the first poll's plus `delta`."""
    first = dict(engine(ps[0])[key])
    engine(ps[-1])[key] = pairs({b: first.get(b, 0) + delta.get(b, 0) for b in first.keys() | delta.keys()})
    return ps


def add_last(ps: list[dict], key: str, extra: dict[int, int]) -> None:
    """Adds `extra` to the last poll's histogram `key`."""
    last = dict(engine(ps[-1])[key])
    engine(ps[-1])[key] = pairs({b: last.get(b, 0) + extra.get(b, 0) for b in last.keys() | extra.keys()})


def harness(**change) -> dict:
    return {**HARNESS, **change}


class Verdict(unittest.TestCase):
    def red(self, ps: list[dict] | None, first: str, h: dict | None = HARNESS) -> dict:
        v = sv.verdict(ps, h, SHA)
        self.assertEqual(v["conclusion"], "failure", v["summary"])
        self.assertEqual(v["first_failure"], first)
        self.assertTrue(v["summary"].startswith(f"red: {first}; "), v["summary"])
        return v

    def green(self, ps: list[dict], h: dict = HARNESS) -> dict:
        v = sv.verdict(ps, h, SHA)
        self.assertEqual((v["conclusion"], v["first_failure"]), ("success", None), v["summary"])
        return v

    def test_eight_hours_on_one_engine_inside_every_bound_is_green(self):
        v = self.green(polls())
        self.assertEqual(v["summary"], GREEN)
        n = v["numbers"]
        self.assertEqual((n["polled_hours"], n["polls"], n["missed"], n["resets"]), (8.0, 481, 0, 0))
        self.assertEqual((n["intervals"], n["late_intervals"], n["process_p999_us"]), (86_400_000, 1920, 41))
        self.assertEqual((n["frames"], n["expected_frames"], n["gaps"], n["reconnects"]), (1_449_000, 1_449_000, 0, 0))

    def test_less_than_eight_hours_of_polls_is_red_with_the_hours(self):
        self.red(polls(451), "polled 7.50 h of 8 h")
        self.red(polls(480), "polled 7.98 h of 8 h")
        self.assertEqual(sv.verdict(polls(241), HARNESS, SHA, hours=4)["conclusion"], "success")

    def test_a_hole_in_the_polls_is_red(self):
        for extra, first in ((240, None), (241, "no poll for 301 s after poll 200")):
            ps = polls()
            for p in ps[200:]:
                p["t"] += extra
            if first:
                self.red(ps, first)
            else:
                self.green(ps)
        ps = polls()
        ps[10]["t"], ps[11]["t"] = ps[11]["t"], ps[10]["t"]
        self.red(ps, "poll 12 is older than poll 11")
        del ps[10]["t"]
        self.red(ps, "poll 11 has no time")

    def test_another_engine_pid_or_bundle_is_red(self):
        for key, value, first in (("pid", 4243, "poll 301 runs another engine pid"),
                                  ("pid", None, "poll 301 has no engine pid"),
                                  ("build", SHA2, "poll 301 runs another build")):
            ps = polls()
            engine(ps[300])[key] = value
            self.red(ps, first)
        ps = polls()
        engine(ps[0])["pid"] = None
        self.red(ps, "poll 1 has no engine pid")

    def test_a_poll_without_an_engine_or_out_of_dev_is_red(self):
        self.red([], "no polls")
        self.red(None, "no polls")
        cases = (
            (lambda p: p["status"].pop("engine"), "poll 101 has no engine"),
            (lambda p: p["status"].update(mode="event"), "poll 101 is out of dev"),
            (lambda p: p["status"].update(switching={"from": "dev", "to": "event"}), "poll 101 is switching"),
            (lambda p: p.update(exit=1), "poll 101 exited 1"),
            (lambda p: p.update(exit=False), "poll 101 has no exit code"),
            (lambda p: p.pop("status"), "poll 101 is unreadable"),
        )
        for change, first in cases:
            ps = polls()
            change(ps[100])
            self.red(ps, first)
        ps = polls()
        ps[100] = None
        self.red(ps, "poll 101 is unreadable")

    def test_one_missed_period_or_one_reset_is_red(self):
        for key, value, first in (("missed", 1, "missed +1"), ("resets", 1, "resets +1"),
                                  ("missed", "1", "missed of poll 481 is unreadable")):
            ps = polls()
            engine(ps[-1])[key] = value
            v = self.red(ps, first)
        self.assertNotIn("missed", v["numbers"])
        self.assertEqual(sv.verdict(polls(), HARNESS, SHA)["numbers"]["resets"], 0)
        ps = polls()
        engine(ps[0])["missed"] = 2
        engine(ps[-1])["missed"] = 1
        self.red(ps, "missed -1")

    def test_late_counts_intervals_of_347_us_or_more_from_the_histogram(self):
        v = self.green(set_delta(polls(), "interval_hist", {333: 900, 346: 100}))
        self.assertIn("late 0.000 % (≥ 347 µs)", v["summary"])
        self.red(set_delta(polls(), "interval_hist", {333: 9979, 347: 21}), "late 0.210 % (≥ 347 µs) above 0.2 %")
        # The overflow bucket (two periods or more) is late too.
        self.red(set_delta(polls(), "interval_hist", {333: 997, 667: 3}), "late 0.300 % (≥ 347 µs) above 0.2 %")

    def test_late_at_exactly_two_per_mille_is_green_and_one_more_is_red(self):
        v = self.green(set_delta(polls(), "interval_hist", {333: 998, 347: 2}))
        self.assertIn("late 0.200 %", v["summary"])
        self.red(set_delta(polls(), "interval_hist", {333: 997, 347: 3}), "late 0.300 % (≥ 347 µs) above 0.2 %")
        # 2001 of 1 000 000 is shown rounded up: red never reads 0.200 %.
        self.red(set_delta(polls(), "interval_hist", {333: 997_999, 347: 2001}), "late 0.201 % (≥ 347 µs) above 0.2 %")
        self.red(set_delta(polls(), "interval_hist", {}), "no intervals in the soak")

    def test_counts_before_the_first_poll_do_not_count(self):
        ps = polls()
        for p in ps:
            e = engine(p)
            e["missed"], e["resets"] = 5, 2
            e["interval_hist"] = pairs({**dict(e["interval_hist"]), 500: 1_000_000})
            e["process_hist"] = pairs({**dict(e["process_hist"]), 300: 1_000_000})
        self.assertEqual(self.green(ps)["summary"], GREEN)

    def test_a_histogram_that_went_back_or_is_missing_is_red(self):
        ps = polls()
        engine(ps[-1])["interval_hist"] = pairs({**dict(engine(ps[-1])["interval_hist"]), 333: 1})
        self.red(ps, "the interval histogram bucket 333 went back")
        ps = polls()
        engine(ps[0])["process_hist"] = pairs({40: 1, 41: 1})
        self.red(ps, "the process histogram bucket 41 went back")
        for i, key, first in ((-1, "process_hist", "the process histogram of poll 481 is missing"),
                              (0, "interval_hist", "the interval histogram of poll 1 is missing")):
            ps = polls()
            del engine(ps[i])[key]
            self.red(ps, first)
        for bad in ([[333]], [[333, -1]], [[333, 1], [333, 2]], [[True, 1]], {"333": 1}):
            ps = polls()
            engine(ps[-1])["interval_hist"] = bad
            self.red(ps, "the interval histogram of poll 481 is unreadable")
        for i, top, first in ((-1, 347, "the histogram top in poll 481 is not above 347 µs"),
                              (0, None, "the histogram top in poll 1 is not above 347 µs")):
            ps = polls()
            engine(ps[i])["hist_top_us"] = top
            self.red(ps, first)
        ps = polls()
        for p in ps:
            engine(p)["hist_top_us"] = 348
        self.green(ps)

    def test_process_p999_at_the_83_us_edge_is_green_and_one_bucket_more_is_red(self):
        v = self.green(set_delta(polls(), "process_hist", {10: 998, 82: 2}))
        self.assertIn("process p99.9 83 µs", v["summary"])
        self.red(set_delta(polls(), "process_hist", {10: 998, 83: 2}), "process p99.9 84 µs above 83 µs")
        self.red(set_delta(polls(), "process_hist", {}), "no process times in the soak")

    def test_the_quantile_is_the_upper_edge_at_rank_ceil_999_n_per_mille(self):
        # The cases of Rust's iem_audio_io::hist::quantile_us.
        self.assertEqual(sv.quantile_us({10: 998, 82: 1, 83: 1}, 999), 83)
        self.assertEqual(sv.quantile_us({10: 997, 83: 3}, 999), 84)
        self.assertEqual(sv.quantile_us({5: 1}, 1), 6)
        self.assertEqual(sv.quantile_us({5: 1, 9: 1}, 1000), 10)
        self.assertEqual(sv.quantile_us({1: 1, 2: 1, 3: 1}, 500), 3)
        self.assertIsNone(sv.quantile_us({}, 999))
        # Unsorted input, and a bucket emptied by the delta.
        self.assertEqual(sv.quantile_us({83: 3, 10: 997, 5: 0}, 999), 84)
        self.assertEqual(sv.at_or_above({346: 5, 347: 2, 667: 1}, 347), 3)
        self.assertEqual(sv.delta({1: 2, 3: 4}, {1: 2, 3: 9, 5: 1}, "h"), {3: 5, 5: 1})

    def test_harness_gaps_reconnects_or_thin_frames_are_red(self):
        self.red(polls(), "gaps 1", harness(gaps=1))
        self.red(polls(), "reconnects 1", harness(reconnects=1))
        self.green(polls(), harness(frames=990, expected_frames=1000))
        self.red(polls(), "frames 98.90 % of expected", harness(frames=989, expected_frames=1000))
        self.red(polls(), "no listen frames", harness(frames=0, expected_frames=0, gaps=0))

    def test_an_incomplete_or_short_harness_is_red(self):
        self.red(polls(), "no harness summary", None)
        self.red(polls(), "harness incomplete (server-gone)", harness(complete=False, error="server-gone"))
        self.red(polls(), "harness incomplete", harness(complete=False, error="mixer.example.org"))
        self.red(polls(), "harness ran 28799.9 s of 28800 s", harness(seconds=28_799.9))
        self.green(polls(), harness(seconds=28_800))
        for bad in ({"frames": "1"}, {"seconds": True}, {"complete": 1}, {"gaps": -1}):
            self.red(polls(), "the harness summary is unreadable", harness(**bad))
        self.red(polls(), "the harness summary is unreadable", [])

    def test_red_names_the_first_failing_number_in_order(self):
        ps, h = polls(), harness()
        last = engine(ps[-1])
        # The verdict's checks in order, each broken here; they are broken from
        # the last to the first, so each one is the first failure once broken.
        breaks = [
            (lambda: ps[1].update(exit=1), "poll 2 exited 1"),
            (lambda: engine(ps[1]).update(build=SHA2), "poll 2 runs another build"),
            (lambda: [p.update(t=p["t"] + 400) for p in ps[100:]], "no poll for 460 s after poll 100"),
            (lambda: last.update(missed=1), "missed +1"),
            (lambda: last.update(resets=2), "resets +2"),
            (lambda: last.pop("process_hist"), "the process histogram of poll 481 is missing"),
            (lambda: add_last(ps, "interval_hist", {400: 200_000}), "late 0.234 % (≥ 347 µs) above 0.2 %"),
            (lambda: add_last(ps, "process_hist", {100: 200_000}), "process p99.9 101 µs above 83 µs"),
            (lambda: h.update(complete=False, error="server-gone"), "harness incomplete (server-gone)"),
            (lambda: h.update(gaps=2), "gaps 2"),
            (lambda: h.update(reconnects=3), "reconnects 3"),
            (lambda: h.update(frames=1000), "frames 0.06 % of expected"),
        ]
        for brk, first in reversed(breaks):
            brk()
            self.red(ps, first, h)
        self.assertIn("missed +1, resets +2,", sv.verdict(ps, h, SHA)["summary"])

    def test_the_summary_holds_numbers_only(self):
        ps = polls()
        for p in ps:
            p["status"]["alarms"] = [{"id": 7, "at": T0 + 60, "text": "tuning drift: member9 at http://10.0.0.10"}]
        bad = harness(complete=False, error="member9")
        for v in (sv.verdict(ps, HARNESS, SHA), sv.verdict(ps, bad, SHA), sv.verdict([], None, SHA),
                  sv.report("failure", {"reason": "member9"}, ps, HARNESS, SHA, 8),
                  sv.report("cancelled", None, None, None, SHA, 8)):
            self.assertRegex(v["summary"], NUMBERS_ONLY)
            for site in (SHA, "member9", "10.0.0.10", "mixer.example.org"):
                self.assertNotIn(site, v["summary"])
            json.dumps(v["numbers"])

    def test_drift_alarms_during_the_soak_are_counted_as_information(self):
        ps = polls()
        before = {"id": 1, "at": T0 - 10, "text": "tuning drift: before the soak"}
        other = {"id": 2, "at": T0 + 30, "text": "engine respawned"}
        for i, p in enumerate(ps):
            alarms = [before, other]
            if i >= 60:
                alarms.append({"id": 3, "at": T0 + 3600, "text": "tuning drift: one"})
            if i >= 120:
                alarms.append({"id": 4, "at": T0 + 7200, "text": "tuning drift: two"})
            p["status"]["alarms"] = alarms
        n = self.green(ps)["numbers"]
        self.assertEqual(n["drift_alarms"], 2)
        self.assertEqual((n["late_counter"], n["overruns"], n["process_max_us"]), (1440, 0, 52.5))
        self.assertEqual((n["decode_errors"], n["meter_frames"]), (0, 289_800))


class Report(unittest.TestCase):
    def report(self, pc: str, record: dict | None) -> dict:
        return sv.report(pc, record, polls(), HARNESS, SHA, 8)

    def test_report_maps_a_cancelled_or_unrecorded_pc_job_to_cancelled(self):
        done = {"conclusion": "success", "reason": "finished"}
        for pc, record, summary in (
                ("cancelled", done, "cancelled: the pc job was cancelled"),
                ("cancelled", {"conclusion": "failure", "reason": "not-finished"}, "cancelled: the pc job was cancelled"),
                ("failure", None, "cancelled: no record of the pc job"),
                ("success", None, "cancelled: no record of the pc job"),
                ("skipped", None, "cancelled: no record of the pc job")):
            v = self.report(pc, record)
            self.assertEqual((v["conclusion"], v["summary"], v["first_failure"]), ("cancelled", summary, None))
        self.assertEqual(self.report("success", done)["summary"], GREEN)

    def test_report_maps_a_left_dev_record_to_cancelled_and_other_pc_failures_to_failure(self):
        left = {"conclusion": "cancelled", "reason": "left-dev"}
        for pc in ("success", "failure"):
            v = self.report(pc, left)
            self.assertEqual((v["conclusion"], v["summary"]), ("cancelled", "cancelled: the pc left dev"))
        for pc, record, first in (
                ("failure", {"conclusion": "failure", "reason": "not-finished"}, "pc job failure (not-finished)"),
                ("failure", {"conclusion": "failure", "reason": "harness-did-not-end"}, "pc job failure (harness-did-not-end)"),
                ("failure", {"conclusion": "success", "reason": "finished"}, "pc job failure (finished)"),
                ("success", {"conclusion": "failure", "reason": "finished"}, "pc job success (finished)"),
                ("skipped", {"conclusion": "failure", "reason": "no-client"}, "pc job skipped (no-client)"),
                ("failure", {"reason": "member9"}, "pc job failure (unknown)"),
                ("failure", [], "pc job failure (unknown)")):
            v = self.report(pc, record)
            self.assertEqual((v["conclusion"], v["summary"], v["first_failure"]), ("failure", f"red: {first}", first))


class Harness(unittest.TestCase):
    def test_harness_problems_for_the_ci_step(self):
        ci = harness(seconds=600.2, frames=30_000, expected_frames=30_010)
        self.assertEqual(sv.harness_problems(ci, 599, 3), [])
        self.assertEqual(sv.harness_problems({**ci, "gaps": 3}, 599, 3), [])
        self.assertEqual(sv.harness_problems({**ci, "gaps": 4}, 599, 3), ["gaps 4"])
        self.assertEqual(sv.harness_problems({**ci, "seconds": 598.5}, 599, 3), ["harness ran 598.5 s of 599 s"])
        self.assertEqual(sv.harness_problems({**ci, "seconds": 599}, 599, 3), [])
        self.assertEqual(sv.harness_problems({**ci, "complete": False, "error": "login-refused", "frames": 0,
                                              "expected_frames": 0, "gaps": 5, "reconnects": 1}, 599, 3),
                         ["harness incomplete (login-refused)", "gaps 5", "reconnects 1", "no listen frames"])
        self.assertEqual(sv.harness_problems(None, 599, 3), ["no harness summary"])


class Main(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name)

    def run_main(self, *argv: str) -> tuple[int, str]:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = sv.main(list(argv))
        return code, out.getvalue()

    def report(self, pc: str = "success") -> dict:
        code, out = self.run_main("report", "--pc", pc, "--dir", str(self.dir), "--sha", SHA, "--hours", "8")
        self.assertEqual(code, 0)
        self.assertEqual(out.count("\n"), 1, out)
        return json.loads(out)

    def write(self, name: str, text: str) -> None:
        (self.dir / name).write_text(text, encoding="utf-8")

    def test_main_report_reads_the_record_directory_and_prints_one_json(self):
        self.assertEqual(self.report()["summary"], "cancelled: no record of the pc job")
        # PowerShell's files: a BOM tolerated, one poll per line.
        self.write("result.json", '\ufeff{"conclusion":"success","reason":"finished"}\r\n')
        self.write("polls.jsonl", "\ufeff" + "".join(json.dumps(p) + "\n" for p in polls()))
        self.assertEqual(self.report()["summary"], "red: no harness summary; " + GREEN[len("green: "):].replace(
            ", gaps 0, reconnects 0, frames 1449000", ""))
        self.write("soakclient.json", json.dumps(HARNESS))
        self.assertEqual(self.report(), sv.verdict(polls(), HARNESS, SHA))
        # A poll iemmode could not answer, and a line cut short.
        with (self.dir / "polls.jsonl").open("ab") as f:
            f.write(b'{"t":1,"exit":1,"status":}\n{"t":2,"st\xc3')
        self.assertEqual(self.report()["first_failure"], "poll 482 is unreadable")
        self.write("result.json", "{")
        self.assertEqual(self.report()["summary"], "red: result.json is unreadable")
        self.assertEqual(self.report("cancelled")["summary"], "cancelled: the pc job was cancelled")
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            self.run_main("report", "--pc", "success", "--dir", str(self.dir), "--sha", "member9", "--hours", "8")

    def test_main_harness_exits_1_on_a_problem(self):
        path = self.dir / "soakclient.json"
        args = ("harness", str(path), "--min-seconds", "599", "--max-gaps", "3")
        self.assertEqual(self.run_main(*args), (1, "no harness summary\n"))
        self.write("soakclient.json", json.dumps(harness(seconds=600.0, frames=30_000, expected_frames=30_000, gaps=3)))
        code, out = self.run_main(*args)
        self.assertEqual(code, 0)
        self.assertTrue(out.startswith("harness ok: "), out)
        self.write("soakclient.json", json.dumps(harness(seconds=600.0, gaps=4, reconnects=1)))
        self.assertEqual(self.run_main(*args), (1, "gaps 4\nreconnects 1\n"))
        self.write("soakclient.json", "not json")
        self.assertEqual(self.run_main(*args), (1, "soakclient.json is unreadable\n"))


if __name__ == "__main__":
    unittest.main()
