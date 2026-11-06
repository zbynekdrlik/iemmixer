"""Tests for scripts/iem-pc/iempc_gen3_data.py, the band's data on the Gen 3 box
(#72): no box in CI, its ssh is a fake that runs the real band_receive.py on
temporary folders. Every value is synthetic (P6: invented names and contents)."""
from __future__ import annotations

import base64
import hashlib
import io
import json
import shutil
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
GEN3 = HERE.parent / "gen3"
sys.path.insert(0, str(HERE))
sys.path.insert(1, str(GEN3))
import band_receive  # noqa: E402
import iempc_gen3_data as data  # noqa: E402
from iempc_test_support import ip  # noqa: E402

HOST = "admin@192.0.2.40"
PEPPER = bytes(range(32))
BAND = {
    "server/secrets/jwt_secret": b"zyxqw-jwt",
    "server/secrets/pin_hashes.json": b'{"members": {}}',
    "server/lineups/zyxqw/state.json": b"{}",
    "server/photos/member1.jpg": b"\xff\xd8zyxqw",
    "engine/current.json": b'{"rev": 7}',
    "engine/current.json.rev-7": b"",
    "engine/baseline.json": b'{"rev": 1}',
}


def zipped(members: dict[str, bytes]) -> bytes:
    raw = io.BytesIO()
    with zipfile.ZipFile(raw, "w") as z:
        for name, blob in members.items():
            z.writestr(name, blob)
    return raw.getvalue()


def answer(members: dict[str, bytes], manifest: dict | None = None) -> str:
    man = manifest if manifest is not None else {k: hashlib.sha256(v).hexdigest() for k, v in members.items()}
    return "noise from the profile\n" + json.dumps({"manifest": man, "zip": base64.b64encode(zipped(members)).decode()})


def template() -> set[tuple[str, str]]:
    """sudo-lines.txt's lines as (runas, command)."""
    out = set()
    for line in (GEN3 / "sudo-lines.txt").read_text(encoding="utf-8").splitlines():
        if line.strip() and not line.startswith("#"):
            runas, _, command = line.partition(" | ")
            out.add((runas, command))
    return out


class ListTests(unittest.TestCase):
    def test_every_box_command_is_a_line_of_the_sudo_template(self) -> None:
        lines = template()
        self.assertIn(("iemmixer", data.RECEIVE.removeprefix("sudo -n -u iemmixer ")), lines)
        self.assertIn(("root", data.ENCRYPT.removeprefix("sudo -n ")), lines)
        self.assertIn(("root", data.STAT.removeprefix("sudo -n ")), lines)
        self.assertIn(("root", data.CHMOD.removeprefix("sudo -n ")), lines)


class BoxSideTests(unittest.TestCase):
    """What the new box and the backup share: the zip's check, the stopped stack, the receiver, the pepper."""

    def setUp(self) -> None:
        box = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, box)
        self.site, self.engine = box / "site", box / "engine"
        (self.site / "secrets").mkdir(parents=True)
        self.engine.mkdir()
        (self.engine / "current.json").write_text("{}", encoding="utf-8")
        self.sent: list[tuple[str, bytes]] = []
        self.states = "inactive\ninactive\ninactive\n"
        # As the box prints it (2026-10-27): each unit's value, a blank line between units.
        self.tasks = "[not set]\n\n[not set]\n\n[not set]\n"
        self.stat = "600.root.180\n"
        self.encrypt_code = 0
        self.mode = "644"   # what systemd-creds leaves under sudo's umask 0022 (the box, 2026-10-27)
        patcher = mock.patch.object(data, "box", self.box)
        patcher.start()
        self.addCleanup(patcher.stop)

    def box(self, host: str, command: str, stdin: bytes, timeout: float) -> tuple[int, bytes, bytes]:
        self.assertEqual(host, HOST)
        self.sent.append((command, stdin))
        if command == data.STATES:
            return 0, self.states.encode(), b""
        if command == data.TASKS:
            return 0, self.tasks.encode(), b""
        if command == data.RECEIVE:
            out, err = io.StringIO(), io.StringIO()
            with mock.patch("sys.stdout", out), mock.patch("sys.stderr", err):
                code = band_receive.main([str(self.site), str(self.engine)], io.BytesIO(stdin))
            return code, out.getvalue().encode(), err.getvalue().encode()
        if command == data.ENCRYPT:
            self.mode = "644"
            return self.encrypt_code, b"", b"zyxqw: refused" if self.encrypt_code else b""
        if command == data.CHMOD:
            self.mode = "600"
            return 0, b"", b""
        if command == data.STAT:
            return 0, self.stat.replace("600", self.mode).encode(), b""
        self.fail(f"an unexpected box command: {command}")

    def commands(self) -> list[str]:
        return [c for c, _ in self.sent]

    def test_the_bands_files_reach_the_box_and_read_back(self) -> None:
        raw, manifest = data.unpack(ip, answer(BAND), "the box")
        self.assertEqual(data.receive(ip, HOST, raw, manifest), manifest)
        self.assertEqual(self.commands(), [data.RECEIVE])
        self.assertEqual((self.site / "secrets" / "jwt_secret").read_bytes(), b"zyxqw-jwt")
        self.assertEqual((self.engine / "current.json").read_bytes(), b'{"rev": 7}')

    def test_the_pepper_goes_only_to_the_credential_and_must_read_back(self) -> None:
        data.install_pepper(ip, HOST, PEPPER)
        self.assertEqual(self.commands(), [data.ENCRYPT, data.CHMOD, data.STAT])
        self.assertEqual([stdin for c, stdin in self.sent if c == data.ENCRYPT], [PEPPER])
        self.assertNotIn(PEPPER.hex(), " ".join(self.commands()))
        self.stat = "640.root.180\n"
        with self.assertRaisesRegex(ip.StepError, "does not read back as 600 root"):
            data.install_pepper(ip, HOST, PEPPER)
        self.encrypt_code, self.stat = 1, "600.root.180\n"
        with self.assertRaisesRegex(ip.StepError, "systemd-creds encrypt failed on the box \\(exit 1\\)"):
            data.install_pepper(ip, HOST, PEPPER)

    def test_a_running_stack_is_not_quiet(self) -> None:
        self.assertTrue(data.quiet(HOST))
        self.assertEqual(self.commands(), [data.STATES, data.TASKS])
        self.sent.clear()
        self.states = "active\nactive\ninactive\n"
        self.assertFalse(data.quiet(HOST))
        self.assertEqual(self.commands(), [data.STATES])   # a unit runs: its processes need no read

    def test_a_failed_unit_with_no_process_left_is_stopped_and_one_with_a_process_is_not(self) -> None:
        # The reviews of PR #83: an engine exit 2/3 leaves its unit `failed` (not running); a stop past its
        # bound (SendSIGKILL=no) leaves `failed` or, after reset-failed, `inactive` with its process alive.
        self.states, self.tasks = "failed\ninactive\ninactive\n", "0\n[not set]\n[not set]\n"
        self.assertTrue(data.quiet(HOST))
        self.states, self.tasks = "inactive\ninactive\ninactive\n", "2\n[not set]\n[not set]\n"
        self.assertFalse(data.quiet(HOST))

    def test_a_zip_that_does_not_match_its_manifest_or_lacks_the_engine_state_is_refused(self) -> None:
        wrong = answer(BAND, {**{k: hashlib.sha256(v).hexdigest() for k, v in BAND.items()},
                              "server/roster.toml": "0" * 64})
        with self.assertRaisesRegex(ip.StepError, "does not match its own manifest"):
            data.unpack(ip, wrong, "the box")
        with self.assertRaisesRegex(ip.StepError, "holds no engine/current.json"):
            data.unpack(ip, answer({k: v for k, v in BAND.items() if k != "engine/current.json"}), "the box")
        with self.assertRaisesRegex(ip.StepError, "is not the band's zip"):
            data.unpack(ip, "not json", "the box")

    def test_a_receiver_refusal_fails(self) -> None:
        raw, manifest = data.unpack(ip, answer({**BAND, "server/site.toml": b"x"}), "the box")   # the box takes no site
        with self.assertRaisesRegex(ip.StepError, "the box's receiver failed \\(exit 1\\)"):
            data.receive(ip, HOST, raw, manifest)
        self.assertEqual((self.engine / "current.json").read_text(encoding="utf-8"), "{}")   # nothing written


if __name__ == "__main__":
    unittest.main()
