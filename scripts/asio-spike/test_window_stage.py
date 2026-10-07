"""The S1a/S1c/golden window sessions import our PowerShell modules only from
the admin-only stage (#15, the review lane's findings of 2026-10-07): the
elevated ssh session reads each module from PC_ROOT\\bin once, checks it
against the fetched, attested bundle record on this box, stages it under
<elevated root>\\bootstrap-stage (elevated_ps) and imports only that copy."""
from __future__ import annotations

import contextlib
import hashlib
import io
import json
import os
import re
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "pc-tuning"))
sys.path.insert(0, str(HERE.parent / "golden"))
import golden_window as gw  # noqa: E402
import spike_window as sw  # noqa: E402
import tuning_window as tw  # noqa: E402

SHA = "a" * 40
SUMS = {n: hashlib.sha256(n.encode()).hexdigest() for n in sw.BUNDLE_FILES}
ENV = {"PC_ROOT": "R", "PC_ROOT_SCP": "/R", "PC_SSH": "u@h", "PC_TUNING_ROOT": "T", "PC_XPERF": "xperf.exe"}
STAGED = re.compile(r"Import-Module \(Join-Path \$iemStage '([A-Za-z]+\.psm1)'\)")


def bundle_record(test: unittest.TestCase, state: dict | None = None) -> dict[str, str]:
    """A temp window state naming SHA as the bundle the PC holds, and that
    bundle's record under RAW_DIR (SHA256SUMS and .source-sha, as fetch-bundle
    leaves them). Returns the env."""
    d = Path(tempfile.mkdtemp())
    saved = sw.STATE
    test.addCleanup(setattr, sw, "STATE", saved)
    sw.STATE = d / "spike-window.json"
    env = dict(ENV, RAW_DIR=str(d / "raw"))
    bundle = sw.bundle_dir(env, SHA)
    bundle.mkdir(parents=True)
    (bundle / "SHA256SUMS").write_text("".join(f"{h}  {n}\n" for n, h in sorted(SUMS.items())), encoding="utf-8")
    (bundle.parent / f"{SHA}.source-sha").write_text(SHA + "\n", encoding="utf-8")
    sw.STATE.write_text(json.dumps(state if state is not None else {"id": "w", "closed": False, "bundle_sha": SHA}),
                        encoding="utf-8")
    return env


class StageTests(unittest.TestCase):
    def only_staged(self, script: str, names: list[str]) -> None:
        """Every import in `script` is a stage copy, these in this order."""
        self.assertEqual(STAGED.findall(script), names, script)
        self.assertEqual(script.count("Import-Module"), len(names), script)

    def staged_from_bin(self, script: str, root: str, name: str, want: str | None = None) -> int:
        """`name` is read once from <root>\\bin, checked (by $iemSums or `want`),
        and written into the stage; returns where its stage copy is read back."""
        src = sw.ps_quote(f"{root}\\bin\\{name}")
        want = want or f"$iemSums['{name}']"
        self.assertEqual(script.count(src), 2, name)   # read once, named in the mismatch
        at = 0   # each step searched after the one before: this module's own, in this order
        for step in (f"$iemB = [IO.File]::ReadAllBytes({src})", f"if ($iemH -cne {want})",
                     f"$iemMod = Join-Path $iemStage '{name}'", "& $iemOnly $iemMod"):
            at = script.index(step, at)
        return at

    def test_every_window_session_imports_spikepc_only_from_the_stage(self) -> None:
        s = sw.ps_script("C:\\r", "Get-X", SUMS)
        self.only_staged(s, ["SpikePc.psm1"])
        golden = self.staged_from_bin(s, "C:\\r", "GoldenPc.psm1")   # SpikePc loads it from its own folder
        spike = self.staged_from_bin(s, "C:\\r", "SpikePc.psm1")
        self.assertLess(golden, spike)
        self.assertLess(spike, s.index("Import-Module (Join-Path $iemStage 'SpikePc.psm1') -Force"))
        self.assertLess(s.index("Import-Module"), s.index("$r = & { Get-X }"))
        table = s[s.index("$iemSums = @{"):]
        for name in ("GoldenPc.psm1", "SpikePc.psm1", "IemTuning.psm1", "IemMeasure.psm1"):
            self.assertIn(f"'{name}' = '{SUMS[name]}'", table[:table.index("}")])
        self.assertNotIn("asio_spike.exe", s)
        with self.assertRaisesRegex(sw.StepError, "SpikePc.psm1"):
            sw.ps_script("C:\\r", "Get-X", {k: v for k, v in SUMS.items() if k != "SpikePc.psm1"})

    def test_the_tuning_modules_are_staged_after_temp_and_only_iemmeasure_is_imported(self) -> None:
        for body in (sw.tuning_body(ENV, "Get-IemNow"), tw.analysis_step("R", "2026-01-01T00:00:00Z", "B")):
            self.only_staged(body, ["IemMeasure.psm1"])
            tuning = self.staged_from_bin(body, "R", "IemTuning.psm1")   # IemMeasure loads it from its own folder
            measure = self.staged_from_bin(body, "R", "IemMeasure.psm1")
            self.assertLess(body.index("$env:TEMP = $iemTemp"), tuning)
            self.assertLess(tuning, measure)
            self.assertIn("Import-Module (Join-Path $iemStage 'IemMeasure.psm1') -Force -Global", body)

    def test_the_trace_stop_stages_iemmeasure_alone_and_sets_up_no_temp(self) -> None:
        stop = sw.trace_stop_body(ENV, "R\\run")
        self.only_staged(stop, ["IemMeasure.psm1"])
        self.staged_from_bin(stop, "R", "IemMeasure.psm1")
        self.assertIn("Import-Module (Join-Path $iemStage 'IemMeasure.psm1') -ArgumentList 'stop-only' -Force -Global", stop)
        self.assertNotIn("IemTuning", stop)
        self.assertNotIn("TEMP", stop)

    def test_the_golden_session_imports_goldenpc_only_from_the_stage(self) -> None:
        want = hashlib.sha256((HERE.parent / "golden" / "GoldenPc.psm1").read_bytes()).hexdigest()
        s = gw.ps_script({"PC_ROOT": "R"}, "Get-X")
        self.only_staged(s, ["GoldenPc.psm1"])
        self.staged_from_bin(s, "R", "GoldenPc.psm1", want=f"'{want}'")
        self.assertLess(s.index("Import-Module"), s.index("$r = & { Get-X }"))


class BundleRecordTests(unittest.TestCase):
    """sw.ps takes the sums from the attested bundle the window's PC holds."""

    def setUp(self) -> None:
        saved = sw.guarded
        self.addCleanup(setattr, sw, "guarded", saved)
        self.sent: list[str] = []
        sw.guarded = lambda cmd, stdin, timeout, event: self.sent.append(stdin) or json.dumps({"ok": True, "r": 1})

    def test_ps_sends_the_sums_of_the_window_s_bundle_record(self) -> None:
        env = bundle_record(self)
        self.assertEqual(sw.ps(env, "Get-X"), 1)
        self.assertEqual(self.sent, [sw.ps_script("R", "Get-X", SUMS) + "\n"])

    def test_without_a_bundle_record_nothing_is_sent(self) -> None:
        env = bundle_record(self, {"id": "w", "closed": False})
        with self.assertRaisesRegex(sw.StepError, "setup --sha"):
            sw.ps(env, "Get-X")
        env = bundle_record(self)
        (sw.bundle_dir(env, SHA).parent / f"{SHA}.source-sha").write_text("b" * 40 + "\n", encoding="utf-8")
        with self.assertRaisesRegex(sw.StepError, "source-sha"):
            sw.ps(env, "Get-X")
        self.assertEqual(self.sent, [])

    def test_a_new_window_keeps_the_bundle_the_pc_holds(self) -> None:
        env = bundle_record(self, {"id": "old", "closed": True, "bundle_sha": SHA})
        with mock.patch.object(sw, "EVENT_NOW", sw.STATE.with_name("EVENT-NOW")), contextlib.redirect_stdout(io.StringIO()):
            sw.cmd_new(dict(env, PC_BUFFER_ORIGINAL="64"),
                       type("Args", (), {"signal": "owner, 13:07: event skončil", "dev_time": True})())
        self.assertEqual(sw.load_state()["bundle_sha"], SHA)

    def test_setup_records_the_bundle_before_its_pc_call(self) -> None:
        env = bundle_record(self, {"id": "w", "closed": False})
        bundle = sw.bundle_dir(env, SHA)
        lines = []
        for name in sw.BUNDLE_FILES:
            (bundle / name).write_bytes(name.encode())
            lines.append(f"{SUMS[name]}  {name}\n")
        (bundle / "SHA256SUMS").write_text("".join(lines), encoding="utf-8")
        seen: list[str | None] = []
        with mock.patch.object(sw, "scp"), mock.patch.object(sw, "EVENT_NOW", sw.STATE.with_name("EVENT-NOW")), \
                mock.patch.object(sw, "ps", lambda e, body, **kw: seen.append(sw.load_state().get("bundle_sha")) or []), \
                contextlib.redirect_stdout(io.StringIO()):
            sw.cmd_setup(env, type("Args", (), {"sha": SHA})())
        self.assertEqual(seen, [SHA])


class CiScriptTests(unittest.TestCase):
    """poll-script and analysis-script take the bundle's SHA256SUMS (the CI
    runner's bundle), as the dev box takes the fetched record's."""

    def test_the_ci_scripts_carry_the_bundle_s_sums(self) -> None:
        sums = Path(tempfile.mkdtemp()) / "SHA256SUMS"
        sums.write_text("".join(f"{h}  {n}\n" for n, h in sorted(SUMS.items())), encoding="utf-8")
        for argv, body in ((["poll-script", "--governor", "G"], tw.poll_body("G", 0, 0)),
                           (["analysis-script", "--since", "S"], tw.analysis_step("C:\\r", "S", tw.ANALYSIS_PROBE))):
            out = io.StringIO()
            with mock.patch.dict(os.environ, {"SPIKE_ENV": "/nonexistent/asio-spike.env"}), contextlib.redirect_stdout(out):
                code = tw.main([*argv, "--root", "C:\\r", "--sums", str(sums)])
            self.assertEqual(code, 0)
            self.assertEqual(out.getvalue(), sw.ps_script("C:\\r", body, SUMS) + "\n")
        # The CI runner asserts the four modules' paths are all in the stage (#15).
        self.assertIn("Get-Module -Name SpikePc, GoldenPc, IemMeasure, IemTuning", tw.ANALYSIS_PROBE)


if __name__ == "__main__":
    unittest.main()
