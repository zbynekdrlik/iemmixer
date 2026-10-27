"""Tests for scripts/gen3/band_receive.py, the band's data onto the Gen 3 box
(#72 phase E Task 5). A temporary site and engine folder stand in for the
box's; every value is synthetic (P6: invented names and contents)."""
from __future__ import annotations

import contextlib
import hashlib
import io
import json
import stat
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import band_receive  # noqa: E402

STAMP = "20261027T120000"


def archive(members: dict[str, bytes], links: tuple[str, ...] = ()) -> bytes:
    raw = io.BytesIO()
    with zipfile.ZipFile(raw, "w") as z:
        for name, data in members.items():
            info = zipfile.ZipInfo(name)
            if name in links:
                info.external_attr = (stat.S_IFLNK | 0o777) << 16
            z.writestr(info, data)
    return raw.getvalue()


BAND = {
    "server/secrets/jwt_secret": b"zyxqw-new-jwt",
    "server/secrets/pin_hashes.json": b'{"members": {}}',
    "server/roster.toml": b"[[people]]\nid = 'member1'\n",
    "server/lineups/active.json": b'{"lineup": "zyxqw"}',
    "server/lineups/zyxqw/state.json": b"{}",
    "server/photos/member1.jpg": b"\xff\xd8zyxqw",
    "engine/current.json": b'{"rev": 5}',
    "engine/current.json.rev-5": b"",
    "engine/gen-0000000001-r4.json": b'{"rev": 4}',
    "engine/baseline.json": b'{"rev": 1}',
}


class ReceiveTests(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.site, self.engine = self.root / "site", self.root / "engine"
        (self.site / "secrets").mkdir(parents=True)
        (self.site / "iemmixer.toml").write_text("port = 8080\n", encoding="utf-8")
        (self.site / "secrets" / "jwt_secret").write_bytes(b"zyxqw-old-jwt")
        (self.site / "lineups" / "old").mkdir(parents=True)
        (self.site / "lineups" / "old" / "state.json").write_text("{}", encoding="utf-8")
        self.engine.mkdir()
        (self.engine / "current.json").write_text('{"rev": 900}', encoding="utf-8")
        (self.engine / "engine.lock").write_text("", encoding="utf-8")

    def tree(self) -> dict[str, bytes]:
        return {p.relative_to(self.root).as_posix(): p.read_bytes() for p in sorted(self.root.rglob("*")) if p.is_file()}

    def run_main(self, raw: bytes) -> tuple[int, dict | None, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err), \
                mock.patch.object(band_receive.time, "strftime", lambda f: STAMP):
            code = band_receive.main([str(self.site), str(self.engine)], io.BytesIO(raw))
        return code, (json.loads(out.getvalue()) if out.getvalue() else None), err.getvalue()

    def test_the_band_replaces_its_items_and_the_engine_folder_keeping_every_old_one(self) -> None:
        code, doc, err = self.run_main(archive(BAND))
        self.assertEqual(code, 0, err)
        self.assertEqual(doc["written"], {k: hashlib.sha256(v).hexdigest() for k, v in sorted(BAND.items())})
        for name, data in BAND.items():
            root, rest = name.split("/", 1)
            path = (self.site if root == "server" else self.engine) / rest
            self.assertEqual(path.read_bytes(), data, name)
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600, name)
        # The site's own config is untouched; what was replaced is kept beside it.
        self.assertEqual((self.site / "iemmixer.toml").read_text(encoding="utf-8"), "port = 8080\n")
        self.assertFalse((self.site / "lineups" / "old").exists())
        self.assertEqual((self.site / f".lineups.before-{STAMP}" / "old" / "state.json").read_text(encoding="utf-8"),
                         "{}")
        self.assertEqual((self.site / "secrets" / f".jwt_secret.before-{STAMP}").read_bytes(), b"zyxqw-old-jwt")
        old_engine = self.root / f"engine.before-{STAMP}"
        self.assertEqual((old_engine / "current.json").read_text(encoding="utf-8"), '{"rev": 900}')
        self.assertFalse((self.engine / "engine.lock").exists())   # the new folder holds only what came
        self.assertEqual(sorted(doc["kept"]), sorted([f"engine.before-{STAMP}", f".lineups.before-{STAMP}",
                                                       f"secrets/.jwt_secret.before-{STAMP}"]))
        self.assertEqual([p.name for p in self.root.rglob("*.incoming")], [])

    def test_a_second_run_in_the_same_second_keeps_both_old_versions(self) -> None:
        self.assertEqual(self.run_main(archive(BAND))[0], 0)
        code, doc, err = self.run_main(archive({**BAND, "engine/current.json": b'{"rev": 6}'}))
        self.assertEqual(code, 0, err)
        self.assertIn(f"engine.before-{STAMP}-1", doc["kept"])
        self.assertEqual((self.root / f"engine.before-{STAMP}-1" / "current.json").read_bytes(), b'{"rev": 5}')
        self.assertEqual((self.root / f"engine.before-{STAMP}" / "current.json").read_bytes(), b'{"rev": 900}')

    def test_anything_but_the_bands_files_refuses_the_whole_archive(self) -> None:
        before = self.tree()
        bad = [{"/server/roster.toml": b"x"}, {"server/../escape": b"x"}, {"server\\roster.toml": b"x"},
               {"server/site.toml": b"x"}, {"server/key.pem": b"x"}, {"server/cert.pem": b"x"},
               {"server/secrets/pepper.dpapi": b"x"}, {"server/stale-secrets-x/jwt_secret": b"x"},
               {"server/lineups": b"x"}, {"engine/engine.lock": b""}, {"engine/save.tmp": b"x"},
               {"engine/current.json.damaged-1": b"x"}, {"engine/current.json.rev-x": b""},
               {"engine/sub/gen-1.json": b"x"}, {"other/x": b"x"}, {"roster.toml": b"x"}]
        for extra in bad:
            with self.subTest(extra=list(extra)):
                code, doc, err = self.run_main(archive({**BAND, **extra}))
                self.assertEqual((code, doc), (1, None))
                self.assertIn("nothing was written", err)
                self.assertNotIn("zyxqw", err)
        for raw, why in ((archive(BAND, links=("server/photos/member1.jpg",)), "a member is a link"),
                         (archive({k: v for k, v in BAND.items() if k != "engine/current.json"}), "no engine/current"),
                         (b"not a zip", "not a readable zip")):
            code, doc, err = self.run_main(raw)
            self.assertEqual((code, doc), (1, None))
            self.assertIn(why, err)
        with mock.patch.object(band_receive, "MAX_MEMBER", 8):
            self.assertEqual(self.run_main(archive(BAND))[0], 1)
        with mock.patch.object(band_receive, "MAX_ARCHIVE", 8):
            self.assertEqual(self.run_main(archive(BAND))[0], 1)
        self.assertEqual(self.tree(), before)

    def test_a_staging_left_by_an_earlier_run_refuses(self) -> None:
        (self.site / ".lineups.incoming").mkdir()
        before = self.tree()
        code, doc, err = self.run_main(archive(BAND))
        self.assertEqual((code, doc), (1, None))
        self.assertIn("an earlier run's staging is still there", err)
        self.assertEqual(self.tree(), before)

    def test_a_missing_or_linked_target_refuses_and_usage_is_exit_2(self) -> None:
        self.engine.rename(self.root / "elsewhere")
        self.engine.symlink_to(self.root / "elsewhere")
        self.assertEqual(self.run_main(archive(BAND))[0], 1)
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(band_receive.main([str(self.site)], io.BytesIO(b"")), 2)


if __name__ == "__main__":
    unittest.main()
