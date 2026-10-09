"""HIL v2 (#10, plan Task 31) from the dev box's side: IemHil.psm1 rides in the
bundle next to hil-v1.ps1 (not required: older bundles still install), the
windows job runs its self-test, the test alarm text it acknowledges is the
guard's own, and the module carries no site value (P6). The checks themselves
run on Windows PowerShell 5.1 in Test-IemHil.ps1."""
from __future__ import annotations

import re
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc as ip  # noqa: E402

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
CI = ROOT / ".github" / "workflows" / "ci.yml"


def job(name: str, following: str) -> str:
    ci = CI.read_text(encoding="utf-8")
    return ci[ci.index(f"\n  {name}:\n"):ci.index(f"\n  {following}:\n")]


class HilV2Tests(unittest.TestCase):
    def test_the_bundle_job_ships_the_module_beside_the_script(self) -> None:
        self.assertRegex(job("bundle", "attest"),
                         r"Copy-Item -LiteralPath [^\n]*scripts/iem-pc/hil-v1\.ps1[^\n]*scripts/iem-pc/IemHil\.psm1")

    def test_the_module_is_not_required_so_older_bundles_still_install(self) -> None:
        self.assertNotIn("IemHil.psm1", ip.BUNDLE_REQUIRED)
        bundle_rs = (ROOT / "crates" / "iem-guard" / "src" / "bundle.rs").read_text(encoding="utf-8")
        required = bundle_rs[bundle_rs.index("pub const REQUIRED"):]
        self.assertNotIn("IemHil.psm1", required[:required.index("];")])

    def test_the_windows_job_runs_the_self_test(self) -> None:
        self.assertIn("-File scripts/iem-pc/Test-IemHil.ps1", job("windows", "bundle"))

    def test_the_script_imports_the_module_from_its_own_folder(self) -> None:
        text = (HERE / "hil-v1.ps1").read_text(encoding="ascii")
        self.assertIn("Import-Module (Join-Path $PSScriptRoot 'IemHil.psm1')", text)

    def test_the_test_alarm_text_is_the_guards(self) -> None:
        """alarm-ack acknowledges only this exact text (Review Focus 4): the
        module's constant must be the string the guard's alarm_test raises."""
        module = (HERE / "IemHil.psm1").read_text(encoding="ascii")
        m = re.search(r"^\$script:HilTestAlarmText = '([^']+)'$", module, re.MULTILINE)
        self.assertIsNotNone(m)
        daemon = (ROOT / "crates" / "iem-guard" / "src" / "daemon.rs").read_text(encoding="utf-8")
        self.assertIn(f'g.raise(None, "{m.group(1)}", false)', daemon)

    def test_the_files_are_ascii_and_hold_no_site_value(self) -> None:
        for name in ("IemHil.psm1", "Test-IemHil.ps1", "hil-v1.ps1"):
            text = (HERE / name).read_text(encoding="ascii")
            # Only loopback and the public placeholder LAN address (public-repo-hygiene.md).
            self.assertIsNone(re.search(r"\b(?!127\.0\.0\.1\b|10\.0\.0\.10\b)\d{1,3}(?:\.\d{1,3}){3}\b", text), f"{name}: an IPv4 address")
            self.assertIsNone(re.search(r"\b[A-Za-z]:\\", text), f"{name}: a Windows drive path")
            self.assertIsNone(re.search(r"https?://(?!127\.0\.0\.1)[A-Za-z0-9]", text), f"{name}: a host")


if __name__ == "__main__":
    unittest.main()
