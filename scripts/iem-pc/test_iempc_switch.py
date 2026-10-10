"""Tests for scripts/iem-pc/iempc_switch.py: `iempc switch-test` (S7 part 3,
#10; plan Task 14). They reuse iempc_test_support's fakes (FakePc stands in for ssh);
every value is synthetic."""
from __future__ import annotations

import copy
import datetime as dt
import fcntl
import json
import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc_switch as sw  # noqa: E402
from iempc_test_support import SHA, SHA2, Base, ip  # noqa: E402


def st(step: str, ms: int) -> dict:
    return {"step": step, "ms": ms}


# The event plan from dev: its silence (engine_stop through reaper_handover) is
# 25 050 ms, its handover (reaper_start through app_handover) 34 000 ms.
EVENT_STEPS = [st("jobs_cancel", 5), st("runner_stop", 900), st("engine_stop", 600), st("server_stop", 300),
               st("tray_stop", 100), st("tuning_exit", 2000), st("pref_check", 50), st("reaper_start", 15_000),
               st("reaper_handover", 7000), st("app_start", 9000), st("app_handover", 3000), st("fingerprint", 200)]
# A dev entry from event: its silence (reaper_save_quit through engine_arm) is 14 000 ms.
DEV_STEPS = [st("precheck", 400), st("app_stop", 3000), st("reaper_save_quit", 8000), st("tuning_enter", 1500),
             st("data", 700), st("pref_check", 50), st("engine_start", 2500), st("engine_arm", 1250),
             st("server_start", 900), st("tray_start", 300), st("identity_check", 600), st("runner_start", 800)]
SWITCHING = {"from": "event", "to": "dev", "done": ["precheck"], "started": 1790000100}
OWNER_FLAG = "2026-09-27T20:00:00+02:00\n"   # what Base.flag writes: the owner's "ide event"


def record(frm: str, to: str, steps: list[dict], silence, *, ended_in: str | None = None, outcome: str = "done",
           unwound: str | None = None, started: int = 1_790_000_000) -> dict:
    """A `last_switch` as the guard writes it (switch_log.rs LastSwitch)."""
    return {"from": frm, "to": to, "ended_in": ended_in or to, "outcome": outcome, "started": started,
            "ended": started + 60, "steps": copy.deepcopy(steps), "silence_ms": silence, "unwound": unwound}


BEFORE = record("event", "dev", DEV_STEPS, 14_000)   # the switch that brought the PC into dev
EVENT = record("dev", "event", EVENT_STEPS, 25_050, started=1_790_000_100)
DEV = record("event", "dev", DEV_STEPS, 14_000, started=1_790_000_200)
# A dev entry that failed at its arm and went back to event: one record of both (#10 2026-10-07).
UNWOUND = record("event", "event", DEV_STEPS[:8] + EVENT_STEPS[2:12], 52_250, unwound="dev", started=1_790_000_200)
# The guard's `iemmode status` in dev, the engine of the active bundle playing.
READY = {"ok": True, "mode": "dev", "switching": None, "detail": f"mode dev; bundle {SHA}", "alarms": [],
         "engine": {"build": SHA, "pid": 4242, "parked": False, "faulted": False}, "last_switch": BEFORE}
GREEN_NUMBERS = {"event_silence_ms": 25_050, "handover_ms": 34_000, "dev_silence_ms": 14_000}
GREEN_TEXT = "event-leg silence 25050 ms, handover 34000 ms, dev-leg silence 14000 ms"
NOTHING = "(nothing was switched)"


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


def said(mode: str) -> str:
    """The detail of `answer(mode, …)`."""
    return f"mode {mode}; bundle {SHA}"


def answer(mode: str, rec=..., code: int = 0) -> tuple[int, str]:
    """An `iemmode event|dev|status` answer: the guard in `mode`, its `last_switch` `rec` (... leaves it out)."""
    doc = {"ok": code == 0, "mode": mode, "switching": None, "alarms": [], "detail": said(mode)}
    if rec is not ...:
        doc["last_switch"] = rec
    return code, json.dumps(doc)


def in_turn(*answers):
    """A FakePc reply that gives `answers` in turn; a callable one is called (its side effects too)."""
    left = list(answers)

    def give():
        a = left.pop(0)
        return a() if callable(a) else a
    return give


def leg(exit_code: int | None, rec, detail: str | None = None, **extra) -> dict:
    """A leg as the command prints it (`detail`: the guard's reply's)."""
    return {"exit": exit_code, "record": rec, "detail": detail, **extra}


class SwitchBase(Base):
    def setUp(self) -> None:
        super().setUp()
        self.defaults()

    def defaults(self) -> None:
        """The PC in dev, both legs green; no call recorded yet."""
        self.pc.calls.clear()
        self.pc.replies.clear()
        self.status(READY)
        self.pc.replies[("event",)] = answer("event", EVENT)
        self.pc.replies[("dev",)] = answer("dev", DEV)

    def status(self, doc: dict, code: int = 0) -> None:
        self.pc.replies[("status",)] = (code, json.dumps(doc))

    def switch(self) -> tuple[int, list[dict], str]:
        return self.run_main("switch-test")

    def result(self, docs: list[dict]) -> dict:
        [out] = [d["switch_test"] for d in docs if "switch_test" in d]
        return out

    def calls(self) -> list[tuple[list[str], str]]:
        return [(args, event) for _, args, event in self.pc.calls]

    def refused(self, words: str, what=None) -> str:
        """One switch-test refused after its status read: exit 1, no output,
        no switch, no dev entry; returns stderr."""
        self.pc.calls.clear()
        code, docs, err = self.switch()
        self.assertEqual((code, docs, self.calls(), ip.current_entry()), (1, [], [(["status"], "abandon")], 0), what)
        self.assertIn(words, err, what)
        self.assertNotIn("Traceback", err, what)
        return err

    def flag_text(self) -> str:
        return ip.EVENT_NOW.read_text(encoding="utf-8")


class SwitchTestTests(SwitchBase):
    def test_a_green_run_goes_to_event_and_back_and_reports_both_records(self) -> None:
        code, docs, err = self.switch()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.calls(), [(["status"], "abandon"), (["event"], "ignore"), (["dev"], "abandon")])
        self.assertEqual(self.pc.timeouts, [ip.STATUS_S, ip.SWITCH_S, ip.SWITCH_S])
        self.assertEqual(docs, [{"switch_test": {
            "conclusion": "success", "summary": f"green: {GREEN_TEXT}", "first_failure": None,
            "numbers": GREEN_NUMBERS, "event_leg": leg(0, EVENT, said("event")), "dev_leg": leg(0, DEV, said("dev")),
            "dev_entry": 1}}])
        # The dev leg is a dev entry of this box, as `iempc dev`'s (dispatch-hil, dispatch-soak).
        self.assertEqual(ip.current_entry(), 1)
        self.assertFalse(ip.EVENT_NOW.exists())

    def test_the_running_engine_must_be_the_active_bundle_whichever_it_is(self) -> None:
        eng = dict(READY["engine"], build=SHA2)
        self.status(ready(detail=f"mode dev; bundle {SHA2}; 1 unacknowledged alarm", engine=eng))
        code, docs, err = self.switch()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.result(docs)["conclusion"], "success")

    def test_a_soak_of_this_dev_entry_that_may_still_run_refuses_the_test(self) -> None:
        # A switch would end it: the soak job leaves on any mode but dev (#10).
        now = dt.datetime.now().astimezone()
        path = ip.state_dir() / "soak.json"

        def soak(entry: int, hours: int, ago_h: float) -> dict:
            at = (now - dt.timedelta(hours=ago_h)).isoformat(timespec="seconds")
            return {"sha": SHA, "branch": "dev", "run": 1, "hours": hours, "entry": entry, "at": at}

        path.write_text(json.dumps({"soaks": [soak(ip.current_entry(), 8, 1)]}), encoding="utf-8")
        err = self.refused("a soak dispatched in this dev entry may still run")
        self.assertIn("a switch would end it", err)
        # Its hours plus the margin still count; a record it cannot read counts too (fail safe).
        path.write_text(json.dumps({"soaks": [soak(ip.current_entry(), 1, 1.4)]}), encoding="utf-8")
        self.refused("a soak dispatched in this dev entry may still run")
        path.write_text(json.dumps({"soaks": [dict(soak(ip.current_entry(), 1, 1), at="later")]}), encoding="utf-8")
        self.refused("a soak dispatched in this dev entry may still run")
        # One of another dev entry, or one past its hours and margin, does not.
        path.write_text(json.dumps({"soaks": [soak(ip.current_entry() + 1, 8, 1), soak(ip.current_entry(), 1, 1.6)]}),
                        encoding="utf-8")
        self.pc.calls.clear()
        code, docs, err = self.switch()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.result(docs)["conclusion"], "success")

    def test_the_flag_refuses_the_test_before_any_call(self) -> None:
        self.flag()
        code, docs, err = self.switch()
        self.assertEqual((code, docs, self.pc.calls, ip.current_entry()), (1, [], [], 0))
        self.assertIn("'switch-test' runs only in dev time", err)
        self.assertEqual(self.flag_text(), OWNER_FLAG)

    def test_an_open_spike_window_refuses_it_before_the_pc(self) -> None:
        self.open_window()
        code, docs, err = self.switch()
        self.assertEqual((code, docs, self.pc.calls), (1, [], []))
        self.assertIn("'switch-test' waits until 'iempc handover-s1a'", err)

    def test_not_dev_switching_a_hil_job_or_no_ok_is_refused(self) -> None:
        for doc, words in (
            (ready(mode="event", detail=f"mode event; bundle {SHA}"), "the guard is in mode event, not dev"),
            (ready(mode="live", detail=f"mode live; bundle {SHA}"), "the guard is in mode live, not dev"),
            (ready(switching=SWITCHING), f"a switch runs ({json.dumps(SWITCHING)})"),
            (ready(switching={}), "a switch runs ({})"),
            (ready(detail=f"mode dev; bundle {SHA}; HIL job 42"), "a HIL job runs (HIL job 42)"),
            (ready(ok=False), "the guard's status did not answer ok"),
            (ready(ok="true"), "the guard's status did not answer ok"),
            (ready(ok=...), "the guard's status did not answer ok"),
        ):
            self.status(doc)
            self.refused(f"no switch test: {words} {NOTHING}", doc)
        self.status(READY, code=3)
        self.refused(f"no switch test: iemmode status failed (exit 3) {NOTHING}")

    def test_no_active_bundle_or_another_engine_is_refused(self) -> None:
        for doc, words in (
            (ready(detail="mode dev; no bundle"), "no active bundle (the guard's status names none)"),
            (ready(detail=f"mode dev; no bundle; bundle {SHA}"), "no active bundle"),
            (ready(detail=...), "no active bundle"),
            (ready(engine=...), "no engine runs (the guard's status shows none)"),
            (ready(engine=None), "no engine runs"),
            (ready(engine=[READY["engine"]]), "no engine runs"),
            (engine(build=SHA2), f"the running engine's build is {SHA2!r}, not the active bundle {SHA}"),
            (engine(build=...), f"the running engine's build is None, not the active bundle {SHA}"),
        ):
            self.status(doc)
            self.refused(f"no switch test: {words}", doc)

    def test_a_parked_or_faulted_engine_is_refused(self) -> None:
        """An engine already silent or broken gives no silence of a switch from
        a playing one: the test needs one that plays."""
        for state in ("parked", "faulted"):
            for value in (True, None, "false", 0, ...):
                self.status(engine(**{state: value}))
                shown = None if value is ... else value
                self.refused(f"no switch test: the engine is {state} ({shown!r}); a switch test measures an engine "
                             "that plays", (state, value))

    def test_a_flag_that_appears_during_the_status_read_runs_the_event_path(self) -> None:
        self.pc.replies[("status",)] = lambda: (self.flag(), (0, json.dumps(READY)))[1]
        code, docs, _ = self.switch()
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.calls(), [(["status"], "abandon"), (["event"], "ignore")])
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})
        self.assertEqual(self.flag_text(), OWNER_FLAG)

    def test_it_is_a_locked_dev_time_pc_command(self) -> None:
        self.assertEqual(ip.COMMANDS["switch-test"],
                         ip.Spec(ip.cmd_switch_test, pc=True, dev_time=True, locked=True))
        with open(ip.state_dir() / "iempc.lock", "a+", encoding="utf-8") as held:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            code, _, err = self.switch()
        self.assertEqual((code, self.pc.calls), (1, []))
        self.assertIn("another iempc command runs", err)


class LegTests(SwitchBase):
    def test_the_record_comes_from_the_reply_or_else_from_one_status_read(self) -> None:
        self.pc.replies[("event",)] = answer("event")
        self.pc.replies[("dev",)] = answer("dev", None)
        self.pc.replies[("status",)] = in_turn((0, json.dumps(READY)), answer("event", EVENT), answer("dev", DEV))
        code, docs, err = self.switch()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.calls(), [(["status"], "abandon"), (["event"], "ignore"), (["status"], "ignore"),
                                        (["dev"], "abandon"), (["status"], "abandon")])
        self.assertEqual(self.pc.timeouts, [ip.STATUS_S, ip.SWITCH_S, ip.STATUS_S, ip.SWITCH_S, ip.STATUS_S])
        out = self.result(docs)
        self.assertEqual((out["event_leg"], out["dev_leg"], out["conclusion"]),
                         (leg(0, EVENT, said("event")), leg(0, DEV, said("dev")), "success"))

    def test_a_record_seen_before_a_leg_is_no_record_of_it(self) -> None:
        """A request the guard refused at once answers with the record it
        already had: the one before the event leg, or the event leg's."""
        self.pc.replies[("event",)] = answer("event", BEFORE)
        code, docs, _ = self.switch()
        out = self.result(docs)
        self.assertEqual((code, out["event_leg"], out["dev_leg"]),
                         (1, leg(0, None, said("event")), leg(0, DEV, said("dev"))))
        self.assertEqual(out["first_failure"], "event leg: no record of a switch dev → event (iemmode event exit 0)")
        self.pc.replies[("event",)] = answer("event", EVENT)
        self.pc.replies[("dev",)] = answer("event", EVENT, code=1)
        code, docs, _ = self.switch()
        out = self.result(docs)
        self.assertEqual((code, out["event_leg"], out["dev_leg"]),
                         (1, leg(0, EVENT, said("event")), leg(1, None, said("event"))))
        self.assertEqual(out["first_failure"], "dev leg: no record of a switch from event (iemmode dev exit 1)")

    def test_a_red_event_leg_still_goes_back_to_dev(self) -> None:
        slow = dict(EVENT, silence_ms=60_001)
        self.pc.replies[("event",)] = answer("event", slow)
        code, docs, _ = self.switch()
        self.assertEqual(code, 1)
        self.assertEqual(self.calls()[-1], (["dev"], "abandon"))
        self.assertEqual(self.result(docs), {
            "conclusion": "failure", "first_failure": "event-leg silence 60001 ms > 60000 ms",
            "summary": "red: event-leg silence 60001 ms > 60000 ms; event-leg silence 60001 ms, handover 34000 ms, "
                       "dev-leg silence 14000 ms",
            "numbers": dict(GREEN_NUMBERS, event_silence_ms=60_001), "event_leg": leg(0, slow, said("event")),
            "dev_leg": leg(0, DEV, said("dev")), "dev_entry": 1})
        self.assertFalse(ip.EVENT_NOW.exists())

    def test_an_event_leg_that_did_not_end_in_event_has_no_dev_leg(self) -> None:
        kept = record("dev", "event", EVENT_STEPS[:3] + [st("engine_health", 40)], None, ended_in="dev",
                      outcome="kept_serving", started=1_790_000_100)
        self.pc.replies[("event",)] = answer("dev", kept, code=1)
        code, docs, err = self.switch()
        self.assertEqual(code, 1, err)
        self.assertEqual(self.calls(), [(["status"], "abandon"), (["event"], "ignore")])
        out = self.result(docs)
        self.assertEqual((out["event_leg"], out["dev_leg"], out["conclusion"]),
                         (leg(1, kept, said("dev")), None, "failure"))
        self.assertEqual(out["first_failure"], "event leg: outcome kept_serving, ended in dev")
        self.assertEqual(out["no_dev_leg"], sw.EVENT_FAILED)
        self.assertEqual(sw.EVENT_FAILED, "the event leg did not exit 0: no dev leg; the guard's state decides "
                                          "(check 'iempc status'), never force-end")
        self.assertEqual(ip.current_entry(), 0)
        self.assertFalse(ip.EVENT_NOW.exists())

    def test_an_event_leg_that_needs_the_owner_is_red_and_gets_no_dev_leg(self) -> None:
        """#10 (2026-10-08): REAPER's handover failed, the app still started; the
        guard ends the switch `needs_owner` in event and iemmode exits 1. Red, and
        the guard's state decides (no dev leg)."""
        owner = record("dev", "event", EVENT_STEPS, 25_050, outcome="needs_owner", started=1_790_000_100)
        self.pc.replies[("event",)] = answer("event", owner, code=1)
        code, docs, err = self.switch()
        self.assertEqual(code, 1, err)
        self.assertEqual(self.calls(), [(["status"], "abandon"), (["event"], "ignore")])
        out = self.result(docs)
        self.assertEqual((out["event_leg"], out["dev_leg"], out["conclusion"]),
                         (leg(1, owner, said("event")), None, "failure"))
        self.assertEqual(out["first_failure"], "event leg: outcome needs_owner, ended in event")
        self.assertEqual(out["no_dev_leg"], sw.EVENT_FAILED)
        self.assertEqual(ip.current_entry(), 0)

    def test_an_iemmode_that_prints_no_json_or_crashes_gets_no_dev_leg(self) -> None:
        """A crashed iemmode.exe exits with a negative code in PowerShell, and
        output that is no JSON with a failing exit is no reply (iempc's `call`)."""
        self.pc.replies[("event",)] = (-1073741819, "")
        self.pc.replies[("status",)] = in_turn((0, json.dumps(READY)), (4, "garbage"))
        code, docs, err = self.switch()
        self.assertEqual(code, 1, err)
        self.assertEqual(self.calls(), [(["status"], "abandon"), (["event"], "ignore"), (["status"], "ignore")])
        out = self.result(docs)
        self.assertEqual((out["event_leg"], out["dev_leg"], out["no_dev_leg"]),
                         (leg(-1073741819, None), None, sw.EVENT_FAILED))
        self.assertEqual(out["first_failure"],
                         "event leg: no record of a switch dev → event (iemmode event exit -1073741819)")
        self.assertEqual(ip.current_entry(), 0)

    def test_an_unreachable_guard_on_the_event_leg_gets_no_direct_switch_and_no_flag(self) -> None:
        gone = (4, json.dumps({"ok": False, "detail": "the guard is unreachable: no pipe"}))
        self.pc.replies[("event",)] = gone
        self.pc.replies[("status",)] = in_turn((0, json.dumps(READY)), gone)
        code, docs, _ = self.switch()
        self.assertEqual(code, 1)
        self.assertEqual(self.calls(), [(["status"], "abandon"), (["event"], "ignore"), (["status"], "ignore")])
        out = self.result(docs)
        self.assertEqual((out["event_leg"], out["dev_leg"]), (leg(4, None, "the guard is unreachable: no pipe"), None))
        self.assertEqual(out["first_failure"], "event leg: no record of a switch dev → event (iemmode event exit 4)")
        self.assertFalse(ip.EVENT_NOW.exists())

    def test_a_failed_call_is_printed_red_then_fails_the_command_naming_its_leg(self) -> None:
        """An ssh error or a call left running past its bound: the output keeps
        what was measured (the event leg's numbers when the dev leg's call
        fails), then the command fails with the error."""
        reset = "ssh failed (exit 255): connection reset"

        def broken():
            raise ip.StepError(reset)
        self.pc.replies[("event",)] = broken
        code, docs, err = self.switch()
        self.assertEqual(code, 1)
        self.assertEqual(docs, [{"switch_test": {
            "conclusion": "failure", "first_failure": "event leg: the iemmode event call failed",
            "summary": "red: event leg: the iemmode event call failed; event-leg silence none, handover none, "
                       "dev-leg silence none",
            "numbers": {"event_silence_ms": None, "handover_ms": None, "dev_silence_ms": None},
            "event_leg": leg(None, None, error=reset), "dev_leg": None, "no_dev_leg": sw.EVENT_FAILED}}])
        self.assertIn(f"switch-test: the event leg: {reset} (the guard decides where the PC ends: check 'iempc "
                      "status'; never force-end)", err)
        self.assertEqual(self.calls(), [(["status"], "abandon"), (["event"], "ignore")])
        self.defaults()
        self.pc.replies[("dev",)] = broken
        code, docs, err = self.switch()
        self.assertEqual((code, ip.current_entry()), (1, 0))
        out = self.result(docs)
        self.assertEqual((out["event_leg"], out["dev_leg"], out["first_failure"], out["numbers"]),
                         (leg(0, EVENT, said("event")), leg(None, None, error=reset),
                          "dev leg: the iemmode dev call failed", dict(GREEN_NUMBERS, dev_silence_ms=None)))
        self.assertNotIn("dev_entry", out)
        self.assertIn(f"switch-test: the dev leg: {reset}", err)

    def test_a_switch_whose_record_read_fails_keeps_its_exit_and_goes_on(self) -> None:
        """The event switch exited 0 (the PC is in event) but its status read
        failed: the dev leg still runs, the output is red, then the error."""
        def broken():
            raise ip.StillRunning("ssh still running after 120 s (bounded on the PC; check 'iempc status', never "
                                  "force-end)")
        self.pc.replies[("event",)] = answer("event")
        self.pc.replies[("status",)] = in_turn((0, json.dumps(READY)), broken)
        code, docs, err = self.switch()
        self.assertEqual(code, 1)
        self.assertEqual(self.calls()[-1], (["dev"], "abandon"))
        out = self.result(docs)
        self.assertEqual(out["event_leg"]["exit"], 0)
        self.assertEqual((out["event_leg"]["record"], out["dev_leg"], out["dev_entry"]),
                         (None, leg(0, DEV, said("dev")), 1))
        self.assertIn("ssh still running after 120 s", out["event_leg"]["error"])
        self.assertIn("switch-test: the event leg: ssh still running after 120 s", err)

    def test_an_unwound_dev_leg_is_red_and_opens_no_dev_entry(self) -> None:
        self.pc.replies[("dev",)] = answer("event", UNWOUND, code=1)
        code, docs, _ = self.switch()
        self.assertEqual(code, 1)
        out = self.result(docs)
        self.assertEqual((out["dev_leg"], out["first_failure"]),
                         (leg(1, UNWOUND, said("event")), "dev leg: unwound (its dev entry went back to event)"))
        self.assertNotIn("dev_entry", out)
        self.assertEqual(ip.current_entry(), 0)
        self.assertFalse(ip.EVENT_NOW.exists())


class FlagTests(SwitchBase):
    def test_a_flag_during_the_event_leg_means_no_dev_leg_and_the_event_paths_code(self) -> None:
        self.pc.replies[("event",)] = in_turn(lambda: (self.flag(), answer("event", EVENT))[1], answer("event", EVENT))
        code, docs, err = self.switch()
        self.assertEqual(code, ip.PREEMPTED, err)
        # No `iemmode dev`: the event path's `iemmode event` follows the event leg.
        self.assertEqual(self.calls(), [(["status"], "abandon"), (["event"], "ignore"), (["event"], "ignore")])
        self.assertEqual(docs[0], {"switch_test": {
            "conclusion": "cancelled", "summary": "cancelled: no dev leg; event-leg silence 25050 ms, handover "
                                                  "34000 ms, dev-leg silence none",
            "first_failure": None, "numbers": dict(GREEN_NUMBERS, dev_silence_ms=None),
            "event_leg": leg(0, EVENT, said("event")), "dev_leg": None, "no_dev_leg": sw.FLAG_BEFORE}})
        self.assertEqual(docs[1], {"event": "ide event (flag file)", "action": "iempc event"})
        self.assertIn("no dev leg, the PC stays in event", sw.FLAG_BEFORE)
        self.assertEqual((self.flag_text(), ip.current_entry()), (OWNER_FLAG, 0))

    def test_a_flag_during_the_record_read_means_no_dev_leg_either(self) -> None:
        self.pc.replies[("event",)] = answer("event")
        self.pc.replies[("status",)] = in_turn((0, json.dumps(READY)),
                                               lambda: (self.flag(), answer("event", EVENT))[1])
        code, docs, _ = self.switch()
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.calls(), [(["status"], "abandon"), (["event"], "ignore"), (["status"], "ignore"),
                                        (["event"], "ignore")])
        out = self.result(docs)
        self.assertEqual((out["event_leg"], out["dev_leg"], out["no_dev_leg"]), (leg(0, EVENT, said("event")), None, sw.FLAG_BEFORE))

    def test_the_flag_wins_over_an_event_leg_that_did_not_exit_0(self) -> None:
        """The owner's flag is checked before the failed leg's return: the
        event path runs (it brings the PC to event; a kept-serving leg left it
        in dev), and the output says no dev leg ran."""
        kept = record("dev", "event", EVENT_STEPS[:3] + [st("engine_health", 40)], None, ended_in="dev",
                      outcome="kept_serving", started=1_790_000_100)
        self.pc.replies[("event",)] = in_turn(lambda: (self.flag(), answer("dev", kept, code=1))[1],
                                              answer("event", EVENT))
        code, docs, _ = self.switch()
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.calls(), [(["status"], "abandon"), (["event"], "ignore"), (["event"], "ignore")])
        out = self.result(docs)
        self.assertEqual((out["event_leg"], out["dev_leg"], out["no_dev_leg"], out["first_failure"]),
                         (leg(1, kept, said("dev")), None, sw.FLAG_FAILED, "event leg: outcome kept_serving, ended in dev"))
        self.assertIn("did not exit 0: no dev leg; the event path follows", sw.FLAG_FAILED)
        self.assertNotIn("stays in event", sw.FLAG_FAILED)
        self.assertEqual(docs[1], {"event": "ide event (flag file)", "action": "iempc event"})
        self.assertEqual(self.flag_text(), OWNER_FLAG)

    def test_a_red_event_leg_stays_red_when_the_flag_takes_the_dev_leg(self) -> None:
        slow = dict(EVENT, silence_ms=60_001)
        self.pc.replies[("event",)] = in_turn(lambda: (self.flag(), answer("event", slow))[1], answer("event", slow))
        code, docs, _ = self.switch()
        self.assertEqual(code, ip.PREEMPTED)
        out = self.result(docs)
        self.assertEqual((out["conclusion"], out["first_failure"], out["dev_leg"]),
                         ("failure", "event-leg silence 60001 ms > 60000 ms", None))

    def test_a_flag_during_the_dev_leg_abandons_it_and_the_event_path_runs(self) -> None:
        self.pc.replies[("dev",)] = lambda: (self.flag(), answer("dev", DEV))[1]
        code, docs, _ = self.switch()
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.calls(), [(["status"], "abandon"), (["event"], "ignore"), (["dev"], "abandon"),
                                        (["event"], "ignore")])
        out = self.result(docs)
        self.assertEqual((out["conclusion"], out["event_leg"], out["dev_leg"], out["no_dev_leg"]),
                         ("cancelled", leg(0, EVENT, said("event")), None, sw.FLAG_DURING))
        self.assertIn("the guard unwinds it to event", sw.FLAG_DURING)
        self.assertEqual(docs[-2], {"event": "ide event (flag file)", "action": "iempc event"})
        self.assertEqual((self.flag_text(), ip.current_entry()), (OWNER_FLAG, 0))

    def test_no_switch_test_ever_writes_the_flag(self) -> None:
        """EVENT-NOW is the owner's signal: green, red, failed, unwound or
        refused, the test never writes it (the event path's own write is for
        a flag that is missing, never the case when it runs here)."""
        kept = record("dev", "event", EVENT_STEPS[:3], None, ended_in="dev", outcome="kept_serving")
        scenarios = (
            ("green", {}, 0),
            ("red", {("event",): answer("event", dict(EVENT, silence_ms=60_001))}, 1),
            ("event failed", {("event",): answer("dev", kept, code=1)}, 1),
            ("unreachable", {("event",): (4, json.dumps({"ok": False, "detail": "unreachable"}))}, 1),
            ("unwound", {("dev",): answer("event", UNWOUND, code=1)}, 1),
            ("refused", {("status",): (0, json.dumps(ready(mode="event")))}, 1),
        )
        with self.patched(ensure_flag=mock.Mock(side_effect=AssertionError("the flag was written"))):
            for name, replies, want in scenarios:
                self.defaults()
                self.pc.replies.update(replies)
                code, _, err = self.switch()
                self.assertEqual(code, want, (name, err))
                self.assertFalse(ip.EVENT_NOW.exists(), name)
                self.assertNotIn("--direct", [a for _, args, _ in self.pc.calls for a in args], name)


class VerdictTests(unittest.TestCase):
    def judge(self, event: dict, dev: dict | None) -> str | None:
        return sw.verdict(event, dev)["first_failure"]

    def test_the_bounds_are_the_design_notes(self) -> None:
        self.assertEqual((sw.SILENCE_MAX_MS, sw.HANDOVER_MAX_MS), (60_000, 90_000))

    def test_the_event_legs_silence_bound_on_both_sides(self) -> None:
        for silence, first in ((0, None), (60_000, None), (60_001, "event-leg silence 60001 ms > 60000 ms"),
                               (None, "event-leg silence: none")):
            v = sw.verdict(leg(0, dict(EVENT, silence_ms=silence)), leg(0, DEV))
            self.assertEqual((v["first_failure"], v["conclusion"], v["numbers"]["event_silence_ms"]),
                             (first, "failure" if first else "success", silence), silence)

    def test_the_handover_bound_on_both_sides(self) -> None:
        def with_app_start(ms: int) -> dict:
            return dict(EVENT, steps=[st("app_start", ms) if s["step"] == "app_start" else s for s in EVENT_STEPS])
        # reaper_start 15 000 + reaper_handover 7000 + app_start + app_handover 3000
        for app_start, handover, first in ((65_000, 90_000, None), (65_001, 90_001, "handover 90001 ms > 90000 ms")):
            v = sw.verdict(leg(0, with_app_start(app_start)), leg(0, DEV))
            self.assertEqual((v["first_failure"], v["numbers"]["handover_ms"]), (first, handover))
        no_app = dict(EVENT, steps=[s for s in EVENT_STEPS if s["step"] != "app_handover"])
        self.assertEqual(self.judge(leg(0, no_app), leg(0, DEV)), "handover: none")

    def test_the_event_leg_must_be_a_switch_dev_to_event_that_ended_there(self) -> None:
        none = "event leg: no record of a switch dev → event (iemmode event exit 0)"
        for rec in (None, dict(EVENT, **{"from": "event"}), dict(EVENT, to="dev"), dict(EVENT, unwound="dev")):
            self.assertEqual(self.judge(leg(0, rec), leg(0, DEV)), none, rec)
        self.assertEqual(self.judge(leg(1, None), leg(0, DEV)),
                         "event leg: no record of a switch dev → event (iemmode event exit 1)")
        self.assertEqual(self.judge(leg(0, dict(EVENT, ended_in="dev")), leg(0, DEV)),
                         "event leg: outcome done, ended in dev")
        for outcome in ("kept_serving", "needs_owner", "unknown"):
            self.assertEqual(self.judge(leg(0, dict(EVENT, outcome=outcome)), leg(0, DEV)),
                             f"event leg: outcome {outcome}, ended in event")
        self.assertEqual(self.judge(leg(1, EVENT), leg(0, DEV)), "event leg: iemmode event exit 1")

    def test_the_dev_leg_must_reach_dev_unwound_none_outcome_done(self) -> None:
        for dev, first in (
            (leg(0, None), "dev leg: no record of a switch from event (iemmode dev exit 0)"),
            (leg(0, dict(DEV, **{"from": "dev"})), "dev leg: no record of a switch from event (iemmode dev exit 0)"),
            (leg(1, UNWOUND), "dev leg: unwound (its dev entry went back to event)"),
            (leg(0, dict(DEV, unwound="dev")), "dev leg: unwound (its dev entry went back to event)"),
            (leg(1, dict(DEV, to="event", ended_in="event")), "dev leg: to event, not dev"),
            (leg(1, dict(DEV, outcome="kept_serving")), "dev leg: outcome kept_serving"),
            (leg(1, dict(DEV, outcome="unknown")), "dev leg: outcome unknown"),
            (leg(1, DEV), "dev leg: iemmode dev exit 1"),
            (leg(0, DEV), None),
        ):
            v = sw.verdict(leg(0, EVENT), dev)
            self.assertEqual((v["first_failure"], v["conclusion"]), (first, "failure" if first else "success"), dev)

    def test_red_names_the_first_failure_in_order(self) -> None:
        """The event leg's checks (its record, its end, its silence, the
        handover, its exit code), then the dev leg's (its record, unwound,
        its target, its outcome, its exit code)."""
        no_app = [s for s in EVENT_STEPS if s["step"] != "app_handover"]
        steps = (
            (leg(1, dict(EVENT, outcome="needs_owner", silence_ms=None, steps=no_app)),
             leg(1, dict(UNWOUND, outcome="needs_owner")), "event leg: outcome needs_owner, ended in event"),
            (leg(1, dict(EVENT, silence_ms=None, steps=no_app)), leg(1, UNWOUND), "event-leg silence: none"),
            (leg(1, dict(EVENT, steps=no_app)), leg(1, UNWOUND), "handover: none"),
            (leg(1, EVENT), leg(1, UNWOUND), "event leg: iemmode event exit 1"),
            (leg(0, EVENT), leg(1, dict(UNWOUND, outcome="needs_owner")),
             "dev leg: unwound (its dev entry went back to event)"),
            (leg(0, EVENT), leg(1, dict(DEV, to="event", outcome="needs_owner")), "dev leg: to event, not dev"),
            (leg(0, EVENT), leg(1, dict(DEV, outcome="needs_owner")), "dev leg: outcome needs_owner"),
            (leg(0, EVENT), leg(1, DEV), "dev leg: iemmode dev exit 1"),
        )
        for event, dev, first in steps:
            v = sw.verdict(event, dev)
            self.assertEqual(v["first_failure"], first)
            self.assertTrue(v["summary"].startswith(f"red: {first}; event-leg silence "), v["summary"])

    def test_no_dev_leg_is_cancelled_unless_the_event_leg_is_red(self) -> None:
        v = sw.verdict(leg(0, EVENT), None)
        self.assertEqual((v["conclusion"], v["first_failure"]), ("cancelled", None))
        self.assertEqual(v["numbers"], dict(GREEN_NUMBERS, dev_silence_ms=None))
        v = sw.verdict(leg(1, EVENT), None)
        self.assertEqual((v["conclusion"], v["first_failure"]), ("failure", "event leg: iemmode event exit 1"))

    def test_a_leg_without_a_record_shows_none(self) -> None:
        v = sw.verdict(leg(4, None), None)
        self.assertEqual(v["numbers"], {"event_silence_ms": None, "handover_ms": None, "dev_silence_ms": None})
        self.assertEqual(v["summary"], "red: event leg: no record of a switch dev → event (iemmode event exit 4); "
                                       "event-leg silence none, handover none, dev-leg silence none")

    def test_no_numbers_from_a_record_that_is_not_the_legs_own_switch(self) -> None:
        none = {"event_silence_ms": None, "handover_ms": None}
        for rec in (dict(EVENT, **{"from": "event"}), dict(EVENT, to="dev"), dict(EVENT, unwound="dev")):
            self.assertEqual(sw.verdict(leg(0, rec), leg(0, DEV))["numbers"], dict(GREEN_NUMBERS, **none), rec)
        # The dev leg's numbers come from a switch from event: its entry, or that entry's unwind.
        self.assertIsNone(sw.verdict(leg(0, EVENT), leg(0, dict(DEV, **{"from": "dev"})))["numbers"]["dev_silence_ms"])
        self.assertEqual(sw.verdict(leg(0, EVENT), leg(1, UNWOUND))["numbers"]["dev_silence_ms"], 52_250)

    def test_a_failed_call_is_named_before_anything_else_of_its_leg(self) -> None:
        self.assertEqual(sw.verdict(leg(None, None), None)["first_failure"], "event leg: the iemmode event call failed")
        self.assertEqual(sw.verdict(leg(None, EVENT), None)["first_failure"], "event leg: the iemmode event call failed")
        self.assertEqual(sw.verdict(leg(0, EVENT), leg(None, DEV))["first_failure"],
                         "dev leg: the iemmode dev call failed")


class TraceRecordTests(SwitchBase):
    """A trace a dead `iempc trace` left recorded (#15) is stopped first, as by
    `iempc dev` and `iempc event`; one that may still run refuses the test."""

    OLD = "X:\\root\\traces\\old-20261007T060000Z"

    def setUp(self) -> None:
        super().setUp()
        ip.state_dir()
        self.record = ip.STATE_DIR / "trace.json"
        self.record.write_text(json.dumps({"dir": self.OLD, "run": "old-20261007T060000Z", "label": "old",
                                           "started": "2026-10-07T08:00:00+02:00", "answered": True}),
                               encoding="utf-8")
        # Only the recorded stop's module script is answered (no profile check here).
        self.pc.texts = {"Stop-IemTraceSessions": {"stopped": ["NT Kernel Logger", "IemMarkers"], "gone": [],
                                                   "kept": [], "notes": []}}

    def test_a_recorded_trace_is_stopped_before_the_event_leg(self) -> None:
        code, docs, err = self.switch()
        self.assertEqual(code, 0, err)
        self.assertFalse(self.record.exists())
        [(script, mode)] = self.pc.modules
        self.assertIn(f"Stop-IemTraceSessions -Dir '{self.OLD}'", script)
        self.assertEqual(mode, "ignore")
        self.assertEqual(self.calls(), [(["status"], "abandon"), (["event"], "ignore"), (["dev"], "abandon")])
        self.assertEqual([next(iter(d)) for d in docs], ["recorded_trace", "switch_test"])

    def test_a_recorded_trace_that_may_still_run_refuses_the_test(self) -> None:
        self.pc.texts["Stop-IemTraceSessions"] = {"stopped": [], "gone": [], "kept": ["NT Kernel Logger"], "notes": []}
        code, docs, err = self.switch()
        self.assertEqual(code, 1)
        self.assertEqual(self.calls(), [(["status"], "abandon")])
        self.assertIn(f"no switch test: a trace recorded in {self.record} may still run on the PC (above) "
                      "(nothing was switched)", err)
        self.assertTrue(self.record.exists())
        self.assertEqual([d.get("recorded_trace") for d in docs], ["failed"])

    def test_a_flag_during_the_recorded_stop_runs_the_event_path_and_switches_nothing(self) -> None:
        stopped = self.pc.texts["Stop-IemTraceSessions"]
        self.pc.texts["Stop-IemTraceSessions"] = lambda: (self.flag(), stopped)[1]
        code, docs, _ = self.switch()
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.calls(), [(["status"], "abandon"), (["event"], "ignore")])
        self.assertFalse(any("switch_test" in d for d in docs))
        self.assertEqual(self.flag_text(), OWNER_FLAG)


class HandoverTests(unittest.TestCase):
    def test_the_handover_runs_from_reapers_start_through_the_apps_handover(self) -> None:
        self.assertEqual(sw.handover_ms(EVENT_STEPS), 34_000)   # both ends included, nothing around them
        self.assertEqual(sw.handover_ms([st("reaper_start", 5), st("app_handover", 7)]), 12)

    def test_without_reapers_start_it_runs_from_reapers_handover(self) -> None:
        """A REAPER that already runs gets no start (the plan's facts)."""
        no_start = [s for s in EVENT_STEPS if s["step"] != "reaper_start"]
        self.assertEqual(sw.handover_ms(no_start), 19_000)
        self.assertEqual(sw.handover_ms([st("reaper_handover", 7), st("reaper_start", 5), st("app_handover", 3)]), 15)

    def test_it_ends_at_the_first_app_handover_after_its_start(self) -> None:
        self.assertEqual(sw.handover_ms([st("app_handover", 1000), st("reaper_start", 5), st("app_start", 7),
                                         st("app_handover", 11), st("fingerprint", 100), st("app_handover", 13)]), 23)

    def test_none_when_either_end_is_missing(self) -> None:
        for steps in ([], [s for s in EVENT_STEPS if s["step"] != "app_handover"],
                      [s for s in EVENT_STEPS if s["step"] not in ("reaper_start", "reaper_handover")],
                      [st("app_handover", 3), st("reaper_start", 5)]):
            self.assertIsNone(sw.handover_ms(steps), steps)


class RecordTests(unittest.TestCase):
    def test_a_record_reads_as_the_guard_writes_it(self) -> None:
        # A step of 0 ms: the guard's `Laps::lap` without a start.
        for rec in (EVENT, DEV, UNWOUND, dict(EVENT, silence_ms=None), dict(EVENT, steps=[]),
                    dict(EVENT, silence_ms=0, steps=[st("engine_stop", 0), st("reaper_handover", 0)])):
            self.assertEqual(sw.read_record(copy.deepcopy(rec)), rec)

    def test_an_older_guards_record_without_unwound_reads_it_as_none(self) -> None:
        old = {k: v for k, v in EVENT.items() if k != "unwound"}
        self.assertEqual(sw.read_record(old), EVENT)

    def test_only_the_judged_fields_are_kept(self) -> None:
        rec = dict(EVENT, note="a newer guard's field", steps=[dict(s, extra=1) for s in EVENT_STEPS])
        self.assertEqual(sw.read_record(rec), EVENT)

    def test_a_record_of_another_shape_is_no_record(self) -> None:
        """As the guard's own `switch_log::lenient`: what cannot be read is none."""
        bad = [None, [], "record", 42]
        for key in ("from", "to", "ended_in", "outcome"):
            bad += [dict(EVENT, **{key: None}), dict(EVENT, **{key: 1}), {k: v for k, v in EVENT.items() if k != key}]
        bad += [dict(EVENT, steps=None), dict(EVENT, steps={"step": "app_handover", "ms": 1}),
                {k: v for k, v in EVENT.items() if k != "steps"}]
        for step in (None, "app_handover", ["app_handover", 1], {"ms": 1}, {"step": 7, "ms": 1},
                     {"step": "app_handover"}, {"step": "app_handover", "ms": -1}, {"step": "app_handover", "ms": 1.0},
                     {"step": "app_handover", "ms": True}, {"step": "app_handover", "ms": "1"}):
            bad.append(dict(EVENT, steps=EVENT_STEPS + [step]))
        for silence in (-1, 1.5, True, "25050"):
            bad.append(dict(EVENT, silence_ms=silence))
        for unwound in (1, True, ["dev"]):
            bad.append(dict(EVENT, unwound=unwound))
        for rec in bad:
            self.assertIsNone(sw.read_record(rec), rec)


class RefusalTests(unittest.TestCase):
    def test_switch_refusal_is_pure_and_judges_in_order(self) -> None:
        before = copy.deepcopy(READY)
        self.assertIsNone(sw.switch_refusal(READY))
        self.assertEqual(sw.switch_refusal(None), "iemmode status gave no reply")
        self.assertEqual(sw.switch_refusal([READY]), "iemmode status gave no reply")
        # The order: the guard settled in dev (ok, mode, switch, HIL job), the bundle, the engine.
        self.assertEqual(sw.switch_refusal(ready(mode="event", switching=SWITCHING, detail="mode event; no bundle",
                                                 engine=None)), "the guard is in mode event, not dev")
        self.assertEqual(sw.switch_refusal(ready(switching=SWITCHING, detail="mode dev; no bundle; HIL job 7",
                                                 engine=None)), f"a switch runs ({json.dumps(SWITCHING)})")
        self.assertEqual(sw.switch_refusal(ready(detail="mode dev; no bundle; HIL job 7", engine=None)),
                         "a HIL job runs (HIL job 7)")
        self.assertEqual(sw.switch_refusal(ready(detail="mode dev; no bundle", engine=None)),
                         "no active bundle (the guard's status names none)")
        self.assertEqual(sw.switch_refusal(ready(engine=None)), "no engine runs (the guard's status shows none)")
        self.assertEqual(READY, before)


if __name__ == "__main__":
    unittest.main()
