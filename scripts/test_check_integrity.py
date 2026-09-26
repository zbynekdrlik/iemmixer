"""Tests for scripts/check_integrity.py."""
from __future__ import annotations

import shutil
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_integrity as ci  # noqa: E402

PINNED = "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n"


class IntegrityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp())
        self.put("crates/a/src/lib.rs", "#[test]\nfn ok() {}\n")
        self.put("e2e/tests/a.spec.ts", 'test("ok", async () => {});\n')
        self.put(".github/workflows/ci.yml", "jobs:\n  a:\n    steps:\n" + PINNED)

    def tearDown(self) -> None:
        shutil.rmtree(self.root)

    def put(self, rel: str, text: str) -> None:
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def test_clean_tree(self) -> None:
        self.assertEqual(ci.violations(self.root), [])

    def test_ignored_rust_test(self) -> None:
        self.put("crates/a/src/lib.rs", "#[test]\n#[ignore]\nfn skipped() {}\n")
        self.assertEqual(len(ci.violations(self.root)), 1)

    def test_skipped_or_focused_e2e(self) -> None:
        for body in ('test.skip("x", async () => {});', 'test.only("x", async () => {});',
                     'test.describe.skip("x", () => {});', 'test.fixme("x", async () => {});'):
            self.put("e2e/tests/a.spec.ts", body + "\n")
            self.assertEqual(len(ci.violations(self.root)), 1, body)

    def test_forbidden_workflow_constructs(self) -> None:
        for line in ("    continue-on-error: true\n", "    runs-on: [self-hosted, x]\n", "on: pull_request_target\n"):
            self.put(".github/workflows/ci.yml", "jobs:\n" + line + PINNED)
            self.assertEqual(len(ci.violations(self.root)), 1, line)

    def test_unpinned_action(self) -> None:
        self.put(".github/workflows/ci.yml", "jobs:\n  a:\n    steps:\n      - uses: actions/checkout@v7\n")
        self.assertEqual(len(ci.violations(self.root)), 1)

    def test_force_kill_command(self) -> None:
        self.put("scripts/stop.ps1", "taskkill /F /IM engine.exe\n")
        self.assertEqual(len(ci.violations(self.root)), 1)

    def test_force_kill_in_a_powershell_module_is_found(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "scripts" / "golden").mkdir(parents=True)
            (root / "scripts" / "golden" / "M.psm1").write_text("function X { Stop-Process -Id 1 }\n", encoding="utf-8")
            self.assertEqual(ci.violations(root), ["scripts/golden/M.psm1:1: force-kill command (program spec I8)"])

if __name__ == "__main__":
    unittest.main()
