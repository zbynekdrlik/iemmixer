"""Tests for scripts/iem-pc/iempc_live.py: `iempc dispatch-live` and the
reverse guards of `dispatch-soak` and `switch-test` (S7 part 4, #10; plan
Task 24, Review Focus 7). They reuse test_iempc's fakes (FakePc stands in for
ssh, FakeGh for GitHub); every value is synthetic."""
from __future__ import annotations

import contextlib
import datetime as dt
import fcntl
import io
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc_live as live  # noqa: E402
import iempc_soak  # noqa: E402
from test_iempc import RUN, SHA, SHA2, Base, ip  # noqa: E402
from test_iempc_soak import READY, SWITCHING, engine, ready  # noqa: E402
import test_iempc_switch as sw  # noqa: E402

NOTHING = "(nothing was dispatched)"


def ago(seconds: float) -> str:
    """An aware ISO time `seconds` before now, as `now_iso` writes it."""
    return (dt.datetime.now().astimezone() - dt.timedelta(seconds=seconds)).isoformat(timespec="seconds")


def run_record(sha: str = SHA, entry: int = 0, seconds_ago: float = 60, **kw) -> dict:
    """One live.json record as `dispatch` writes it."""
    return {"sha": sha, "branch": "dev", "run": RUN, "entry": entry, "at": ago(seconds_ago), **kw}


def soak_record(entry: int = 0, hours: int = 8, seconds_ago: float = 60) -> dict:
    return {"sha": SHA, "branch": "dev", "run": RUN, "hours": hours, "entry": entry, "at": ago(seconds_ago)}


class LiveBase(Base):
    def setUp(self) -> None:
        super().setUp()
        self.status(READY)

    def status(self, reply: dict, code: int = 0) -> None:
        self.pc.replies[("status",)] = (code, json.dumps(reply))

    def live(self, *extra: str) -> tuple[int, list[dict], str]:
        return self.run_main("dispatch-live", "--sha", SHA, *extra)

    def runs(self) -> list[dict]:
        return ip.read_json(ip.STATE_DIR / "live.json", {}).get("runs", [])

    def write_runs(self, runs) -> None:
        ip.write_json(ip.state_dir() / "live.json", {"runs": runs})

    def write_soaks(self, soaks) -> None:
        ip.write_json(ip.state_dir() / "soak.json", {"soaks": soaks})

    def soaks(self) -> list[dict]:
        return ip.read_json(ip.STATE_DIR / "soak.json", {}).get("soaks", [])

    def dispatched(self) -> list[list[str]]:
        return self.gh.named("workflow", "run")

    def refused(self, words: str, what=None, runs=None) -> str:
        """One dispatch-live refused after its status read: exit 1, no output,
        no dispatch, the record unchanged; returns stderr."""
        before = self.runs() if runs is None else runs
        code, docs, err = self.live()
        self.assertEqual((code, docs, self.dispatched(), self.runs()), (1, [], [], before), what)
        self.assertIn(words, err, what)
        self.assertNotIn("Traceback", err, what)
        return err

    def refused_before_any_call(self, words: str, what=None) -> str:
        """One dispatch-live refused before the PC or gh was asked anything."""
        before = self.runs()
        self.pc.calls.clear()
        self.gh.calls.clear()
        code, docs, err = self.live()
        self.assertEqual((code, docs, self.pc.calls, self.gh.calls, self.runs()), (1, [], [], [], before), what)
        self.assertIn(words, err, what)
        self.assertNotIn("Traceback", err, what)
        return err


class DispatchLiveTests(LiveBase):
    def test_a_live_run_is_dispatched_with_the_active_bundle_in_dev(self) -> None:
        code, docs, err = self.live()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.dispatched(), [["workflow", "run", "live.yml", "-R", ip.OPS_REPO, "-f", f"sha={SHA}",
                                              "-f", "branch=dev", "-f", f"run={RUN}"]])
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "abandon")])
        [rec] = self.runs()
        at = rec.pop("at")
        self.assertEqual(rec, {"sha": SHA, "branch": "dev", "run": RUN, "entry": 0})
        self.assertIsNotNone(dt.datetime.fromisoformat(at).tzinfo)
        self.assertEqual(docs, [{"dispatched_live": {**rec, "at": at}}])
        # The green run is looked up for this SHA, push runs of ci.yml only (P5).
        [listing] = self.gh.named("run", "list")
        self.assertEqual(listing[listing.index("--commit") + 1], SHA)
        self.assertEqual(listing[listing.index("--event") + 1], "push")
        # The command's spec: a PC command, dev time, one at a time.
        spec = ip.COMMANDS["dispatch-live"]
        self.assertEqual((spec.pc, spec.dev_time, spec.locked), (True, True, True))

    def test_the_green_runs_branch_and_id_are_passed(self) -> None:
        self.gh.runs = [{"databaseId": RUN + 1, "headSha": SHA, "event": "push", "headBranch": "main",
                         "conclusion": "success"}]
        self.gh.jobs[RUN + 1] = self.gh.jobs[RUN]
        code, _, err = self.live()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.dispatched(), [["workflow", "run", "live.yml", "-R", ip.OPS_REPO, "-f", f"sha={SHA}",
                                              "-f", "branch=main", "-f", f"run={RUN + 1}"]])
        self.assertEqual({k: self.runs()[0][k] for k in ("branch", "run")}, {"branch": "main", "run": RUN + 1})

    def test_the_flag_refuses_before_any_call(self) -> None:
        self.flag()
        code, docs, err = self.live()
        self.assertEqual((code, docs, self.pc.calls, self.gh.calls, self.runs()), (1, [], [], [], []))
        self.assertIn("'dispatch-live' runs only in dev time", err)

    def test_a_flag_that_appears_during_the_status_read_runs_the_event_path(self) -> None:
        self.pc.replies[("status",)] = lambda: (self.flag(), (0, json.dumps(READY)))[1]
        code, docs, _ = self.live()
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.pc.calls[0], ("iemmode.exe", ["status"], "abandon"))
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["event"], "ignore"))
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})
        self.assertEqual((self.gh.calls, self.runs()), ([], []))

    def test_a_flag_during_the_gh_waits_stops_the_dispatch(self) -> None:
        self.gh.on_list = self.flag
        code, docs, err = self.live()
        self.assertEqual((code, docs, self.dispatched(), self.runs()), (1, [], [], []))
        self.assertIn("no live run dispatch during an event (nothing was dispatched)", err)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "abandon")])

    def test_a_failed_step_after_a_new_flag_runs_the_event_path(self) -> None:
        """dispatch-live talks to the PC: a failure once a new flag exists must
        still bring REAPER back (Spec pc=True)."""
        self.gh.on_list = self.flag
        self.gh.runs[0]["conclusion"] = "failure"
        code, _, err = self.live()
        self.assertEqual(code, ip.PREEMPTED)
        self.assertIn("no green push run", err)
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["event"], "ignore"))
        self.assertEqual((self.dispatched(), self.runs()), ([], []))

    def test_not_settled_another_bundle_or_engine_or_parked_is_refused(self) -> None:
        cases = [
            (ready(mode="event", detail=f"mode event; bundle {SHA}"), "the guard is in mode event, not dev"),
            (ready(mode="live", detail=f"mode live; bundle {SHA}"), "the guard is in mode live, not dev"),
            (ready(switching=SWITCHING), f"a switch runs ({json.dumps(SWITCHING)})"),
            (ready(detail=f"mode dev; bundle {SHA}; HIL job 42"), "a HIL job runs (HIL job 42)"),
            (ready(ok=False), "the guard's status did not answer ok"),
            (ready(ok=...), "the guard's status did not answer ok"),
            (ready(detail=f"mode dev; bundle {SHA2}"), f"the active bundle is {SHA2}, not {SHA}"),
            (ready(detail="mode dev; no bundle"), f"the active bundle is none, not {SHA}"),
            (engine(build=SHA2), f"the running engine's build is {SHA2!r}, not {SHA}"),
            (engine(build=...), f"the running engine's build is None, not {SHA}"),
            (ready(engine=None), "no engine runs"),
            (ready(engine="running"), "no engine runs"),
        ]
        for state in ("parked", "faulted"):
            for value in (True, None, "false", 0, ...):
                shown = None if value is ... else value
                cases.append((engine(**{state: value}),
                              f"the engine is {state} ({shown!r}); a live run measures an engine that plays"))
        for reply, words in cases:
            self.status(reply)
            self.refused(f"no live run: {words} {NOTHING}", reply)
        self.status(READY, code=3)
        self.refused(f"no live run: iemmode status failed (exit 3) {NOTHING}")
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "abandon")] * (len(cases) + 1))
        self.assertEqual(self.gh.calls, [])

    def test_a_sha_without_a_green_push_run_is_refused(self) -> None:
        for key, value in (("conclusion", "failure"), ("event", "pull_request"), ("headBranch", "feature"),
                           ("headSha", SHA2)):
            good = dict(self.gh.runs[0])
            self.gh.runs[0][key] = value
            self.refused("no green push run", (key, value))
            self.gh.runs[0] = good
        self.gh.jobs[RUN] = [{"name": "bundle", "conclusion": "success"}, {"name": "attest", "conclusion": "failure"}]
        self.refused("with its 'bundle' and 'attest' jobs succeeded")

    def test_one_live_run_per_sha_per_dev_entry(self) -> None:
        self.assertEqual(self.live()[0], 0)
        err = self.refused_before_any_call(f"a live run of {SHA} was already dispatched in dev entry 0")
        self.assertIn(f"gh run rerun <id> -R {ip.OPS_REPO}", err)
        # Still refused once its window has passed: one dispatch per SHA per dev entry.
        self.write_runs([run_record(seconds_ago=live.WINDOW_S + 60)])
        self.refused_before_any_call(f"a live run of {SHA} was already dispatched in dev entry 0")
        ip.next_entry(SHA)
        code, _, err = self.live()
        self.assertEqual(code, 0, err)
        self.assertEqual([(r["sha"], r["entry"]) for r in self.runs()], [(SHA, 0), (SHA, 1)])

    def test_another_live_run_of_this_entry_that_may_still_run_refuses(self) -> None:
        """A second run would queue behind the first (live.yml's concurrency
        group) and outlive the window its record promises."""
        self.write_runs([run_record(sha=SHA2, seconds_ago=live.WINDOW_S - 60)])
        err = self.refused_before_any_call("no live run: a live run dispatched in this dev entry may still run")
        self.assertIn(SHA2, err)
        self.assertIn(NOTHING, err)
        # Past its window, or of another dev entry, it does not.
        for rec in (run_record(sha=SHA2, seconds_ago=live.WINDOW_S + 60), run_record(sha=SHA2, entry=1)):
            self.write_runs([rec])
            self.pc.calls.clear()
            code, _, err = self.live()
            self.assertEqual(code, 0, (rec, err))
            self.write_runs([])

    def test_a_soak_of_this_entry_that_may_still_run_refuses(self) -> None:
        """The live run's pc-begin restarts the engine in a HIL job: a soak of
        this entry would end red (another engine pid, frames lost)."""
        self.write_soaks([soak_record(hours=8, seconds_ago=3600)])
        err = self.refused_before_any_call("no live run: a soak dispatched in this dev entry may still run")
        self.assertIn(NOTHING, err)
        # Its hours plus the soak's margin still count; a record whose time cannot be read counts too.
        self.write_soaks([soak_record(hours=1, seconds_ago=3600 + iempc_soak.RUN_MARGIN_S - 60)])
        self.refused_before_any_call("a soak dispatched in this dev entry may still run")
        self.write_soaks([dict(soak_record(hours=1), at="later")])
        self.refused_before_any_call("a soak dispatched in this dev entry may still run")
        # One of another dev entry, or one past its hours and margin, does not.
        self.write_soaks([soak_record(entry=1, hours=8), soak_record(hours=1, seconds_ago=3600 +
                                                                     iempc_soak.RUN_MARGIN_S + 60)])
        code, _, err = self.live()
        self.assertEqual(code, 0, err)

    def test_an_unreadable_live_record_counts_as_running(self) -> None:
        """Fail safe: a record whose time, or whose dev entry, cannot be read
        may still run; a record file of another shape is an error to check by
        hand, never read as none."""
        for rec in (run_record(sha=SHA2, at="later"), run_record(sha=SHA2, at=None),
                    run_record(sha=SHA2, at="2026-10-09T10:00:00"),   # no zone: whose clock?
                    run_record(sha=SHA2, seconds_ago=live.WINDOW_S + 60, entry=None),
                    run_record(sha=SHA2, seconds_ago=live.WINDOW_S + 60, entry="0"),
                    run_record(sha=SHA2, seconds_ago=live.WINDOW_S + 60, entry=True)):
            self.write_runs([rec])
            self.refused_before_any_call("a live run dispatched in this dev entry may still run", rec)
        path = ip.state_dir() / "live.json"
        for text in ("[]", "not json", json.dumps({"runs": {"sha": SHA}}), json.dumps({"runs": [SHA]}),
                     json.dumps({"runs": None})):
            path.write_text(text, encoding="utf-8")
            self.pc.calls.clear()
            self.gh.calls.clear()
            code, _, err = self.live()
            self.assertEqual((code, self.pc.calls, self.gh.calls), (1, [], []), text)
            self.assertIn("live.json", err, text)
            self.assertIn("check it by hand", err, text)
            self.assertEqual(path.read_text(encoding="utf-8"), text)

    def test_a_failed_workflow_dispatch_is_not_recorded(self) -> None:
        self.gh_program("sys.stderr.write('HTTP 422: Workflow does not have workflow_dispatch trigger'); sys.exit(1)")
        self.route_to_real_gh("workflow", "run")
        code, docs, err = self.live()
        self.assertEqual((code, docs, self.runs()), (1, [], []))
        self.assertIn("gh workflow run failed (exit 1): HTTP 422", err)
        ip.gh = self.gh   # gh works again: the same SHA and entry is no repeat
        self.assertEqual(self.live()[0], 0)
        self.assertEqual(len(self.runs()), 1)

    def test_the_record_keeps_the_last_200_runs(self) -> None:
        old = [run_record(sha=SHA2, entry=n + 1) for n in range(200)]
        self.write_runs(old)
        self.assertEqual(self.live()[0], 0)
        runs = self.runs()
        self.assertEqual(len(runs), 200)
        self.assertEqual(runs[:-1], old[1:])
        self.assertEqual(runs[-1]["sha"], SHA)

    def test_a_bad_sha_is_refused_before_any_call(self) -> None:
        with self.assertRaises(SystemExit) as cm, contextlib.redirect_stderr(io.StringIO()):
            ip.main(["dispatch-live"])   # --sha is required: argparse's usage error
        self.assertEqual(cm.exception.code, 2)
        for bad in (SHA.upper(), SHA[:-1], SHA + "0", "main"):
            code, _, err = self.run_main("dispatch-live", "--sha", bad)
            self.assertEqual((code, self.pc.calls, self.gh.calls), (1, [], []), bad)
            self.assertIn("not a full commit SHA", err, bad)

    def test_one_command_at_a_time(self) -> None:
        with open(ip.state_dir() / "iempc.lock", "a+", encoding="utf-8") as held:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            code, _, err = self.live()
        self.assertEqual((code, self.pc.calls, self.gh.calls), (1, [], []))
        self.assertIn("another iempc command runs", err)
        self.assertEqual(self.live()[0], 0)


class ReverseGuardTests(LiveBase):
    """`dispatch-soak` and `switch-test` refuse while a live run of this dev
    entry may still run: the soak would meet the run's engine restart, a
    switch would end the run (its pc job leaves on any mode but dev)."""

    def switch_ready(self) -> None:
        self.pc.replies.clear()
        self.pc.replies[("status",)] = (0, json.dumps(sw.READY))
        self.pc.replies[("event",)] = sw.answer("event", sw.EVENT)
        self.pc.replies[("dev",)] = sw.answer("dev", sw.DEV)

    def test_dispatch_soak_and_switch_test_refuse_while_a_live_run_may_still_run(self) -> None:
        self.write_runs([run_record(seconds_ago=live.WINDOW_S - 60)])
        for argv, words in ((["dispatch-soak", "--sha", SHA], "no soak: a live run dispatched in this dev entry may "
                                                              "still run"),
                            (["switch-test"], "no switch test: a live run dispatched in this dev entry may still run")):
            code, docs, err = self.run_main(*argv)
            self.assertEqual((code, docs, self.pc.calls, self.gh.calls, self.soaks(), ip.current_entry()),
                             (1, [], [], [], [], 0), argv)
            self.assertIn(words, err, argv)
            self.assertIn(SHA, err, argv)
            self.assertNotIn("Traceback", err, argv)
        # Past WINDOW_S both go through.
        self.write_runs([run_record(seconds_ago=live.WINDOW_S + 60)])
        code, _, err = self.run_main("dispatch-soak", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.assertEqual(len(self.soaks()), 1)
        self.write_soaks([])   # that soak would refuse the switch test itself
        self.switch_ready()
        code, docs, err = self.run_main("switch-test")
        self.assertEqual(code, 0, err)
        [out] = [d["switch_test"] for d in docs if "switch_test" in d]
        self.assertEqual(out["conclusion"], "success")

    def test_a_live_run_of_another_dev_entry_refuses_neither(self) -> None:
        self.write_runs([run_record(entry=1, seconds_ago=60)])
        code, _, err = self.run_main("dispatch-soak", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.write_soaks([])   # that soak would refuse the switch test itself
        self.switch_ready()
        code, _, err = self.run_main("switch-test")
        self.assertEqual(code, 0, err)

    def test_an_unreadable_live_record_refuses_both(self) -> None:
        self.write_runs([run_record(at="later")])
        for argv in (["dispatch-soak", "--sha", SHA], ["switch-test"]):
            code, _, err = self.run_main(*argv)
            self.assertEqual((code, self.pc.calls, self.gh.calls), (1, [], []), argv)
            self.assertIn("a live run dispatched in this dev entry may still run", err, argv)
        (ip.state_dir() / "live.json").write_text("not json", encoding="utf-8")
        for argv in (["dispatch-soak", "--sha", SHA], ["switch-test"]):
            code, _, err = self.run_main(*argv)
            self.assertEqual((code, self.pc.calls, self.gh.calls), (1, [], []), argv)
            self.assertIn("check it by hand", err, argv)

    def test_the_flag_still_refuses_both_first(self) -> None:
        self.write_runs([run_record()])
        self.flag()
        for argv in (["dispatch-soak", "--sha", SHA], ["switch-test"]):
            code, _, err = self.run_main(*argv)
            self.assertEqual((code, self.pc.calls, self.gh.calls), (1, [], []), argv)
            self.assertIn("runs only in dev time", err, argv)


class PureTests(LiveBase):
    def test_live_refusal_is_pure_and_in_order(self) -> None:
        self.assertIsNone(live.live_refusal(READY, SHA))
        self.assertEqual(live.live_refusal(READY, SHA2), f"the active bundle is {SHA}, not {SHA2}")
        self.assertEqual(live.live_refusal(None, SHA), "iemmode status gave no reply")
        self.assertEqual(live.live_refusal(ready(mode="event", switching=SWITCHING, detail="mode event; no bundle",
                                                 engine=None), SHA), "the guard is in mode event, not dev")
        self.assertEqual(live.live_refusal(ready(detail="mode dev; no bundle; HIL job 7", engine=None), SHA),
                         "a HIL job runs (HIL job 7)")
        self.assertEqual(live.live_refusal(engine(build=SHA2, parked=True), SHA),
                         f"the running engine's build is {SHA2!r}, not {SHA}")
        self.assertEqual(live.live_refusal(engine(parked=True), SHA),
                         "the engine is parked (True); a live run measures an engine that plays")
        # The soak's own refusal is unchanged.
        self.assertEqual(iempc_soak.soak_refusal(engine(faulted=True), SHA),
                         "the engine is faulted (True); a soak measures an engine that plays")
        self.assertEqual((self.pc.calls, self.gh.calls), ([], []))
        self.assertFalse((ip.STATE_DIR / "live.json").exists())

    def test_running_live_reads_the_window(self) -> None:
        now = dt.datetime.now().astimezone()
        at = now - dt.timedelta(seconds=live.WINDOW_S)
        self.write_runs([{"sha": SHA, "entry": 0, "at": at.isoformat()}])
        self.assertIsNotNone(live.running_live(ip, 0, now - dt.timedelta(seconds=1)))
        self.assertIsNone(live.running_live(ip, 0, now))
        self.assertIsNone(live.running_live(ip, 1, now - dt.timedelta(seconds=1)))
        # The newest record of the entry that may still run is the one named.
        self.write_runs([{"sha": SHA, "entry": 0, "at": now.isoformat()},
                         {"sha": SHA2, "entry": 0, "at": now.isoformat()},
                         {"sha": SHA, "entry": 1, "at": now.isoformat()}])
        self.assertEqual(live.running_live(ip, 0, now)["sha"], SHA2)
        self.assertIsNone(live.running_live(ip, 2, now))

    def test_the_window_covers_live_ymls_jobs(self) -> None:
        """WINDOW_S is the bound of every live.yml job that holds the PC, one
        after another (Task 25): pc-begin 15 min, then pc 60 min (browser's 45
        run beside it; Playwright's globalTimeout of 40 inside that), then
        pc-end 10, plus 5 for verify and the queue."""
        self.assertEqual(live.JOB_MINUTES, {"verify": 5, "pc-begin": 15, "pc": 60, "pc-end": 10})
        self.assertEqual(live.WINDOW_S, 5400)
        self.assertEqual(live.WINDOW_S, sum(live.JOB_MINUTES.values()) * 60)
        self.assertGreaterEqual(live.JOB_MINUTES["pc"], live.BROWSER_MINUTES)
        self.assertEqual((live.LIVE_WORKFLOW, live.RECORD), ("live.yml", "live.json"))


if __name__ == "__main__":
    unittest.main()
