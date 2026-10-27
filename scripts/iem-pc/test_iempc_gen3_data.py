"""Tests for scripts/iem-pc/iempc_gen3_data.py, the band's data from the IEM
PC onto the Gen 3 box (#72 phase E Task 5). No PC and no box in CI: the PC's
PowerShell session is a fake that answers the band's zip or the pepper, and
the box's ssh a fake that runs the real band_receive.py on temporary
folders. Every value is synthetic (P6: invented names and contents)."""
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
from iempc_test_support import Base, ip  # noqa: E402

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
    def test_the_pc_sends_exactly_what_the_box_takes(self) -> None:
        self.assertEqual((data.SERVER_FILES, data.SERVER_FOLDERS),
                         (band_receive.SERVER_FILES, band_receive.SERVER_FOLDERS))
        script = data.pc_script("C:\\IEM\\zyxqw")
        for name in (*data.SERVER_FILES, *data.SERVER_FOLDERS):
            self.assertIn(f"'{name}'", script)
        self.assertIn("'C:\\IEM\\zyxqw'", script)
        self.assertNotIn("pepper", script)   # the pepper has its own session, never in the zip

    def test_every_box_command_is_a_line_of_the_sudo_template(self) -> None:
        lines = template()
        self.assertIn(("iemmixer", data.RECEIVE.removeprefix("sudo -n -u iemmixer ")), lines)
        self.assertIn(("root", data.ENCRYPT.removeprefix("sudo -n ")), lines)
        self.assertIn(("root", data.STAT.removeprefix("sudo -n ")), lines)
        self.assertIn(("root", data.CHMOD.removeprefix("sudo -n ")), lines)


class Gen3DataTests(Base):
    def setUp(self) -> None:
        super().setUp()
        text = ip.env_path().read_text(encoding="utf-8")
        ip.env_path().write_text(text + "GEN3_HOST=admin@192.0.2.40\n", encoding="utf-8")
        box = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, box)
        self.site, self.engine = box / "site", box / "engine"
        (self.site / "secrets").mkdir(parents=True)
        self.engine.mkdir()
        (self.engine / "current.json").write_text("{}", encoding="utf-8")
        self.pc_answer = answer(BAND)
        self.scripts: list[str] = []
        self.sent: list[tuple[str, bytes]] = []
        self.states = "inactive\ninactive\ninactive\n"
        self.tasks = "[not set]\n0\n[not set]\n"
        self.stat = "600.root.180\n"
        self.encrypt_code = 0
        self.mode = "644"   # what systemd-creds leaves under sudo's umask 0022 (the box, 2026-10-27)
        self.patch(ssh_ps=self.ssh_ps)
        patcher = mock.patch.object(data, "box", self.box)
        patcher.start()
        self.addCleanup(patcher.stop)

    def ssh_ps(self, env, script, timeout, event) -> str:
        self.scripts.append(script)
        if script == data.PEPPER_SCRIPT:
            return base64.b64encode(PEPPER).decode() + "\n"
        return self.pc_answer

    def box(self, host: str, command: str, stdin: bytes, timeout: float) -> tuple[int, bytes, bytes]:
        self.assertEqual(host, "admin@192.0.2.40")
        self.sent.append((command, stdin))
        if command.startswith("systemctl is-active"):
            return 0, self.states.encode(), b""
        if command == getattr(data, "TASKS", None):
            return 0, self.tasks.encode(), b""
        if command == data.RECEIVE:
            out, err = io.StringIO(), io.StringIO()
            with mock.patch("sys.stdout", out), mock.patch("sys.stderr", err):
                code = band_receive.main([str(self.site), str(self.engine)], io.BytesIO(stdin))
            return code, out.getvalue().encode(), err.getvalue().encode()
        if command == data.ENCRYPT:
            self.mode = "644"
            return self.encrypt_code, b"", b"zyxqw: refused" if self.encrypt_code else b""
        if command == getattr(data, "CHMOD", None):
            self.mode = "600"
            return 0, b"", b""
        if command == data.STAT:
            return 0, self.stat.replace("600", self.mode).encode(), b""
        self.fail(f"an unexpected box command: {command}")

    def commands(self) -> list[str]:
        return [c for c, _ in self.sent]

    def test_the_bands_files_reach_the_box_and_read_back(self) -> None:
        code, docs, err = self.run_main("gen3-data")
        self.assertEqual(code, 0, err)
        self.assertEqual(docs, [{"gen3_data": {"members": len(BAND), "bytes": len(zipped(BAND)), "dry_run": False,
                                               "written": len(BAND)}}])
        self.assertEqual(self.commands(), [data.STATES, data.TASKS, data.RECEIVE])
        self.assertEqual((self.site / "secrets" / "jwt_secret").read_bytes(), b"zyxqw-jwt")
        self.assertEqual((self.engine / "current.json").read_bytes(), b'{"rev": 7}')
        self.assertEqual(self.scripts, [data.pc_script(ip.load_env(ip.env_path())["PC_ROOT"])])

    def test_the_pepper_goes_only_to_the_credential_and_is_never_printed(self) -> None:
        code, docs, err = self.run_main("gen3-data", "--pepper")
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[0]["gen3_data"]["pepper"], "replaced")
        self.assertEqual(self.commands()[-3:], [data.ENCRYPT, data.CHMOD, data.STAT])
        self.assertEqual([stdin for c, stdin in self.sent if c == data.ENCRYPT], [PEPPER])
        shown = json.dumps(docs) + err
        for form in (PEPPER.hex(), base64.b64encode(PEPPER).decode()):
            self.assertNotIn(form, shown)
        self.assertNotIn(PEPPER.hex(), " ".join(self.commands()))

    def test_a_pepper_that_is_not_32_bytes_or_does_not_read_back_fails(self) -> None:
        self.patch(ssh_ps=lambda env, script, t, e: "QUJD\n" if script == data.PEPPER_SCRIPT else self.pc_answer)
        code, docs, err = self.run_main("gen3-data", "--pepper")
        self.assertEqual(code, 1)
        self.assertIn("the PC's pepper is not 32 bytes", err)
        self.assertNotIn(data.ENCRYPT, self.commands())
        self.patch(ssh_ps=self.ssh_ps)
        self.stat = "640.root.180\n"
        code, docs, err = self.run_main("gen3-data", "--pepper")
        self.assertEqual(code, 1)
        self.assertIn("does not read back as 600 root", err)
        self.encrypt_code, self.stat = 1, "600.root.180\n"
        code, docs, err = self.run_main("gen3-data", "--pepper")
        self.assertEqual(code, 1)
        self.assertIn("systemd-creds encrypt failed on the box (exit 1)", err)

    def test_a_dry_run_or_a_running_stack_sends_nothing_to_the_box(self) -> None:
        code, docs, err = self.run_main("gen3-data", "--dry-run")
        self.assertEqual(code, 0, err)
        self.assertTrue(docs[0]["gen3_data"]["dry_run"])
        self.assertEqual(self.commands(), [data.STATES, data.TASKS])
        self.sent.clear()
        self.states = "active\nactive\ninactive\n"
        code, docs, err = self.run_main("gen3-data")
        self.assertEqual((code, docs), (1, []))
        self.assertIn("the box's stack is not stopped", err)
        self.assertEqual(self.scripts[-1:], [data.pc_script(ip.load_env(ip.env_path())["PC_ROOT"])])   # the dry run's
        self.assertEqual(self.commands(), [data.STATES, data.TASKS])

    def test_a_failed_unit_with_no_process_left_is_stopped_and_one_with_a_process_is_not(self) -> None:
        # The reviews of PR #83: an engine exit 2/3 leaves its unit `failed` (not running); a stop past its
        # bound (SendSIGKILL=no) leaves `failed` or, after reset-failed, `inactive` with its process alive.
        self.states, self.tasks = "failed\ninactive\ninactive\n", "0\n[not set]\n[not set]\n"
        self.assertEqual(self.run_main("gen3-data", "--dry-run")[0], 0)
        self.states, self.tasks = "inactive\ninactive\ninactive\n", "2\n[not set]\n[not set]\n"
        code, docs, err = self.run_main("gen3-data", "--dry-run")
        self.assertEqual((code, docs), (1, []))
        self.assertIn("the box's stack is not stopped", err)

    def test_a_zip_that_does_not_match_its_manifest_or_lacks_the_engine_state_sends_nothing(self) -> None:
        self.pc_answer = answer(BAND, {**{k: hashlib.sha256(v).hexdigest() for k, v in BAND.items()},
                                       "server/roster.toml": "0" * 64})
        code, docs, err = self.run_main("gen3-data")
        self.assertEqual((code, docs), (1, []))
        self.assertIn("does not match its own manifest", err)
        self.pc_answer = answer({k: v for k, v in BAND.items() if k != "engine/current.json"})
        code, docs, err = self.run_main("gen3-data")
        self.assertIn("holds no engine/current.json", err)
        self.pc_answer = "not json"
        code, docs, err = self.run_main("gen3-data")
        self.assertIn("is not the band's zip", err)
        self.assertNotIn(data.RECEIVE, self.commands())

    def test_a_receiver_refusal_or_a_different_answer_fails(self) -> None:
        self.pc_answer = answer({**BAND, "server/site.toml": b"x"})   # the box takes no site
        code, docs, err = self.run_main("gen3-data")
        self.assertEqual(code, 1)
        self.assertIn("the box's receiver failed (exit 1)", err)
        self.assertEqual((self.engine / "current.json").read_text(encoding="utf-8"), "{}")   # nothing written

    def test_the_command_is_dev_time_and_locked_and_needs_gen3_host(self) -> None:
        spec = ip.COMMANDS["gen3-data"]
        self.assertEqual((spec.fn, spec.pc, spec.dev_time, spec.locked), (ip.cmd_gen3_data, True, True, True))
        text = ip.env_path().read_text(encoding="utf-8")
        ip.env_path().write_text("".join(l + "\n" for l in text.splitlines() if not l.startswith("GEN3_HOST")),
                                 encoding="utf-8")
        code, docs, err = self.run_main("gen3-data")
        self.assertEqual((code, docs, self.sent), (1, [], []))
        self.assertIn("no GEN3_HOST", err)


if __name__ == "__main__":
    unittest.main()
