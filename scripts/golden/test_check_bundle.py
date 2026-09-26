"""Tests for scripts/golden/check_bundle.py."""
from __future__ import annotations

import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_bundle as cb  # noqa: E402

GOOD_RPP = """<REAPER_PROJECT 0.1 "7.65/win64" 0 0
  RENDER_FILE "@@OUT@@\\p"
  <TRACK {X}
    <FXCHAIN
      <JS utility/volume_pan ""
        6 0 0
      >
    >
    <ITEM
      <SOURCE WAVE
        FILE "@@JOB@@\\stimuli\\imp-dm-96000.wav"
      >
    >
  >
>
"""


def make(root: Path, rpp: str = GOOD_RPP, extra: bool = False, tamper: bool = False) -> None:
    (root / "projects").mkdir(parents=True)
    (root / "stimuli").mkdir()
    files = {"projects/p.rpp": rpp.encode(), "stimuli/imp-dm-96000.wav": b"RIFF-test"}
    for rel, data in files.items():
        (root / rel).write_bytes(data)
    manifest = {"schema": 1, "projects": [{"file": "projects/p.rpp", "id": "p"}],
                "files": [{"path": p, "sha256": hashlib.sha256(d).hexdigest(), "bytes": len(d)} for p, d in files.items()]}
    (root / "bundle.json").write_text(json.dumps(manifest), encoding="utf-8")
    if extra:
        (root / "stimuli" / "unlisted.wav").write_bytes(b"x")
    if tamper:
        (root / "stimuli" / "imp-dm-96000.wav").write_bytes(b"RIFF-evil")


class CheckBundleTests(unittest.TestCase):
    def check(self, **kw) -> list[str]:
        with tempfile.TemporaryDirectory() as d:
            make(Path(d), **kw)
            return cb.problems(Path(d))

    def test_a_clean_bundle_passes(self) -> None:
        self.assertEqual(self.check(), [])

    def test_tampered_and_unlisted_files_fail(self) -> None:
        self.assertEqual(self.check(tamper=True), ["stimuli/imp-dm-96000.wav: sha256 mismatch"])
        self.assertEqual(self.check(extra=True), ["stimuli/unlisted.wav: not listed in bundle.json"])

    def test_foreign_plugins_and_outside_paths_fail(self) -> None:
        foreign = GOOD_RPP.replace('<JS utility/volume_pan ""', '<VST "VST3: Other" o.vst3 0 "" 1 ""')
        self.assertIn("projects/p.rpp:5: plug-in not on the allowlist", self.check(rpp=foreign))
        outside = GOOD_RPP.replace("@@JOB@@\\stimuli\\imp-dm-96000.wav", "C:\\Windows\\x.wav")
        self.assertNotEqual(outside, GOOD_RPP)
        self.assertIn("projects/p.rpp:11: media path outside the job", self.check(rpp=outside))
        render = GOOD_RPP.replace('"@@OUT@@\\p"', '"D:\\elsewhere"')
        self.assertNotEqual(render, GOOD_RPP)
        self.assertIn("projects/p.rpp:2: render path outside the job", self.check(rpp=render))

    def test_main_exit_codes(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            make(Path(d))
            self.assertEqual(cb.main([d]), 0)
        with tempfile.TemporaryDirectory() as d:
            make(Path(d), tamper=True)
            self.assertEqual(cb.main([d]), 1)


if __name__ == "__main__":
    unittest.main()
