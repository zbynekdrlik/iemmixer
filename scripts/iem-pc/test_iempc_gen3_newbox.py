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
import tomllib
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

    def env_path(self):
        return self.folder / "iem-pc.env"

    def state_dir(self):
        return self.folder

    def check_sha(self, text):
        if len(text) != 40:
            raise self.Refused("not a sha")
        return text

    def emit(self, doc):
        self.emitted.append(doc)

    def pause(self, _ctx, _seconds):
        pass

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
            path.write_text("name = 'synthetic'\nstate_file = '/x'\n")
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

    def test_only_the_newest_backups_are_kept(self) -> None:
        # About 0.8 MB of the band's secrets a night: the newest KEEP_BACKUPS stay, older stamps go, anything
        # that is not a stamp is left alone.
        root = nb.backups(self.ip)
        root.mkdir(mode=0o700)
        old = [f"202601{d:02d}T043000" for d in range(1, nb.KEEP_BACKUPS + 1)]
        for stamp in old:
            (root / stamp).mkdir()
            (root / stamp / "pepper").write_bytes(b"x")
        (root / "notes").mkdir()
        (root / ".20260101T000000.incoming").mkdir()
        self.assertEqual(nb.run_backup(self.ctx(), self.ip), 0)
        doc = self.ip.emitted[-1]["gen3_backup"]
        self.assertEqual(doc["pruned"], 1)
        left = sorted(p.name for p in root.iterdir())
        self.assertEqual(left, sorted([".20260101T000000.incoming", "notes", *old[1:], doc["stamp"]]))

    def test_a_stamp_is_no_path(self) -> None:
        with self.assertRaises(Ip.Refused):
            nb.load_backup(self.ip, nb.backups(self.ip), "../secret")


class NewBoxTests(Base):
    def fresh(self) -> None:
        self.box.verify.update(realtime_running=0)

    def test_the_steps_run_in_the_cookbook_s_order_and_the_box_ends_verified(self) -> None:
        self.fresh()
        self.assertEqual(self.new_box(), 0)
        self.assertEqual(self.scripts(), ["install-dantesync.sh", "provision.sh", "deploy.sh", "install-cloudflared.sh"])
        cmds = self.box.commands(NEW)
        self.assertLess(cmds.index(nb.REBOOT), cmds.index(nb.data.RECEIVE))
        self.assertLess(cmds.index(nb.data.RECEIVE), cmds.index(nb.START))
        self.assertEqual(self.box.credential, PEPPER)
        self.assertEqual(self.ip.emitted[-1]["gen3_new_box"]["pass"], 1)
        self.assertTrue((self.box.new_engine / "current.json").is_file())

    def test_the_running_box_is_only_read(self) -> None:
        self.assertEqual(self.new_box(), 0)
        self.assertEqual(set(self.box.commands(OLD)), {nb.SEND, nb.DECRYPT})

    def test_the_deploy_is_a_spare_s(self) -> None:
        self.new_box()
        words = next(c for c in self.ip.calls if any(w.endswith("deploy.sh") for w in c))
        self.assertIn("--new-box", words)
        self.assertEqual(words[words.index("--iface") + 1], "eth7")

    def test_a_spare_runs_under_a_name_and_a_state_file_of_its_own(self) -> None:
        # Beside the serving box the band's name would be a second device its receivers can resolve.
        text = 'name = "Zyxqw-Band"\nrate = 1\nstate_file = "/x/state.toml"\ntx = ["a"]\n'
        self.assertEqual(tomllib.loads(nb.spare_config(self.ip, text)),
                         {"name": "Zyxqw-Band-spare", "rate": 1, "state_file": nb.SPARE_STATE, "tx": ["a"]})
        with self.assertRaisesRegex(self.ip.Refused, "no single name and state_file"):
            nb.spare_config(self.ip, 'name = "Zyxqw-Band"\n')
        with self.assertRaisesRegex(self.ip.Refused, "31 characters"):
            nb.spare_config(self.ip, f'name = "{"z" * 26}"\nstate_file = "/x"\n')

    def test_a_rerun_past_the_kernel_does_not_reboot(self) -> None:
        self.assertEqual(self.new_box(), 0)
        self.assertNotIn(nb.REBOOT, self.box.commands(NEW))

    def test_the_serving_box_is_refused_under_any_user(self) -> None:
        for host in (OLD, "other@192.0.2.40"):
            with self.subTest(host=host), self.assertRaises(Ip.Refused):
                self.new_box(host=host)
        self.assertEqual(self.ip.calls, [])

    def test_a_dirty_or_other_checkout_changes_nothing(self) -> None:
        self.ip.fail["dirty"] = " M file\n"
        with self.assertRaises(Ip.Refused):
            self.new_box()
        self.assertEqual(self.box.sent, [])

    def test_a_missing_key_is_named_and_its_value_is_not(self) -> None:
        del self.env["GEN3_CLOUDFLARED_SHA256"]
        with self.assertRaises(Ip.Refused) as e:
            self.new_box()
        self.assertIn("GEN3_CLOUDFLARED_SHA256", str(e.exception))
        self.assertNotIn(OLD, str(e.exception))

    def test_a_fresh_provision_s_expected_verify_failures_pass(self) -> None:
        self.ip.fail["provision.sh"] = "provision.sh failed (exit 1): provision.sh: verify_box.py found the box not"
        self.box.verify.update({"pass": 0, "units": 0, "napi": 0})
        with mock.patch.object(nb, "reboot_once", lambda *_: False), \
                mock.patch.object(nb, "deploy_new", lambda *_: self.box.verify.update(GOOD)):
            self.assertEqual(self.new_box(), 0)

    def test_any_other_provision_failure_stops(self) -> None:
        self.ip.fail["provision.sh"] = "provision.sh failed (exit 1): verify_box.py found the box not"
        self.box.verify.update({"pass": 0, "state": 0})
        with self.assertRaises(Ip.StepError) as e:
            self.new_box()
        self.assertIn("state", str(e.exception))
        self.ip.fail["provision.sh"] = "provision.sh failed (exit 1): provision-root.sh failed"
        with self.assertRaises(Ip.StepError):
            self.new_box()

    def test_a_box_back_without_the_realtime_kernel_stops(self) -> None:
        self.fresh()
        self.box.realtime_after_boot = False
        with self.assertRaises(Ip.StepError) as e:
            self.new_box()
        self.assertIn("realtime", str(e.exception))
        self.assertNotIn(nb.data.RECEIVE, self.box.commands(NEW))

    def test_a_stack_that_does_not_stop_gets_no_data(self) -> None:
        self.box.overrides[nb.data.STATES] = (0, b"active\ninactive\ninactive\n", b"")
        with self.assertRaises(Ip.StepError):
            self.new_box()
        self.assertNotIn(nb.data.RECEIVE, self.box.commands(NEW))

    def test_a_failing_final_verify_fails_the_command(self) -> None:
        self.box.verify.update({"pass": 0, "journal": 0})
        self.assertEqual(self.new_box(), 1)
        self.assertEqual(self.ip.emitted[-1]["gen3_new_box"]["failed"], ["journal"])

    def test_a_backup_replaces_the_running_box(self) -> None:
        nb.run_backup(self.ctx(), self.ip)
        stamp = self.ip.emitted[-1]["gen3_backup"]["stamp"]
        self.box.sent.clear()
        self.assertEqual(self.new_box(from_backup=stamp), 0)
        self.assertEqual(self.box.commands(OLD), [])
        self.assertEqual(self.box.credential, PEPPER)
        self.assertEqual(self.ip.emitted[-1]["gen3_new_box"]["data"], "backup")

    def test_a_bad_backup_is_refused_before_the_first_step(self) -> None:
        with self.assertRaises(Ip.Refused):
            self.new_box(from_backup="20260101T000000")
        self.assertEqual(self.box.sent, [])


class DriftTests(Base):
    def setUp(self) -> None:
        super().setUp()
        self.config = "synthetic device config\n"
        patcher = mock.patch.object(nb, "expected_config", lambda *_: self.config)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.box.sums = {path: hashlib.sha256((nb.GEN3 / name).read_bytes()).hexdigest()
                         for path, name in nb.CHECKOUT_FILES.items()}
        self.box.sums[nb.DEVICE_CONFIG] = hashlib.sha256(self.config.encode()).hexdigest()

    def drift(self) -> tuple[int, list[str]]:
        code = nb.run_drift(self.ctx(), self.ip)
        return code, self.ip.emitted[-1]["gen3_drift"]["differ"]

    def test_a_box_equal_to_the_checkout_passes(self) -> None:
        self.assertEqual(self.drift(), (0, []))

    def test_every_kind_of_difference_is_named(self) -> None:
        path = "/etc/systemd/system/iem-engine.service"
        for word, change in (
                ("file:iem-engine.service", lambda: self.box.sums.update({path: "0" * 64})),
                ("file:iem-engine.service", lambda: self.box.sums.pop(path)),
                ("file:dante-device.toml", lambda: self.box.sums.update({nb.DEVICE_CONFIG: "0" * 64})),
                ("build", lambda: self.ip.fail.update(crates="crates/iem-engine/src/lib.rs\n")),
                ("verify:journal", lambda: self.box.verify.update(journal=0)),
                ("realtime_running", lambda: self.box.verify.update(realtime_running=0)),
                ("boot:iem-engine", lambda: self.box.overrides.update(
                    {"systemctl is-enabled " + " ".join(nb.ENABLED): (1, b"disabled\nenabled\nenabled\nenabled\n", b"")})),
                ("version:dantesync", lambda: self.box.overrides.update({"dantesync --version": (0, b"0.0.1\n", b"")})),
                ("version:cloudflared", lambda: self.box.overrides.update({"cloudflared --version": (127, b"", b"")}))):
            with self.subTest(word=word):
                saved = (dict(self.box.sums), dict(self.ip.fail), dict(self.box.verify), dict(self.box.overrides))
                change()
                code, words = self.drift()
                self.assertEqual(code, 1)
                self.assertIn(word, words)
                self.box.sums, self.ip.fail, self.box.verify, self.box.overrides = saved

    def test_docs_after_the_build_are_no_drift(self) -> None:
        self.ip.fail["crates"] = ""
        self.assertEqual(self.drift()[0], 0)

    def test_a_build_this_checkout_does_not_hold_is_drift(self) -> None:
        with mock.patch.object(self.ip, "guarded", side_effect=[f"iem-engine 2.0.0+{SHA}\n", Ip.StepError("bad object")]):
            self.assertEqual(self.drift(), (1, ["build"]))


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
