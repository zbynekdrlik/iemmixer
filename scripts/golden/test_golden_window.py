"""Tests for scripts/golden/golden_window.py (pure parts; ssh is the PC)."""
from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import golden_window as gw  # noqa: E402

FULL = "\n".join(f"{k}=v" for k in gw.REQUIRED if k != "PC_DUMMY_MODE") + "\nPC_DUMMY_MODE=4\n"


class EnvTests(unittest.TestCase):
    def write(self, text: str) -> Path:
        d = tempfile.mkdtemp()
        p = Path(d) / "golden.env"
        p.write_text(text, encoding="utf-8")
        return p

    def test_complete_env_loads_and_quotes_are_stripped(self) -> None:
        env = gw.load_env(self.write(FULL.replace("PC_SSH=v", 'PC_SSH="u@h"') + "# comment\n"))
        self.assertEqual(env["PC_SSH"], "u@h")

    def test_missing_keys_are_named(self) -> None:
        with self.assertRaisesRegex(gw.StepError, "missing PC_SSH"):
            gw.load_env(self.write(FULL.replace("PC_SSH=v\n", "")))

    def test_dummy_mode_three_is_refused(self) -> None:
        with self.assertRaisesRegex(gw.StepError, "ASIO"):
            gw.load_env(self.write(FULL.replace("PC_DUMMY_MODE=4", "PC_DUMMY_MODE=3")))


class OrderTests(unittest.TestCase):
    def test_steps_run_in_order(self) -> None:
        state = {"done": ["preflight"]}
        gw.check_order(state, "interlock")
        with self.assertRaisesRegex(gw.StepError, "next step is 'interlock'"):
            gw.check_order(state, "backup")

    def test_signal_must_quote_the_owner(self) -> None:
        gw.check_signal("owner 2026-09-27 21:05: event skončil")
        with self.assertRaises(gw.StepError):
            gw.check_signal("I think the event is over")


class UndoPlanTests(unittest.TestCase):
    def test_undo_plan_before_anything_changed_is_empty(self) -> None:
        self.assertEqual(gw.undo_plan(["preflight", "interlock"], ["preflight", "interlock"], render_running=False), [])

    def test_undo_plan_mid_render_stops_restores_and_brings_back(self) -> None:
        done = ["preflight", "interlock", "save-quit", "app-stopped", "backup", "stage", "seed-res"]
        self.assertEqual(gw.undo_plan(done, done + ["render"], render_running=True), ["stop-render", "verify-restore", "bring-back"])

    def test_undo_plan_after_a_failed_quit_still_brings_back(self) -> None:
        self.assertEqual(gw.undo_plan(["preflight", "interlock"], ["preflight", "interlock", "save-quit"], render_running=False), ["bring-back"])

    def test_undo_plan_after_restore_only_brings_back(self) -> None:
        done = list(gw.STEPS[:-1])
        self.assertEqual(gw.undo_plan(done, done, render_running=False), ["bring-back"])


class ParseTests(unittest.TestCase):
    def test_holders_parse_tasklist_csv(self) -> None:
        self.assertEqual(gw.parse_holders('"reaper.exe","6496","x.dll"\r\n'), [("reaper.exe", 6496)])
        self.assertEqual(gw.parse_holders("INFO: No tasks are running which match the specified criteria.\r\n"), [])

    def test_meter_peaks_skip_the_master(self) -> None:
        text = "NTRACK\t2\nTRACK\t0\tMASTER\t0\t1\t0\t-100\t-100\nTRACK\t1\tin\t0\t1\t0\t-620\t-620\nTRACK\t2\tbus\t0\t1\t0\t-1500\t-1500\n"
        self.assertEqual(gw.parse_meter_peaks(text), {1: -620, 2: -1500})

    def test_interlock_verdict(self) -> None:
        self.assertEqual(gw.interlock_hits([{1: -620, 2: -1500}, {1: -480}]), {1: -480})
        self.assertEqual(gw.interlock_hits([{1: -620}]), {})


class DevWindowTests(unittest.TestCase):
    """This window keeps the predecessor app running (its only graceful exit is
    the tray menu); REAPER was already quit before the window."""

    def test_app_step_is_skipped_when_the_app_stays(self) -> None:
        self.assertIn("skipped", gw.step_app_stopped({}, {"keep_app": True}, None))

    def test_bring_back_never_restarts_a_kept_app_or_a_quit_reaper(self) -> None:
        self.assertEqual(gw.bring_back_wants({"pre": {"reaper": False, "app": True}, "keep_app": True}), (False, False))
        self.assertEqual(gw.bring_back_wants({"pre": {"reaper": True, "app": True}}), (True, True))
        self.assertEqual(gw.bring_back_wants({"pre": {}}), (True, True))

    def test_source_sha_is_a_sidecar_outside_the_bundle(self) -> None:
        self.assertEqual(gw.source_sha_path(Path("/raw/bundles/abc")), Path("/raw/bundles/abc.source-sha"))

    def test_volatile_trees_are_optional_and_split(self) -> None:
        self.assertEqual(gw.volatile_trees({}), [])
        self.assertEqual(gw.volatile_trees({"PC_VOLATILE_TREES": "app-roaming; app-local;"}), ["app-roaming", "app-local"])

    def test_vst_path_is_the_plugins_folder_next_to_the_exe(self) -> None:
        self.assertEqual(gw.vst_path("C:\\Program Files\\REAPER (x64)\\reaper.exe"), "C:\\Program Files\\REAPER (x64)\\Plugins\\FX")

if __name__ == "__main__":
    unittest.main()
