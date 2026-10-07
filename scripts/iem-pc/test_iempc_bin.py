"""Tests for scripts/iem-pc/iempc_bin.py (#15, ROZHODNUTE of 2026-10-07: the
elevated ssh session runs only admin-only copies of our executables). They
reuse test_iempc's fakes (FakePc stands in for ssh and scp, FakeGh for
GitHub); every value is synthetic."""
from __future__ import annotations

import contextlib
import io
import json
import os
import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc_bin as ib  # noqa: E402
from test_iempc import ENV, SHA, SHA2, Base, ip, sha256  # noqa: E402
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
    elevated root, bin and the file read back admin-only and the file is the
    build this box installed there (its record); else PC_BIN's, with one note
    per command."""

    def setUp(self) -> None:
        super().setUp()
        self.pc.replies[("activate", SHA)] = ACTIVATED
        self.pc.replies[("status",)] = STATUS
        self.fetched()

    def record(self) -> None:
        ip.write_json(ip.state_dir() / ib.RECORD, {"sha": SHA, "sha256": IEMMODE})

    def seen(self, build: str) -> None:
        """The guard answers a status read naming `build` (every reply carries
        guard_build); the read itself is forgotten, as a new command would."""
        self.pc.replies[("status",)] = (0, json.dumps({"ok": True, "mode": "dev", "alarms": [], "guard_build": build}))
        with contextlib.redirect_stderr(io.StringIO()):
            ip.iemmode(dict(ENV), ["status"], 10, "abandon")
        self.pc.replies[("status",)] = STATUS
        self.pc.calls.clear()
        self.pc.native_scripts.clear()
        ib.NOTED.clear()

    def picks(self, *replies) -> list[bool]:
        """One `iemmode status` per reply; whether each ran the admin-only copy."""
        for reply in replies:
            self.pc.replies[("status",)] = reply
            with contextlib.redirect_stderr(io.StringIO()):
                ip.iemmode(dict(ENV), ["status"], 10, "abandon")
        return ["$iemUse" in s for s in self.pc.native_scripts]

    def test_iemmode_runs_the_admin_only_copy_of_the_installed_build(self) -> None:
        self.record()
        self.seen(SHA)   # the guard runs that build (#15, the last lane, item 4)
        ip.iemmode(dict(ENV), ["status"], 10, "abandon")
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "abandon")])
        script = self.pc.native_scripts[0]
        in_order(self, script, [
            "$x = 'X:\\root\\bin\\iemmode.exe' ; $a = @('status') ; ",
            f"$iemE = Join-Path {ROOT} 'bin\\iemmode.exe'",
            "foreach ($p in @((Split-Path -Parent (Split-Path -Parent $iemE)), (Split-Path -Parent $iemE), $iemE)) { & $iemOnly $p }",
            f"(Get-FileHash -LiteralPath $iemE -Algorithm SHA256).Hash.ToLowerInvariant() -cne '{IEMMODE}'",
            "$iemUse = $iemE", "catch { $iemNote = \"$_\" }", "if ($iemUse) { $x = $iemUse } ; $r = @(& $x @a 2>&1)",
            "note = $iemNote"])
        for write in ("& $iemDir", "Delete(", "WriteAllBytes", "Move("):   # the pick on the event path only reads
            self.assertNotIn(write, script)

    def test_without_an_installed_build_on_record_pc_bin_runs_with_a_note(self) -> None:
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            ip.iemmode(dict(ENV), ["status"], 10, "abandon")
        self.assertNotIn("$iemUse", self.pc.native_scripts[0])
        self.assertIn("iemmode ran from PC_BIN", err.getvalue())
        self.assertIn("no admin-only copy is recorded on this box", err.getvalue())

    def test_a_copy_that_does_not_read_back_runs_pc_bin_with_one_note_per_command(self) -> None:
        self.record()
        self.seen(SHA)
        self.pc.bin_note = "X:\\bin\\iemmode.exe may be changed by S-1-5-21-1-2-3-1001: refused"
        for _ in range(2):
            code, _, err = self.run_main("activate", "--sha", SHA)
            self.assertEqual(code, 0, err)
            self.assertEqual(err.count("may be changed by S-1-5-21-1-2-3-1001"), 1, err)
            self.assertIn("iemmode ran from PC_BIN", err)
        self.assertGreater(len(self.pc.calls), 2)

    # #15, the last lane, item 4: the admin-only copy runs only while the guard last
    # seen runs its recorded build. A guard of another build (a newer one HIL v1
    # activated) or one this box does not know gets PC_BIN's iemmode with the note.
    def test_a_guard_running_another_build_gets_pc_bin_with_a_note(self) -> None:
        self.record()
        self.seen(SHA2)
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            ip.iemmode(dict(ENV), ["status"], 10, "abandon")
        self.assertNotIn("$iemUse", self.pc.native_scripts[0])
        self.assertIn("iemmode ran from PC_BIN", err.getvalue())
        self.assertIn(f"build {SHA}", err.getvalue())
        self.assertIn(f"runs {SHA2}", err.getvalue())

    def test_a_guard_whose_build_this_box_does_not_know_gets_pc_bin_with_a_note(self) -> None:
        self.record()
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            ip.iemmode(dict(ENV), ["status"], 10, "abandon")
        self.assertNotIn("$iemUse", self.pc.native_scripts[0])
        self.assertIn("iemmode ran from PC_BIN", err.getvalue())
        self.assertIn("not known on this box", err.getvalue())

    def test_each_reply_s_guard_build_decides_the_next_pick(self) -> None:
        self.record()
        self.seen(SHA)
        other = (0, json.dumps({"ok": True, "mode": "dev", "alarms": [], "guard_build": SHA2}))
        # A reply without guard_build (iemmode's own, --direct) leaves what was seen.
        direct = (0, json.dumps({"ok": True, "mode": "event", "alarms": []}))
        self.assertEqual(self.picks(other, STATUS, direct, STATUS), [True, False, True, True])

    def test_a_hil_dispatch_forgets_the_guard_s_build(self) -> None:
        # The HIL run activates the SHA it was dispatched for (hil-v1.ps1): the guard's
        # build is not known again until a reply names it.
        self.record()
        self.seen(SHA)
        code, _, err = self.run_main("dispatch-hil", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.pc.native_scripts.clear()
        self.assertEqual(self.picks(STATUS, STATUS), [False, True])

    # The lane's review, finding 3: until the HIL run activates its SHA the guard runs
    # the build before, and its replies must not bring the older copy back.
    def test_a_hil_dispatch_waits_for_a_reply_naming_its_build(self) -> None:
        self.record()
        self.seen(SHA2)
        code, _, err = self.run_main("dispatch-hil", "--sha", SHA)
        self.assertEqual(code, 0, err)
        note = io.StringIO()
        with contextlib.redirect_stderr(note):
            ip.iemmode(dict(ENV), ["status"], 10, "abandon")
        self.assertIn(f"HIL run dispatched for {SHA}", note.getvalue())

    def test_replies_of_the_guard_before_the_hil_s_activation_are_not_kept(self) -> None:
        self.record()                     # the admin-only copy is SHA
        self.seen(SHA)
        ib.hil_dispatched(ip, SHA2)       # HIL v1 for SHA2, the guard still runs SHA
        other = (0, json.dumps({"ok": True, "mode": "dev", "alarms": [], "guard_build": SHA2}))
        self.assertEqual(self.picks(STATUS, other, STATUS, STATUS), [False, False, False, True])

    def test_an_activate_ends_the_wait_for_a_hil_build(self) -> None:
        self.record()
        ib.hil_dispatched(ip, SHA2)
        code, _, err = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.pc.native_scripts.clear()
        self.assertEqual(self.picks(STATUS), [True])

    # The lane's review, findings 5 to 7.
    def test_an_unreadable_record_of_the_build_is_a_note_never_a_stop(self) -> None:
        self.record()
        self.seen(SHA)
        path = ip.state_dir() / ib.SEEN
        os.chmod(path, 0)
        self.addCleanup(os.chmod, path, 0o600)
        self.assertRaises(PermissionError, path.read_text)   # the premise (not root)
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            ip.iemmode(dict(ENV), ["status"], 10, "abandon")   # PC_BIN's, and its reply is kept
        self.assertIn("cannot be read", err.getvalue())
        self.assertEqual(self.picks(STATUS), [False, True])   # that reply wrote the record again

    def test_a_record_that_cannot_be_written_is_a_warning(self) -> None:
        self.record()
        (ip.state_dir() / ib.SEEN).mkdir()
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            ip.iemmode(dict(ENV), ["status"], 10, "abandon")
        self.assertIn("WARNING: the guard's build", err.getvalue())
        self.assertIn("was not recorded", err.getvalue())

    def test_only_a_build_name_the_guard_makes_is_kept(self) -> None:
        self.record()
        self.seen(SHA)
        odd = [(0, json.dumps({"ok": True, "mode": "dev", "alarms": [], "guard_build": b}))
               for b in (123, "", "A" * 40, "../" + SHA, SHA + "\n", "x" * 101)]
        local = (0, json.dumps({"ok": True, "mode": "dev", "alarms": [], "guard_build": "local"}))
        self.assertEqual(self.picks(*odd, local, STATUS), [True] * 7 + [False])

    def test_the_build_is_written_only_when_it_changes(self) -> None:
        self.record()
        stamps = iter(["t1", "t2", "t3"])
        with mock.patch.object(ip, "now_iso", lambda: next(stamps)):
            self.picks(STATUS, STATUS, STATUS)
        self.assertEqual(ip.read_json(ip.state_dir() / ib.SEEN, None), {"build": SHA, "at": "t1"})


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
        self.assertEqual(ip.read_json(ip.state_dir() / ib.RECORD, None), {"sha": SHA, "sha256": IEMMODE})
        script, mode = next(m for m in self.pc.modules if "$iemDst" in m[0])
        self.assertEqual(mode, "finish")
        in_order(self, script, [
            f"$iemB = [IO.File]::ReadAllBytes('{UPLOAD}')", f"if ($iemH -cne '{IEMMODE}')",
            "$iemStage = Join-Path $iemRoot 'bootstrap-stage'", "$iemMod = Join-Path $iemStage 'iemmode.exe'",
            "& $iemOnly $iemMod", "$iemBin = Join-Path $iemRoot 'bin' ; & $iemDir $iemBin",
            "$iemDst = Join-Path $iemBin 'iemmode.exe'", "[IO.File]::Delete($iemDst) ; [IO.File]::Move($iemMod, $iemDst)",
            "& $iemOnly $iemDst", f"-cne '{IEMMODE}') {{ throw ('sha256 mismatch after the copy: ' + $iemDst) }}"])
        self.assertEqual(script.count(f"'{UPLOAD}'"), 2)   # read once, named in the mismatch
        # Right after `iemmode activate` put the new bins in place, before the hand-over's reads (#15 review).
        self.assertEqual([next(iter(d)) for d in docs][:3], ["iemmode", "elevated_bin", "handover"])
        self.assertEqual(docs[1], {"elevated_bin": SHA, "path": ib.SHOWN, "sha256": IEMMODE})

    def test_a_read_back_that_differs_is_reported_and_the_activation_counts(self) -> None:
        ip.write_json(ip.state_dir() / ib.RECORD, {"sha": "b" * 40, "sha256": "1" * 64})   # an earlier install
        self.pc.texts["$iemDst"] = "0" * 64
        code, docs, err = self.run_main("activate", "--sha", SHA)
        self.assertIsNone(ip.read_json(ip.state_dir() / ib.RECORD, None))   # no copy on record: PC_BIN runs
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
        self.assertEqual([c[1][0] for c in self.pc.calls], ["activate", "event"])

    def test_an_install_that_outlives_its_bound_is_named_as_still_running(self) -> None:
        def still_running():
            raise ip.StillRunning("ssh still running after 540 s (bounded on the PC; check 'iempc status', never force-end)")

        self.pc.texts["$iemDst"] = still_running
        code, docs, err = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.assertEqual(next(d for d in docs if "elevated_bin" in d)["elevated_bin"], "still-running")
        self.assertIn("the install may still run on the PC", err)
        self.assertNotIn("was not installed", err)
        self.assertIsNone(ip.read_json(ip.state_dir() / ib.RECORD, None))


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
