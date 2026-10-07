"""Tests for scripts/iem-pc/iempc_soak.py: `iempc dispatch-soak` (S7, #10;
plan Task 8, Review Focus 7). They reuse test_iempc's fakes (FakePc stands in
for ssh, FakeGh for GitHub); every value is synthetic."""
from __future__ import annotations

import copy
import datetime as dt
import fcntl
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc_soak as soak  # noqa: E402
from test_iempc import RUN, SHA, SHA2, Base, ip  # noqa: E402

# The guard's `iemmode status` while the PC runs bundle SHA in dev, its engine playing.
READY = {"ok": True, "mode": "dev", "switching": None, "detail": f"mode dev; bundle {SHA}", "alarms": [],
         "engine": {"build": SHA, "pid": 4242, "parked": False, "faulted": False}}
SWITCHING = {"from": "event", "to": "dev", "done": ["precheck"], "started": 1790000100}
NOTHING = "(nothing was dispatched)"


def ready(**kw) -> dict:
    """READY with top-level fields replaced (a key set to ... is removed)."""
    doc = copy.deepcopy(READY)
    for key, value in kw.items():
        if value is ...:
            doc.pop(key, None)
        else:
            doc[key] = value
    return doc


def engine(**kw) -> dict:
    """READY with engine fields replaced (a key set to ... is removed)."""
    eng = copy.deepcopy(READY["engine"])
    for key, value in kw.items():
        if value is ...:
            eng.pop(key, None)
        else:
            eng[key] = value
    return ready(engine=eng)


class SoakBase(Base):
    def setUp(self) -> None:
        super().setUp()
        self.status(READY)

    def status(self, reply: dict, code: int = 0) -> None:
        self.pc.replies[("status",)] = (code, json.dumps(reply))

    def soak(self, *extra: str) -> tuple[int, list[dict], str]:
        return self.run_main("dispatch-soak", "--sha", SHA, *extra)

    def soaks(self) -> list[dict]:
        return ip.read_json(ip.STATE_DIR / "soak.json", {}).get("soaks", [])

    def dispatched(self) -> list[list[str]]:
        return self.gh.named("workflow", "run")

    def refused(self, words: str, what=None) -> str:
        """One dispatch-soak refused after its status read: exit 1, no output,
        no dispatch, nothing recorded; returns stderr."""
        code, docs, err = self.soak()
        self.assertEqual((code, docs, self.dispatched(), self.soaks()), (1, [], [], []), what)
        self.assertIn(words, err, what)
        self.assertNotIn("Traceback", err, what)
        return err

    def write_soaks(self, soaks) -> None:
        ip.write_json(ip.state_dir() / "soak.json", {"soaks": soaks})


class DispatchSoakTests(SoakBase):
    def test_a_soak_is_dispatched_with_the_active_bundle_in_dev(self) -> None:
        code, docs, err = self.soak()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.dispatched(), [["workflow", "run", "soak.yml", "-R", ip.OPS_REPO, "-f", f"sha={SHA}",
                                              "-f", "branch=dev", "-f", f"run={RUN}", "-f", "hours=8"]])
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "abandon")])
        [rec] = self.soaks()
        at = rec.pop("at")
        self.assertEqual(rec, {"sha": SHA, "branch": "dev", "run": RUN, "hours": 8, "entry": 0})
        self.assertIsNotNone(dt.datetime.fromisoformat(at).tzinfo)
        self.assertEqual(docs, [{"dispatched_soak": {**rec, "at": at}}])
        # The green run is looked up for this SHA, push runs of ci.yml only (P5).
        [listing] = self.gh.named("run", "list")
        self.assertEqual(listing[listing.index("--commit") + 1], SHA)
        self.assertEqual(listing[listing.index("--event") + 1], "push")

    def test_the_hours_and_the_green_runs_branch_and_id_are_passed(self) -> None:
        self.gh.runs = [{"databaseId": RUN + 1, "headSha": SHA, "event": "push", "headBranch": "main",
                         "conclusion": "success"}]
        self.gh.jobs[RUN + 1] = self.gh.jobs[RUN]
        code, docs, err = self.soak("--hours", "3")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.dispatched(), [["workflow", "run", "soak.yml", "-R", ip.OPS_REPO, "-f", f"sha={SHA}",
                                              "-f", "branch=main", "-f", f"run={RUN + 1}", "-f", "hours=3"]])
        self.assertEqual({k: self.soaks()[0][k] for k in ("branch", "run", "hours")},
                         {"branch": "main", "run": RUN + 1, "hours": 3})

    def test_the_flag_refuses_the_dispatch_before_any_call(self) -> None:
        self.flag()
        code, docs, err = self.soak()
        self.assertEqual((code, docs, self.pc.calls, self.gh.calls, self.soaks()), (1, [], [], [], []))
        self.assertIn("'dispatch-soak' runs only in dev time", err)

    def test_a_flag_that_appears_during_the_status_read_runs_the_event_path(self) -> None:
        self.pc.replies[("status",)] = lambda: (self.flag(), (0, json.dumps(READY)))[1]
        code, docs, _ = self.soak()
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.pc.calls[0], ("iemmode.exe", ["status"], "abandon"))
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["event"], "ignore"))
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})
        self.assertEqual((self.gh.calls, self.soaks()), ([], []))

    def test_a_flag_that_appears_during_the_gh_waits_stops_the_dispatch(self) -> None:
        self.gh.on_list = self.flag
        code, docs, err = self.soak()
        self.assertEqual((code, docs, self.dispatched(), self.soaks()), (1, [], [], []))
        self.assertIn("no soak dispatch during an event (nothing was dispatched)", err)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "abandon")])

    def test_a_failed_step_after_a_new_flag_runs_the_event_path(self) -> None:
        """dispatch-soak talks to the PC: a failure once a new flag exists must
        still bring REAPER back (Spec pc=True)."""
        self.gh.on_list = self.flag
        self.gh.runs[0]["conclusion"] = "failure"
        code, _, err = self.soak()
        self.assertEqual(code, ip.PREEMPTED)
        self.assertIn("no green push run", err)
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["event"], "ignore"))
        self.assertEqual((self.dispatched(), self.soaks()), ([], []))

    def test_not_dev_switching_or_a_hil_job_is_refused(self) -> None:
        for reply, words in (
            (ready(mode="event", detail=f"mode event; bundle {SHA}"), "the guard is in mode event, not dev"),
            (ready(mode="live", detail=f"mode live; bundle {SHA}"), "the guard is in mode live, not dev"),
            (ready(switching=SWITCHING), f"a switch runs ({json.dumps(SWITCHING)})"),
            (ready(detail=f"mode dev; bundle {SHA}; HIL job 42"), "a HIL job runs (HIL job 42)"),
            (ready(detail=f"mode dev; bundle {SHA}; HIL job 42; 2 unacknowledged alarms"), "a HIL job runs (HIL job 42)"),
            (ready(detail=f"mode dev; bundle {SHA}; HIL job"), "a HIL job runs (HIL job)"),
            (ready(ok=False), "the guard's status did not answer ok"),
            (ready(ok=None), "the guard's status did not answer ok"),
            (ready(ok="true"), "the guard's status did not answer ok"),
            (ready(ok=...), "the guard's status did not answer ok"),
        ):
            self.status(reply)
            self.refused(f"no soak: {words} {NOTHING}", reply)
        self.status(READY, code=3)
        self.refused(f"no soak: iemmode status failed (exit 3) {NOTHING}")
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "abandon")] * 11)
        self.assertEqual(self.gh.calls, [])

    def test_another_active_bundle_or_engine_build_or_no_engine_is_refused(self) -> None:
        for reply, words in (
            (ready(detail=f"mode dev; bundle {SHA2}"), f"the active bundle is {SHA2}, not {SHA}"),
            (ready(detail="mode dev; no bundle"), f"the active bundle is none, not {SHA}"),
            (ready(detail=f"mode dev; no bundle; bundle {SHA}"), f"the active bundle is none, not {SHA}"),
            (ready(detail=...), f"the active bundle is none, not {SHA}"),
            (engine(build=SHA2), f"the running engine's build is {SHA2!r}, not {SHA}"),
            (engine(build=...), f"the running engine's build is None, not {SHA}"),
            (ready(engine=...), "no engine runs"),
            (ready(engine=None), "no engine runs"),
            (ready(engine="running"), "no engine runs"),
            (ready(engine=[READY["engine"]]), "no engine runs"),
        ):
            self.status(reply)
            self.refused(f"no soak: {words}", reply)
        self.assertEqual(self.gh.calls, [])

    def test_a_parked_or_faulted_engine_is_refused(self) -> None:
        for state in ("parked", "faulted"):
            for value in (True, None, "false", 0, ...):
                self.status(engine(**{state: value}))
                shown = None if value is ... else value
                self.refused(f"no soak: the engine is {state} ({shown!r}); a soak measures an engine that plays",
                             (state, value))
        self.assertEqual(self.gh.calls, [])

    def test_a_sha_without_a_green_push_run_is_refused(self) -> None:
        cases = (
            ("conclusion", "failure"),
            ("event", "pull_request"),
            ("headBranch", "feature"),
            ("headSha", SHA2),
        )
        for key, value in cases:
            good = dict(self.gh.runs[0])
            self.gh.runs[0][key] = value
            self.refused("no green push run", (key, value))
            self.gh.runs[0] = good
        self.gh.jobs[RUN] = [{"name": "bundle", "conclusion": "success"}, {"name": "attest", "conclusion": "failure"}]
        self.refused("with its 'bundle' and 'attest' jobs succeeded")
        self.gh.jobs[RUN] = [{"name": "attest", "conclusion": "success"}]
        self.refused("with its 'bundle' and 'attest' jobs succeeded")

    def test_one_soak_per_sha_per_dev_entry(self) -> None:
        self.assertEqual(self.soak()[0], 0)
        self.pc.calls.clear()
        self.gh.calls.clear()
        code, docs, err = self.soak()
        self.assertEqual((code, docs, self.pc.calls, self.gh.calls), (1, [], [], []))
        self.assertIn(f"a soak of {SHA} was already dispatched in dev entry 0", err)
        self.assertIn(f"gh run rerun <id> -R {ip.OPS_REPO}", err)
        ip.next_entry(SHA)
        code, _, err = self.soak()
        self.assertEqual(code, 0, err)
        self.assertEqual([(s["sha"], s["entry"]) for s in self.soaks()], [(SHA, 0), (SHA, 1)])
        self.assertEqual(len(self.dispatched()), 1)

    def test_another_shas_soak_or_another_entrys_is_no_repeat(self) -> None:
        self.write_soaks([{"sha": SHA2, "entry": 0}, {"sha": SHA, "entry": 1}])
        code, _, err = self.soak()
        self.assertEqual(code, 0, err)
        self.assertEqual([(s["sha"], s["entry"]) for s in self.soaks()], [(SHA2, 0), (SHA, 1), (SHA, 0)])

    def test_a_failed_workflow_dispatch_is_not_recorded(self) -> None:
        self.gh_program("sys.stderr.write('HTTP 422: Workflow does not have workflow_dispatch trigger'); sys.exit(1)")
        self.route_to_real_gh("workflow", "run")
        code, docs, err = self.soak()
        self.assertEqual((code, docs, self.soaks()), (1, [], []))
        self.assertIn("gh workflow run failed (exit 1): HTTP 422", err)
        ip.gh = self.gh   # gh works again: the same SHA and entry is no repeat
        self.assertEqual(self.soak()[0], 0)
        self.assertEqual(len(self.soaks()), 1)

    def test_the_record_keeps_the_last_200_soaks(self) -> None:
        old = [{"sha": SHA2, "entry": n} for n in range(200)]
        self.write_soaks(old)
        self.assertEqual(self.soak()[0], 0)
        soaks = self.soaks()
        self.assertEqual(len(soaks), 200)
        self.assertEqual(soaks[:-1], old[1:])
        self.assertEqual(soaks[-1]["sha"], SHA)

    def test_hours_outside_1_to_9_are_refused(self) -> None:
        for hours in ("0", "10", "-1", "99"):
            code, docs, err = self.run_main("dispatch-soak", "--sha", SHA, f"--hours={hours}")
            self.assertEqual((code, docs, self.pc.calls, self.gh.calls, self.soaks()), (1, [], [], [], []), hours)
            self.assertIn("--hours must be 1..9", err, hours)
        for n, hours in enumerate(("1", "9")):
            ip.next_entry(SHA)
            code, _, err = self.soak("--hours", hours)
            self.assertEqual(code, 0, err)
            self.assertEqual(self.dispatched()[n][-1], f"hours={hours}")
            self.assertEqual(self.soaks()[n]["hours"], int(hours))

    def test_a_bad_sha_or_soak_record_is_refused_before_any_call(self) -> None:
        for bad in (SHA.upper(), SHA[:-1], SHA + "0", "main"):
            code, _, err = self.run_main("dispatch-soak", "--sha", bad)
            self.assertEqual((code, self.pc.calls, self.gh.calls), (1, [], []), bad)
            self.assertIn("not a full commit SHA", err, bad)
        path = ip.state_dir() / "soak.json"
        for text in ("[]", "not json", json.dumps({"soaks": {"sha": SHA}}), json.dumps({"soaks": [SHA]}),
                     json.dumps({"soaks": None})):
            path.write_text(text, encoding="utf-8")
            code, _, err = self.soak()
            self.assertEqual((code, self.pc.calls, self.gh.calls), (1, [], []), text)
            self.assertIn("soak.json", err, text)
            self.assertIn("check it by hand", err, text)
        self.assertEqual(path.read_text(encoding="utf-8"), json.dumps({"soaks": None}))

    def test_one_command_at_a_time(self) -> None:
        with open(ip.state_dir() / "iempc.lock", "a+", encoding="utf-8") as held:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            code, _, err = self.soak()
        self.assertEqual((code, self.pc.calls, self.gh.calls), (1, [], []))
        self.assertIn("another iempc command runs", err)
        self.assertEqual(self.soak()[0], 0)


class PureTests(SoakBase):
    def test_active_bundle_and_soak_refusal_are_pure(self) -> None:
        replies = {
            "ready": READY,
            "no bundle": ready(detail="mode dev; no bundle"),
            "hil": ready(detail=f"mode dev; bundle {SHA}; HIL job 42"),
            "upper": ready(detail=f"mode dev; bundle {SHA.upper()}"),
            "long": ready(detail=f"mode dev; bundle {SHA}0"),
            "short": ready(detail=f"mode dev; bundle {SHA[:-1]}"),
            "note": ready(detail=f"mode dev; no bundle; bundle {SHA}"),
            "first": ready(detail=f"bundle {SHA}; mode dev"),
            "none": ready(detail=None),
            "number": ready(detail=42),
            "absent": ready(detail=...),
        }
        before = copy.deepcopy(replies)
        self.assertEqual({k: soak.active_bundle(r) for k, r in replies.items()},
                         {"ready": SHA, "no bundle": None, "hil": SHA, "upper": None, "long": None, "short": None,
                          "note": None, "first": None, "none": None, "number": None, "absent": None})
        self.assertIsNone(soak.soak_refusal(READY, SHA))
        self.assertEqual(soak.soak_refusal(READY, SHA2), f"the active bundle is {SHA}, not {SHA2}")
        self.assertEqual(soak.soak_refusal(replies["hil"], SHA), "a HIL job runs (HIL job 42)")
        self.assertEqual(soak.soak_refusal(replies["no bundle"], SHA), f"the active bundle is none, not {SHA}")
        self.assertEqual(soak.soak_refusal(None, SHA), "iemmode status gave no reply")
        self.assertEqual(soak.soak_refusal([READY], SHA), "iemmode status gave no reply")
        # The order: mode, switch, HIL job, bundle, engine.
        self.assertEqual(soak.soak_refusal(ready(mode="event", switching=SWITCHING, detail="mode event; no bundle",
                                                 engine=None), SHA),
                         "the guard is in mode event, not dev")
        self.assertEqual(soak.soak_refusal(ready(switching=SWITCHING, detail="mode dev; no bundle; HIL job 7",
                                                 engine=None), SHA),
                         f"a switch runs ({json.dumps(SWITCHING)})")
        self.assertEqual(soak.soak_refusal(ready(detail="mode dev; no bundle; HIL job 7", engine=None), SHA),
                         "a HIL job runs (HIL job 7)")
        self.assertEqual(soak.soak_refusal(ready(detail="mode dev; no bundle", engine=None), SHA),
                         f"the active bundle is none, not {SHA}")
        self.assertEqual(replies, before)
        self.assertEqual((self.pc.calls, self.gh.calls), ([], []))
        self.assertFalse((ip.STATE_DIR / "soak.json").exists())

    def test_hours_must_be_an_int_of_1_to_9(self) -> None:
        """argparse gives an int; the check holds for any caller (a bool is no hour count)."""
        for hours in (1, 5, 9):
            self.assertEqual(soak.check_hours(ip, hours), hours)
        for hours in (0, 10, -1, True, 3.0, "3", None):
            with self.assertRaises(ip.Refused, msg=repr(hours)) as cm:
                soak.check_hours(ip, hours)
            self.assertIn("--hours must be 1..9", str(cm.exception))

    def test_the_constants_match_the_ops_job(self) -> None:
        self.assertEqual((soak.SOAK_WORKFLOW, soak.HOURS_DEFAULT, soak.HOURS_MIN, soak.HOURS_MAX),
                         ("soak.yml", 8, 1, 9))


if __name__ == "__main__":
    unittest.main()
