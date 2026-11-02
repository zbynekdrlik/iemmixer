"""Tests for scripts/iem-pc/iempc_gen3_newbox.py, `iempc gen3-backup`,
`gen3-new-box` and `gen3-drift` (#72). No box in CI: a stand-in plays the ssh
commands and the scripts, with the real band_send.py and band_receive.py on
temporary folders. Every value is synthetic (P6)."""
from __future__ import annotations

import hashlib
import io
import json
import stat
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(1, str(HERE.parent / "gen3"))
import band_receive
import band_send
import iempc_gen3_newbox as nb
from test_deploy import template_lines

SHA = "a" * 40
OLD, NEW = "admin@192.0.2.40", "admin@192.0.2.41"
PEPPER = bytes(range(32))
GOOD = {"pass": 1, "user": 1, "units": 1, "napi": 1, "state": 1, "tpm2": 0, "realtime_running": 1}


class Ip:
    class StepError(Exception):
        pass

    class Refused(Exception):
        pass

    class StillRunning(StepError):
        pass

    EVENT_NOW = Path("/nonexistent/flag")
    STATE_DIR = Path("/nonexistent/state")

    def __init__(self, folder: Path) -> None:
        self.folder, self.emitted, self.calls, self.fail = folder, [], [], {}
        self.serves = True

    def env_path(self):
        return self.folder / "iem-pc.env"

    def check_sha(self, text):
        if len(text) != 40:
            raise self.Refused("not a sha")
        return text

    def emit(self, doc):
        self.emitted.append(doc)

    def pause(self, _ctx, _seconds):
        pass

    def gen3_serves(self):
        return self.serves

    def guarded(self, words, _stdin, _timeout, _event, *, env=None, cwd=None):
        self.calls.append(words)
        key = " ".join(words)
        if " rev-parse HEAD" in key:
            return SHA + "\n"
        if " status --porcelain" in key:
            return self.fail.get("dirty", "")
        if " diff --name-only" in key:
            return self.fail.get("crates", "")
        if words[-2:] == ["--version"] or key.endswith("--version"):
            return f"iem-engine 2.0.0+{SHA}\n"
        for script, message in self.fail.items():
            if script in key:
                raise self.StepError(message)
        return ""


class Box:
    """Two boxes behind `data.box`: the running one and the new one."""

    def __init__(self, test) -> None:
        self.test, self.sent, self.boot = test, [], 1
        tmp = Path(test.tmp.name)
        self.site, self.engine = tmp / "site", tmp / "engine"
        self.new_site, self.new_engine = tmp / "new-site", tmp / "new-engine"
        for folder in (self.site / "secrets", self.engine, self.new_site, self.new_engine):
            folder.mkdir(parents=True, exist_ok=True)
        (self.site / "secrets" / "jwt_secret").write_text("synthetic")
        (self.engine / "current.json").write_text("{}")
        self.verify = dict(GOOD)
        self.realtime_after_boot = True
        self.credential = b""
        self.health = [0]
        self.sums = {}
        self.overrides = {}

    def __call__(self, host, command, stdin, _timeout):
        self.sent.append((host, command))
        if command in self.overrides:
            return self.overrides[command]
        if command == nb.SEND:
            return 0, json.dumps(band_send.send(self.site, self.engine)).encode(), b""
        if command == nb.DECRYPT:
            return 0, PEPPER, b""
        if command == nb.data.RECEIVE:
            out, err = io.StringIO(), io.StringIO()
            with mock.patch("sys.stdout", out), mock.patch("sys.stderr", err):
                code = band_receive.main([str(self.new_site), str(self.new_engine)], io.BytesIO(stdin))
            return code, out.getvalue().encode(), err.getvalue().encode()
        if command == nb.data.ENCRYPT:
            self.credential = stdin
            return 0, b"", b""
        if command == nb.data.CHMOD:
            return 0, b"", b""
        if command == nb.data.STAT:
            return 0, b"600.root.100", b""
        if command == nb.data.STATES:
            return 3, b"inactive\ninactive\ninactive\n", b""
        if command == nb.data.TASKS:
            return 0, b"0\n\n0\n\n0\n", b""
        if command == "python3 -":
            return int(self.verify["pass"] != 1), json.dumps({"verify": self.verify}).encode(), b""
        if command == nb.BOOT_ID:
            return 0, f"boot-{self.boot}\n".encode(), b""
        if command == nb.REBOOT:
            self.boot += 1
            self.verify["realtime_running"] = int(self.realtime_after_boot)
            return 255, b"", b"closed"
        if command == nb.ROUTE:
            return 0, b"default via 192.0.2.1 dev eth7 proto dhcp\n", b""
        if command == nb.HEALTH:
            return self.health.pop(0) if self.health else 0, b"", b""
        if command in (nb.STOP, nb.START):
            return 0, b"", b""
        if command.startswith("sha256sum "):
            return 0, "".join(f"{v}  {k}\n" for k, v in self.sums.items()).encode(), b""
        if command.startswith("systemctl is-enabled"):
            return 0, b"enabled\n" * len(nb.ENABLED), b""
        if command == "dantesync --version":
            return 0, b"dantesync 1.2.3\n", b""
        if command == "cloudflared --version":
            return 0, b"cloudflared version 2026.1.0\n", b""
        self.test.fail(f"an unexpected box command: {command}")

    def commands(self, host):
        return [c for h, c in self.sent if h == host]


class Base(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.ip = Ip(root)
        self.box = Box(self)
        patcher = mock.patch.object(nb.data, "box", self.box)
        patcher.start()
        self.addCleanup(patcher.stop)
        files = {}
        for key in nb.FILE_KEYS:
            path = root / key
            path.write_text("name = 'synthetic'\n")
            files[key] = str(path)
        self.env = {"GEN3_HOST": OLD, "GEN3_DANTESYNC_VERSION": "v1.2.3", "GEN3_DANTESYNC_SHA256": "b" * 64,
                    "GEN3_CLOUDFLARED_VERSION": "2026.1.0", "GEN3_CLOUDFLARED_SHA256": "c" * 64, **files}
        self.bundle = root / "bundle"
        self.bundle.mkdir()

    def ctx(self, **args):
        ns = types.SimpleNamespace(host=NEW, bundle=str(self.bundle), sha=SHA, iface=None, from_backup=None)
        ns.__dict__.update(args)
        return types.SimpleNamespace(env=self.env, args=ns, watch=lambda abandon: "finish")

    def new_box(self, **args) -> int:
        return nb.run_new_box(self.ctx(**args), self.ip)

    def scripts(self) -> list[str]:
        return [Path(w).name for c in self.ip.calls for w in c if w.endswith(".sh")]


class BackupTests(Base):
    def test_a_backup_is_private_whole_and_readable(self) -> None:
        self.assertEqual(nb.run_backup(self.ctx(), self.ip), 0)
        doc = self.ip.emitted[-1]["gen3_backup"]
        folder = nb.backups(self.ip) / doc["stamp"]
        self.assertEqual(stat.S_IMODE(folder.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(folder.parent.stat().st_mode), 0o700)
        for name in ("band.zip", "manifest.json", "pepper"):
            self.assertEqual(stat.S_IMODE((folder / name).stat().st_mode), 0o600)
        self.assertEqual((folder / "pepper").read_bytes(), PEPPER)
        _, manifest, value = nb.load_backup(self.ip, nb.backups(self.ip), doc["stamp"])
        self.assertEqual(value, PEPPER)
        self.assertEqual(hashlib.sha256((self.box.engine / "current.json").read_bytes()).hexdigest(),
                         manifest["engine/current.json"])

    def test_the_report_holds_no_value(self) -> None:
        nb.run_backup(self.ctx(), self.ip)
        text = json.dumps(self.ip.emitted)
        self.assertNotIn("synthetic", text)
        self.assertNotIn(OLD, text)

    def test_a_pepper_of_another_length_stops_the_backup_and_is_not_named(self) -> None:
        self.box.overrides[nb.DECRYPT] = (0, b"zyxqw", b"")
        with self.assertRaises(Ip.StepError) as e:
            nb.run_backup(self.ctx(), self.ip)
        self.assertNotIn("zyxqw", str(e.exception))
        self.assertFalse(nb.backups(self.ip).exists())

    def test_a_damaged_backup_is_refused(self) -> None:
        nb.run_backup(self.ctx(), self.ip)
        stamp = self.ip.emitted[-1]["gen3_backup"]["stamp"]
        folder = nb.backups(self.ip) / stamp
        (folder / "manifest.json").write_text('{"engine/current.json": "0"}')
        with self.assertRaises(Ip.StepError):
            nb.load_backup(self.ip, nb.backups(self.ip), stamp)

    def test_a_stamp_is_no_path(self) -> None:
        with self.assertRaises(Ip.Refused):
            nb.load_backup(self.ip, nb.backups(self.ip), "../secret")


class SudoTests(unittest.TestCase):
    def test_every_sudo_command_is_a_line_of_the_template(self) -> None:
        lines = set(template_lines())
        for command in (nb.SEND, nb.DECRYPT, nb.STOP, nb.START, nb.REBOOT):
            words = command.split()[2:]
            runas = "root"
            if words[0] == "-u":
                runas, words = words[1], words[2:]
            self.assertIn((runas, " ".join(words)), lines)


if __name__ == "__main__":
    unittest.main()
