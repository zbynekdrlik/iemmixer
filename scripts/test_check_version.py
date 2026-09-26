"""Tests for scripts/check_version.py."""
from __future__ import annotations

import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_version as cv  # noqa: E402

CRATE = '[package]\nname = "{name}"\nversion.workspace = true\n'


def write_tree(root: Path, version: str, lock_versions: dict[str, str] | None = None, tauri_version: bool = False) -> None:
    (root / "Cargo.toml").write_text(f'[workspace]\nmembers = []\n\n[workspace.package]\nversion = "{version}"\n', encoding="utf-8")
    for name in cv.CRATES:
        (root / "crates" / name).mkdir(parents=True, exist_ok=True)
        (root / "crates" / name / "Cargo.toml").write_text(CRATE.format(name=name), encoding="utf-8")
    tauri = {"productName": "iemmixer"}
    if tauri_version:
        tauri["version"] = version
    (root / "crates" / "iem-tray" / "tauri.conf.json").write_text(json.dumps(tauri), encoding="utf-8")
    locks = lock_versions or {name: version for name in cv.CRATES}
    lock = "version = 4\n" + "".join(f'\n[[package]]\nname = "{n}"\nversion = "{v}"\n' for n, v in locks.items())
    (root / "Cargo.lock").write_text(lock, encoding="utf-8")


class CompareTests(unittest.TestCase):
    def test_semver_precedence(self) -> None:
        self.assertGreater(cv.compare("2.0.0-dev.1", "2.0.0-dev.0"), 0)
        self.assertGreater(cv.compare("2.0.0-dev.10", "2.0.0-dev.9"), 0)
        self.assertGreater(cv.compare("2.0.0", "2.0.0-dev.5"), 0)
        self.assertGreater(cv.compare("2.0.1-dev.0", "2.0.0"), 0)
        self.assertGreater(cv.compare("2.0.0-dev.1.1", "2.0.0-dev.1"), 0)
        self.assertGreater(cv.compare("2.0.0-dev.a", "2.0.0-dev.1"), 0)
        self.assertEqual(cv.compare("2.0.0-dev.3", "2.0.0-dev.3"), 0)
        self.assertLess(cv.compare("1.9.9", "2.0.0-dev.0"), 0)

    def test_rejects_non_semver(self) -> None:
        with self.assertRaises(ValueError):
            cv.parse("2.0")


class ConsistencyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp())

    def tearDown(self) -> None:
        shutil.rmtree(self.root)

    def test_consistent_tree_passes(self) -> None:
        write_tree(self.root, "2.0.0-dev.1")
        self.assertEqual(cv.consistency_errors(self.root), [])

    def test_crate_with_its_own_version_fails(self) -> None:
        write_tree(self.root, "2.0.0-dev.1")
        (self.root / "crates" / "iem-ui" / "Cargo.toml").write_text('[package]\nname = "iem-ui"\nversion = "1.0.0"\n', encoding="utf-8")
        self.assertTrue(any("iem-ui" in e for e in cv.consistency_errors(self.root)))

    def test_tauri_version_fails(self) -> None:
        write_tree(self.root, "2.0.0-dev.1", tauri_version=True)
        self.assertTrue(any("tauri.conf.json" in e for e in cv.consistency_errors(self.root)))

    def test_stale_lockfile_fails(self) -> None:
        write_tree(self.root, "2.0.0-dev.2", lock_versions={n: "2.0.0-dev.1" for n in cv.CRATES})
        self.assertEqual(len(cv.consistency_errors(self.root)), len(cv.CRATES))


class BaseRefTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp())
        for args in (["init", "-q", "-b", "main"], ["config", "user.email", "t@example.org"], ["config", "user.name", "t"],
                     ["config", "commit.gpgsign", "false"]):
            subprocess.run(["git", "-C", str(self.root), *args], check=True)
        write_tree(self.root, "2.0.0-dev.0")
        subprocess.run(["git", "-C", str(self.root), "add", "-A"], check=True)
        subprocess.run(["git", "-C", str(self.root), "commit", "-qm", "base"], check=True)

    def tearDown(self) -> None:
        shutil.rmtree(self.root)

    def test_bumped_head_passes(self) -> None:
        write_tree(self.root, "2.0.0-dev.1")
        self.assertEqual(cv.main(["--root", str(self.root), "--base-ref", "main"]), 0)

    def test_unbumped_head_fails(self) -> None:
        self.assertEqual(cv.main(["--root", str(self.root), "--base-ref", "main"]), 1)


class WorkspaceMembersTests(unittest.TestCase):
    def test_crates_list_matches_the_workspace_members(self) -> None:
        import tomllib
        root = Path(__file__).resolve().parent.parent
        members = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["members"]
        self.assertEqual(sorted(m.split("/")[-1] for m in members), sorted(cv.CRATES))


if __name__ == "__main__":
    unittest.main()
