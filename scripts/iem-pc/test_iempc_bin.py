"""Tests for scripts/iem-pc/iempc_bin.py (#15, ROZHODNUTE of 2026-10-07: the
elevated ssh session runs only admin-only copies of our executables). They
reuse test_iempc's fakes (FakePc stands in for ssh and scp, FakeGh for
GitHub); every value is synthetic."""
from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc_bin as ib  # noqa: E402
from test_iempc import ENV, SHA, Base, ip, sha256  # noqa: E402
from test_iempc_tuning import TuningBase  # noqa: E402

IEMMODE = sha256(b"synthetic iemmode.exe")
UPLOAD = f"X:\\root\\incoming\\iemmode-{SHA}.exe"
ROOT = "(Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'iemmixer')"
ACTIVATED = (0, json.dumps({"ok": True, "mode": "dev", "alarms": [], "detail": f"activated {SHA}"}))
STATUS = (0, json.dumps({"ok": True, "mode": "dev", "alarms": [], "detail": "mode dev", "guard_build": SHA}))


def in_order(test: unittest.TestCase, text: str, steps: list[str]) -> None:
    at = 0
    for step in steps:
        test.assertIn(step, text[at:], step)
        at = text.index(step, at)


class PickTests(Base):
    """Every iemmode call runs %ProgramData%\\iemmixer\\bin\\iemmode.exe when the
    elevated root, bin and the file read back admin-only; else PC_BIN's, with
    one note per command."""

    def setUp(self) -> None:
        super().setUp()
        self.pc.replies[("activate", SHA)] = ACTIVATED
        self.pc.replies[("status",)] = STATUS

    def test_iemmode_runs_the_admin_only_copy_when_it_reads_back(self) -> None:
        ip.iemmode(dict(ENV), ["status"], 10, "abandon")
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "abandon")])
        in_order(self, self.pc.native_scripts[0], [
            "$x = 'X:\\root\\bin\\iemmode.exe' ; $a = @('status') ; ",
            f"$iemE = Join-Path {ROOT} 'bin\\iemmode.exe'",
            "foreach ($p in @((Split-Path -Parent (Split-Path -Parent $iemE)), (Split-Path -Parent $iemE), $iemE)) { & $iemOnly $p }",
            "$iemUse = $iemE", "catch { $iemNote = \"$_\" }", "if ($iemUse) { $x = $iemUse } ; $r = @(& $x @a 2>&1)",
            "note = $iemNote"])

    def test_a_copy_that_does_not_read_back_runs_pc_bin_with_one_note_per_command(self) -> None:
        self.pc.bin_note = "X:\\bin\\iemmode.exe may be changed by S-1-5-21-1-2-3-1001: refused"
        for _ in range(2):
            code, _, err = self.run_main("activate", "--sha", SHA)
            self.assertEqual(code, 0, err)
            self.assertEqual(err.count("may be changed by S-1-5-21-1-2-3-1001"), 1, err)
            self.assertIn("iemmode ran from PC_BIN", err)
        self.assertGreater(len(self.pc.calls), 2)


class InstallTests(Base):
    """activate and tuning-install put the attested iemmode.exe into the
    admin-only bin; activate --offline runs the guard from the stage."""

    def setUp(self) -> None:
        super().setUp()
        self.pc.replies[("activate", SHA)] = ACTIVATED
        self.pc.replies[("status",)] = STATUS
        self.fetched()

    def test_activate_puts_the_attested_iemmode_into_the_admin_only_bin(self) -> None:
        code, docs, err = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.assertIn((str(ip.bundle_dir(SHA) / "iemmode.exe"), f"tester@pc.test:/X:/root/incoming/iemmode-{SHA}.exe",
                       "finish"), self.pc.scps)
        script, mode = next(m for m in self.pc.modules if "$iemDst" in m[0])
        self.assertEqual(mode, "finish")
        in_order(self, script, [
            f"$iemB = [IO.File]::ReadAllBytes('{UPLOAD}')", f"if ($iemH -cne '{IEMMODE}')",
            "$iemStage = Join-Path $iemRoot 'bootstrap-stage'", "$iemMod = Join-Path $iemStage 'iemmode.exe'",
            "& $iemOnly $iemMod", "$iemBin = Join-Path $iemRoot 'bin' ; & $iemDir $iemBin",
            "$iemDst = Join-Path $iemBin 'iemmode.exe'", "[IO.File]::Delete($iemDst) ; [IO.File]::Move($iemMod, $iemDst)",
            "& $iemOnly $iemDst", f"-cne '{IEMMODE}') {{ throw ('sha256 mismatch after the copy: ' + $iemDst) }}"])
        self.assertEqual(script.count(f"'{UPLOAD}'"), 2)   # read once, named in the mismatch
        handover = [next(iter(d)) for d in docs].index("handover")
        self.assertEqual(docs[handover + 1], {"elevated_bin": SHA, "path": ib.SHOWN, "sha256": IEMMODE})

    def test_a_read_back_that_differs_is_reported_and_the_activation_counts(self) -> None:
        self.pc.texts["$iemDst"] = "0" * 64
        code, docs, err = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, 0, err)
        failed = next(d for d in docs if "elevated_bin" in d)
        self.assertEqual((failed["elevated_bin"], failed["sha"]), ("failed", SHA))
        self.assertIn("the PC read back iemmode.exe 0000", failed["error"])
        self.assertIn("WARNING: the admin-only iemmode.exe was not installed", err)
        self.assertIn(f"iempc activate --sha {SHA}", err)

    def test_a_new_flag_during_the_install_runs_the_event_path(self) -> None:
        def flag_then_hash():
            self.flag()
            return IEMMODE

        self.pc.texts["$iemDst"] = flag_then_hash
        code, _, _ = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual([c[1][0] for c in self.pc.calls], ["activate", "status", "event"])


class TuningInstallBinTests(TuningBase):
    def test_tuning_install_puts_iemmode_into_the_admin_only_bin_too(self) -> None:
        self.fetched()
        code, docs, err = self.run_main("tuning-install", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[-1], {"elevated_bin": SHA, "path": ib.SHOWN, "sha256": IEMMODE})

    def test_a_failed_bin_install_fails_tuning_install(self) -> None:
        self.fetched()
        self.pc.texts["$iemDst"] = "ok"
        code, _, err = self.run_main("tuning-install", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn("the PC read back iemmode.exe 'ok'", err)


if __name__ == "__main__":
    unittest.main()
