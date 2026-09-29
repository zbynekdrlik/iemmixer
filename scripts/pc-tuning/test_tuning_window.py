"""Tests for scripts/pc-tuning/tuning_window.py (pure parts; ssh is the PC)."""
from __future__ import annotations

import argparse
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
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
    journaled but still matches its tuned value, so it counts ok). The PC calls
    (tps) are mocked; only the failed-row handling is under test."""

    def setUp(self) -> None:
        self.saved = (tw.sw.open_state, tw.sw.save_state, tw.tps)
        tw.sw.open_state = lambda: {"id": "w", "card": "free"}
        tw.sw.save_state = lambda s: None
        self.env = {"PC_TUNING_ROOT": "T"}
        self.args = argparse.Namespace(tier=3, only="")

    def tearDown(self) -> None:
        tw.sw.open_state, tw.sw.save_state, tw.tps = self.saved

    def test_a_clean_revert_succeeds(self) -> None:
        tw.tps = lambda env, body, **kw: [{"key": "irq:card:policy", "action": "restored", "error": None}]
        tw.cmd_undo(self.env, self.args)   # no raise on a clean revert

    def test_a_failed_revert_row_raises(self) -> None:
        tw.tps = lambda env, body, **kw: [{"key": "irq:card:policy", "action": "failed", "error": "Access is denied"},
                                          {"key": "irq:nic:rss", "action": "restored", "error": None}]
        with self.assertRaisesRegex(tw.StepError, "revert item.*irq:card:policy.*Access is denied"):
            tw.cmd_undo(self.env, self.args)


class RebootPrepareTests(unittest.TestCase):
    """reboot-prepare prepares a reboot only over a cleanly preempted window
    (I1): unwind(bring_back_reaper=False) breaks at bring-back before the
    graceful-stop check, so a spike that did not stop still holds the card. A
    graceful reboot must never be prepared over it — reboot-prepare refuses and
    keeps the card free and the window open; the spike is never force-ended
    (I8). The PC calls (unwind, spike_running, tps) are mocked."""

    def setUp(self) -> None:
        self.saved = (tw.sw.open_state, tw.sw.save_state, tw.sw.spike_running, tw.sw.unwind, tw.tps)
        self.state = {"id": "w", "card": "free", "pref_original": 64, "pref_current": 64}
        tw.sw.open_state = lambda: self.state
        tw.sw.save_state = lambda s: None
        tw.sw.spike_running = lambda env, **kw: False
        tw.tps = lambda env, body, **kw: {"items": []} if "Get-IemTuningState" in body else "2026-01-01T00:00:00Z"
        self.env = {"PC_TUNING_ROOT": "T"}
        self.args = argparse.Namespace()

    def tearDown(self) -> None:
        tw.sw.open_state, tw.sw.save_state, tw.sw.spike_running, tw.sw.unwind, tw.tps = self.saved

    def test_a_clean_unwind_prepares_the_reboot(self) -> None:
        tw.sw.unwind = lambda env, state, running, bring_back_reaper=True: [{"stop-spike": True}, {"restore-buffer": {"ok": True}}]
        tw.cmd_reboot_prepare(self.env, self.args)
        self.assertEqual(self.state["card"], "rebooting")
        self.assertIn("reboot", self.state)

    def test_a_spike_that_did_not_stop_refuses_the_reboot(self) -> None:
        tw.sw.unwind = lambda env, state, running, bring_back_reaper=True: [{"stop-spike": False}, {"restore-buffer": {"ok": True}}]
        with self.assertRaisesRegex(tw.StepError, "did not stop"):
            tw.cmd_reboot_prepare(self.env, self.args)
        self.assertEqual(self.state["card"], "free")   # never entered the rebooting state
        self.assertNotIn("reboot", self.state)          # no reboot prepared over a held card


if __name__ == "__main__":
    unittest.main()
