"""Tests for scripts/gen3/band_send.py, the band's data off the Gen 3 box (#72).
Real folders in a temporary directory, and the real band_receive.py as the
other end. Every value is synthetic (P6)."""
from __future__ import annotations

import base64
import contextlib
import hashlib
import io
import json
import os
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import band_receive
import band_send


class SendTests(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = Path(tmp.name)
        self.site, self.engine, self.other = root / "site", root / "engine", root / "other"
        for folder in (self.site / "secrets", self.site / "presets" / "deep", self.engine, self.other / "site",
                       self.other / "engine"):
            folder.mkdir(parents=True)
        (self.site / "secrets" / "jwt_secret").write_text("synthetic")
        (self.site / "roster.toml").write_text("[[people]]\n")
        (self.site / "iemmixer.toml").write_text("the site's own")
        (self.site / "presets" / "deep" / "a.json").write_text("{}")
        (self.engine / "current.json").write_text("{}")
        (self.engine / "gen-1-r1.json").write_text("{}")
        (self.engine / "current.json.rev-7").write_text("")
        (self.engine / "engine.lock").write_text("")
        (self.engine / "save.tmp").write_text("half")
        (self.engine / "baseline.json").write_text("{}")   # an older engine's: no load reads it (#72)

    def sent(self) -> dict:
        return band_send.send(self.site, self.engine)

    def test_the_lists_are_the_receiver_s(self) -> None:
        self.assertEqual((band_send.SERVER_FILES, band_send.SERVER_FOLDERS),
                         (band_receive.SERVER_FILES, band_receive.SERVER_FOLDERS))

    def test_only_the_band_s_files_go_each_with_its_sha256(self) -> None:
        doc = self.sent()
        self.assertEqual(sorted(doc["manifest"]), [
            "engine/current.json", "engine/current.json.rev-7", "engine/gen-1-r1.json", "server/presets/deep/a.json",
            "server/roster.toml", "server/secrets/jwt_secret"])
        with zipfile.ZipFile(io.BytesIO(base64.b64decode(doc["zip"]))) as z:
            got = {i.filename: hashlib.sha256(z.read(i)).hexdigest() for i in z.infolist()}
        self.assertEqual(got, doc["manifest"])

    def test_the_receiver_takes_what_is_sent_whole(self) -> None:
        doc = self.sent()
        raw = base64.b64decode(doc["zip"])
        out = band_receive.receive(raw, self.other / "site", self.other / "engine", "20260101T000000")
        self.assertEqual(out["written"], doc["manifest"])

    def test_a_link_is_never_followed(self) -> None:
        os.symlink(self.site / "iemmixer.toml", self.site / "presets" / "link.json")
        os.symlink(self.site / "iemmixer.toml", self.engine / "gen-2-r2.json")
        names = self.sent()["manifest"]
        self.assertNotIn("server/presets/link.json", names)
        self.assertNotIn("engine/gen-2-r2.json", names)

    def test_no_current_state_refuses(self) -> None:
        (self.engine / "current.json").unlink()
        with self.assertRaises(band_send.Refused):
            self.sent()

    def test_main_prints_one_line_and_a_refusal_names_no_content(self) -> None:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(band_send.main([str(self.site), str(self.engine)]), 0)
        self.assertEqual(sorted(json.loads(out.getvalue())), ["manifest", "zip"])
        with contextlib.redirect_stderr(err):
            self.assertEqual(band_send.main([str(self.site), str(self.site / "none")]), 1)
            self.assertEqual(band_send.main([str(self.site)]), 2)
        self.assertNotIn("synthetic", err.getvalue())


if __name__ == "__main__":
    unittest.main()
