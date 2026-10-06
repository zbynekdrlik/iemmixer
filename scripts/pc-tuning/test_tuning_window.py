"""Tests for scripts/pc-tuning/tuning_window.py (pure parts; ssh is the PC)."""
from __future__ import annotations

import argparse
import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import tuning_window as tw  # noqa: E402

PROFILE = {"version": 1, "journal": "C:\\j.json", "registry_root": "",
           "layout": {"housekeeping": [0, 1, 6, 7, 8, 9, 10, 11, 12, 13], "card": [2], "nic": [4, 5], "audio": [14]},
           "plan": {"guid": "6c1b0d6e-0a39-4f55-9c2a-1e5a3b7d9c01", "source": "00000000-0000-0000-0000-000000000001"},
           "governor": "gov", "placement": [], "services_disable": [], "services_mode": [],
           "updates": {"services": [], "tasks": []}, "maintenance": {"off": True, "tasks": []},
           "defender": {"paths": [], "processes": []}, "devices": [], "nic": {"adapter": "a", "properties": {}, "rss": {"base": 4, "max": 5}, "pnp_capabilities": 24},
           "fingerprint": {"files": [], "keys": []}}


# The profile rules' shared table (Test-IemTuning.ps1 runs it too, #32 MINOR-6).
CASES = json.loads((Path(__file__).resolve().parent / "profile_cases.json").read_text(encoding="utf-8"))


def write(obj) -> Path:
    p = Path(tempfile.mkdtemp()) / "pc-tuning.json"
    p.write_text(json.dumps(obj), encoding="utf-8")
    return p


class ProfileTests(unittest.TestCase):
    def test_a_complete_profile_loads(self) -> None:
        self.assertEqual(tw.load_profile(write(PROFILE))["layout"]["audio"], [14])

    def test_missing_keys_and_overlapping_layout_are_refused(self) -> None:
        bad = dict(PROFILE); del bad["governor"]
        with self.assertRaisesRegex(tw.StepError, "missing governor"):
            tw.load_profile(write(bad))
        overlap = json.loads(json.dumps(PROFILE)); overlap["layout"]["audio"] = [2]
        with self.assertRaisesRegex(tw.StepError, "processor 2 has two roles"):
            tw.load_profile(write(overlap))
        wide = json.loads(json.dumps(PROFILE)); wide["layout"]["card"] = [64]
        with self.assertRaisesRegex(tw.StepError, "0..63"):
            tw.load_profile(write(wide))

    def test_watch_lps_are_the_card_and_the_audio_cpus(self) -> None:
        self.assertEqual(tw.watch_lps(PROFILE, ""), [2, 14])
        self.assertEqual(tw.watch_lps(PROFILE, "3"), [2, 3])

    def test_the_shared_layout_cases(self) -> None:
        # One layout rule on both sides (#32 MINOR-6): Test-IemTuning.ps1 runs the
        # same table against Assert-IemLayout, so a profile passes both or neither.
        cases = CASES["layouts"]
        self.assertGreaterEqual(len(cases), 10)
        for case in cases:
            with self.subTest(case["name"]):
                p = json.loads(json.dumps(PROFILE))
                p["layout"] = case["layout"]
                if case["ok"]:
                    tw.load_profile(write(p))
                else:
                    with self.assertRaisesRegex(tw.StepError, "layout"):
                        tw.load_profile(write(p))

    def test_the_shared_number_cases(self) -> None:
        # One processor-number rule (#32 MAJOR-2 review): Test-IemTuning.ps1 runs the
        # same table against ConvertTo-IemLpNumber.
        self.assertGreaterEqual(len(CASES["numbers"]), 8)
        for case in CASES["numbers"]:
            with self.subTest(case["name"]):
                self.assertEqual(tw.lp_number(case["value"]), case["ok"])

    def test_the_shared_device_cases(self) -> None:
        # The dev box refuses the device processors the PC refuses (#32 MAJOR-2
        # review): a profile tuning-setup copies to the PC can never make the state
        # step throw there. Test-IemTuning.ps1 runs the table against Get-IemDeviceLps.
        self.assertGreaterEqual(len(CASES["device_lps"]), 8)
        for case in CASES["device_lps"]:
            with self.subTest(case["name"]):
                p = json.loads(json.dumps(PROFILE))
                p["devices"] = [{"id": "card", "role": "card", "lps": case["lps"], "enabled": True}]
                if case["ok"]:
                    tw.load_profile(write(p))
                else:
                    with self.assertRaisesRegex(tw.StepError, "device card"):
                        tw.load_profile(write(p))

    def test_devices_that_are_not_a_list_of_objects_are_refused(self) -> None:
        # Python-only structure checks of check_devices: a clear StepError, never an
        # AttributeError or TypeError from the loop.
        for devices, text in (({"id": "card"}, "devices: not a list"), ([2], "an entry is not an object")):
            with self.subTest(devices=devices):
                p = json.loads(json.dumps(PROFILE))
                p["devices"] = devices
                with self.assertRaisesRegex(tw.StepError, text):
                    tw.load_profile(write(p))

    def test_the_rss_bounds_are_processor_numbers(self) -> None:
        # nic.rss base and max follow the number rule, as Assert-IemNicRss does (#32
        # MAJOR-2 review); a missing range is refused too (the PC refuses the NIC write).
        for value in ("4", 4.0, None, True, 64, -1):
            for key in ("base", "max"):
                with self.subTest(key=key, value=value):
                    p = json.loads(json.dumps(PROFILE))
                    p["nic"]["rss"][key] = value
                    with self.assertRaisesRegex(tw.StepError, f"nic.rss.{key}"):
                        tw.load_profile(write(p))
        p = json.loads(json.dumps(PROFILE))
        del p["nic"]["rss"]
        with self.assertRaisesRegex(tw.StepError, "nic.rss"):
            tw.load_profile(write(p))
        p = json.loads(json.dumps(PROFILE))
        p["nic"]["rss"] = {"base": 5, "max": 4}
        with self.assertRaisesRegex(tw.StepError, "above max"):
            tw.load_profile(write(p))

    def test_an_absent_role_has_no_processors(self) -> None:
        # The rule's "absent role = none" holds for the readers too (#32 MINOR-6).
        p = json.loads(json.dumps(PROFILE))
        del p["layout"]["audio"]
        self.assertEqual(tw.watch_lps(tw.load_profile(write(p)), ""), [2])


class ArgumentTests(unittest.TestCase):
    def test_lists_levers_labels(self) -> None:
        self.assertEqual(tw.parse_lps("0,1,6-8"), [0, 1, 6, 7, 8])
        self.assertEqual(tw.parse_lps(""), [])
        for bad in ("5-3", "64", "1,1", "x"):
            with self.assertRaises(tw.StepError, msg=bad):
                tw.parse_lps(bad)
        self.assertEqual(tw.mode_only("plan,governor"), ["plan", "governor"])
        with self.assertRaises(tw.StepError):
            tw.mode_only("plan,reboot")
        self.assertTrue(tw.label_ok("tier1-c1-load"))
        for bad in ("", "a b", "x/../y", "a" * 41):
            self.assertFalse(tw.label_ok(bad), bad)

    def test_the_reboot_approval_quotes_the_owner(self) -> None:
        tw.check_approval("owner, 14:05: áno, reštartuj")
        for bad in ("", "yes", "reštartuj"):
            with self.assertRaises(tw.StepError, msg=bad):
                tw.check_approval(bad)


class CutTests(unittest.TestCase):
    def test_a_new_glitch_cuts_a_circular_trace_at_most_five_times(self) -> None:
        p = {"missed": 1, "overruns": 0, "position_gaps": 0}
        self.assertEqual(tw.should_cut(p, seen=0, cuts=0, circular=True), (True, 1))
        self.assertEqual(tw.should_cut(p, seen=1, cuts=1, circular=True), (False, 1))
        self.assertEqual(tw.should_cut({"missed": 2, "overruns": 1}, seen=1, cuts=5, circular=True), (False, 3))
        self.assertEqual(tw.should_cut(p, seen=0, cuts=0, circular=False), (False, 1))
        self.assertEqual(tw.should_cut(None, seen=4, cuts=0, circular=True), (False, 4))


class PostBootTests(unittest.TestCase):
    def test_every_check_must_hold(self) -> None:
        ok = {"booted_after_request": True, "reaper": True, "handover": {"asio": "reaper"}, "fingerprint": [], "pending": [], "failed_items": []}
        self.assertEqual(tw.post_boot_verdict(ok), [])
        self.assertEqual(tw.post_boot_verdict({**ok, "fingerprint": [{"key": "plan.active"}]}), ["REAPER mode differs: plan.active"])
        self.assertEqual(tw.post_boot_verdict({**ok, "pending": ["irq:card:mask"]}), ["still pending after the reboot: irq:card:mask"])
        self.assertEqual(tw.post_boot_verdict({**ok, "booted_after_request": False}), ["the PC did not reboot after the request"])
        self.assertEqual(tw.post_boot_verdict({**ok, "handover": {"error": "no meters"}}), ["handover checks failed: no meters"])

    def test_a_reboot_that_cannot_be_told_is_named(self) -> None:
        # F2 round 3, decision 5: the boot token tells whether the PC rebooted; without
        # one on either side the verdict says why, never "did not reboot".
        ok = {"booted_after_request": True, "reaper": True, "handover": {"asio": "reaper"}, "fingerprint": [], "pending": [], "failed_items": []}
        self.assertEqual(tw.post_boot_verdict({**ok, "booted_after_request": False, "boot_unknown": "reboot-prepare recorded no boot token"}),
                         ["whether the PC rebooted cannot be told (reboot-prepare recorded no boot token)"])
        self.assertEqual(tw.boot_changed("tok-a", "tok-b"), (True, None))
        self.assertEqual(tw.boot_changed("tok-a", "tok-a"), (False, None))
        self.assertEqual(tw.boot_changed(None, "tok-b"), (False, "reboot-prepare recorded no boot token"))
        self.assertEqual(tw.boot_changed("tok-a", None), (False, "the boot token cannot be read now"))

    def test_an_unknown_boot_identity_is_a_problem(self) -> None:
        # #32 MINOR-4: without a boot token nothing reads as pending, so an empty
        # "pending" proves nothing; the state's boot_problem fails the verdict.
        ok = {"booted_after_request": True, "reaper": True, "handover": {"asio": "reaper"}, "fingerprint": [], "pending": [], "failed_items": []}
        self.assertEqual(tw.post_boot_verdict({**ok, "boot_problem": "boot key: not volatile (synthetic)"}),
                         ["the boot identity is unknown (boot key: not volatile (synthetic)): what is still pending cannot be told"])
        self.assertEqual(tw.post_boot_verdict({**ok, "boot_problem": None}), [])


class UndoTests(unittest.TestCase):
    """cmd_undo must fail loud when a Tier-3 revert item fails (I2,
    script-failure-policy): a silent exit 0 hides an un-reverted global lever
    (post_boot_verdict's failed_items can't see it — a failed revert stays
    journaled but still matches its tuned value, so it counts ok). Only sw.ps
    (the ssh boundary) is faked; the real open_state/save_state run on a temp
    STATE (#32 B14)."""

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.saved = (tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps)
        tw.sw.STATE = self.dir / "spike-window.json"
        tw.sw.EVENT_NOW = self.dir / "EVENT-NOW"
        tw.sw.save_state({"id": "w", "card": "free", "closed": False})
        self.rows: list[dict] = []
        self.bodies: list[str] = []

        def fake_ps(env, body, timeout=300, event="finish"):
            self.bodies.append(body)
            return self.rows

        tw.sw.ps = fake_ps
        self.env = {"PC_ROOT": "R", "PC_TUNING_ROOT": "T", "PC_XPERF": "xperf.exe"}
        self.args = argparse.Namespace(tier=3, only="")

    def tearDown(self) -> None:
        tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps = self.saved

    def test_a_clean_revert_succeeds(self) -> None:
        self.rows = [{"key": "irq:card:policy", "action": "restored", "error": None}]
        tw.cmd_undo(self.env, self.args)   # no raise on a clean revert
        self.assertIn("Undo-IemTuning -ProfilePath 'T\\profile.json' -Tier 3", self.bodies[0])
        self.assertEqual([s["undo"] for s in tw.sw.load_state()["tuning_steps"]], [3])

    def test_a_failed_revert_row_raises(self) -> None:
        self.rows = [{"key": "irq:card:policy", "action": "failed", "error": "Access is denied"},
                     {"key": "irq:nic:rss", "action": "restored", "error": None}]
        with self.assertRaisesRegex(tw.StepError, "revert item.*irq:card:policy.*Access is denied"):
            tw.cmd_undo(self.env, self.args)
        self.assertEqual(len(tw.sw.load_state()["tuning_steps"]), 1)   # recorded before the fail-loud raise

    def test_a_problem_row_alarms_the_owner_and_the_revert_stands(self) -> None:
        # #32 MINOR-4: an unreadable boot key no longer blocks the revert; the
        # module records the revert's boot as unknown and says so in a row.
        alarms: list[str] = []
        with mock.patch.object(tw.sw, "alarm", alarms.append):
            self.rows = [{"key": "irq:card:policy", "action": "restored", "error": None},
                         {"key": "boot", "action": "problem", "error": "the revert's boot is unknown (synthetic)"}]
            tw.cmd_undo(self.env, self.args)   # no raise: nothing failed
        self.assertEqual(len(alarms), 1)
        self.assertIn("boot: the revert's boot is unknown (synthetic)", alarms[0])

    def test_an_event_or_a_held_card_refuses_before_the_pc(self) -> None:
        tw.sw.save_state({"id": "w", "card": "reaper", "closed": False})
        with self.assertRaisesRegex(tw.StepError, "not free"):
            tw.cmd_undo(self.env, self.args)
        (self.dir / "EVENT-NOW").touch()
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_undo(self.env, self.args)
        self.assertEqual(self.bodies, [])


class ExitTests(unittest.TestCase):
    """cmd_exit (lane F4's round 3): Exit-IemTuningMode reports a schema-1 journal
    entry it could not convert as a row with action "problem"; the exit itself
    completes, and the owner hears which entries. Only sw.ps is faked."""

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.saved = (tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps, tw.sw.alarm)
        tw.sw.STATE, tw.sw.EVENT_NOW = self.dir / "spike-window.json", self.dir / "EVENT-NOW"
        tw.sw.save_state({"id": "w", "card": "free", "tuning_mode": True, "closed": False})
        self.alarms: list[str] = []
        tw.sw.alarm = self.alarms.append
        self.rows: list[dict] = []
        tw.sw.ps = lambda env, body, timeout=300, event="finish": self.rows
        self.env = {"PC_ROOT": "R", "PC_TUNING_ROOT": "T", "PC_XPERF": "xperf.exe"}

    def tearDown(self) -> None:
        tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps, tw.sw.alarm = self.saved

    def test_problem_rows_alarm_the_owner_and_the_exit_completes(self) -> None:
        self.rows = [{"key": "plan:active", "action": "restored", "error": None},
                     {"key": "global:nic-rss", "action": "problem", "error": "schema-1 entry not convertible (synthetic)"}]
        tw.cmd_exit(self.env, argparse.Namespace())
        self.assertFalse(tw.sw.load_state()["tuning_mode"])
        self.assertEqual(len(self.alarms), 1)
        self.assertIn("global:nic-rss", self.alarms[0])
        self.assertNotIn("plan:active", self.alarms[0])

    def test_a_clean_exit_raises_no_alarm(self) -> None:
        self.rows = [{"key": "plan:active", "action": "restored", "error": None}]
        tw.cmd_exit(self.env, argparse.Namespace())
        self.assertEqual(self.alarms, [])


class RebootPrepareTests(unittest.TestCase):
    """reboot-prepare prepares a reboot only over a cleanly preempted window
    (I1), exercised through the REAL sw.unwind: a spike that did not stop still
    holds the card, a kernel trace left running or a mode lever not reverted
    would carry into the reboot (#32 B4). A reboot is never prepared over any
    of them — reboot-prepare refuses (owner alarm + StepError), keeps the card
    free and the window open; the spike is never force-ended (I8). Only sw.ps
    (the ssh boundary) and sw.alarm (the owner-alarm printer, to count alarms)
    are faked; sw.STATE/sw.EVENT_NOW point at a temp dir."""

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.saved = (tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps, tw.sw.alarm)
        tw.sw.STATE = self.dir / "spike-window.json"
        tw.sw.EVENT_NOW = self.dir / "EVENT-NOW"   # absent → no "ide event"
        self.gone = True
        self.fail: set[str] = set()   # PowerShell verbs whose call fails on the PC
        self.alarms: list[str] = []
        tw.sw.alarm = lambda text: self.alarms.append(text)
        self.tuning_state: dict = {"items": [], "boot_token": "tok-prepare"}

        def fake_ps(env, body, timeout=300, event="finish"):
            for verb in self.fail:
                if verb in body:
                    raise tw.StepError(f"PC step failed: {verb}")
            if "Stop-SpikeGracefully" in body:
                return self.gone
            if "Get-IemTuningState" in body:
                return self.tuning_state
            if "Get-IemNow" in body:
                return "2026-01-01T00:00:00Z"
            return {"ok": True}

        self.fake_ps = fake_ps
        tw.sw.ps = fake_ps   # tw.tps wraps sw.ps via sw.tuning_body — kept real
        self.env = {"PC_ROOT": "R", "PC_TUNING_ROOT": "T", "PC_XPERF": "xperf.exe",
                    "PC_BUFFER_KEY": "K", "PC_BUFFER_NAME": "N", "PC_REAPER_HTTP": "H",
                    "PC_REAPER_START_TASK_PATH": "P", "PC_REAPER_START_TASK": "TK", "PC_NTRACK": "9",
                    "PC_METER_BRIDGE": "B", "PC_METER_ACTION": "A", "PC_METER_HEARTBEAT": "HB",
                    "PC_ASIO_MODULE": "M", "PC_APP_HTTP": "AH"}
        self.args = argparse.Namespace()

    def tearDown(self) -> None:
        tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps, tw.sw.alarm = self.saved

    def write_state(self, **kw) -> None:
        tw.sw.save_state({"id": "w", "card": "free", "pref_original": 64, "pref_current": 64,
                          "pref_restored": False, "closed": False, **kw})

    def read_state(self) -> dict:
        return json.loads((self.dir / "spike-window.json").read_text(encoding="utf-8"))

    def test_a_clean_stop_prepares_the_reboot(self) -> None:
        self.gone = True
        self.write_state()
        tw.cmd_reboot_prepare(self.env, self.args)
        st = self.read_state()
        self.assertEqual(st["card"], "rebooting")
        self.assertIn("reboot", st)
        self.assertEqual(self.alarms, [])           # a clean stop raises no alarm

    def test_the_boot_token_is_recorded_for_post_boot(self) -> None:
        # F2 round 3, decision 5: post-boot tells a reboot by the boot token.
        self.write_state()
        tw.cmd_reboot_prepare(self.env, self.args)
        self.assertEqual(self.read_state()["reboot"]["boot_token"], "tok-prepare")

    # F2 round 3, m3: the long read-only calls (the tuning state compiles IemTuning)
    # run outside the window lock, so a preempt never waits for them; the write
    # takes the lock again and decides on the state as saved then.
    def wrap_reads(self, during) -> list[str]:
        events: list[str] = []
        real = tw.sw.ps

        def ps(env, body, timeout=300, event="finish"):
            if "Get-IemTuningState" in body or "Get-IemNow" in body:
                events.append(event)
                during()
            return real(env, body, timeout, event)

        tw.sw.ps = ps
        return events

    def lock_is_free(self) -> bool:
        got: list[bool] = []

        def probe() -> None:   # another process's preempt, in short
            try:
                with tw.sw.window_lock():
                    got.append(True)
            except tw.StepError:
                got.append(False)

        saved = tw.sw.LOCK_WAIT_S
        tw.sw.LOCK_WAIT_S = 0.3
        try:
            t = threading.Thread(target=probe)
            t.start()
            t.join()
        finally:
            tw.sw.LOCK_WAIT_S = saved
        return got[0]

    def test_the_read_only_calls_run_outside_the_window_lock(self) -> None:
        self.write_state()
        free: list[bool] = []
        events = self.wrap_reads(lambda: free.append(self.lock_is_free()))
        tw.cmd_reboot_prepare(self.env, self.args)
        self.assertEqual(free, [True, True])
        self.assertEqual(events, ["abandon", "abandon"])   # read-only: "ide event" does not wait for them
        self.assertEqual(self.read_state()["card"], "rebooting")

    def test_a_pc_change_in_flight_refuses_the_reboot(self) -> None:
        # F2 round 3, MAJOR: a step of another process still changing the PC is no
        # cleanly preempted window (I1).
        self.write_state(in_flight={"step": "apply", "started": time.time(), "bound_s": 600})
        with self.assertRaisesRegex(tw.StepError, "in flight"):
            tw.cmd_reboot_prepare(self.env, self.args)
        st = self.read_state()
        self.assertEqual(st["card"], "free")
        self.assertNotIn("reboot", st)

    def test_a_step_begun_during_the_reads_refuses_the_reboot(self) -> None:
        # Review of lane G2, finding 3: between the unwind and the write the lock is
        # free; a set-buffer or an enter begun there leaves a buffer not verified as
        # restored, a mode recorded as entered, or a change in flight. A reboot is not
        # prepared over any of them (I1; after the reboot a buffer write could meet a
        # REAPER that holds the driver, I2).
        for change in ({"pref_current": 32, "pref_restored": False},
                       {"tuning_mode": True},
                       {"in_flight": {"step": "set-buffer", "started": time.time(), "bound_s": 60}}):
            with self.subTest(change=change):
                self.write_state()
                self.wrap_reads(lambda: tw.sw.update_state(change))
                with self.assertRaisesRegex(tw.StepError, "no reboot prepared"):
                    tw.cmd_reboot_prepare(self.env, self.args)
                st = self.read_state()
                self.assertEqual(st["card"], "free")
                self.assertNotIn("reboot", st)
                tw.sw.ps = self.fake_ps

    def test_a_window_a_preempt_closed_during_the_reads_is_not_prepared(self) -> None:
        self.write_state()

        def preempt() -> None:
            st = tw.sw.load_state()
            st.update(card="reaper", closed=True)
            tw.sw.save_state(st)

        self.wrap_reads(preempt)
        with self.assertRaisesRegex(tw.StepError, "closed"):
            tw.cmd_reboot_prepare(self.env, self.args)
        st = self.read_state()
        self.assertEqual((st["card"], st["closed"]), ("reaper", True))
        self.assertNotIn("reboot", st)

    def test_an_unknown_boot_identity_refuses_the_reboot(self) -> None:
        # #32 MINOR-4: the state reports a boot-key problem as a field. Since decision 5
        # post-boot tells the reboot by the boot token alone, so without one the reboot
        # could never be confirmed (review of lane G2, finding 7): no reboot is
        # prepared, the owner hears why, the card stays free and the window open.
        self.tuning_state = {"items": [], "boot_problem": "boot key: not volatile (synthetic)"}
        self.write_state()
        with self.assertRaisesRegex(tw.StepError, "no reboot prepared"):
            tw.cmd_reboot_prepare(self.env, self.args)
        st = self.read_state()
        self.assertEqual((st["card"], st.get("closed")), ("free", False))
        self.assertNotIn("reboot", st)
        self.assertEqual(len(self.alarms), 1)
        self.assertIn("boot key: not volatile (synthetic)", self.alarms[0])

    def test_a_spike_that_did_not_stop_refuses_the_reboot(self) -> None:
        self.gone = False
        self.write_state()
        with self.assertRaisesRegex(tw.StepError, "did not stop"):
            tw.cmd_reboot_prepare(self.env, self.args)
        st = self.read_state()
        self.assertEqual(st["card"], "free")        # never entered the rebooting state
        self.assertNotIn("reboot", st)               # no reboot prepared over a held card
        self.assertFalse(st.get("closed"))           # the window stays open
        self.assertEqual(len(self.alarms), 1)        # the owner is alarmed
        self.assertIn("did not stop", self.alarms[0])

    def test_a_failed_trace_stop_or_mode_exit_refuses_the_reboot(self) -> None:
        for verb, flag in (("Stop-IemTrace", {"trace": "C:\\t\\runs\\x"}), ("Exit-IemTuningMode", {"tuning_mode": True})):
            self.fail = {verb}
            self.alarms.clear()
            self.write_state(**flag)
            with self.assertRaisesRegex(tw.StepError, "no reboot prepared", msg=verb):
                tw.cmd_reboot_prepare(self.env, self.args)
            st = self.read_state()
            self.assertEqual(st["card"], "free", verb)
            self.assertNotIn("reboot", st, verb)
            self.assertFalse(st.get("closed"), verb)
            self.assertTrue(any("no reboot is prepared" in a for a in self.alarms), verb)


ENV = {"PC_ROOT": "R", "PC_TUNING_ROOT": "T", "PC_XPERF": "xperf.exe",
       "PC_BUFFER_KEY": "K", "PC_BUFFER_NAME": "N", "PC_REAPER_HTTP": "H",
       "PC_REAPER_START_TASK_PATH": "P", "PC_REAPER_START_TASK": "TK", "PC_NTRACK": "9",
       "PC_METER_BRIDGE": "B", "PC_METER_ACTION": "A", "PC_METER_HEARTBEAT": "HB",
       "PC_ASIO_MODULE": "M", "PC_APP_HTTP": "AH"}


class PollScriptTests(unittest.TestCase):
    """The measurement poll's PowerShell (review m11): an error is reported,
    and the exact script the PC receives can be printed for the Windows CI
    runner (no private env needed)."""

    def test_a_failed_powercfg_is_an_error_not_an_unknown_plan(self) -> None:
        body = tw.poll_body("gov", 0, 0)
        self.assertNotIn("'unknown'", body)
        self.assertRegex(body, r"\$LASTEXITCODE -ne 0 -or -not \(\$pc -match '[^']+'\)\) \{ throw ")

    def test_poll_script_prints_what_sw_ps_sends(self) -> None:
        out = io.StringIO()
        with mock.patch.dict(os.environ, {"SPIKE_ENV": "/nonexistent/asio-spike.env"}), contextlib.redirect_stdout(out):
            code = tw.main(["poll-script", "--root", "C:\\r", "--governor", "EventLog", "--pid", "12", "--tid", "34"])
        self.assertEqual(code, 0)
        self.assertEqual(out.getvalue(), tw.sw.ps_script("C:\\r", tw.poll_body("EventLog", 12, 34)) + "\n")
        self.assertIn("Import-Module (Join-Path 'C:\\r' 'bin\\SpikePc.psm1')", out.getvalue())

    def test_analysis_script_prints_an_analysis_steps_start_as_sw_ps_sends_it(self) -> None:
        # F2 round 3, m11: the PC side of the analysis guard (Idle, the stop file's
        # time against the analysis start, the refusal through ps_script's catch)
        # runs on the Windows CI runner: this prints one analysis step exactly as
        # _measure composes it (analysis_step), with a probe as the step's body.
        out = io.StringIO()
        root, since = "C:\\r", "2026-01-01T00:00:00.0000000Z"
        with mock.patch.dict(os.environ, {"SPIKE_ENV": "/nonexistent/asio-spike.env"}), contextlib.redirect_stdout(out):
            code = tw.main(["analysis-script", "--root", root, "--since", since])
        self.assertEqual(code, 0)
        step = tw.analysis_step(root, since, tw.ANALYSIS_PROBE)
        self.assertEqual(out.getvalue(), tw.sw.ps_script(root, step) + "\n")
        self.assertEqual(step, " ; ".join([tw.analysis_guard(root, since), tw.sw.measure_import(root), tw.ANALYSIS_PROBE]))
        self.assertIn("PriorityClass", tw.ANALYSIS_PROBE)
        self.assertIn("Get-IemNow", tw.ANALYSIS_PROBE)              # IemMeasure loaded after the guard
        self.assertIn("Get-Command -Name ConvertTo-IemLpNumber", tw.ANALYSIS_PROBE)   # and IemTuning with it


class RebootTests(unittest.TestCase):
    """reboot asks Windows for a graceful restart (I8). Only sw.ps (the ssh
    boundary) is faked; sw.STATE/sw.EVENT_NOW point at a temp dir."""

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.saved = (tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps)
        tw.sw.STATE = self.dir / "spike-window.json"
        tw.sw.EVENT_NOW = self.dir / "EVENT-NOW"
        self.calls: list[str] = []

        def fake_ps(env, body, timeout=300, event="finish"):
            self.calls.append(body)
            return 0

        tw.sw.ps = fake_ps
        tw.sw.save_state({"id": "w", "card": "rebooting", "reboot": {"prepared_at": "2026-01-01T00:00:00Z"}, "closed": False})
        self.args = argparse.Namespace(approval="owner, 14:05: áno, reštartuj", by_owner=False)

    def tearDown(self) -> None:
        tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps = self.saved

    def test_the_restart_is_immediate_planned_and_never_forced(self) -> None:
        tw.cmd_reboot(ENV, self.args)
        self.assertEqual(len(self.calls), 1)
        body = self.calls[0]
        # Microsoft: a timeout above 0 implies the force flag. An immediate restart
        # without it lets an app veto (post-boot then reports that the PC did not reboot).
        # (The integrity scan refuses every restart spelled out unmarked, so the
        # program is compared without its extension.)
        command = body.split(" ; ")[0].split()
        self.assertEqual((command[0], command[1].lower().removesuffix(".exe"), command[2:5]), ("&", "shutdown", ["/r", "/t", "0"]))
        self.assertIn("/d p:", body)                        # a planned restart, with its reason
        self.assertNotRegex(body, r"[/-]f\b")
        self.assertNotRegex(body, r"[/-]t[\s:]+0*[1-9]")
        self.assertEqual(tw.sw.load_state()["reboot"]["by"], "agent")

    def test_an_event_refuses_the_restart_and_records_nothing(self) -> None:
        # Every command checks the "ide event" flag (#32 B2): main() then pre-empts.
        (self.dir / "EVENT-NOW").touch()
        for by_owner in (False, True):
            with self.assertRaises(tw.sw.EventNow):
                tw.cmd_reboot(ENV, argparse.Namespace(approval=self.args.approval, by_owner=by_owner))
        self.assertEqual(self.calls, [])
        self.assertNotIn("approval", tw.sw.load_state()["reboot"])

    def test_a_lost_answer_to_the_immediate_restart_points_at_post_boot(self) -> None:
        # The restart starts at once, so the ssh session may end before its answer:
        # that is not "nothing restarts" — post-boot tells whether the PC rebooted.
        def lost(env, body, timeout=300, event="finish"):
            self.calls.append(body)
            raise tw.sw.NoReply("PC command failed (exit 255): Connection reset")

        tw.sw.ps = lost
        with self.assertRaisesRegex(tw.StepError, "may be restarting.*post-boot"):
            tw.cmd_reboot(ENV, self.args)
        self.assertEqual(tw.sw.load_state()["reboot"]["approval"], self.args.approval)   # post-boot can run

    def test_an_error_reply_to_the_restart_request_is_not_a_lost_answer(self) -> None:
        # Review m9: the PC answered with an error, so nothing restarted.
        def refused(env, body, timeout=300, event="finish"):
            self.calls.append(body)
            raise tw.StepError("PC step failed: Access is denied")

        tw.sw.ps = refused
        with self.assertRaisesRegex(tw.StepError, "Access is denied") as cm:
            tw.cmd_reboot(ENV, self.args)
        self.assertNotIn("may be restarting", str(cm.exception))


class PostBootRunTests(unittest.TestCase):
    """post-boot after the approved reboot: the window closes only with REAPER
    back (#32 B13). Only the ssh boundary is faked (sw.ps, and the reachability
    probe's subprocess.run); the waits are skipped (time.sleep)."""

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.saved = (tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps, tw.sw.alarm)
        tw.sw.STATE = self.dir / "spike-window.json"
        tw.sw.EVENT_NOW = self.dir / "EVENT-NOW"
        self.alarms: list[str] = []
        tw.sw.alarm = self.alarms.append
        self.env = dict(ENV, PC_SSH="u@pc", RAW_DIR=str(self.dir / "raw"))
        tw.baseline_path(self.env).parent.mkdir(parents=True)
        tw.baseline_path(self.env).write_text(json.dumps({"plan.active": "reaper"}), encoding="utf-8")
        self.autostart = False     # REAPER started by itself after the boot
        self.started = False       # REAPER started by the bring-back
        self.bring_back_fails = False
        self.event_at_poll = 0     # "ide event" comes during this autostart poll (0: never)
        self.bring_backs = 0
        self.bring_back_s = 0.0
        self.calls: list[str] = []
        self.tuning_state: dict = {"items": [], "boot_token": "tok-after"}
        self.boot_time = "2026-01-02T00:00:00Z"

        def fake_ps(env, body, timeout=300, event="finish"):
            self.calls.append(body)
            if "Get-IemBootTime" in body:
                return self.boot_time
            if "Get-Process reaper" in body:
                if sum("Get-Process reaper" in c for c in self.calls) == self.event_at_poll:
                    (self.dir / "EVENT-NOW").touch()
                return 1 if (self.autostart or self.started) else 0
            if body.startswith("Invoke-SpikeBringBack"):
                self.bring_backs += 1
                threading.Event().wait(self.bring_back_s)   # a bring-back takes a while
                if self.bring_back_fails:
                    raise tw.StepError("PC step failed: REAPER did not load the project within 120 s")
                self.started = True
                return {"asio": "reaper"}
            if "Test-SpikeTaskBusy" in body and "Remove-Item" in body:   # the spike's stop file, once REAPER is back
                return "removed"
            if "Get-Process -Name asio_spike" in body:          # a preempt's spike check
                return 0
            if "Stop-SpikeGracefully" in body:
                return True
            if "Get-IemReaperFingerprint" in body:
                return {"plan.active": "reaper"}
            if "Get-IemTuningState" in body:
                return self.tuning_state
            if "Get-IemCpuSample" in body:
                return {"cpus": []}
            raise AssertionError(f"unexpected PC call: {body}")

        tw.sw.ps = fake_ps
        for patch in (mock.patch.object(tw.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)),
                      mock.patch.object(tw.time, "sleep")):
            patch.start()
            self.addCleanup(patch.stop)
        tw.sw.save_state({"id": "w", "card": "rebooting", "pref_original": 64, "pref_current": 64, "pref_restored": True,
                          "reboot": {"prepared_at": "2026-01-01T00:00:00Z", "boot_token": "tok-prepare",
                                     "approval": "owner, 14:05: áno, reštartuj", "by": "agent"},
                          "closed": False})

    def tearDown(self) -> None:
        tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps, tw.sw.alarm = self.saved

    def test_a_clean_return_closes_the_window(self) -> None:
        self.autostart = True
        tw.cmd_post_boot(self.env, argparse.Namespace())
        st = tw.sw.load_state()
        self.assertEqual((st["card"], st["closed"], st["post_boot"]["problems"]), ("reaper", True, []))

    def test_post_boot_removes_the_stop_file_once_reaper_is_back(self) -> None:
        # F2 round 3, m1: reboot-prepare's unwind wrote the spike's stop file and kept
        # it (the window stayed open); the close after the reboot removes it, so the
        # next window's start is not refused.
        self.autostart = True
        tw.cmd_post_boot(self.env, argparse.Namespace())
        back = next(i for i, c in enumerate(self.calls) if c.startswith("Invoke-SpikeBringBack"))
        cleanup = [i for i, c in enumerate(self.calls) if "Test-SpikeTaskBusy" in c and "Remove-Item" in c]
        self.assertEqual(len(cleanup), 1)
        self.assertGreater(cleanup[0], back)

    def test_a_failed_bring_back_keeps_the_stop_file(self) -> None:
        self.bring_back_fails = True
        with self.assertRaises(tw.StepError):
            tw.cmd_post_boot(self.env, argparse.Namespace())
        self.assertFalse(any("Remove-Item" in c for c in self.calls))

    # F2 round 3, decision 5: a reboot is the boot token changing, never the boot time
    # (a clock set back or forward says nothing about whether the PC rebooted).
    def test_the_same_boot_token_is_no_reboot_whatever_the_time_says(self) -> None:
        self.autostart = True
        self.tuning_state = {"items": [], "boot_token": "tok-prepare"}
        with self.assertRaisesRegex(tw.StepError, "post-boot checks failed"):
            tw.cmd_post_boot(self.env, argparse.Namespace())
        self.assertIn("the PC did not reboot after the request", tw.sw.load_state()["post_boot"]["problems"])

    def test_a_new_boot_token_is_a_reboot_whatever_the_time_says(self) -> None:
        self.autostart = True
        self.boot_time = "2025-12-31T00:00:00Z"   # earlier than reboot-prepare: the clock moved
        tw.cmd_post_boot(self.env, argparse.Namespace())
        self.assertEqual(tw.sw.load_state()["post_boot"]["problems"], [])

    def test_a_prepare_without_a_boot_token_cannot_tell_the_reboot(self) -> None:
        self.autostart = True
        st = tw.sw.load_state()
        del st["reboot"]["boot_token"]
        tw.sw.save_state(st)
        with self.assertRaisesRegex(tw.StepError, "post-boot checks failed"):
            tw.cmd_post_boot(self.env, argparse.Namespace())
        self.assertIn("whether the PC rebooted cannot be told (reboot-prepare recorded no boot token)",
                      tw.sw.load_state()["post_boot"]["problems"])

    def test_an_unknown_boot_identity_fails_the_checks(self) -> None:
        # #32 MINOR-4: no token, so "pending: none" would be vacuous; the window still
        # closes with REAPER back, and the checks fail naming the boot problem.
        self.autostart = True
        self.tuning_state = {"items": [], "boot_problem": "boot key: not volatile (synthetic)"}
        with self.assertRaisesRegex(tw.StepError, "post-boot checks failed"):
            tw.cmd_post_boot(self.env, argparse.Namespace())
        st = tw.sw.load_state()
        self.assertEqual((st["card"], st["closed"]), ("reaper", True))
        self.assertTrue(any("boot identity is unknown" in p for p in st["post_boot"]["problems"]))

    def test_reaper_that_did_not_autostart_is_brought_back_before_the_window_closes(self) -> None:
        with self.assertRaisesRegex(tw.StepError, "post-boot checks failed"):
            tw.cmd_post_boot(self.env, argparse.Namespace())
        self.assertTrue(any(c.startswith("Invoke-SpikeBringBack") for c in self.calls))
        st = tw.sw.load_state()
        self.assertEqual((st["card"], st["closed"]), ("reaper", True))
        self.assertIn("REAPER did not start by itself within 5 min", st["post_boot"]["problems"])

    def test_an_event_during_the_autostart_wait_brings_reaper_back_at_once(self) -> None:
        # Review m10: the flag is checked on every poll; the event path is the bring-back, now,
        # without the remaining read-only checks.
        self.event_at_poll = 2
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_post_boot(self.env, argparse.Namespace())
        self.assertEqual(sum("Get-Process reaper" in c for c in self.calls), 2)
        self.assertTrue(any(c.startswith("Invoke-SpikeBringBack") for c in self.calls))
        self.assertFalse(any("Get-IemReaperFingerprint" in c or "Get-IemCpuSample" in c for c in self.calls))
        st = tw.sw.load_state()
        self.assertEqual((st["card"], st["closed"]), ("reaper", True))

    def test_the_autostart_wait_sleeps_in_short_slices_that_see_the_flag(self) -> None:
        # Review round 3, m4: the flag is seen within a second, not after a 10 s nap.
        naps: list[float] = []

        def nap(seconds: float) -> None:
            naps.append(seconds)
            if len(naps) == 3:
                (self.dir / "EVENT-NOW").touch()

        tw.time.sleep.side_effect = nap
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_post_boot(self.env, argparse.Namespace())
        self.assertTrue(naps and all(s <= 1 for s in naps), naps)
        self.assertEqual(sum("Get-Process reaper" in c for c in self.calls), 1)   # no further poll after the flag

    def test_the_event_path_says_reaper_is_back_only_when_it_is(self) -> None:
        self.event_at_poll = 1
        self.bring_back_fails = True
        out = io.StringIO()
        with contextlib.redirect_stdout(out), self.assertRaises(tw.sw.EventNow):
            tw.cmd_post_boot(self.env, argparse.Namespace())
        self.assertNotIn("REAPER brought back", out.getvalue())
        self.assertIn("bring-back failed", out.getvalue())

    def test_post_boot_and_a_preempt_bring_reaper_back_once(self) -> None:
        # Review round 3, MAJOR 1: "ide event" during post-boot starts `iempc event` →
        # spike_window preempt in another process; two bring-backs at once would trigger
        # the meter bridge twice (a REAPER dialog during the event, #9). One lock, one
        # bring-back: whoever comes second re-reads the state and finds REAPER back.
        self.event_at_poll = 1
        self.bring_back_s = 0.3
        env = dict(self.env, PC_ROOT="R")
        errors: list[BaseException] = []

        def run(fn) -> None:
            try:
                fn()
            except (tw.StepError, tw.sw.EventNow) as e:
                errors.append(e)

        threads = [threading.Thread(target=run, args=(lambda: tw.cmd_post_boot(env, argparse.Namespace()),)),
                   threading.Thread(target=run, args=(lambda: tw.sw.cmd_preempt(env),))]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        self.assertEqual(self.bring_backs, 1)
        st = tw.sw.load_state()
        self.assertEqual((st["card"], st["closed"]), ("reaper", True))

    def test_post_boot_never_reopens_a_window_a_preempt_closed(self) -> None:
        # Review m5 (same class): a preempt in another process may close the window
        # while post-boot runs; post-boot's own save never takes that back.
        self.bring_back_fails = True
        real = tw.sw.ps

        def preempt_meanwhile(env, body, timeout=300, event="finish"):
            if body.startswith("Invoke-SpikeBringBack"):
                st = tw.sw.load_state()
                st.update(card="reaper", closed=True)
                tw.sw.save_state(st)
            return real(env, body, timeout, event)

        tw.sw.ps = preempt_meanwhile
        with self.assertRaises(tw.StepError):
            tw.cmd_post_boot(self.env, argparse.Namespace())
        st = tw.sw.load_state()
        self.assertEqual((st["card"], st["closed"]), ("reaper", True))
        self.assertIn("post_boot", st)

    def test_a_failed_bring_back_keeps_the_window_open(self) -> None:
        self.bring_back_fails = True
        with self.assertRaises(tw.StepError):
            tw.cmd_post_boot(self.env, argparse.Namespace())
        st = tw.sw.load_state()
        self.assertEqual((st["card"], st["closed"]), ("rebooting", False))   # preempt / to-event brings REAPER back
        self.assertTrue(any("window stays open" in a for a in self.alarms))


class TuningSetupTests(unittest.TestCase):
    """tuning-setup creates the PC folder with a restricted ACL (#32 B12). Faked
    at the ssh seam itself (sw.guarded runs the ssh process; sw.scp copies), so
    the PowerShell script is checked exactly as the PC would receive it."""

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.saved = (tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.guarded, tw.sw.scp, tw.PROFILE)
        tw.sw.STATE = self.dir / "spike-window.json"
        tw.sw.EVENT_NOW = self.dir / "EVENT-NOW"
        tw.PROFILE = write(PROFILE)
        self.scripts: list[str] = []
        self.copies: list[tuple[str, str]] = []
        self.icacls_fails = False

        def fake_guarded(cmd, stdin, timeout, event):
            self.scripts.append(stdin)
            if "icacls" in stdin and self.icacls_fails:
                return json.dumps({"ok": False, "error": "icacls exited 1332"}) + "\n"
            return json.dumps({"ok": True, "r": 1}) + "\n"

        tw.sw.guarded = fake_guarded
        tw.sw.scp = lambda src, dst: self.copies.append((src, dst))
        self.env = dict(ENV, PC_SSH="u@pc", PC_TUNING_ROOT="C:\\t", PC_TUNING_ROOT_SCP="/C:/t", RAW_DIR=str(self.dir / "raw"))
        tw.sw.save_state({"id": "w", "card": "reaper", "closed": False})

    def tearDown(self) -> None:
        tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.guarded, tw.sw.scp, tw.PROFILE = self.saved

    def acl_script(self) -> str:
        return next(x for x in self.scripts if "icacls" in x)

    def test_the_folder_script_stops_on_errors_and_checks_icacls(self) -> None:
        tw.cmd_tuning_setup(self.env, argparse.Namespace())
        script = self.acl_script()
        self.assertIn("$ErrorActionPreference = 'Stop'", script)
        self.assertRegex(script, r"icacls\.exe [^\n]*\$LASTEXITCODE")   # a native exit code never throws by itself
        self.assertEqual(len(self.copies), 1)

    def test_a_failed_acl_stops_the_setup_before_the_profile_is_copied(self) -> None:
        self.icacls_fails = True
        with self.assertRaisesRegex(tw.StepError, "icacls"):
            tw.cmd_tuning_setup(self.env, argparse.Namespace())
        self.assertEqual(self.copies, [])


if __name__ == "__main__":
    unittest.main()
