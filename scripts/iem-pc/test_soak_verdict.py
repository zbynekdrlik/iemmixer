"""Tests for scripts/iem-pc/soak_verdict.py: CI's harness check and the box
gate's check. Every value is synthetic."""
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

HARNESS = {"schema": 1, "build": "a" * 40, "complete": True, "seconds": 28_980.0, "frames": 1_449_000,
           "expected_frames": 1_449_000, "decode_errors": 0, "gaps": 0, "max_gap_ms": 41, "first_frame_ms": 180,
           "meter_frames": 289_800, "reconnects": 0, "no_source": 0, "error": None}


def problems(summary: object, min_seconds: float, max_gaps: int) -> list[str]:
    """CI's check of a summary, in order; [] when it passed."""
    return sv._ci_check(summary, min_seconds, max_gaps)[0]


def harness(**change) -> dict:
    return {**HARNESS, **change}


class Harness(unittest.TestCase):
    def test_harness_problems_for_the_ci_step(self):
        ci = harness(seconds=600.2, frames=30_000, expected_frames=30_010)
        self.assertEqual(problems(ci, 599, 3), [])
        self.assertEqual(problems({**ci, "gaps": 3}, 599, 3), [])
        self.assertEqual(problems({**ci, "gaps": 4}, 599, 3), ["gaps 4"])
        self.assertEqual(problems({**ci, "seconds": 598.5}, 599, 3), ["harness ran 598.5 s of 599 s"])
        self.assertEqual(problems({**ci, "seconds": 599}, 599, 3), [])
        self.assertEqual(problems({**ci, "complete": False, "error": "login-refused", "frames": 0,
                                              "expected_frames": 0, "gaps": 5, "reconnects": 1}, 599, 3),
                         ["harness incomplete (login-refused)", "gaps 5", "reconnects 1", "no listen frames"])
        # CI's step names a lost socket and a server that does not name the build (#10).
        for code in ("connection-lost", "wrong-server"):
            self.assertEqual(problems({**ci, "complete": False, "error": code}, 599, 3),
                             [f"harness incomplete ({code})"])
        self.assertEqual(problems(None, 599, 3), ["no harness summary"])
        # A reason code only when it is a known one (P6).
        self.assertEqual(problems({**ci, "complete": False, "error": "member9.example.org"}, 599, 3),
                         ["harness incomplete"])
        # Frames: at least one, and at least 99 % of the expected, rounded toward red.
        self.assertEqual(problems({**ci, "frames": 0}, 599, 3), ["no listen frames"])
        self.assertEqual(problems({**ci, "frames": 29_710}, 599, 3), [])
        self.assertEqual(problems({**ci, "frames": 29_709}, 599, 3), ["frames 98.99 % of expected"])

    def test_a_summary_of_another_shape_is_unreadable(self):
        for bad in (sv.UNREADABLE, [], {}, harness(seconds=float("inf")), harness(complete=1), harness(gaps=-1),
                    harness(frames=1.5)):
            self.assertEqual(sv.box_check(bad, 60), (["the harness summary is unreadable"], {}), bad)

    def test_the_gen3_box_gate_judges_the_summary_with_a_meter_frame(self):
        # iempc gen3-gate soak (#72 phase H): the summary's checks with a meter frame (the box's real server sends
        # them), and the numbers for the gate's line. Every listen gap fails it (#88: 0 gaps in 30 min at three
        # times the gate's reads, 2026-11-08).
        box = harness(seconds=7200.4, frames=360_000, expected_frames=360_000, meter_frames=72_000)
        self.assertEqual(sv.box_check(box, 7200), ([], {"frames": 360_000, "expected_frames": 360_000, "gaps": 0,
                                                         "reconnects": 0, "decode_errors": 0, "meter_frames": 72_000,
                                                         "seconds": 7200.4}))
        self.assertEqual(sv.box_check({**box, "gaps": 1}, 7200), (["gaps 1"], {"frames": 360_000, "expected_frames": 360_000,
                                                                              "gaps": 1, "reconnects": 0, "decode_errors": 0,
                                                                              "meter_frames": 72_000, "seconds": 7200.4}))
        self.assertEqual(sv.box_check({**box, "gaps": 8, "reconnects": 1}, 7200)[0], ["gaps 8", "reconnects 1"])
        self.assertEqual(sv.box_check({**box, "meter_frames": 0}, 7200)[0], ["no meter frames"])
        self.assertEqual(sv.box_check({**box, "frames": 350_000}, 7200)[0], ["frames 97.22 % of expected"])
        self.assertEqual(sv.box_check({**box, "seconds": 7199.9}, 7200)[0], ["harness ran 7199.9 s of 7200 s"])
        self.assertEqual(sv.box_check({**box, "complete": False, "error": "connection-lost"}, 7200)[0],
                         ["harness incomplete (connection-lost)"])
        self.assertEqual(sv.box_check(None, 7200), (["no harness summary"], {}))

    def test_the_ci_step_needs_a_meter_frame(self):
        # CI's run against the real server is the proof that the client reads its Meters.
        ci = harness(seconds=600.2, frames=30_000, expected_frames=30_010)
        self.assertEqual(problems({**ci, "meter_frames": 1}, 599, 3), [])
        self.assertEqual(problems({**ci, "meter_frames": 0}, 599, 3), ["no meter frames"])
        # Named after the summary's other checks.
        self.assertEqual(problems({**ci, "meter_frames": 0, "reconnects": 2}, 599, 3),
                         ["reconnects 2", "no meter frames"])
        # An unreadable summary is named once, never also as meterless.
        self.assertEqual(problems({**ci, "meter_frames": -1}, 599, 3),
                         ["the harness summary is unreadable"])
        self.assertEqual(problems(None, 599, 3), ["no harness summary"])

    def test_the_harness_reason_codes_are_the_clients_own(self):
        # Reason::code is an exhaustive match: a code the client gains fails here until the verdict knows it,
        # so a red summary never drops it as unknown.
        lib = (Path(__file__).resolve().parents[2] / "crates" / "iem-soakclient" / "src" / "lib.rs").read_text(
            encoding="utf-8")
        body = lib.split("pub fn code(self) -> &'static str {", 1)[1].split("\n    }\n", 1)[0]
        self.assertEqual(set(re.findall(r'Reason::\w+ => "([a-z-]+)"', body)), sv.HARNESS_REASONS)


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

    def write(self, name: str, text: str) -> None:
        (self.dir / name).write_text(text, encoding="utf-8")

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

    def test_main_harness_exits_1_without_a_meter_frame(self):
        path = self.dir / "soakclient.json"
        args = ("harness", str(path), "--min-seconds", "599", "--max-gaps", "3")
        self.write("soakclient.json", json.dumps(harness(seconds=600.0, frames=30_000, expected_frames=30_000,
                                                         meter_frames=0)))
        self.assertEqual(self.run_main(*args), (1, "no meter frames\n"))


if __name__ == "__main__":
    unittest.main()
