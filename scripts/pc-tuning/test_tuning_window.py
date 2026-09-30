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
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_latency_report as tlr  # noqa: E402  (the xperf fixtures)
import tuning_window as tw  # noqa: E402

PROFILE = {"version": 1, "journal": "C:\\j.json", "registry_root": "",
           "layout": {"housekeeping": [0, 1, 6, 7, 8, 9, 10, 11, 12, 13], "card": [2], "nic": [4, 5], "audio": [14]},
           "plan": {"guid": "6c1b0d6e-0a39-4f55-9c2a-1e5a3b7d9c01", "source": "00000000-0000-0000-0000-000000000001"},
           "governor": "gov", "placement": [], "services_disable": [], "services_mode": [],
           "updates": {"services": [], "tasks": []}, "maintenance": {"off": True, "tasks": []},
           "defender": {"paths": [], "processes": []}, "devices": [], "nic": {"adapter": "a", "properties": {}, "rss": {"base": 4, "max": 5}, "pnp_capabilities": 24},
           "fingerprint": {"files": [], "keys": []}}


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

    def test_an_event_or_a_held_card_refuses_before_the_pc(self) -> None:
        tw.sw.save_state({"id": "w", "card": "reaper", "closed": False})
        with self.assertRaisesRegex(tw.StepError, "not free"):
            tw.cmd_undo(self.env, self.args)
        (self.dir / "EVENT-NOW").touch()
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_undo(self.env, self.args)
        self.assertEqual(self.bodies, [])


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

        def fake_ps(env, body, timeout=300, event="finish"):
            for verb in self.fail:
                if verb in body:
                    raise tw.StepError(f"PC step failed: {verb}")
            if "Stop-SpikeGracefully" in body:
                return self.gone
            if "Get-IemTuningState" in body:
                return {"items": []}
            if "Get-IemNow" in body:
                return "2026-01-01T00:00:00Z"
            return {"ok": True}

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
        self.assertIn("shutdown.exe /r /t 0 ", body)
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

        def fake_ps(env, body, timeout=300, event="finish"):
            self.calls.append(body)
            if "Get-IemBootTime" in body:
                return "2026-01-02T00:00:00Z"
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
            if "Get-Process -Name asio_spike" in body:          # a preempt's spike check
                return 0
            if "Stop-SpikeGracefully" in body:
                return True
            if "Get-IemReaperFingerprint" in body:
                return {"plan.active": "reaper"}
            if "Get-IemTuningState" in body:
                return {"items": []}
            if "Get-IemCpuSample" in body:
                return {"cpus": []}
            raise AssertionError(f"unexpected PC call: {body}")

        tw.sw.ps = fake_ps
        for patch in (mock.patch.object(tw.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)),
                      mock.patch.object(tw.time, "sleep")):
            patch.start()
            self.addCleanup(patch.stop)
        tw.sw.save_state({"id": "w", "card": "rebooting", "pref_original": 64, "pref_current": 64, "pref_restored": True,
                          "reboot": {"prepared_at": "2026-01-01T00:00:00Z", "approval": "owner, 14:05: áno, reštartuj", "by": "agent"},
                          "closed": False})

    def tearDown(self) -> None:
        tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps, tw.sw.alarm = self.saved

    def test_a_clean_return_closes_the_window(self) -> None:
        self.autostart = True
        tw.cmd_post_boot(self.env, argparse.Namespace())
        st = tw.sw.load_state()
        self.assertEqual((st["card"], st["closed"], st["post_boot"]["problems"]), ("reaper", True, []))

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


class FakeClock:
    """spike_window's clock inside a test: each reading moves 5 s on, sleeps
    return at once (cmd_run polls the PC every 10 s of this clock)."""

    def __init__(self) -> None:
        self.t = 0.0

    def monotonic(self) -> float:
        self.t += 5
        return self.t

    def sleep(self, seconds: float) -> None:
        pass


def hwlat_report(cpu: int, **change) -> dict:
    h = {"cpu": cpu, "threshold_us": 10, "placed": [256 + cpu], "priority": "time-critical", "reads": 1000, "over": 2,
         "gaps_us": {"p50": 11.0, "p99": 15.0, "p999": 20.0, "max": 31.5}, "largest": [{"at_us": 5.0, "gap_us": 31.5}]}
    return {"outcome": "done", "hwlat": {**h, **change}}


DUPLEX = {"outcome": "done", "segments": [{"telemetry": {"callbacks": 1000, "late": 0, "missed": 0, "overruns": 0, "position_gaps": 0,
                                                         "callback_cpus": {"14": 1000}, "callback_thread": 4243}}]}
# The spike's exit code per outcome (examples/asio_spike/main.rs code_of; "done"/"stopped" 0).
EXIT_CODES = {"error": 1, "refused": 4, "band-activity": 5, "fault-caught": 6, "rate-changed": 7, "stop-hung": 8}
# Outcomes that end a run without a completed measurement.
NOT_MEASURED = (("refused", 4), ("band-activity", 5), ("fault-caught", 6), ("rate-changed", 7), ("stop-hung", 8))


class FakePc:
    """The IEM PC at the ssh seam: `ps` answers sw.ps by the PowerShell verb
    and records (body, event); `scp` writes the file the PC would hold and
    records (name, event). Both keep sw.guarded's "ide event" contract: a
    call that is not event="ignore" raises EventNow when the flag exists once
    it has ended (the hooks may create it mid-call). Each spike run is
    running on its first status poll and exited on the next."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, str]] = []
        self.copies: list[tuple[str, str]] = []
        self.reports: list[dict] = []          # one spike report per run, in order
        self.progress: dict = {"missed": 0, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        self.fail: set[str] = set()            # PowerShell verbs that fail on the PC
        self.dpcisr = tlr.DPCISR_XPERF         # what every dpcisr.txt holds
        self.runs = 0
        self.polls = 0
        self.on_call = None                    # a hook: (body) -> None, called before answering
        self.on_copy = None                    # a hook: (name) -> None, called during a copy

    def bodies(self, verb: str) -> list[str]:
        return [b for b, _ in self.calls if verb in b]

    def ps(self, env, body, timeout=300, event="finish"):
        answer = self.answer(body, event)
        if event != "ignore" and tw.sw.event_now():
            raise tw.sw.EventNow()
        return answer

    def answer(self, body, event):
        self.calls.append((body, event))
        if self.on_call:
            self.on_call(body)
        for verb in self.fail:
            if verb in body:
                raise tw.StepError(f"PC step failed: {verb} (synthetic)")
        if "Write-GoldenRequest" in body:
            self.runs += 1
            self.polls = 0
            return f"spike-{self.runs}"
        if ".progress.json" in body:
            self.polls += 1
            if self.polls == 1:
                return {"status": {"state": "running", "results": [{"pid": 4242}]}, "progress": self.progress}
            code = EXIT_CODES.get(self.reports[self.runs - 1]["outcome"], 0)
            return {"status": {"state": "exited", "results": [{"exit": code}]}, "progress": self.progress}
        if "Get-IemNow" in body:
            return "2026-01-01T00:00:00Z"
        if "Get-IemSystemEvents" in body:
            return []
        if "Win32_PerfRawData_PerfOS_Processor" in body or "Get-IemPollSample" in body:
            return {"at": "2026-01-01T00:00:10Z", "cpu": {"cpus": []}, "plan": "p", "governor": "Stopped",
                    "thread": {"base": 15, "current": 26}}
        return {"ok": True}   # Start-/Stop-IemTrace, Invoke-IemDpcIsr, Export-IemNearGlitch

    def scp(self, src: str, dst: str, event: str = "ignore") -> None:
        name = src.rsplit("/", 1)[-1]
        self.copies.append((name, event))
        if self.on_copy:
            self.on_copy(name)
        if event == "abandon" and tw.sw.event_now():
            raise tw.sw.EventNow()   # the copy is interrupted, nothing is written
        if name.endswith(".report.json"):
            text = json.dumps(self.reports[self.runs - 1])
        elif name.endswith(".stderr.txt"):
            text = ""
        elif name.endswith("dpcisr.txt"):
            text = self.dpcisr
        elif name.endswith("near.txt"):
            text = tlr.DUMPER
        else:
            raise AssertionError(f"unexpected copy: {src}")
        Path(dst).write_text(text, encoding="utf-8")


class WindowHarness(unittest.TestCase):
    """A dev-time window with a free card at buffer 32 and the real spike_window
    run loop; only the ssh seam (sw.ps, sw.scp) is faked and spike_window's
    clock runs fast."""

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.saved = (tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps, tw.sw.scp, tw.sw.alarm, tw.PROFILE)
        tw.sw.STATE, tw.sw.EVENT_NOW = self.dir / "spike-window.json", self.dir / "EVENT-NOW"
        tw.PROFILE = write(PROFILE)
        self.pc = FakePc()
        tw.sw.ps, tw.sw.scp = self.pc.ps, self.pc.scp
        self.alarms: list[str] = []
        tw.sw.alarm = self.alarms.append
        clock = mock.patch.object(tw.sw, "time", FakeClock())
        clock.start()
        self.addCleanup(clock.stop)
        self.env = dict(ENV, PC_SSH="u@pc", PC_ROOT_SCP="/R", PC_TUNING_ROOT="C:\\t", PC_TUNING_ROOT_SCP="/C:/t",
                        PC_ASIO_DRIVER="D", PC_ACTIVITY_CHANNELS="101-110", RAW_DIR=str(self.dir / "raw"))
        tw.sw.save_state({"id": "w", "card": "free", "dev_time": True, "preflight": {"pref": 64}, "pref_original": 64,
                          "pref_current": 32, "pref_restored": False, "runs": [], "closed": False})

    def tearDown(self) -> None:
        tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps, tw.sw.scp, tw.sw.alarm, tw.PROFILE = self.saved

    def state(self) -> dict:
        return tw.sw.load_state()


class HwlatTests(WindowHarness):
    """hwlat per CPU through the real cmd_run (#32 C2)."""

    def args(self, lps: str = "2,14") -> argparse.Namespace:
        return argparse.Namespace(lps=lps, seconds=30, threshold_us=10)

    def rows(self) -> list[dict]:
        files = sorted((self.dir / "raw" / "pc-tuning" / "w").glob("hwlat-*.json"))
        self.assertEqual(len(files), 1)
        return json.loads(files[0].read_text(encoding="utf-8"))

    def test_every_cpu_is_measured_placed_and_raised(self) -> None:
        self.pc.reports = [hwlat_report(2), hwlat_report(14)]
        tw.cmd_hwlat(self.env, self.args())
        self.assertEqual([(r["cpu"], r["placed"], r["priority"], r["failed"]) for r in self.rows()],
                         [(2, [258], "time-critical", False), (14, [270], "time-critical", False)])

    def test_a_scanner_reported_unplaced_fails_the_step_and_keeps_what_was_measured(self) -> None:
        # An older spike wrote the placement error as text and still said "done".
        self.pc.reports = [hwlat_report(2), hwlat_report(14, placed="the CPU set could not be applied (synthetic)"), hwlat_report(15)]
        with self.assertRaisesRegex(tw.StepError, "CPU 14"):
            tw.cmd_hwlat(self.env, self.args("2,14,15"))
        self.assertEqual([(r["cpu"], r["failed"]) for r in self.rows()], [(2, False), (14, True)])
        self.assertEqual(self.pc.runs, 2)   # stops at the failed CPU

    def test_an_error_outcome_fails_the_step_and_keeps_what_was_measured(self) -> None:
        self.pc.reports = [hwlat_report(2), {"outcome": "error", "error": "hwlat: cpu 14 not raised (synthetic)",
                                             "hwlat": {"cpu": 14, "threshold_us": 10, "error": "cpu 14 not raised (synthetic)"}}]
        with self.assertRaisesRegex(tw.StepError, "not raised"):
            tw.cmd_hwlat(self.env, self.args())
        self.assertEqual([r["cpu"] for r in self.rows()], [2])

    def test_an_outcome_that_is_no_measurement_fails_the_step_and_is_recorded(self) -> None:
        # #32 follow-up: refused, fault-caught, rate-changed (and band activity, stop-hung)
        # end a run without a measurement: the step fails, the row names the outcome.
        for outcome, code in NOT_MEASURED:
            with self.subTest(outcome):
                for f in (self.dir / "raw" / "pc-tuning" / "w").glob("hwlat-*.json"):
                    f.unlink()
                self.pc.runs = 0
                self.pc.reports = [{"outcome": outcome, "error": f"synthetic {outcome}"}]
                with self.assertRaisesRegex(tw.StepError, f"outcome {outcome}"):
                    tw.cmd_hwlat(self.env, self.args("2"))
                self.assertEqual([(r["outcome"], r["failed"]) for r in self.rows()], [(outcome, True)])


class MeasureTests(WindowHarness):
    """measure through the real cmd_run and unwind, the PC faked at the ssh seam."""

    def setUp(self) -> None:
        super().setUp()
        self.pc.reports = [DUPLEX]

    def args(self, **kw) -> argparse.Namespace:
        base = {"label": "load-32", "frames": 32, "seconds": 60, "burn_us": 40, "stress": 0, "audio_cpus": "", "stress_cpus": "",
                "trace": "dpc", "circular_mb": 0}
        return argparse.Namespace(**{**base, **kw})

    def summary(self) -> dict:
        return json.loads(Path(self.state()["measurements"][-1]["summary"]).read_text(encoding="utf-8"))

    def record_trace(self, where: str = "C:\\t\\runs\\old-20260101T000000Z") -> None:
        st = self.state()
        st["trace"] = where
        tw.sw.save_state(st)

    def test_a_traced_run_is_summarised(self) -> None:
        tw.cmd_measure(self.env, self.args())
        s = self.summary()
        self.assertEqual((s["label"], s["verdict"]["stable"]), ("load-32", True))
        self.assertIn("isr nicdrv.sys: above 2048 us (a full period is 333)", s["findings"])
        self.assertIsNone(self.state()["trace"])

    # #32 follow-up: only a completed run ("done", or "stopped" by the stop file) is a measurement.
    def test_an_outcome_that_is_no_measurement_fails_the_step_and_is_recorded(self) -> None:
        tel = DUPLEX["segments"]
        self.pc.reports = [{"outcome": outcome, "segments": tel} for outcome, _ in NOT_MEASURED]
        for outcome, code in NOT_MEASURED:
            with self.subTest(outcome):
                with self.assertRaisesRegex(tw.StepError, f"{outcome}.*exit {code}"):
                    tw.cmd_measure(self.env, self.args(label=f"load-{code}"))
                row = self.state()["measurements"][-1]
                self.assertEqual((row["label"], row["outcome"], row["exit"], row["failed"]), (f"load-{code}", outcome, code, True))
                self.assertNotIn("summary", row)
                self.assertIsNone(self.state()["trace"])                 # stopped on the way out
        self.assertEqual(list((self.dir / "raw" / "pc-tuning" / "w").rglob("summary.json")), [])
        self.assertEqual(self.pc.bodies("Invoke-IemDpcIsr"), [])         # never analysed as a measurement

    def test_a_run_stopped_by_the_stop_file_is_still_a_measurement(self) -> None:
        self.pc.reports = [{**DUPLEX, "outcome": "stopped"}]
        tw.cmd_measure(self.env, self.args())
        self.assertEqual(self.summary()["verdict"]["outcome"], "stopped")

    # B5: a measure never leaves a kernel trace running behind it.
    def test_an_error_during_the_run_stops_the_trace_and_clears_it(self) -> None:
        self.pc.fail = {".progress.json"}
        with self.assertRaisesRegex(tw.StepError, "progress.json"):
            tw.cmd_measure(self.env, self.args())
        stops = self.pc.bodies("Stop-IemTrace")
        self.assertEqual(len(stops), 1)
        self.assertNotIn("-Merge", stops[0])      # quick: the raw files stay
        self.assertIsNone(self.state()["trace"])

    def test_a_failed_cleanup_keeps_the_trace_recorded_and_alarms(self) -> None:
        self.pc.fail = {".progress.json", "Stop-IemTrace"}
        with self.assertRaisesRegex(tw.StepError, "progress.json"):   # the run's error, not the cleanup's
            tw.cmd_measure(self.env, self.args())
        self.assertTrue(self.state()["trace"])     # trace-stop, or a preempt, retries it
        self.assertTrue(any("trace-stop" in a for a in self.alarms))

    def test_an_event_leaves_the_trace_to_the_preempt(self) -> None:
        self.pc.on_call = lambda body: (self.dir / "EVENT-NOW").touch() if ".progress.json" in body else None
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_measure(self.env, self.args())
        self.assertEqual(self.pc.bodies("Stop-IemTrace"), [])   # no delay before REAPER comes back
        self.assertIn("trace-stop", tw.sw.undo_plan(self.state(), spike_running=False))

    def test_a_leftover_trace_refuses_measure_and_hwlat(self) -> None:
        self.record_trace()
        with self.assertRaisesRegex(tw.StepError, "trace-stop"):
            tw.cmd_measure(self.env, self.args())
        with self.assertRaisesRegex(tw.StepError, "trace-stop"):
            tw.cmd_hwlat(self.env, argparse.Namespace(lps="2", seconds=30, threshold_us=10))
        self.assertEqual(self.pc.calls, [])

    def test_trace_stop_stops_the_recorded_trace(self) -> None:
        self.record_trace()
        with mock.patch.object(tw.sw, "load_env", return_value=self.env):
            self.assertEqual(tw.main(["trace-stop"]), 0)
        stops = self.pc.bodies("Stop-IemTrace")
        self.assertEqual(len(stops), 1)
        self.assertIn("-Dir 'C:\\t\\runs\\old-20260101T000000Z'", stops[0])
        self.assertNotIn("-Merge", stops[0])
        self.assertIsNone(self.state()["trace"])

    def test_trace_stop_is_a_window_command_the_event_pre_empts(self) -> None:
        # Review m4: the stop completes (it changes the PC), then "ide event" wins (main pre-empts).
        self.record_trace()
        self.pc.on_call = lambda body: (self.dir / "EVENT-NOW").touch() if "Stop-IemTrace" in body else None
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_trace_stop(self.env, argparse.Namespace())
        self.assertEqual([e for b, e in self.pc.calls if "Stop-IemTrace" in b], ["finish"])

    # Review m5: the error path never races a preempt over the window state.
    def test_abandon_trace_leaves_the_trace_to_the_preempt_once_the_flag_exists(self) -> None:
        self.record_trace()
        (self.dir / "EVENT-NOW").touch()
        tw.abandon_trace(self.env)
        self.assertEqual(self.pc.calls, [])
        self.assertTrue(self.state()["trace"])

    def test_abandon_trace_changes_only_the_trace_field(self) -> None:
        self.record_trace()

        def concurrent_preempt(body: str) -> None:
            if "Stop-IemTrace" in body:   # another process's preempt saves its own changes meanwhile
                st = tw.sw.load_state()
                st.update(card="reaper", closed=True, pref_restored=True)
                tw.sw.save_state(st)

        self.pc.on_call = concurrent_preempt
        tw.abandon_trace(self.env)
        st = self.state()
        self.assertEqual((st["trace"], st["card"], st["closed"], st["pref_restored"]), (None, "reaper", True, True))

    def test_trace_stop_without_a_recorded_trace_touches_nothing(self) -> None:
        tw.cmd_trace_stop(self.env, argparse.Namespace())
        self.assertEqual(self.pc.calls, [])

    # B6: "ide event" waits for the trace stop, never for the xperf analysis.
    def test_the_trace_stop_is_its_own_call_and_the_analysis_is_abandonable(self) -> None:
        seen: dict = {}

        def on_call(body: str) -> None:
            if "Invoke-IemDpcIsr" in body:
                seen["trace"] = self.state()["trace"]

        self.pc.on_call = on_call
        tw.cmd_measure(self.env, self.args())
        stops = [(b, e) for b, e in self.pc.calls if "Stop-IemTrace" in b]
        self.assertEqual(len(stops), 1)
        self.assertNotIn("-Merge", stops[0][0])                      # the merge belongs to the analysis (M1)
        self.assertNotIn("Invoke-IemDpcIsr", stops[0][0])
        self.assertEqual(stops[0][1], "finish")                      # the stop changes the PC: it completes
        self.assertEqual([e for b, e in self.pc.calls if "Invoke-IemDpcIsr" in b], ["abandon"])
        self.assertIsNone(seen["trace"])                             # recorded as stopped before the analysis

    # Review M1: "ide event" never waits for a merge, and no trace starts after it.
    def test_the_final_stop_does_not_merge_and_the_merge_is_abandonable(self) -> None:
        tw.cmd_measure(self.env, self.args())
        stops = [(b, e) for b, e in self.pc.calls if "Stop-IemTrace" in b]
        self.assertEqual([("-Merge" in b, e) for b, e in stops], [(False, "finish")])
        merges = [(b, e) for b, e in self.pc.calls if "'-merge'" in b]
        self.assertEqual([e for _, e in merges], ["abandon"])
        self.assertRegex(merges[0][0], r"'kernel\.etl', 'markers\.etl'.*'trace\.etl'")

    def test_a_cut_sets_the_raw_files_aside_and_merges_them_with_the_analysis(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        tw.cmd_measure(self.env, self.args(circular_mb=1024))
        cut_stops = [b for b in self.pc.bodies("Stop-IemTrace") if "cut-1" in b]
        self.assertEqual(len(cut_stops), 1)
        self.assertNotIn("-Merge", cut_stops[0])
        self.assertNotIn("Start-IemTrace", cut_stops[0])
        self.assertIn("'cut-1.kernel.etl'", cut_stops[0])
        analysis = " ; ".join(b for b, e in self.pc.calls if e == "abandon" and "'-merge'" in b)
        self.assertRegex(analysis, r"'cut-1\.kernel\.etl', 'cut-1\.markers\.etl'.*'cut-1\.etl'")

    def test_no_trace_starts_once_the_event_flag_exists(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}

        def on_call(body: str) -> None:
            if "Stop-IemTrace" in body and "cut-1" in body:
                (self.dir / "EVENT-NOW").touch()   # "ide event" during the cut's stop

        self.pc.on_call = on_call
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_measure(self.env, self.args(circular_mb=1024))
        self.assertEqual(len(self.pc.bodies("Start-IemTrace")), 1)   # the first start only
        self.assertTrue(self.state()["trace"])                       # left to the preempt's trace-stop

    # Review round 3, decision B: the analysis never competes with the event.
    ANALYSIS = ("'-merge'", "Invoke-IemDpcIsr", "Export-IemNearGlitch")

    def analysis_calls(self) -> list[tuple[str, str]]:
        return [(b, e) for b, e in self.pc.calls if any(v in b for v in self.ANALYSIS)]

    def test_each_analysis_step_is_its_own_idle_call_that_checks_the_stop_file(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        tw.cmd_measure(self.env, self.args(trace="diag", circular_mb=1024))
        calls = self.analysis_calls()
        self.assertEqual(len(calls), 6)                                   # (trace + 1 cut) × (merge, dpcisr, near)
        for body, event in calls:
            self.assertEqual(sum(v in body for v in self.ANALYSIS), 1, body)
            self.assertEqual(event, "abandon")
            self.assertIn("(Get-Process -Id $PID).PriorityClass = 'Idle'", body)   # xperf inherits it
            # A preempt writes the spike's stop file first: newer than the analysis start → no start.
            self.assertRegex(body, r"queue\\stop'\) ; if \(\(Test-Path -LiteralPath \$s\) -and .*LastWriteTimeUtc -gt .*'2026-01-01T00:00:00Z'.*throw ")

    def test_a_merged_trace_leaves_no_raw_files_behind(self) -> None:
        # Review round 3, m8: a soak's raw kernel/marker files (per cut) would double
        # the disk used. They are deleted only after `xperf -merge` succeeded: a failed
        # merge throws (sw.ps runs under $ErrorActionPreference='Stop') before the delete.
        for base in ("", "cut-2."):
            body = tw.merge("'xperf.exe'", "'D'", base)
            self.assertRegex(body, r"\[void\]\(Invoke-IemXperf [^;]*'-merge'[^;]*\) ; Remove-Item -LiteralPath \$m$")

    def test_no_analysis_step_starts_once_the_flag_exists(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        self.pc.on_call = lambda body: (self.dir / "EVENT-NOW").touch() if "Invoke-IemDpcIsr" in body else None
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_measure(self.env, self.args(trace="diag", circular_mb=1024))
        self.assertEqual(len(self.analysis_calls()), 2)                  # the first merge, then the dpcisr during which it came

    def test_a_step_the_pc_refused_after_a_preempt_is_the_event(self) -> None:
        # The flag is not on the dev box yet, but a preempt already wrote the stop file.
        real = self.pc.answer

        def refusing(body, event):
            if "Invoke-IemDpcIsr" in body:
                self.pc.calls.append((body, event))
                raise tw.StepError("PC step failed: ide event: the analysis step did not start")
            return real(body, event)

        self.pc.answer = refusing
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_measure(self.env, self.args())
        self.assertNotIn("measurements", self.state())

    # Review M3: the analysis downloads and parses give way to "ide event".
    def analysis_copies(self) -> list[tuple[str, str]]:
        return [(n, e) for n, e in self.pc.copies if not n.endswith((".report.json", ".stderr.txt"))]

    def test_the_analysis_copies_are_abandonable(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        tw.cmd_measure(self.env, self.args(trace="diag", circular_mb=1024))
        self.assertEqual([n for n, _ in self.analysis_copies()], ["dpcisr.txt", "near.txt", "cut-1.dpcisr.txt", "cut-1.near.txt"])
        self.assertEqual({e for _, e in self.analysis_copies()}, {"abandon"})

    def test_an_event_during_a_copy_stops_the_analysis_there(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        self.pc.on_copy = lambda name: (self.dir / "EVENT-NOW").touch() if name == "near.txt" else None
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_measure(self.env, self.args(trace="diag", circular_mb=1024))
        self.assertEqual([n for n, _ in self.analysis_copies()], ["dpcisr.txt", "near.txt"])
        self.assertNotIn("measurements", self.state())

    def test_the_near_dumps_are_parsed_from_their_files_with_the_event_check(self) -> None:
        seen: list = []
        real = tw.lr.near_glitch

        def spy(source, period_us, window_periods=2, check=None):
            seen.append((type(source).__name__, check))
            return real(source, period_us, window_periods, check)

        with mock.patch.object(tw.lr, "near_glitch", spy):
            tw.cmd_measure(self.env, self.args(trace="diag"))
        self.assertEqual([(kind, check is not None) for kind, check in seen], [("PosixPath", True)])

    # B7: a cut keeps the trace's options, and every cut gets its own near-glitch view.
    def test_a_diag_cut_restarts_with_context_switches_and_gets_its_near_glitch_view(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        tw.cmd_measure(self.env, self.args(trace="diag", circular_mb=1024))
        starts = [b.split("Start-IemTrace", 1)[1] for b in self.pc.bodies("Start-IemTrace")]
        self.assertEqual(len(starts), 2)                             # the start and the cut's restart
        for options in starts:
            self.assertIn("-CSwitch", options)
            self.assertIn("-CircularMB 1024", options)
        analysis = " ; ".join(self.pc.bodies("Export-IemNearGlitch"))
        self.assertRegex(analysis, r"Export-IemNearGlitch [^;]*-Name 'cut-1\.etl'")
        s = self.summary()
        self.assertEqual([c["cut"] for c in s["cuts"]], [1])
        self.assertEqual(s["cuts"][0]["near_glitch"][0]["kind"], "missed")
        self.assertIn("isr nicdrv.sys: above 2048 us (a full period is 333)", s["cuts"][0]["findings"])
        self.assertEqual(s["near_glitch"][0]["kind"], "missed")       # the final trace's view stays

    # B8 in the window: an unreadable dpcisr is a failed step with a clear message, not a traceback.
    def test_a_dpcisr_without_per_cpu_usage_fails_the_step(self) -> None:
        start = tlr.DPCISR_XPERF.index("     CPU 0 Usage,")
        self.pc.dpcisr = tlr.DPCISR_XPERF[:start] + tlr.DPCISR_XPERF[tlr.DPCISR_XPERF.index("\nTotal = 3261"):]
        with self.assertRaisesRegex(tw.StepError, "per-CPU usage.*dpcisr.txt"):
            tw.cmd_measure(self.env, self.args())
        self.assertIsNone(self.state()["trace"])                         # the trace was stopped before the analysis
        self.assertNotIn("measurements", self.state())

    # B11: the 10 s poll must not load the PC being measured with a C# compile or heavy WMI.
    def test_the_poll_is_light_and_still_samples_the_sentinels(self) -> None:
        tw.cmd_measure(self.env, self.args())
        polls = [(b, e) for b, e in self.pc.calls if "Win32_PerfRawData_PerfOS_Processor" in b or "Get-IemPollSample" in b]
        self.assertEqual(len(polls), 2)                                  # one per status poll
        for body, event in polls:
            self.assertNotIn("IemMeasure", body)          # importing the modules runs Add-Type (a C# compile) every time
            self.assertNotIn("Get-IemPollSample", body)
            self.assertNotIn("Win32_PerfFormattedData", body)            # the second, formatted WMI class is unused
            self.assertEqual(body.count("Get-CimInstance"), 1)
            self.assertIn("-Name 'gov'", body)                           # the governor from the local profile
            self.assertEqual(event, "abandon")
        self.assertIn("-Id 4242", polls[0][0])                           # the spike's pid from the running status
        self.assertIn("-eq 4243", polls[0][0])                           # and its callback thread from the progress
        self.assertNotIn("Get-Process", polls[1][0])                     # exited: no process to read
        self.assertEqual(self.summary()["callback"]["priority"], {"base_min": 15, "base_max": 15, "current_min": 26, "current_max": 26})

    # C1: the busy threads never share the audio CPU: the profile's housekeeping CPUs unless told otherwise.
    def test_stress_runs_on_the_housekeeping_cpus_unless_told_otherwise(self) -> None:
        self.pc.reports = [DUPLEX, DUPLEX]
        tw.cmd_measure(self.env, self.args(stress=4, audio_cpus="14"))
        tw.cmd_measure(self.env, self.args(label="load-32-b", stress=4, audio_cpus="14", stress_cpus="6-13"))
        first, second = self.pc.bodies("Write-GoldenRequest")
        self.assertIn("stress_cpus = '0,1,6,7,8,9,10,11,12,13'", first)
        self.assertIn("audio_cpus = '14'", first)
        self.assertIn("stress_cpus = '6-13'", second)

    def test_the_default_stress_cpus_leave_out_the_audio_cpus(self) -> None:
        # Review m12: an --audio-cpus inside the housekeeping layout is taken out of it.
        tw.cmd_measure(self.env, self.args(stress=4, audio_cpus="6,7"))
        self.assertIn("stress_cpus = '0,1,8,9,10,11,12,13'", self.pc.bodies("Write-GoldenRequest")[0])

    def test_a_dpc_trace_has_no_near_glitch_view(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        tw.cmd_measure(self.env, self.args(circular_mb=1024))
        self.assertEqual(self.pc.bodies("Export-IemNearGlitch"), [])
        self.assertNotIn("-CSwitch", " ".join(self.pc.bodies("Start-IemTrace")))
        self.assertEqual(list(self.summary()["cuts"][0]), ["cut", "findings"])


if __name__ == "__main__":
    unittest.main()
