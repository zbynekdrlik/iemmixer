"""Tests for scripts/iem-pc/iempc_live.py: `iempc dispatch-live` and the
reverse guards of `dispatch-soak` and `switch-test` (S7 part 4, #10; plan
Task 24, Review Focus 7). They reuse iempc_test_support's fakes (FakePc stands in for
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
from iempc_test_support import RUN, SHA, SHA2, Base, ip  # noqa: E402
import test_iempc_bundle  # noqa: E402  (ActivateTests by its module: imported by name it would run twice)
from test_iempc_soak import READY, SWITCHING, engine, ready  # noqa: E402
import test_iempc_switch as sw  # noqa: E402
from test_iempc_trace import TraceBase  # noqa: E402

NOTHING = "(nothing was dispatched)"


def ago(seconds: float) -> str:
    """An aware ISO time `seconds` before now, as `now_iso` writes it."""
    return (dt.datetime.now().astimezone() - dt.timedelta(seconds=seconds)).isoformat(timespec="seconds")


def run_record(sha: str = SHA, entry: int = 0, seconds_ago: float = 60, **kw) -> dict:
    """One live.json record as `dispatch` writes it."""
    return {"sha": sha, "branch": "dev", "run": RUN, "entry": entry, "at": ago(seconds_ago), **kw}


def hil_record(entry: int = 0, seconds_ago: float = 60, **kw) -> dict:
    """One dispatch.json record as `dispatch-hil` writes it."""
    return {"sha": SHA, "branch": "dev", "run": RUN, "digest": "sha256:" + "0" * 64, "entry": entry,
            "at": ago(seconds_ago), **kw}


def hil_run(run_id: int, state: str) -> dict:
    """One row of `gh run list --json databaseId,status,createdAt`."""
    return {"databaseId": run_id, "status": state, "createdAt": "2026-10-22T05:00:00Z"}


# The HIL guard's question to the ops repo (iempc_live.active_hil_runs).
HIL_LISTING = ["run", "list", "-R", ip.OPS_REPO, "-w", "hil.yml", "--json", "databaseId,status,createdAt",
               "--limit", "20"]


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

    def write_hil(self, *records) -> None:
        ip.write_json(ip.state_dir() / "dispatch.json", {"dispatches": list(records)})

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
        [listing] = [c for c in self.gh.named("run", "list") if "--commit" in c]
        self.assertIn(HIL_LISTING, self.gh.calls)   # the HIL guard asked the ops repo too
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
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["event", "--signal"], "ignore"))
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
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["event", "--signal"], "ignore"))
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
            (ready(engine=None), "no engine runs (the guard's status shows none)"),
            (ready(engine="running"), "no engine runs (the guard's status shows none)"),
        ]
        for state in ("parked", "faulted", "held"):
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

    def test_a_queued_or_running_hil_run_refuses_the_live_dispatch(self) -> None:
        """Both begin a HIL job on the PC (hil.yml's pc job, live.yml's
        pc-begin), so the second to reach it is cancelled (#58 Part D, build
        90a7b89: a live run dispatched 5 s after a HIL run began first). The
        PC has one runner and a HIL run may wait for it, or be rerun: any
        hil.yml run the ops repo lists as not completed refuses the dispatch,
        after the status read and before the green run's lookup."""
        for state in ("queued", "in_progress", "waiting"):
            self.gh.workflow_runs["hil.yml"] = [hil_run(4242, state), hil_run(4241, "completed")]
            self.gh.calls.clear()
            err = self.refused(f"no live run: a hil.yml run is {state} in {ip.OPS_REPO} (run 4242", state)
            self.assertIn("wait for it to end", err)
            self.assertIn(NOTHING, err)
            self.assertEqual(self.gh.calls, [HIL_LISTING], state)
        # Only completed runs: a HIL dispatch of this entry past the listing's grace no longer counts.
        self.gh.workflow_runs["hil.yml"] = [hil_run(4241, "completed")]
        self.write_hil(hil_record(seconds_ago=live.LIST_GRACE_S + 60))
        code, _, err = self.live()
        self.assertEqual(code, 0, err)

    def test_a_hil_dispatch_gh_does_not_list_yet_refuses(self) -> None:
        """GitHub lists a dispatched run a moment after the dispatch: a HIL run
        this box dispatched in this dev entry within LIST_GRACE_S counts while
        gh lists none (the incident's 5 s)."""
        self.write_hil(hil_record(seconds_ago=5))
        err = self.refused("no live run: a HIL run dispatched in this dev entry")
        self.assertIn("is not listed by gh yet", err)
        self.assertIn(SHA, err)
        self.assertIn("wait for it to end", err)
        self.write_hil(hil_record(entry=1, seconds_ago=5))
        code, _, err = self.live()
        self.assertEqual(code, 0, err)

    def test_without_gh_the_hil_record_window_decides(self) -> None:
        """gh fails: dispatch-hil's record decides, hil.yml's verify and pc
        job bounds (10 + 30 min) after its dispatch; an unreadable record
        counts as running and names the file to check by hand."""
        path = ip.state_dir() / "dispatch.json"
        self.gh.workflow_list_error = "gh run list failed (exit 1): HTTP 502"
        self.write_hil(hil_record(seconds_ago=40 * 60 - 60))
        err = self.refused("no live run: a HIL run dispatched in this dev entry may still run")
        self.assertIn("gh could not list hil.yml's runs", err)
        self.assertIn("HTTP 502", err)
        self.assertIn("wait for it to end", err)
        self.assertNotIn("cannot be read", err)
        # An answer that is no list of runs is no answer either.
        self.gh.workflow_list_error = None
        self.gh.workflow_runs["hil.yml"] = {"total_count": 1}
        self.assertIn("gh could not list hil.yml's runs (gh run list gave no list of runs)",
                      self.refused("a HIL run dispatched in this dev entry may still run"))
        self.gh.workflow_list_error = "gh run list failed (exit 1): HTTP 502"
        for bad in ({"at": "later"}, {"at": "2026-10-09T10:00:00"}, {"entry": None}):
            self.write_hil(hil_record(**bad))
            err = self.refused("a HIL run dispatched in this dev entry may still run", bad)
            self.assertIn(f"check {path} by hand", err, bad)
        for text in ("not json", json.dumps({"dispatches": {"sha": SHA}})):
            path.write_text(text, encoding="utf-8")
            code, _, err = self.live()
            self.assertEqual((code, self.dispatched()), (1, []), text)
            self.assertIn("dispatch.json", err, text)
            self.assertIn("check it by hand", err, text)
        # Past its window, or of another dev entry, it does not.
        self.write_hil(hil_record(seconds_ago=40 * 60 + 60), hil_record(entry=1))
        code, _, err = self.live()
        self.assertEqual(code, 0, err)
        self.assertEqual((live.HIL_JOB_MINUTES, live.HIL_WINDOW_S, live.LIST_GRACE_S, ip.HIL_RECORD),
                         ({"verify": 10, "pc": 30}, 2400, 120, "dispatch.json"))

    def test_an_unreadable_live_record_counts_as_running(self) -> None:
        """Fail safe: a record whose time cannot be read may still run, one
        whose dev entry cannot be read may be of this entry (its window still
        bounds it); the refusal says to check the file by hand. A record file
        of another shape is an error to check by hand, never read as none."""
        for rec in (run_record(sha=SHA2, at="later"), run_record(sha=SHA2, at=None),
                    run_record(sha=SHA2, at="2026-10-09T10:00:00"),   # no zone: whose clock?
                    run_record(sha=SHA2, at="later", entry=None),
                    run_record(sha=SHA2, entry=None), run_record(sha=SHA2, entry="0"),
                    run_record(sha=SHA2, entry=True)):
            self.write_runs([rec])
            err = self.refused_before_any_call("a live run dispatched in this dev entry may still run", rec)
            self.assertIn(f"its time or dev entry cannot be read: check {ip.state_dir() / 'live.json'} by hand",
                          err, rec)
        # A readable record names no such hint.
        self.write_runs([run_record(sha=SHA2)])
        err = self.refused_before_any_call("a live run dispatched in this dev entry may still run")
        self.assertNotIn("cannot be read", err)
        # Past its window an unreadable entry no longer counts: its time is known.
        for bad in (None, "0", True):
            self.write_runs([run_record(sha=SHA2, seconds_ago=live.WINDOW_S + 60, entry=bad)])
            self.pc.calls.clear()
            code, _, err = self.live()
            self.assertEqual(code, 0, (bad, err))
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
        self.patch(gh=self.gh)   # gh works again: the same SHA and entry is no repeat
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


class SoakRecordTests(LiveBase):
    """A soak record whose dev entry cannot be read may still run (fail safe,
    as a live record's): it refuses dispatch-live and switch-test (review of
    the lane: iempc_soak.running_soak skipped it, and both went through)."""

    def test_a_soak_record_whose_entry_cannot_be_read_refuses_a_live_run_and_a_switch_test(self) -> None:
        for bad in (None, "0", True, 0.0):
            self.write_soaks([dict(soak_record(hours=8, seconds_ago=60), entry=bad)])
            self.refused_before_any_call("no live run: a soak dispatched in this dev entry may still run", bad)
            self.pc.calls.clear()
            code, docs, err = self.run_main("switch-test")
            self.assertEqual((code, docs, ip.current_entry()), (1, [], 0), bad)
            self.assertIn("no switch test: a soak dispatched in this dev entry may still run", err, bad)
            self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "abandon")], bad)
        # Past its hours and margin it does not, whatever its entry.
        self.write_soaks([dict(soak_record(hours=1, seconds_ago=3600 + iempc_soak.RUN_MARGIN_S + 60), entry=None)])
        code, _, err = self.live()
        self.assertEqual(code, 0, err)


class RunningGuardCases:
    """`activate`, `dispatch-hil` and `trace` refuse, before any call, while a
    live run or a soak of this dev entry may still run (iempc_live.
    refuse_while_running): an activation restarts the engine (inside the live
    run's HIL job too), a HIL run begins its own job and restarts it, a
    kernel trace weighs on the times both measure. A mixin: each subclass
    names its command, its words and how it goes through."""
    ARGV: tuple[str, ...] = ()
    HEAD = ""
    NOTHING = ""

    def passes(self) -> None:
        raise NotImplementedError

    def write_runs(self, runs) -> None:
        ip.write_json(ip.state_dir() / "live.json", {"runs": runs})

    def write_soaks(self, soaks) -> None:
        ip.write_json(ip.state_dir() / "soak.json", {"soaks": soaks})

    def refused_before_any_call(self, words: str, what=None) -> str:
        self.pc.calls.clear()
        self.pc.modules.clear()
        self.gh.calls.clear()
        code, docs, err = self.run_main(*self.ARGV)
        self.assertEqual((code, docs, self.pc.calls, self.pc.modules, self.gh.calls), (1, [], [], [], []), (what, err))
        self.assertIn(words, err, what)
        self.assertIn(self.NOTHING, err, what)
        self.assertNotIn("Traceback", err, what)
        return err

    def test_a_live_run_of_this_entry_that_may_still_run_refuses(self) -> None:
        self.write_runs([run_record(sha=SHA2, seconds_ago=live.WINDOW_S - 60)])
        err = self.refused_before_any_call(f"{self.HEAD}: a live run dispatched in this dev entry may still run")
        self.assertIn(SHA2, err)
        self.assertNotIn("cannot be read", err)

    def test_a_soak_of_this_entry_that_may_still_run_refuses(self) -> None:
        self.write_soaks([soak_record(hours=1, seconds_ago=3600 + iempc_soak.RUN_MARGIN_S - 60)])
        err = self.refused_before_any_call(f"{self.HEAD}: a soak dispatched in this dev entry may still run "
                                           "(dispatched ")
        self.assertIn(", 1 h)", err)

    def test_the_live_run_is_named_before_the_soak(self) -> None:
        self.write_runs([run_record(sha=SHA2)])
        self.write_soaks([soak_record()])
        self.refused_before_any_call(f"{self.HEAD}: a live run dispatched in this dev entry may still run")

    def test_unreadable_records_count_as_running(self) -> None:
        self.write_runs([run_record(sha=SHA2, at="later")])
        err = self.refused_before_any_call("a live run dispatched in this dev entry may still run")
        self.assertIn(f"check {ip.state_dir() / 'live.json'} by hand", err)
        self.write_runs([])
        for bad in ({"at": "later"}, {"at": "2026-10-09T10:00:00"}, {"entry": None}, {"hours": "8"}):
            self.write_soaks([{**soak_record(), **bad}])
            err = self.refused_before_any_call(f"{self.HEAD}: a soak dispatched in this dev entry may still run", bad)
            self.assertIn(f"its time, hours or dev entry cannot be read: check {ip.state_dir() / 'soak.json'} by hand",
                          err, bad)
        self.write_soaks([soak_record()])
        self.assertNotIn("cannot be read", self.refused_before_any_call("a soak dispatched in this dev entry"))
        self.write_soaks([])
        for name, text in (("live.json", "not json"), ("soak.json", json.dumps({"soaks": {"sha": SHA}}))):
            (ip.state_dir() / name).write_text(text, encoding="utf-8")
            self.pc.calls.clear()
            self.gh.calls.clear()
            code, docs, err = self.run_main(*self.ARGV)
            self.assertEqual((code, docs, self.pc.calls, self.gh.calls), (1, [], [], []), name)
            self.assertIn(name, err)
            self.assertIn("check it by hand", err)
            (ip.state_dir() / name).unlink()

    def test_records_of_another_entry_or_past_their_window_refuse_nothing(self) -> None:
        self.write_runs([run_record(sha=SHA2, entry=1), run_record(sha=SHA2, seconds_ago=live.WINDOW_S + 60)])
        self.write_soaks([soak_record(entry=1),
                          soak_record(hours=1, seconds_ago=3600 + iempc_soak.RUN_MARGIN_S + 60)])
        self.passes()

    def test_the_flag_still_refuses_first(self) -> None:
        self.write_runs([run_record()])
        self.write_soaks([soak_record()])
        self.flag()
        self.gh.calls.clear()   # TraceBase's own fetch
        code, docs, err = self.run_main(*self.ARGV)
        self.assertEqual((code, docs, self.pc.calls, self.gh.calls), (1, [], [], []))
        self.assertIn("runs only in dev time", err)


class ActivateGuardTests(RunningGuardCases, Base):
    ARGV = ("activate", "--sha", SHA)
    HEAD = "no activation"
    NOTHING = "(nothing was activated)"

    def setUp(self) -> None:
        super().setUp()
        self.patch(HANDOVER_S=10.0, HANDOVER_POLL_S=0.01)
        self.pc.replies[("activate", SHA)] = test_iempc_bundle.ActivateTests.ACTIVATED
        self.pc.replies[("status",)] = test_iempc_bundle.ActivateTests.status(SHA)

    def passes(self) -> None:
        code, docs, err = self.run_main(*self.ARGV)
        self.assertEqual(code, 0, err)
        self.assertEqual(self.pc.calls[0], ("iemmode.exe", ["activate", SHA], "finish"))
        self.assertEqual(docs[-1]["handover"]["guard_build"], SHA)


class HilGuardTests(RunningGuardCases, Base):
    ARGV = ("dispatch-hil", "--sha", SHA)
    HEAD = "no HIL dispatch"
    NOTHING = "(nothing was dispatched)"

    def passes(self) -> None:
        code, docs, err = self.run_main(*self.ARGV)
        self.assertEqual(code, 0, err)
        self.assertEqual(len(self.gh.named("workflow", "run")), 1)
        self.assertEqual(docs[-1]["dispatched"]["sha"], SHA)


class TraceGuardTests(RunningGuardCases, TraceBase):
    ARGV = ("trace", "--label", "base-test", "--seconds", "1")
    HEAD = "no trace"
    NOTHING = "(nothing was traced)"

    def passes(self) -> None:
        code, docs, err = self.run_main(*self.ARGV)
        self.assertEqual(code, 0, err)
        self.assertIn("start", self.names())


class NeverRefusedTests(LiveBase):
    """`iempc dev` is the owner's "event skončil" and `iempc event` his "ide
    event": neither waits for a live run or a soak record."""

    def test_dev_and_event_go_through_while_a_live_run_and_a_soak_may_still_run(self) -> None:
        self.write_runs([run_record()])
        self.write_soaks([soak_record()])
        code, _, err = self.run_main("dev")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["dev"], "abandon"))
        code, _, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["event", "--signal"], "ignore"))


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
        self.assertEqual(live.live_refusal(engine(held=True), SHA),
                         "the engine is held (True); a live run measures an engine that plays")
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

    def test_the_window_is_the_sum_of_the_job_bounds_task_25_uses(self) -> None:
        """Pins the constants only: no public test can read the private
        live.yml, whose `timeout-minutes` Task 25 sets to these bounds
        (pc-begin 15, pc 60 with browser's 45 beside it, pc-end 10, plus 5
        for verify and the pick-up)."""
        self.assertEqual(live.JOB_MINUTES, {"verify": 5, "pc-begin": 15, "pc": 60, "pc-end": 10})
        self.assertEqual(live.WINDOW_S, 5400)
        self.assertEqual((live.LIVE_WORKFLOW, live.RECORD), ("live.yml", "live.json"))


if __name__ == "__main__":
    unittest.main()
