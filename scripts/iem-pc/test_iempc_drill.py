"""Tests for scripts/iem-pc/iempc_drill.py, the rollback drill (S8 lane 3,
#11), at the subprocess boundary only: `subprocess.Popen` is a fake that
answers each step's process (iempc, spike_window, tuning_window) as the real
commands print; nothing else is patched but the flag's path and the bounds.
Every value is synthetic."""
from __future__ import annotations

import contextlib
import io
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc_drill as drill  # noqa: E402

SHA = "0123456789abcdef0123456789abcdef01234567"
SIGNAL = "owner, 21:40: event skončil"
APPROVAL = "owner, 21:41: áno, reštartuj"
SITE_WORD = "zyxsiteword"   # stands for a site value a command may print: never in the drill's output


def iempc_out(args: list[str], reply: dict, **extra) -> str:
    """An iempc command's last line (`result()` and its extras)."""
    return json.dumps({"iemmode": args, "exit": 0, "reply": reply, **extra}) + "\n"


def status(mode: str, detail: str, last=None) -> str:
    return iempc_out(["status"], {"ok": True, "mode": mode, "detail": detail, "alarms": [], "last_switch": last})


# The rollback's switch (step 5) and the start's checks after the reboot (a later end).
DONE_EVENT = {"from": "live", "to": "event", "ended_in": "event", "outcome": "done", "steps": [],
              "ended": 1790000100}
STARTED = {**DONE_EVENT, "from": "event", "ended": 1790000400}
PROD = status("live", f"mode live; bundle {SHA}; prod since 1790000000: pin {SHA}, previous none")
TRIAL = status("event", f"mode event; bundle {SHA}", DONE_EVENT)
AFTER = status("event", f"mode event; bundle {SHA}", STARTED)
# The start's checks still running: the rollback's record, a switch in progress.
SWITCHING = iempc_out(["status"], {"ok": True, "mode": "event", "detail": f"mode event; bundle {SHA}", "alarms": [],
                                   "switching": {"from": "event", "to": "event", "done": []},
                                   "last_switch": DONE_EVENT})
ROLLED = iempc_out(["rollback"], {"ok": True, "mode": "event", "detail": f"rollback done: trial, event; "
                                  f"{drill.rb.ON_EXPORT}; the original project is kept as before-rollback-1 "
                                  f"{SITE_WORD}"}, rollback="done")
POST_BOOT = json.dumps({"post-boot": {"reaper": True, "handover": {"ok": True}, "site": SITE_WORD},
                        "problems": []}) + "\n"


class FakePopen:
    """One step's process, answered by `(script, verb)` from the test's
    script; records every start. It has no way to end a process."""

    script: dict = {}
    started: list = []
    timeout_on: set = set()

    def __init__(self, argv, **kw) -> None:
        self.argv = argv
        self.key = (Path(argv[1]).name, argv[2])
        FakePopen.started.append(self.key)
        answer = FakePopen.script.get(self.key, (0, "", ""))
        if callable(answer):
            answer = answer()
        self.answer = answer
        self.returncode = None

    def communicate(self, timeout=None):
        if self.key in FakePopen.timeout_on:
            raise subprocess.TimeoutExpired(self.argv, timeout)
        code, out, err = self.answer
        self.returncode = code
        return out, err


class DrillTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)
        FakePopen.started = []
        FakePopen.timeout_on = set()
        self.statuses = [PROD, TRIAL, AFTER]
        FakePopen.script = {
            ("iempc.py", "dev"): (0, iempc_out(["dev"], {"ok": True, "mode": "dev", "detail": "dev: done"}), ""),
            ("iempc.py", "cutover"): (0, iempc_out(["cutover"], {"ok": True, "detail": "cutover done"}), ""),
            ("iempc.py", "status"): lambda: (0, self.statuses.pop(0), ""),
            ("iempc.py", "rollback"): (0, ROLLED, ""),
            ("spike_window.py", "new"): (0, "20261010T194000Z\n", ""),
            ("spike_window.py", "to-dev"): (0, json.dumps({"to-dev": {}}) + "\n", ""),
            ("tuning_window.py", "reboot-prepare"): (0, json.dumps({"reboot-prepare": {}}) + "\n", ""),
            ("tuning_window.py", "reboot"): (0, json.dumps({"reboot": "requested", "in_s": 0}) + "\n", ""),
            ("tuning_window.py", "post-boot"): (0, POST_BOOT, ""),
        }
        self.slept: list = []
        patches = [mock.patch.object(drill.subprocess, "Popen", FakePopen),
                   mock.patch.object(drill.core, "EVENT_NOW", self.tmp / "EVENT-NOW"),
                   # After the reboot: one status read, then the bound (a test raises it).
                   mock.patch.object(drill, "SETTLE_S", 0, create=True),
                   mock.patch.object(drill.time, "sleep", self.slept.append)]
        for p in patches:
            p.start()
            self.addCleanup(p.stop)

    def run_drill(self, *extra: str) -> tuple[int, dict, str]:
        out, err = io.StringIO(), io.StringIO()
        argv = ["--sha", SHA, "--signal", SIGNAL, "--approval", APPROVAL, *extra]
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = drill.main(argv)
        lines = [json.loads(line) for line in out.getvalue().splitlines() if line.strip()]
        self.assertEqual(len(lines), 1, out.getvalue())
        return code, lines[0]["drill"], err.getvalue()

    def test_a_green_drill_runs_every_step_in_order(self) -> None:
        code, doc, err = self.run_drill()
        self.assertEqual((code, doc["code"], doc["sha"]), (0, "GREEN", SHA), err)
        names = [s["step"] for s in doc["steps"]]
        self.assertEqual(names, ["dev", "cutover", "prod", "rollback", "event", "window-new", "window-to-dev",
                                 "reboot-prepare", "reboot", "post-boot", "after-reboot"])
        self.assertTrue(all(s["exit"] == 0 and isinstance(s["seconds"], float) for s in doc["steps"]))
        self.assertEqual(FakePopen.started, [("iempc.py", "dev"), ("iempc.py", "cutover"), ("iempc.py", "status"),
                                             ("iempc.py", "rollback"), ("iempc.py", "status"),
                                             ("spike_window.py", "new"), ("spike_window.py", "to-dev"),
                                             ("tuning_window.py", "reboot-prepare"), ("tuning_window.py", "reboot"),
                                             ("tuning_window.py", "post-boot"), ("iempc.py", "status")])

    def test_the_output_is_codes_and_numbers_only(self) -> None:
        FakePopen.script[("iempc.py", "rollback")] = (1, ROLLED, f"iempc: {SITE_WORD} failed")
        code, doc, err = self.run_drill()
        self.assertEqual((code, doc["code"]), (1, "ROLLBACK_FAILED"))
        self.assertEqual(set(doc), {"sha", "code", "steps"})
        self.assertTrue(all(set(s) == {"step", "exit", "seconds"} for s in doc["steps"]))
        self.assertNotIn(SITE_WORD, json.dumps(doc))
        self.assertIn(SITE_WORD, err)   # the operator's, on stderr
        self.assertIn("never paste it", err)

    def test_the_first_failure_stops_the_drill(self) -> None:
        cases = (
            (("iempc.py", "dev"), (1, "", "refused"), "DEV_FAILED", 1),
            (("iempc.py", "cutover"), (1, "", "refused"), "CUTOVER_FAILED", 2),
            (("spike_window.py", "new"), (1, "", "an event is on"), "WINDOW_FAILED", 6),
            (("spike_window.py", "to-dev"), (1, "", "x"), "WINDOW_FAILED", 7),
            (("tuning_window.py", "reboot-prepare"), (1, "", "x"), "REBOOT_FAILED", 8),
            (("tuning_window.py", "reboot"), (1, "", "the restart request exited 5: nothing restarts"), "REBOOT_FAILED",
             9),
            (("tuning_window.py", "post-boot"), (1, POST_BOOT, "post-boot checks failed"), "POST_BOOT_FAILED", 10),
        )
        for key, answer, want, started in cases:
            self.setUp()
            FakePopen.script[key] = answer
            code, doc, _ = self.run_drill()
            self.assertEqual((code, doc["code"], len(FakePopen.started)), (1, want, started), key)

    def test_the_guard_s_state_is_checked_between_the_steps(self) -> None:
        cases = (
            ([TRIAL], "NOT_PROD", 3),
            ([status("dev", f"mode dev; bundle {SHA}; prod since 1: pin {SHA}, previous none")], "NOT_PROD", 3),
            ([status("live", f"mode live; bundle {SHA}; prod since 1: pin {'f' * 40}, previous none")], "NOT_PROD", 3),
            ([PROD, PROD], "NOT_EVENT", 5),
            ([PROD, status("event", "mode event; rolling back to REAPER")], "NOT_EVENT", 5),
            ([PROD, status("event", f"mode event; bundle {SHA}", {**DONE_EVENT, "outcome": "needs_owner"})],
             "NOT_EVENT", 5),
            ([PROD, TRIAL, status("event", f"mode event; bundle {SHA}", None)], "NOT_EVENT_AFTER_REBOOT", 11),
            # The rollback's own record must name its end (the start's checks are told by a later one).
            ([PROD, status("event", f"mode event; bundle {SHA}", {k: v for k, v in DONE_EVENT.items() if k != "ended"})],
             "NOT_EVENT", 5),
            # After the reboot the start's checks ended in REAPER, but not done.
            ([PROD, TRIAL, status("event", f"mode event; bundle {SHA}", {**STARTED, "outcome": "needs_owner"})],
             "NOT_EVENT_AFTER_REBOOT", 11),
        )
        for statuses, want, started in cases:
            self.setUp()
            self.statuses = list(statuses)
            code, doc, _ = self.run_drill()
            self.assertEqual((code, doc["code"], len(FakePopen.started)), (1, want, started), want)

    def test_after_the_reboot_it_waits_for_the_start_s_checks(self) -> None:
        """S8 lane 5 (finding 3): the first status after the reboot can still
        name the rollback's switch (the guard has not begun its start's
        checks) or a switch in progress: the drill reads the status again,
        bounded, until no switch runs and the last one ended later than the
        rollback's, and only then checks it."""
        self.statuses = [PROD, TRIAL, TRIAL, SWITCHING, AFTER]
        with mock.patch.object(drill, "SETTLE_S", 600, create=True):
            code, doc, err = self.run_drill()
        self.assertEqual((code, doc["code"]), (0, "GREEN"), err)
        self.assertEqual(FakePopen.started[-3:], [("iempc.py", "status")] * 3)
        self.assertEqual(len(FakePopen.started), 13)
        self.assertEqual([s["step"] for s in doc["steps"]][-1], "after-reboot")
        self.assertEqual(len(self.slept), 2)
        # Never seen to end within the bound: the drill stops there.
        self.setUp()
        self.statuses = [PROD, TRIAL, TRIAL]
        code, doc, err = self.run_drill()
        self.assertEqual((code, doc["code"], len(FakePopen.started)), (1, "NOT_EVENT_AFTER_REBOOT", 11))
        self.assertIn("the start's checks", err)

    def test_reaper_must_be_on_the_export_after_the_rollback(self) -> None:
        on_original = iempc_out(["rollback"], {"ok": True, "detail": f"rollback done: trial, event; {drill.rb.ON_ORIGINAL}"})
        FakePopen.script[("iempc.py", "rollback")] = (0, on_original, "")
        code, doc, _ = self.run_drill()
        self.assertEqual((code, doc["code"], len(FakePopen.started)), (1, "NOT_ON_EXPORT", 4))

    def test_reaper_must_come_back_by_itself_after_the_reboot(self) -> None:
        for checks in ({"reaper": False}, {}, None):
            self.setUp()
            body = {"post-boot": checks, "problems": []} if checks is not None else {"other": 1}
            FakePopen.script[("tuning_window.py", "post-boot")] = (0, json.dumps(body) + "\n", "")
            code, doc, _ = self.run_drill()
            self.assertEqual((code, doc["code"], len(FakePopen.started)), (1, "REAPER_NOT_BACK", 10), checks)
        self.setUp()
        problems = json.dumps({"post-boot": {"reaper": True}, "problems": ["a tuning item is pending"]}) + "\n"
        FakePopen.script[("tuning_window.py", "post-boot")] = (0, problems, "")
        self.assertEqual(self.run_drill()[1]["code"], "POST_BOOT_FAILED")

    def test_an_answer_the_restart_took_is_no_failure(self) -> None:
        lost = "tuning_window: no answer to the restart request (x): the PC may be restarting; run post-boot, which tells"
        FakePopen.script[("tuning_window.py", "reboot")] = (1, "", lost)
        code, doc, err = self.run_drill()
        self.assertEqual((code, doc["code"]), (0, "GREEN"), err)
        self.assertEqual([s["exit"] for s in doc["steps"] if s["step"] == "reboot"], [1])

    def test_the_flag_stops_the_drill_before_the_next_step(self) -> None:
        def cutover_then_flag():
            drill.core.EVENT_NOW.write_text("2026-10-10T21:50:00+02:00\n", encoding="utf-8")
            return (0, iempc_out(["cutover"], {"ok": True}), "")

        FakePopen.script[("iempc.py", "cutover")] = cutover_then_flag
        code, doc, err = self.run_drill()
        self.assertEqual((code, doc["code"], len(FakePopen.started)), (1, "EVENT", 2))
        self.assertIn("'iempc event' runs the event path", err)

    def test_a_step_that_outlives_its_bound_is_left_running(self) -> None:
        FakePopen.timeout_on = {("iempc.py", "rollback")}
        with mock.patch.dict(drill.BOUND_S, {"rollback": 0}), mock.patch.object(drill, "POLL_S", 0):
            code, doc, err = self.run_drill()
        self.assertEqual((code, doc["code"], len(FakePopen.started)), (1, "STILL_RUNNING", 4))
        self.assertIn("left to end by itself", err)

    def test_a_dry_run_calls_nothing(self) -> None:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = drill.main(["--sha", SHA, "--signal", SIGNAL, "--approval", APPROVAL, "--dry-run"])
        doc = json.loads(out.getvalue())["drill"]
        self.assertEqual((code, doc["sha"], len(doc["plan"]), FakePopen.started), (0, SHA, 11, []))

    def test_a_sha_that_is_no_commit_is_a_usage_error(self) -> None:
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            code = drill.main(["--sha", "main", "--signal", SIGNAL, "--approval", APPROVAL])
        self.assertEqual((code, FakePopen.started), (2, []))
        self.assertIn("not a full commit SHA", err.getvalue())


if __name__ == "__main__":
    unittest.main()
