"""Tests for scripts/gen3/deploy.sh (#72 phase A, plan Task 5): the Gen 3
device's deploy, run against a stand-in `ssh` and `scp` first on PATH that
play the box in a temporary folder (its home and its root). Every value is
synthetic (P6)."""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEPLOY = HERE / "deploy.sh"
UNIT = HERE / "iem-dante.service"
HOST = "tester@gen3.test"

# The box: `ssh` answers the deploy's commands from FAKE_BOX (home/ and
# root/), `scp` copies into its home; both log every call as one JSON line.
FAKE_SSH = r'''
import hashlib, json, os, shutil, sys
box = os.environ["FAKE_BOX"]
args = sys.argv[1:]
open(os.path.join(box, "calls.log"), "a").write(json.dumps(["ssh"] + args) + "\n")
while args and args[0] == "-o":
    args = args[2:]
cmd = " ".join(args[1:])
home, root = os.path.join(box, "home"), os.path.join(box, "root")
if cmd == "id -un":
    print(os.environ.get("FAKE_USER", "tester"))
elif cmd == "sudo -n -l systemctl stop iem-dante":
    if os.environ.get("FAKE_NO_STOP_RULE"):
        sys.stderr.write("Sorry, user tester is not allowed to execute this command.\n")
        sys.exit(1)
    print("/usr/bin/systemctl stop iem-dante")
elif cmd.startswith("mkdir -p "):
    os.makedirs(os.path.join(home, cmd.split()[-1]), exist_ok=True)
    if os.environ.get("FAKE_FLAG_ON") == "mkdir":
        open(os.environ["IEMMIXER_EVENT_NOW"], "w").write("now\n")
elif cmd.startswith("set -e\n"):
    for line in cmd.splitlines()[1:]:
        if os.environ.get("FAKE_SUDO_FAILS") and line.startswith("sudo -n "):
            sys.stderr.write("sudo: a password is required\n")
            sys.exit(1)
        words = line.split()
        if words[:4] == ["sudo", "-n", "install", "-D"]:
            mode, src, dst = words[5], words[6], words[7]
            if os.environ.get("FAKE_SUDO_FAILS_AT") == dst:
                sys.stderr.write("install: cannot create regular file: Read-only file system\n")
                sys.exit(1)
            target = os.path.join(root, dst.lstrip("/"))
            os.makedirs(os.path.dirname(target), exist_ok=True)
            shutil.copyfile(os.path.join(home, src), target)
            open(os.path.join(box, "modes.log"), "a").write(f"{mode} {dst}\n")
        if line == "sudo -n systemctl restart iem-dante" and os.environ.get("FAKE_FLAG_ON") == "restart":
            # "ide event" arrives while the service restarts: its own stop
            # has already run, the restart starts the device again.
            open(os.environ["IEMMIXER_EVENT_NOW"], "w").write("now\n")
elif cmd == "sudo -n systemctl stop iem-dante":
    if os.environ.get("FAKE_STOP_FAILS"):
        sys.stderr.write("sudo: a password is required\n")
        sys.exit(1)
elif cmd.startswith("sha256sum "):
    path = cmd.split()[1]
    data = open(os.path.join(root, path.lstrip("/")), "rb").read()
    print(os.environ.get("FAKE_SHA", hashlib.sha256(data).hexdigest()) + "  " + path)
elif cmd.endswith(" --version"):
    print(os.environ.get("FAKE_VERSION", "iem-dante-dev 2.0.0-dev.41 " + "a" * 40))
elif cmd == "systemctl is-active iem-dante":
    state = os.environ.get("FAKE_ACTIVE", "active")
    print(state)
    sys.exit(0 if state == "active" else 3)
else:
    sys.stderr.write("unexpected command: " + cmd + "\n")
    sys.exit(97)
'''

FAKE_SCP = r'''
import json, os, shutil, sys
box = os.environ["FAKE_BOX"]
args = sys.argv[1:]
open(os.path.join(box, "calls.log"), "a").write(json.dumps(["scp"] + args) + "\n")
words = []
while args:
    if args[0] == "-o":
        args = args[2:]
    elif args[0] == "-q":
        args = args[1:]
    else:
        words.append(args.pop(0))
*sources, dest = words
folder = os.path.join(box, "home", dest.split(":", 1)[1])
for src in sources:
    shutil.copyfile(src, os.path.join(folder, os.path.basename(src)))
'''


class DeployTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.box = self.tmp / "box"
        (self.box / "home").mkdir(parents=True)
        (self.box / "root").mkdir()
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        for name, body in (("ssh", FAKE_SSH), ("scp", FAKE_SCP)):
            p = bin_dir / name
            p.write_text(f"#!{sys.executable}\n{body}", encoding="utf-8")
            p.chmod(0o755)
        self.flag = self.tmp / "config" / "EVENT-NOW"
        # iempc's private env, absent unless a test writes it: the dev box's
        # own is never read.
        self.pc_env = self.tmp / "config" / "iem-pc.env"
        self.env = {**os.environ, "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}", "FAKE_BOX": str(self.box),
                    "IEMMIXER_EVENT_NOW": str(self.flag), "GEN3_SETTLE_S": "0", "PC_ENV": str(self.pc_env)}
        self.artifact = self.tmp / "artifact" / "iem-dante-dev"
        self.artifact.parent.mkdir()
        self.artifact.write_bytes(b"\x7fELF a synthetic iem-dante-dev")
        self.sha = hashlib.sha256(self.artifact.read_bytes()).hexdigest()
        self.config = self.tmp / "dante-device.toml"
        self.config.write_text('name = "test-device"\n', encoding="utf-8")

    def deploy(self, *extra: str, args: list[str] | None = None, **env: str) -> subprocess.CompletedProcess:
        argv = args if args is not None else ["--host", HOST, "--artifact", str(self.artifact), "--sha256", self.sha,
                                              "--config", str(self.config), "--iface", "eth9", *extra]
        return subprocess.run(["bash", str(DEPLOY), *argv], env={**self.env, **env}, capture_output=True, text=True,
                              timeout=60)

    def calls(self) -> list[list[str]]:
        log = self.box / "calls.log"
        if not log.exists():
            return []
        return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]

    def remote(self) -> list[str]:
        """The remote command of every ssh call, in order."""
        out = []
        for call in self.calls():
            if call[0] == "ssh":
                args = call[1:]
                while args and args[0] == "-o":
                    args = args[2:]
                self.assertEqual(args[0], HOST)
                out.append(" ".join(args[1:]))
        return out

    def installed(self, path: str) -> bytes:
        return (self.box / "root" / path.lstrip("/")).read_bytes()

    def test_a_deploy_installs_restarts_and_reads_back(self) -> None:
        r = self.deploy()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.installed("/opt/iemmixer/bin/iem-dante-dev"), self.artifact.read_bytes())
        self.assertEqual(self.installed("/etc/systemd/system/iem-dante.service"), UNIT.read_bytes())
        self.assertEqual(self.installed("/etc/systemd/system/iem-dante.service.d/10-user.conf"),
                         b"[Service]\nUser=tester\n")
        self.assertEqual(self.installed("/etc/iemmixer/dante-device.toml"), self.config.read_bytes())
        self.assertEqual(self.installed("/etc/iemmixer/iem-dante.env"), b"IEM_DANTE_IFACE=eth9\n")
        modes = (self.box / "modes.log").read_text(encoding="utf-8").splitlines()
        # The drop-in (User=) before the unit: no moment has a unit without it.
        self.assertEqual(modes, ["0755 /opt/iemmixer/bin/iem-dante-dev",
                                 "0644 /etc/systemd/system/iem-dante.service.d/10-user.conf",
                                 "0644 /etc/systemd/system/iem-dante.service",
                                 "0644 /etc/iemmixer/dante-device.toml", "0644 /etc/iemmixer/iem-dante.env"])
        remote = self.remote()
        self.assertEqual(remote[:3], ["id -un", "sudo -n -l systemctl stop iem-dante",
                                      "mkdir -p .cache/iem-dante-deploy"])
        install = remote[3].splitlines()
        self.assertEqual(install[0], "set -e")
        self.assertTrue(all(line.startswith("sudo -n ") for line in install[1:]), install)
        self.assertEqual(install[-3:], ["sudo -n systemctl daemon-reload", "sudo -n systemctl enable iem-dante",
                                        "sudo -n systemctl restart iem-dante"])
        self.assertEqual(remote[4:], ["sha256sum /opt/iemmixer/bin/iem-dante-dev",
                                      "/opt/iemmixer/bin/iem-dante-dev --version", "systemctl is-active iem-dante"])
        self.assertNotIn("WARNING", r.stderr)
        self.assertTrue(all("BatchMode=yes" in c for c in self.calls()))
        last = r.stdout.strip().splitlines()[-1]
        self.assertIn(f"iem-dante-dev 2.0.0-dev.41 {'a' * 40}", last)
        self.assertIn(f"sha256 {self.sha}", last)
        self.assertIn("iem-dante active", last)

    def test_the_event_flag_refuses_before_any_call(self) -> None:
        self.flag.parent.mkdir()
        self.flag.write_text("now\n", encoding="utf-8")
        r = self.deploy()
        self.assertEqual(r.returncode, 1)
        self.assertIn("an event is on", r.stderr)
        self.assertIn("nothing was copied", r.stderr)
        self.assertEqual(self.calls(), [])

    def test_a_flag_that_comes_during_the_copy_refuses_the_install(self) -> None:
        self.flag.parent.mkdir()
        r = self.deploy(FAKE_FLAG_ON="mkdir")
        self.assertEqual(r.returncode, 1)
        self.assertIn("nothing installed", r.stderr)
        self.assertEqual([c[0] for c in self.calls()], ["ssh", "ssh", "ssh", "scp"])
        self.assertFalse((self.box / "root" / "opt").exists())

    def test_a_flag_that_comes_during_the_install_stops_the_device_again(self) -> None:
        # The race: the flag is checked, "ide event" arrives and stops the
        # service, then the install's restart starts it again. The flag is
        # read again after the restart: the device is stopped, exit 1.
        self.flag.parent.mkdir()
        r = self.deploy(FAKE_FLAG_ON="restart")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("an event is on", r.stderr)
        self.assertIn("iem-dante stopped again", r.stderr)
        remote = self.remote()
        self.assertTrue(remote[-2].startswith("set -e\n"), remote)
        self.assertEqual(remote[-1], "sudo -n systemctl stop iem-dante")
        self.assertFalse(any(c.startswith("sha256sum ") for c in remote), remote)

    def test_a_stop_after_a_flag_during_the_install_that_fails_says_so(self) -> None:
        self.flag.parent.mkdir()
        r = self.deploy(FAKE_FLAG_ON="restart", FAKE_STOP_FAILS="1")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("an event is on", r.stderr)
        self.assertIn("may still run", r.stderr)
        self.assertEqual(self.remote()[-1], "sudo -n systemctl stop iem-dante")

    def test_an_artifact_with_another_sha256_is_refused_before_any_call(self) -> None:
        r = self.deploy(args=["--host", HOST, "--artifact", str(self.artifact), "--sha256", "0" * 64,
                              "--config", str(self.config), "--iface", "eth9"])
        self.assertEqual(r.returncode, 1)
        self.assertIn(f"the artifact's sha256 is {self.sha}", r.stderr)
        self.assertEqual(self.calls(), [])

    def test_the_service_never_runs_as_root(self) -> None:
        r = self.deploy(FAKE_USER="root")
        self.assertEqual(r.returncode, 1)
        self.assertIn("not root", r.stderr)
        self.assertEqual(self.remote(), ["id -un"])

    def test_a_refused_sudo_stops_before_the_read_back(self) -> None:
        r = self.deploy(FAKE_SUDO_FAILS="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("a password is required", r.stderr)
        self.assertEqual(len(self.remote()), 4)

    def test_an_install_that_fails_at_the_unit_never_leaves_a_unit_without_its_user(self) -> None:
        # A fresh box: the install stops at the unit. The drop-in naming the
        # user is in place already, so no later unit file runs as root.
        r = self.deploy(FAKE_SUDO_FAILS_AT="/etc/systemd/system/iem-dante.service")
        self.assertEqual(r.returncode, 1)
        self.assertIn("Read-only file system", r.stderr)
        self.assertEqual(self.installed("/etc/systemd/system/iem-dante.service.d/10-user.conf"),
                         b"[Service]\nUser=tester\n")
        self.assertFalse((self.box / "root" / "etc/systemd/system/iem-dante.service").exists())
        self.assertFalse(any(c.startswith("sha256sum ") for c in self.remote()))

    def test_a_box_whose_sudo_does_not_allow_the_event_stop_is_refused_before_any_copy(self) -> None:
        # "ide event" stops the device with `sudo -n systemctl stop iem-dante`
        # (iempc_event): a device it could not stop is never installed.
        r = self.deploy(FAKE_NO_STOP_RULE="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("sudo -n systemctl stop iem-dante", r.stderr)
        self.assertIn("sudoers", r.stderr)
        self.assertIn("nothing was copied", r.stderr)
        self.assertEqual(self.remote(), ["id -un", "sudo -n -l systemctl stop iem-dante"])
        self.assertNotIn("scp", [c[0] for c in self.calls()])

    def write_pc_env(self, text: str) -> None:
        self.pc_env.parent.mkdir(exist_ok=True)
        self.pc_env.write_text("PC_SSH=tester@pc.test\n" + text, encoding="utf-8")

    def gen3_warnings(self, r: subprocess.CompletedProcess) -> list[str]:
        return [line for line in r.stderr.splitlines() if "WARNING" in line]

    def test_a_deploy_to_the_host_ide_event_stops_warns_nothing(self) -> None:
        # Read as iempc reads the env: spaces around the key and the value,
        # quotes around it, the last line of a key winning, comments skipped.
        for text in (f"GEN3_HOST={HOST}\n", f"  GEN3_HOST = \"{HOST}\"  \n",
                     f"GEN3_HOST=other@gen3.test\n# GEN3_HOST=x@y\nGEN3_HOST={HOST}\n"):
            with self.subTest(text=text):
                self.write_pc_env(text)
                r = self.deploy()
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(self.gen3_warnings(r), [], r.stderr)

    def test_a_deploy_to_another_host_than_gen3_host_warns(self) -> None:
        self.write_pc_env("GEN3_HOST=other@gen3.test\n")
        r = self.deploy()
        self.assertEqual(r.returncode, 0, r.stderr)
        warnings = self.gen3_warnings(r)
        self.assertEqual(len(warnings), 1, r.stderr)
        self.assertIn("GEN3_HOST", warnings[0])
        self.assertIn("ide event", warnings[0])
        # The values stay out of the line (P6: it may be pasted).
        self.assertNotIn("other@gen3.test", r.stderr)

    def test_an_env_without_gen3_host_warns_and_no_env_is_no_warning(self) -> None:
        self.write_pc_env("PC_ROOT=X:\\root\n")
        r = self.deploy()
        self.assertEqual(r.returncode, 0, r.stderr)
        warnings = self.gen3_warnings(r)
        self.assertEqual(len(warnings), 1, r.stderr)
        self.assertIn("no GEN3_HOST", warnings[0])
        self.pc_env.unlink()
        r = self.deploy()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.gen3_warnings(r), [], r.stderr)

    def test_an_installed_binary_with_another_sha256_fails(self) -> None:
        r = self.deploy(FAKE_SHA="f" * 64)
        self.assertEqual(r.returncode, 1)
        self.assertIn(f"the installed binary's sha256 is {'f' * 64}, not {self.sha}", r.stderr)

    def test_a_version_that_is_not_ours_fails(self) -> None:
        r = self.deploy(FAKE_VERSION="something else")
        self.assertEqual(r.returncode, 1)
        self.assertIn("--version said 'something else'", r.stderr)

    def test_a_service_that_is_not_active_after_the_restart_fails(self) -> None:
        for state in ("activating", "failed"):
            r = self.deploy(FAKE_ACTIVE=state)
            self.assertEqual(r.returncode, 1, state)
            self.assertIn(f"iem-dante is '{state}'", r.stderr)

    def test_a_bad_command_line_is_exit_2_and_calls_nothing(self) -> None:
        full = ["--host", HOST, "--artifact", str(self.artifact), "--sha256", self.sha, "--config", str(self.config),
                "--iface", "eth9"]

        def without(flag: str) -> list[str]:
            i = full.index(flag)
            return full[:i] + full[i + 2:]

        def with_value(flag: str, value: str) -> list[str]:
            i = full.index(flag)
            return full[:i + 1] + [value] + full[i + 2:]

        cases = [
            (without("--host"), "--host is missing"),
            (without("--sha256"), "--sha256 is missing"),
            (without("--iface"), "--iface is missing"),
            (full + ["--port", "22"], "unknown argument '--port'"),
            (full + ["--host"], "--host needs a value"),
            (with_value("--host", "-oProxyCommand=true"), "not an option"),
            (with_value("--sha256", self.sha.upper()), "64 lowercase hex"),
            (with_value("--sha256", self.sha[:-1]), "64 lowercase hex"),
            (with_value("--iface", "eth 9"), "not an interface name"),
            (with_value("--iface", "a" * 16), "not an interface name"),
            (with_value("--artifact", str(self.tmp / "missing")), "--artifact is not a file"),
            (with_value("--config", str(self.tmp)), "--config is not a file"),
        ]
        for argv, want in cases:
            r = self.deploy(args=argv)
            self.assertEqual(r.returncode, 2, (argv, r.stderr))
            self.assertIn(want, r.stderr, argv)
        self.assertEqual(self.calls(), [])


if __name__ == "__main__":
    unittest.main()
