"""Tests for scripts/iem-pc/iempc_event.py: `iempc event`, the flag, the spike
preempt, the one budget, and its end on the guard's verdict (#10; split out of
test_iempc.py, #36). They reuse iempc_test_support's fakes (FakePc stands in
for ssh); every value is synthetic."""
from __future__ import annotations

import ast
import contextlib
import io
import json
import os
import sys
import threading
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from iempc_test_support import ENV, OK, SHA, Base, FakeClock, ip  # noqa: E402


class FlagTests(Base):
    def test_the_flag_refuses_dev_and_touches_nothing(self) -> None:
        self.flag()
        code, docs, err = self.run_main("dev", "--build", SHA)
        self.assertEqual((code, docs, self.pc.calls, self.pc.modules, self.gh.calls), (1, [], [], [], []))
        self.assertIn("runs only in dev time", err)

    def test_every_dev_time_command_is_refused_with_the_flag(self) -> None:
        self.fetched()
        self.flag()
        before = len(self.gh.calls)
        for argv in (["install", "--sha", SHA], ["install", "--sha", SHA, "--first"], ["dispatch-hil", "--sha", SHA],
                     ["rehearse-teardown"], ["probe-task"], ["handover-s1a"], ["bootstrap", "Register-IemTasks"],
                     ["bootstrap", "Get-IemBootstrapState"], ["bootstrap", "Get-IemTunnelOrigin"],
                     ["bootstrap", "Test-IemServiceRight"], ["dev", "--dry-run"]):
            code, docs, err = self.run_main(*argv)
            self.assertEqual((code, docs), (1, []), argv)
            self.assertIn("runs only in dev time", err, argv)
        self.assertEqual((self.pc.calls, self.pc.modules, self.pc.scps, len(self.gh.calls)), ([], [], [], before))

    def test_event_goes_through_with_the_flag(self) -> None:
        self.flag()
        code, docs, _ = self.run_main("event")
        self.assertEqual(code, 0)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
        self.assertNotIn("flag", docs[0])

    def test_event_writes_the_flag_before_anything_else(self) -> None:
        seen = []
        self.pc.replies[("event", "--signal")] = lambda: (seen.append(ip.event_now()), (0, OK))[1]
        code, docs, _ = self.run_main("event")
        self.assertEqual((code, seen, docs[0]), (0, [True], {"flag": str(ip.EVENT_NOW), "written": True}))
        self.assertRegex(ip.EVENT_NOW.read_text(encoding="utf-8"), r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d[+-]\d\d:\d\d\n$")

    def test_an_existing_flag_is_kept(self) -> None:
        self.flag()
        self.assertFalse(ip.ensure_flag())
        self.assertEqual(ip.EVENT_NOW.read_text(encoding="utf-8"), "2026-09-27T20:00:00+02:00\n")

    def test_a_flag_that_cannot_be_written_never_stops_the_event_path(self) -> None:
        blocker = self.tmp / "not-a-folder"
        blocker.write_text("a file where the flag's folder should be", encoding="utf-8")
        self.patch(EVENT_NOW=blocker / "EVENT-NOW")
        self.open_window()
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0)
        self.assertEqual({k: docs[0][k] for k in ("flag", "written")}, {"flag": str(ip.EVENT_NOW), "written": False})
        self.assertIn("File exists", docs[0]["error"])
        self.assertEqual(self.spike_log.read_text(encoding="utf-8"), "preempt\n")
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
        self.assertIn("was not written", err)
        self.assertNotIn("alarm the owner", err)


class EventTests(Base):
    def test_an_open_spike_window_is_preempted_before_iemmode(self) -> None:
        self.open_window()
        seen = []
        self.pc.replies[("event", "--signal")] = lambda: (seen.append(self.spike_log.is_file()), (0, OK))[1]
        code, docs, _ = self.run_main("event")
        self.assertEqual((code, seen, self.spike_log.read_text(encoding="utf-8")), (0, [True], "preempt\n"))
        self.assertEqual(docs[1], {"spike_preempt": {"ok": True, "output": "preempted\n"}})

    def test_a_closed_or_absent_window_is_left_alone(self) -> None:
        for state in (None, {"closed": True}):
            if state is not None:
                self.open_window(**state)
            self.assertEqual(self.run_main("event")[0], 0)
        self.assertFalse(self.spike_log.exists())
        self.assertEqual(len(self.pc.calls), 2)

    def test_a_failed_spike_preempt_still_runs_iemmode_event(self) -> None:
        self.open_window()
        self.patch(SPIKE=self.write_spike(3))
        code, docs, err = self.run_main("event")
        self.assertEqual((code, docs[1]["spike_preempt"]["ok"], "running" in docs[1]["spike_preempt"]), (0, False, False))
        self.assertIn("exit 3", docs[1]["spike_preempt"]["error"])
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
        self.assertIn("spike preempt", err)

    # F2 round 3, m2 and decision 2: after a FAILED spike preempt the window is closed
    # under spike_window's lock first, so a window preempt another process still has
    # queued finds it closed and starts no second bring-back next to the guard's.
    def test_a_failed_spike_preempt_closes_the_window_before_iemmode_event(self) -> None:
        self.open_window()
        self.patch(SPIKE=self.write_spike(3))
        seen: list[dict] = []
        self.pc.replies[("event", "--signal")] = lambda: (seen.append(json.loads(ip.SPIKE_STATE.read_text(encoding="utf-8"))), (0, OK))[1]
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertEqual(len(seen), 1)
        self.assertTrue(seen[0]["closed"])
        self.assertIn("failed spike preempt", seen[0]["closed_by"]["by"])
        self.assertIn("exit 3", seen[0]["closed_by"]["error"])
        self.assertEqual(seen[0]["card"], "free")   # nothing else is claimed: the guard brings REAPER back
        self.assertIn({"spike_window": "closed", "after": "a failed spike preempt"}, docs)

    def test_a_change_still_in_flight_after_a_failed_preempt_is_named(self) -> None:
        # Review of lane G2, finding 5: no settle watches it on this path; the step's own
        # late handler and the guard take it from here, and the agent hears which step.
        intent = {"step": "set-buffer", "started": time.time(), "bound_s": 60}
        self.open_window(in_flight=intent)
        self.patch(SPIKE=self.write_spike(3))
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertIn({"spike_window": "closed", "after": "a failed spike preempt", "in_flight": intent}, docs)
        self.assertIn("set-buffer is still in flight", err)

    def test_a_closed_window_whose_preempt_still_settles_gets_the_spike_preempt(self) -> None:
        # Review of lane G2, finding 1: a window process's own preempt closed the window
        # and still watches REAPER (settle, without the lock); `spike_window.py preempt`
        # waits for that watch, so iemmode event never runs next to its bring-back.
        self.open_window(card="reaper", closed=True, settling={"step": "to-dev", "until": time.time() + 60})
        seen = []
        self.pc.replies[("event", "--signal")] = lambda: (seen.append(self.spike_log.is_file()), (0, OK))[1]
        code, docs, _ = self.run_main("event")
        self.assertEqual((code, seen, self.spike_log.read_text(encoding="utf-8")), (0, [True], "preempt\n"))
        # A watch past its bound (its process died) is no window to pre-empt.
        self.spike_log.unlink()
        self.open_window(card="reaper", closed=True, settling={"step": "to-dev", "until": time.time() - 1})
        self.assertEqual(self.run_main("event")[0], 0)
        self.assertFalse(self.spike_log.exists())

    def test_no_iemmode_call_while_the_window_lock_stays_taken(self) -> None:
        # The lock wait fits the event budget: a window process that holds it (a
        # bring-back of its own?) is never raced by the guard's.
        self.open_window()
        self.patch(SPIKE=self.write_spike(3), EVENT_BUDGET_S=2.0, SWITCH_MIN_S=1.0)
        sw = ip.spike_module()
        saved = sw.STATE
        self.addCleanup(setattr, sw, "STATE", saved)
        sw.STATE = ip.SPIKE_STATE
        taken, release = threading.Event(), threading.Event()

        def hold() -> None:
            with sw.window_lock():
                taken.set()
                release.wait(10)

        holder = threading.Thread(target=hold)
        holder.start()
        taken.wait(5)
        try:
            code, _, err = self.run_main("event")
        finally:
            release.set()
            holder.join()
        self.assertEqual((code, self.pc.calls), (1, []))
        self.assertIn("window lock", err)
        self.assertIn("alarm the owner now", err)
        self.assertFalse(json.loads(ip.SPIKE_STATE.read_text(encoding="utf-8"))["closed"])

    def test_the_event_path_has_one_budget_that_fits_a_bash_call(self) -> None:
        self.assertLessEqual(ip.EVENT_BUDGET_S, 540)  # a Bash call ends at 10 min; the plan's waits stay within 9
        self.assertLessEqual(ip.SPIKE_SHARE_S + ip.SWITCH_MIN_S, ip.EVENT_BUDGET_S)
        self.pc.replies[("event", "--signal")] = (4, OK)
        self.assertEqual(self.run_main("event")[0], 0)
        first, direct = self.pc.timeouts
        self.assertLessEqual(first, ip.EVENT_BUDGET_S)
        self.assertGreater(first, ip.EVENT_BUDGET_S - 10)
        self.assertLess(direct, first)

    def test_iemmode_event_gets_what_the_spike_preempt_left(self) -> None:
        self.open_window()
        self.patch(SPIKE=self.write_spike(0, delay=0.4))
        self.assertEqual(self.run_main("event")[0], 0)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
        self.assertLessEqual(self.pc.timeouts[0], ip.EVENT_BUDGET_S - 0.4)
        self.assertGreater(self.pc.timeouts[0], ip.EVENT_BUDGET_S - 10)

    def test_no_iemmode_call_while_the_spike_preempt_outlives_its_share(self) -> None:
        self.open_window()
        self.patch(SPIKE=self.write_spike(0, delay=3), SPIKE_SHARE_S=0.3)
        t = time.monotonic()
        code, docs, err = self.run_main("event")
        spike_still_runs = not self.spike_done.exists()
        self.assertTrue(spike_still_runs)
        self.assertLess(time.monotonic() - t, 2.0)
        self.assertEqual((code, self.pc.calls), (1, []))
        self.assertEqual((docs[1]["spike_preempt"]["ok"], docs[1]["spike_preempt"]["running"]), (False, True))
        self.assertIn("preempt still runs after its 0.3 s share", err)
        self.assertIn("alarm the owner now", err)
        self.wait_for_spike_end()
        self.assertEqual(self.pc.calls, [])

    def test_an_iemmode_call_never_starts_with_less_than_its_minimum(self) -> None:
        # On a fake clock that only the replies move: the branch never depends on
        # this process's own speed (a loaded run once ate the margin).
        clock = FakeClock()
        self.patch(EVENT_BUDGET_S=1.0, SWITCH_MIN_S=0.5)
        self.pc.replies[("event", "--signal")] = lambda: (clock.sleep(0.625), (4, OK))[1]
        with self.patched(event_clock=clock.now):
            code, _, err = self.run_main("event")
        self.assertEqual((code, [c[1] for c in self.pc.calls]), (1, [["event", "--signal"]]))
        self.assertIn("less than the 0.5 s an iemmode call gets: run 'iempc event' again", err)
        self.assertIn("alarm the owner now", err)
        self.pc.calls.clear()
        # Exactly the minimum left is enough: 1.0 - 0.5 = 0.5.
        self.pc.replies[("event", "--signal")] = lambda: (clock.sleep(0.5), (4, OK))[1]
        with self.patched(event_clock=clock.now):
            self.assertEqual(self.run_main("event")[0], 0)
        self.assertEqual([c[1] for c in self.pc.calls], [["event", "--signal"], ["event", "--signal", "--direct"]])

    def test_an_unreachable_guard_falls_back_to_direct(self) -> None:
        self.pc.replies[("event", "--signal")] = (4, json.dumps({"error": "guard unreachable"}))
        code, docs, _ = self.run_main("event")
        self.assertEqual(code, 0)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore"), ("iemmode.exe", ["event", "--signal", "--direct"], "ignore")])
        self.assertEqual([d.get("iemmode") for d in docs[1:]], [["event", "--signal"], ["event", "--signal", "--direct"]])

    def test_only_exit_4_falls_back(self) -> None:
        for exit_code in (1, 2, 3, 5, 70):
            self.pc.calls.clear()
            self.pc.replies[("event", "--signal")] = (exit_code, OK)
            code, _, err = self.run_main("event")
            self.assertEqual((code, self.pc.calls), (exit_code, [("iemmode.exe", ["event", "--signal"], "ignore")]), exit_code)
            self.assertIn("the event path did not complete", err)

    def test_a_failed_direct_event_is_an_owner_alarm(self) -> None:
        self.pc.replies[("event", "--signal")] = (4, OK)
        self.pc.replies[("event", "--signal", "--direct")] = (1, json.dumps({"error": "a guard runs; use the pipe"}))
        code, _, err = self.run_main("event")
        self.assertEqual((code, len(self.pc.calls)), (1, 2))
        self.assertIn("alarm the owner now", err)

    def test_an_unreachable_pc_is_an_owner_alarm(self) -> None:
        def down():
            raise ip.StepError("ssh failed (exit 255): no route")

        self.pc.replies[("event", "--signal")] = down
        code, _, err = self.run_main("event")
        self.assertEqual(code, 1)
        self.assertIn("no route", err)
        self.assertIn("alarm the owner now", err)

    def test_a_dry_run_writes_no_flag_and_preempts_nothing(self) -> None:
        self.open_window()
        self.pc.replies[("event", "--dry-run", "--signal")] = (4, OK)
        code, docs, err = self.run_main("event", "--dry-run")
        self.assertEqual((code, ip.event_now(), self.spike_log.exists()), (0, False, False))
        self.assertEqual(docs[0], {"spike_window": "open", "plan": "spike_window.py preempt"})
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--dry-run", "--signal"], "ignore"),
                                         ("iemmode.exe", ["event", "--dry-run", "--signal", "--direct"], "ignore")])
        self.assertNotIn("alarm the owner", err)

    def test_a_failed_dry_run_is_no_owner_alarm(self) -> None:
        def down():
            raise ip.StepError("ssh failed (exit 255): no route")

        self.pc.replies[("event", "--dry-run", "--signal")] = down
        code, _, err = self.run_main("event", "--dry-run")
        self.assertEqual((code, ip.event_now()), (1, False))
        self.assertIn("no route", err)
        self.assertNotIn("alarm the owner", err)



class EventVerdictTests(Base):
    def test_an_event_switch_that_needs_the_owner_exits_non_zero(self) -> None:
        """#10 (2026-10-08): a switch to event whose REAPER handover failed ends
        `needs_owner`, never `done`; the guard answers ok false, iemmode exits 1,
        and so does iempc event, with the owner alarm (no --direct)."""
        reply = json.dumps({"ok": False, "mode": "event", "alarms": [],
                            "detail": "event: ended, needs the owner: ReaperHandover failed: REAPER "
                                      "does not run",
                            "last_switch": {"from": "dev", "to": "event", "ended_in": "event",
                                            "outcome": "needs_owner"}})
        self.pc.replies[("event", "--signal")] = (1, reply)
        code, docs, err = self.run_main("event")
        self.assertEqual((code, self.pc.calls), (1, [("iemmode.exe", ["event", "--signal"], "ignore")]))
        self.assertEqual(docs[-1]["iemmode"], ["event", "--signal"])
        self.assertIn("alarm the owner now", err)


PROD = f"mode live; bundle {SHA}; prod since 1790000000: pin {SHA}, previous none"


class LifecycleTests(unittest.TestCase):
    def test_the_guard_s_status_names_the_lifecycle(self) -> None:
        self.assertEqual(ip.guard_lifecycle({"detail": f"mode event; bundle {SHA}"}), "trial")
        self.assertEqual(ip.guard_lifecycle({"detail": PROD}), "prod")
        for bad in (None, "text", {}, {"detail": None}, {"detail": 7}):
            self.assertIsNone(ip.guard_lifecycle(bad), bad)

    def test_the_phrase_is_the_guard_s(self) -> None:
        # The Rust text it reads (lifecycle::status).
        root = Path(__file__).resolve().parents[2] / "crates" / "iem-guard" / "src"
        lifecycle = (root / "lifecycle.rs").read_text(encoding="utf-8")
        self.assertIn('"prod since {}: pin {}, previous {}"', lifecycle)
        self.assertIn(f'"{ip.PROD_SINCE}', '"prod since {}: pin {}, previous {}"')


class EventSignalTests(Base):
    """"ide event" is `iemmode event --signal` (S8 lane 3); an iemmode that
    does not know the flag gets the plain event, the event plan in every
    lifecycle (no rollback, #11)."""

    OLD = (2, "", 'iemmode: unknown argument "--signal"\nusage: iemmode status')

    def test_ide_event_is_the_owner_s_signal(self) -> None:
        code, _, _ = self.run_main("event")
        self.assertEqual((code, [c[1] for c in self.pc.calls]), (0, [["event", "--signal"]]))

    def test_an_iemmode_that_does_not_know_the_signal_gets_the_plain_event(self) -> None:
        # An iemmode older than S8 lane 3 refuses the flag before any guard call (exit 2).
        self.pc.replies[("event", "--signal")] = self.OLD
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertEqual([c[1] for c in self.pc.calls], [["event", "--signal"], ["event"]])
        self.assertEqual([d.get("iemmode") for d in docs[1:]], [["event", "--signal"], ["event"]])

    def test_the_fall_back_keeps_the_direct_path(self) -> None:
        # The guard unreachable: the plain event, then --direct.
        self.pc.replies[("event", "--signal")] = self.OLD
        self.pc.replies[("event",)] = (4, OK)
        code, _, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertEqual([c[1] for c in self.pc.calls], [["event", "--signal"], ["event"], ["event", "--direct"]])

    def test_another_usage_error_is_not_retried(self) -> None:
        self.pc.replies[("event", "--signal")] = (2, "", "iemmode: no command")
        code, _, _ = self.run_main("event")
        self.assertEqual((code, [c[1] for c in self.pc.calls]), (2, [["event", "--signal"]]))


class Breaks(io.StringIO):
    """A stream whose writes holding one of `words` fail as a write to a
    closed pipe does (BrokenPipeError); it keeps every other write."""

    def __init__(self, *words: str) -> None:
        super().__init__()
        self.words = words
        self.broken = 0

    def write(self, s: str) -> int:
        if any(w in s for w in self.words):
            self.broken += 1
            raise BrokenPipeError(32, "Broken pipe")
        return super().write(s)


class Gen3StopTests(Base):
    """"ide event" also takes the Gen 3 device off the network (#72, design
    §5.5): one `ssh <GEN3_HOST> sudo -n systemctl stop iem-dante`, started
    right after the flag beside the PC's switch, waited for at most
    GEN3_STOP_S at the end. It never delays the PC and never changes the
    event's exit code; a stand-in `ssh` first on PATH plays the box."""

    HOST = "tester@gen3.test"

    def setUp(self) -> None:
        super().setUp()
        self.ssh_log = self.tmp / "ssh.log"
        self.ssh_done = self.tmp / "ssh.done"
        self.ssh_flag = self.tmp / "ssh.flag"
        self.ssh_sid = self.tmp / "ssh.sid"

    def gen3_host(self, host: str = HOST) -> None:
        ip.env_path().write_text(ip.env_path().read_text(encoding="utf-8") + f"GEN3_HOST={host}\n", encoding="utf-8")

    def gen3_env(self, host: str | None) -> None:
        """The private env with `host` as its GEN3_HOST, or none."""
        text = "".join(f"{k}={v}\n" for k, v in ENV.items()) + (f"GEN3_HOST={host}\n" if host is not None else "")
        ip.env_path().write_text(text, encoding="utf-8")

    def ssh_program(self, code: int, delay: float = 0.0, err: str = "") -> None:
        """A stand-in `ssh` first on PATH: logs its arguments, its session and
        whether the EVENT-NOW flag exists at start, takes `delay` seconds,
        marks its end, writes `err` and exits with `code`."""
        d = self.tmp / "ssh-bin"
        d.mkdir(exist_ok=True)
        p = d / "ssh"
        p.write_text(f"#!{sys.executable}\nimport os, sys, time\n"
                     f"open({str(self.ssh_flag)!r}, 'a').write(repr(os.path.exists({str(ip.EVENT_NOW)!r})) + '\\n')\n"
                     f"open({str(self.ssh_sid)!r}, 'w').write(str(os.getsid(0)))\n"
                     f"open({str(self.ssh_log)!r}, 'a').write(repr(sys.argv[1:]) + '\\n')\n"
                     f"time.sleep({delay})\nopen({str(self.ssh_done)!r}, 'a').write('ended\\n')\n"
                     f"sys.stderr.write({err!r})\nsys.exit({code})\n", encoding="utf-8")
        p.chmod(0o755)
        self.set_path(f"{d}{os.pathsep}{os.environ.get('PATH', '')}")

    def run_streams(self, *argv: str, out: io.StringIO, err: io.StringIO) -> int:
        """`iempc` with its own stdout and stderr streams (run_main's are plain)."""
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            return ip.main(list(argv))

    def gen3_warnings(self, err: str) -> list[str]:
        return [line for line in err.splitlines() if "WARNING" in line and "Gen 3" in line]

    def test_the_flag_exists_when_the_stop_starts(self) -> None:
        self.gen3_host()
        self.ssh_program(0)
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[0], {"flag": str(ip.EVENT_NOW), "written": True})
        self.assertEqual(self.ssh_flag.read_text(encoding="utf-8"), "True\n")

    def test_a_stop_popen_refuses_never_stops_the_event_path(self) -> None:
        # The env file's bytes are kept: a NUL in GEN3_HOST makes Popen raise
        # ValueError ("embedded null byte"), not OSError.
        self.gen3_env("tester@gen3" + chr(0) + ".test")
        self.ssh_program(0)
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[0], {"flag": str(ip.EVENT_NOW), "written": True})
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
        self.assertEqual(self.ssh_args(), [])
        warnings = self.gen3_warnings(err)
        self.assertEqual(len(warnings), 1, err)
        self.assertIn("could not start", warnings[0])
        self.assertNotIn("alarm the owner", err)

    def test_ssh_missing_from_path_never_stops_the_event_path(self) -> None:
        self.gen3_host()
        empty = self.tmp / "no-programs"
        empty.mkdir()
        self.set_path(str(empty))
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
        warnings = self.gen3_warnings(err)
        self.assertEqual(len(warnings), 1, err)
        self.assertIn("could not start", warnings[0])
        self.assertEqual([d for d in docs if "gen3_stop" in d], [])

    def test_a_closed_stderr_never_stops_the_event_path(self) -> None:
        # Every Gen 3 line (the info line, a warning at the start, a warning
        # at the end) meets a closed pipe; the PC's own lines still go out.
        for host, code_ssh in ((None, 0), ("-oProxyCommand=true", 0), (self.HOST, 255)):
            with self.subTest(host=host):
                self.gen3_env(host)
                self.ssh_program(code_ssh, err="ssh: no route\n")
                self.pc.calls.clear()
                out, err = io.StringIO(), Breaks("Gen 3", "GEN3_HOST")
                code = self.run_streams("event", out=out, err=err)
                self.assertEqual(code, 0, err.getvalue())
                self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
                self.assertGreaterEqual(err.broken, 1)
                docs = [json.loads(line) for line in out.getvalue().splitlines() if line.strip()]
                if host == self.HOST:
                    self.assertEqual(docs[-1], {"gen3_stop": {"ok": False, "exit": 255}})

    def test_a_failure_in_the_end_phase_never_changes_the_exit_code(self) -> None:
        # The stop's report meets a closed stdout: the PC's exit code stays,
        # a failed PC path still ends with its owner alarm.
        self.gen3_host()
        self.ssh_program(0)

        def down():
            raise ip.StepError("ssh failed (exit 255): no route")

        for reply, want in (((0, OK), 0), ((1, OK), 1), (down, 1)):
            with self.subTest(want=want, reply=reply):
                self.pc.replies[("event", "--signal")] = reply
                out, err = Breaks('"gen3_stop"'), io.StringIO()
                code = self.run_streams("event", out=out, err=err)
                self.assertEqual(code, want, err.getvalue())
                self.assertEqual(out.broken, 1)
                warnings = self.gen3_warnings(err.getvalue())
                self.assertEqual(len(warnings), 1, err.getvalue())
                self.assertIn("could not be written", warnings[0])
                if reply is down:
                    self.assertIn("alarm the owner now", err.getvalue())

    def gen3_log(self) -> Path:
        return ip.STATE_DIR / "gen3-stop.log"

    def wait_for_text(self, path: Path, text: str, within: float = 15.0) -> str:
        deadline = time.monotonic() + within
        seen = ""
        while time.monotonic() < deadline:
            seen = path.read_text(encoding="utf-8") if path.exists() else ""
            if text in seen:
                break
            time.sleep(0.02)
        return seen

    def test_a_stop_left_running_writes_into_a_log_that_outlives_the_event_path(self) -> None:
        # Its output goes into iempc's state folder, never into a pipe that
        # closes when iempc ends; in a session of its own, so the end of the
        # terminal (a hang-up) never reaches it.
        self.gen3_host()
        self.ssh_program(0, delay=1.0, err="ssh: a late line\n")
        self.patch(GEN3_STOP_S=0.2)
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[-1], {"gen3_stop": {"ok": False, "running": True}})
        warnings = self.gen3_warnings(err)
        self.assertEqual(len(warnings), 1, err)
        self.assertIn(str(self.gen3_log()), warnings[0])
        self.assertTrue(self.wait_for(self.ssh_done), "the ssh stand-in never ended")
        self.assertIn("ssh: a late line", self.wait_for_text(self.gen3_log(), "ssh: a late line"))
        self.assertNotEqual(int(self.ssh_sid.read_text(encoding="utf-8")), os.getsid(0))

    def test_each_stop_appends_to_the_log_and_its_warning_names_only_its_own_output(self) -> None:
        self.gen3_host()
        for n in (1, 2):
            self.ssh_program(255, err=f"ssh: attempt {n} found no route\n")
            code, docs, err = self.run_main("event")
            self.assertEqual(code, 0, err)
            self.assertEqual(docs[-1], {"gen3_stop": {"ok": False, "exit": 255}})
            warnings = self.gen3_warnings(err)
            self.assertEqual(len(warnings), 1, err)
            self.assertIn(f"attempt {n} found no route", warnings[0])
            self.assertNotIn(f"attempt {3 - n}", warnings[0])
        log = self.gen3_log().read_text(encoding="utf-8")
        self.assertLess(log.index("attempt 1"), log.index("attempt 2"))
        self.assertEqual(log.count("gen3 stop started"), 2, log)

    def test_a_log_that_cannot_be_opened_never_keeps_the_stop_from_running(self) -> None:
        blocker = self.tmp / "a-file"
        blocker.write_text("a file where the state folder should be", encoding="utf-8")
        self.patch(STATE_DIR=blocker / "state")
        self.gen3_host()
        self.ssh_program(0)
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertEqual(len(self.ssh_args()), 1)
        self.assertEqual(docs[-1], {"gen3_stop": {"ok": True, "exit": 0}})
        warnings = self.gen3_warnings(err)
        self.assertEqual(len(warnings), 1, err)
        self.assertIn("its log could not be opened", warnings[0])

    def ssh_args(self) -> list[list[str]]:
        if not self.ssh_log.exists():
            return []
        return [ast.literal_eval(line) for line in self.ssh_log.read_text(encoding="utf-8").splitlines()]

    def wait_for(self, path: Path, within: float = 15.0) -> bool:
        deadline = time.monotonic() + within
        while not path.exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        return path.exists()

    def test_the_event_stops_the_gen3_device_and_never_waits_for_it(self) -> None:
        self.gen3_host()
        self.ssh_program(0, delay=3)
        self.patch(GEN3_STOP_S=0.3)
        seen = []

        def switch():
            # The stop runs beside the PC's switch: its ssh has started while
            # iemmode event is still on, and it has not ended (it hangs).
            seen.append((self.wait_for(self.ssh_log, 2.0), self.ssh_done.exists()))
            return 0, OK

        self.pc.replies[("event", "--signal")] = switch
        t = time.monotonic()
        code, docs, err = self.run_main("event")
        took = time.monotonic() - t
        still_running = not self.ssh_done.exists()
        self.assertEqual((code, seen), (0, [(True, False)]), err)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
        self.assertTrue(still_running)
        self.assertLess(took, 2.0)   # cut at its 0.3 s bound, never the 3 s it hangs
        self.assertEqual(docs[-1], {"gen3_stop": {"ok": False, "running": True}})
        warnings = [line for line in err.splitlines() if "WARNING" in line and "Gen 3" in line]
        self.assertEqual(len(warnings), 1, err)
        self.assertNotIn("alarm the owner", err)
        # Left to end by itself (never force-ended), it was the one stop asked for.
        self.assertTrue(self.wait_for(self.ssh_done), "the ssh stand-in never ended")
        args = self.ssh_args()
        self.assertEqual(len(args), 1, args)
        self.assertIn("BatchMode=yes", args[0])
        self.assertEqual(args[0][-6:], [self.HOST, "sudo", "-n", "systemctl", "stop", "iem-dante"])

    def test_a_stop_that_answers_is_reported_and_warns_nothing(self) -> None:
        self.gen3_host()
        self.ssh_program(0)
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[-1], {"gen3_stop": {"ok": True, "exit": 0}})
        self.assertNotIn("WARNING", err)
        self.assertEqual(len(self.ssh_args()), 1)

    def test_a_failed_stop_is_one_warning_and_never_changes_the_exit_code(self) -> None:
        self.gen3_host()
        self.ssh_program(255, err="ssh: connect to the box: no route\n")
        for pc_exit in (0, 1):
            self.pc.replies[("event", "--signal")] = (pc_exit, OK)
            code, docs, err = self.run_main("event")
            self.assertEqual(code, pc_exit, err)
            self.assertEqual(docs[-1], {"gen3_stop": {"ok": False, "exit": 255}})
            warnings = [line for line in err.splitlines() if "WARNING" in line and "Gen 3" in line]
            self.assertEqual(len(warnings), 1, err)
            self.assertIn("exit 255", warnings[0])
            self.assertIn("no route", warnings[0])

    def test_the_stop_is_collected_when_the_pc_path_fails(self) -> None:
        # An unreachable PC still ends the event path with the owner alarm; the
        # Gen 3 stop started beside it is answered all the same.
        self.gen3_host()
        self.ssh_program(0)

        def down():
            raise ip.StepError("ssh failed (exit 255): no route")

        self.pc.replies[("event", "--signal")] = down
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 1)
        self.assertIn("alarm the owner now", err)
        self.assertIn({"gen3_stop": {"ok": True, "exit": 0}}, docs)

    def test_no_gen3_host_means_no_ssh_and_one_info_line(self) -> None:
        self.ssh_program(0)
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.ssh_args(), [])
        self.assertEqual([d for d in docs if "gen3_stop" in d], [])
        self.assertNotIn("WARNING", err)
        self.assertEqual(len([line for line in err.splitlines() if "GEN3_HOST" in line]), 1, err)

    def test_a_gen3_host_that_is_an_option_is_refused_with_a_warning(self) -> None:
        self.gen3_host("-oProxyCommand=true")
        self.ssh_program(0)
        code, _, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.ssh_args(), [])
        self.assertIn("GEN3_HOST", err)
        self.assertIn("WARNING", err)

    def test_a_dry_run_stops_nothing(self) -> None:
        self.gen3_host()
        self.ssh_program(0)
        code, docs, err = self.run_main("event", "--dry-run")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.ssh_args(), [])
        self.assertIn({"gen3_stop": {"plan": "systemctl stop iem-dante"}}, docs)


if __name__ == "__main__":
    unittest.main()
