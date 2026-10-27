"""Tests for scripts/gen3/install-cloudflared.sh and iem-tunnel.service (#72
phase E, plan Task 6): the install runs against a stand-in `ssh` and `scp`
first on PATH that play the box in a temporary folder (its home and its
root, dpkg and apt-mark too), the release comes from a local folder through
curl's file:// URLs; the unit is read as systemd reads its keys. Every value
is synthetic (P6): the version, the package's bytes, the host."""
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

from test_deploy import UNITS, expand, one, parse_unit, sudo_lines, template_lines
from test_install_dantesync import FAKE_SCP

HERE = Path(__file__).resolve().parent
INSTALL = HERE / "install-cloudflared.sh"
UNIT = HERE / "iem-tunnel.service"
HOST = "tester@gen3.test"
VERSION = "2031.4.1"
ASSET = "cloudflared-linux-amd64.deb"
STAGE = ".cache/cloudflared-install"
UNIT_PATH = "/etc/systemd/system/iem-tunnel.service"
INSTALL_SCRIPT = ("set -e\n"
                  f"sudo -n dpkg -i {STAGE}/{ASSET}\n"
                  "sudo -n apt-mark hold cloudflared\n"
                  f"sudo -n install -D -m 0644 {STAGE}/iem-tunnel.service {UNIT_PATH}\n"
                  "sudo -n systemctl daemon-reload")

# The box: `ssh` answers the install's commands from FAKE_BOX (home/ and
# root/; dpkg puts the package's bytes at /usr/bin/cloudflared, apt-mark's
# holds are a file), logging every call as one JSON line.
FAKE_SSH = r'''
import hashlib, json, os, shutil, sys
box = os.environ["FAKE_BOX"]
args = sys.argv[1:]
open(os.path.join(box, "calls.log"), "a").write(json.dumps(["ssh"] + args) + "\n")
while args and args[0] == "-o":
    args = args[2:]
cmd = " ".join(args[1:])
home, root = os.path.join(box, "home"), os.path.join(box, "root")
holds = os.path.join(box, "holds")
env = os.environ.get

def sha(path):
    return hashlib.sha256(open(path, "rb").read()).hexdigest()

if cmd.startswith("mkdir -p "):
    os.makedirs(os.path.join(home, cmd.split()[-1]), exist_ok=True)
    if env("FAKE_FLAG_ON") == "mkdir":
        open(env("IEMMIXER_EVENT_NOW"), "w").write("now\n")
elif cmd.startswith("sha256sum .cache/"):
    for path in cmd.split()[1:]:
        print((env("FAKE_STAGE_SHA") or sha(os.path.join(home, path))) + "  " + path)
elif cmd.startswith("set -e\n"):
    for line in cmd.splitlines()[1:]:
        words = line.split()
        if env("FAKE_SUDO_FAILS") and words[:3] == ["sudo", "-n", env("FAKE_SUDO_FAILS")]:
            sys.stderr.write("sudo: a password is required\n")
            sys.exit(1)
        if words[:4] == ["sudo", "-n", "dpkg", "-i"]:
            target = os.path.join(root, "usr/bin/cloudflared")
            os.makedirs(os.path.dirname(target), exist_ok=True)
            shutil.copyfile(os.path.join(home, words[4]), target)
            print("Setting up cloudflared (dpkg's own line) ...")
        elif words[:4] == ["sudo", "-n", "apt-mark", "hold"]:
            open(holds, "a").write(words[4] + "\n")
            print(words[4] + " set on hold.")
        elif words[:4] == ["sudo", "-n", "install", "-D"]:
            mode, src, dst = words[5], words[6], words[7]
            target = os.path.join(root, dst.lstrip("/"))
            os.makedirs(os.path.dirname(target), exist_ok=True)
            shutil.copyfile(os.path.join(home, src), target)
            open(os.path.join(box, "modes.log"), "a").write(f"{mode} {dst}\n")
        elif words == ["sudo", "-n", "systemctl", "daemon-reload"]:
            open(os.path.join(box, "reloads.log"), "a").write("reload\n")
        else:
            sys.stderr.write("unexpected line: " + line + "\n")
            sys.exit(97)
elif cmd == "/usr/bin/cloudflared --version":
    if not os.path.exists(os.path.join(root, "usr/bin/cloudflared")):
        sys.stderr.write("bash: /usr/bin/cloudflared: No such file or directory\n")
        sys.exit(127)
    print(env("FAKE_VERSION", "cloudflared version 2031.4.1 (built 2031-04-20-1200 UTC)"))
elif cmd == "apt-mark showhold":
    if os.path.exists(holds) and not env("FAKE_NO_HOLD"):
        sys.stdout.write(open(holds).read())
elif cmd == "sha256sum " + "/etc/systemd/system/iem-tunnel.service":
    path = cmd.split()[1]
    print((env("FAKE_UNIT_SHA") or sha(os.path.join(root, path.lstrip("/")))) + "  " + path)
else:
    sys.stderr.write("unexpected command: " + cmd + "\n")
    sys.exit(97)
'''


class InstallTests(unittest.TestCase):
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
        # The release as GitHub serves it: the .deb, no sums file.
        releases = self.tmp / "releases"
        asset = releases / VERSION / ASSET
        asset.parent.mkdir(parents=True)
        self.package = b"!<arch>\ndebian-binary a synthetic cloudflared package"
        asset.write_bytes(self.package)
        self.sha = hashlib.sha256(self.package).hexdigest()
        self.env = {**os.environ, "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}", "FAKE_BOX": str(self.box),
                    "IEMMIXER_EVENT_NOW": str(self.flag), "CLOUDFLARED_RELEASE_BASE": releases.as_uri()}

    def install(self, *, args: list[str] | None = None, **env: str) -> subprocess.CompletedProcess:
        argv = args if args is not None else ["--host", HOST, "--version", VERSION, "--sha256", self.sha]
        return subprocess.run(["bash", str(INSTALL), *argv], env={**self.env, **env}, capture_output=True,
                              text=True, timeout=60)

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

    def assert_nothing_installed(self) -> None:
        self.assertFalse(any(c.startswith("set -e\n") for c in self.remote()), self.remote())
        self.assertEqual(list((self.box / "root").iterdir()), [])

    def test_an_install_puts_cloudflared_and_its_unit_in_place_and_reads_them_back(self) -> None:
        r = self.install()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.remote(), [
            f"mkdir -p {STAGE}",
            f"sha256sum {STAGE}/{ASSET} {STAGE}/iem-tunnel.service",
            INSTALL_SCRIPT,
            "/usr/bin/cloudflared --version",
            "apt-mark showhold",
            f"sha256sum {UNIT_PATH}",
        ])
        scp = [c for c in self.calls() if c[0] == "scp"]
        self.assertEqual(len(scp), 1)
        self.assertEqual(scp[0][-1], f"{HOST}:{STAGE}/")
        # The package dpkg installed is the pinned release, the unit this
        # checkout's, mode 0644; the package held; one daemon-reload.
        self.assertEqual(self.installed("/usr/bin/cloudflared"), self.package)
        self.assertEqual(self.installed(UNIT_PATH), UNIT.read_bytes())
        self.assertEqual((self.box / "modes.log").read_text(encoding="utf-8").splitlines(), [f"0644 {UNIT_PATH}"])
        self.assertEqual((self.box / "holds").read_text(encoding="utf-8"), "cloudflared\n")
        self.assertEqual((self.box / "reloads.log").read_text(encoding="utf-8"), "reload\n")
        # The stage stays (overwritten by a later run, never deleted).
        stage = self.box / "home" / STAGE
        self.assertEqual(sorted(p.name for p in stage.iterdir()), sorted([ASSET, "iem-tunnel.service"]))
        self.assertTrue(all("BatchMode=yes" in c for c in self.calls()))
        # Only the script's own fixed lines and the version: dpkg's and
        # apt-mark's output is not passed on.
        self.assertEqual(r.stdout.splitlines(), [
            f"install-cloudflared.sh: copied to the box's ~/{STAGE}",
            f"install-cloudflared.sh: cloudflared {VERSION} installed and held; iem-tunnel.service installed, "
            "not enabled, not started",
        ])
        self.assertEqual(r.stderr, "")

    def test_nothing_is_ever_enabled_or_started(self) -> None:
        r = self.install()
        self.assertEqual(r.returncode, 0, r.stderr)
        systemctl = []
        for command in self.remote():
            for line in command.split("\n"):
                words = line.split()
                if "systemctl" in words:
                    systemctl.append(words[words.index("systemctl") + 1:])
        self.assertEqual(systemctl, [["daemon-reload"]])
        for command in self.remote():
            for word in ("enable", "start", "restart", "--now", "iem-tunnel"):
                self.assertNotIn(word, command.replace("iem-tunnel.service", "").split(), command)

    def test_a_second_install_installs_again_over_the_first(self) -> None:
        self.assertEqual(self.install().returncode, 0)
        r = self.install()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.installed(UNIT_PATH), UNIT.read_bytes())
        self.assertEqual(self.remote().count(INSTALL_SCRIPT), 2)

    def test_every_sudo_line_the_install_sends_is_in_the_template(self) -> None:
        r = self.install()
        self.assertEqual(r.returncode, 0, r.stderr)
        allowed = {(runas, text) for runas, command in template_lines() for text in expand(command)}
        lines = sudo_lines(self.remote())
        # dpkg -i, apt-mark hold, the unit's install, daemon-reload.
        self.assertEqual([command for _runas, command, _listed in lines], [
            f"dpkg -i {STAGE}/{ASSET}", "apt-mark hold cloudflared",
            f"install -D -m 0644 {STAGE}/iem-tunnel.service {UNIT_PATH}", "systemctl daemon-reload"])
        for runas, command, listed in lines:
            self.assertFalse(listed)
            self.assertIn((runas, command), allowed, command)

    def test_a_wrong_sha256_refuses_before_anything_reaches_the_box(self) -> None:
        r = self.install(args=["--host", HOST, "--version", VERSION, "--sha256", "0" * 64])
        self.assertEqual(r.returncode, 1)
        self.assertIn(f"sha256 is {self.sha}, not {'0' * 64}: nothing was copied", r.stderr)
        self.assertEqual(self.calls(), [])
        # A version without the asset: the download fails.
        r = self.install(args=["--host", HOST, "--version", "2031.9.9", "--sha256", self.sha])
        self.assertEqual(r.returncode, 1)
        self.assertIn(f"the download of {ASSET} 2031.9.9 failed: nothing was copied", r.stderr)
        self.assertEqual(self.calls(), [])

    def test_a_staged_file_that_is_not_the_one_sent_is_never_installed(self) -> None:
        r = self.install(FAKE_STAGE_SHA="e" * 64)
        self.assertEqual(r.returncode, 1)
        self.assertIn("not the downloaded cloudflared-linux-amd64.deb and this checkout's iem-tunnel.service", r.stderr)
        self.assertIn("nothing installed", r.stderr)
        self.assertEqual(self.remote()[-1], f"sha256sum {STAGE}/{ASSET} {STAGE}/iem-tunnel.service")
        self.assert_nothing_installed()

    def test_the_event_flag_refuses_before_anything(self) -> None:
        self.flag.parent.mkdir()
        self.flag.write_text("now\n", encoding="utf-8")
        r = self.install()
        self.assertEqual(r.returncode, 1)
        self.assertIn("an event is on", r.stderr)
        self.assertIn("nothing was downloaded or changed", r.stderr)
        self.assertEqual(self.calls(), [])

    def test_a_flag_that_comes_during_the_copy_refuses_the_install(self) -> None:
        self.flag.parent.mkdir()
        r = self.install(FAKE_FLAG_ON="mkdir")
        self.assertEqual(r.returncode, 1)
        self.assertIn("an event is on", r.stderr)
        self.assertIn(f"copied to ~/{STAGE}, nothing installed", r.stderr)
        self.assertEqual(self.remote()[-1], f"sha256sum {STAGE}/{ASSET} {STAGE}/iem-tunnel.service")
        self.assert_nothing_installed()

    def test_a_version_read_back_that_is_not_the_pin_fails(self) -> None:
        for said, named in (("cloudflared version 2031.4.0 (built 2031-04-20-1200 UTC)", "'2031.4.0'"),
                            ("cloudflared version DEV", "'no version'")):
            with self.subTest(said=said):
                r = self.install(FAKE_VERSION=said)
                self.assertEqual(r.returncode, 1)
                self.assertIn(f"/usr/bin/cloudflared --version names {named}, not {VERSION}", r.stderr)
                self.assertEqual(self.remote()[-1], "/usr/bin/cloudflared --version")

    def test_a_hold_that_did_not_take_fails(self) -> None:
        r = self.install(FAKE_NO_HOLD="1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("apt-mark showhold does not list cloudflared", r.stderr)
        self.assertEqual(self.remote()[-1], "apt-mark showhold")

    def test_an_installed_unit_that_is_not_this_checkout_s_fails(self) -> None:
        r = self.install(FAKE_UNIT_SHA="f" * 64)
        self.assertEqual(r.returncode, 1)
        self.assertIn(f"{UNIT_PATH}'s sha256 is {'f' * 64}, not this checkout's", r.stderr)

    def test_a_refused_sudo_stops_the_install_and_nothing_is_read_back(self) -> None:
        r = self.install(FAKE_SUDO_FAILS="dpkg")
        self.assertEqual(r.returncode, 1)
        self.assertIn("a password is required", r.stderr)
        self.assertIn("the install on the box stopped", r.stderr)
        self.assertEqual(self.remote()[-1], INSTALL_SCRIPT)
        self.assertEqual(list((self.box / "root").iterdir()), [])
        # A later step refused: what ran before it stays, and nothing follows.
        (self.box / "calls.log").unlink()
        r = self.install(FAKE_SUDO_FAILS="apt-mark")
        self.assertEqual(r.returncode, 1)
        self.assertEqual(self.remote()[-1], INSTALL_SCRIPT)
        self.assertEqual(self.installed("/usr/bin/cloudflared"), self.package)
        self.assertFalse((self.box / "root" / UNIT_PATH.lstrip("/")).exists())

    def test_a_bad_command_line_is_exit_2_and_calls_nothing(self) -> None:
        full = ["--host", HOST, "--version", VERSION, "--sha256", self.sha]

        def without(flag: str) -> list[str]:
            i = full.index(flag)
            return full[:i] + full[i + 2:]

        def with_value(flag: str, value: str) -> list[str]:
            i = full.index(flag)
            return full[:i + 1] + [value] + full[i + 2:]

        cases = [
            (without("--host"), "--host is missing"),
            (without("--version"), "--version is missing"),
            (without("--sha256"), "--sha256 is missing"),
            (full + ["--token", "zyxqw"], "unknown argument '--token'"),
            (full + ["--host"], "--host needs a value"),
            (with_value("--host", "-oProxyCommand=true"), "not an option"),
            (with_value("--version", "latest"), "not a release X.Y.Z"),
            (with_value("--version", f"v{VERSION}"), "not a release X.Y.Z"),
            (with_value("--version", "2031.4"), "not a release X.Y.Z"),
            (with_value("--sha256", self.sha.upper()), "64 lowercase hex"),
            (with_value("--sha256", self.sha[:63]), "64 lowercase hex"),
        ]
        for argv, want in cases:
            with self.subTest(want=want):
                r = self.install(args=argv)
                self.assertEqual(r.returncode, 2, (argv, r.stderr))
                self.assertIn(want, r.stderr)
                self.assertIn("usage: install-cloudflared.sh", r.stderr)
        self.assertEqual(self.calls(), [])


class TunnelUnitTests(unittest.TestCase):
    """iem-tunnel.service (phase E plan Task 6, choice 3)."""

    def setUp(self) -> None:
        self.unit = parse_unit(UNIT)

    def exec_start(self) -> list[str]:
        return one(self.unit, "Service", "ExecStart").split()

    def test_the_token_is_a_credential_never_on_a_command_line_or_in_the_environment(self) -> None:
        self.assertEqual(self.unit["Service"].get("LoadCredentialEncrypted"),
                         ["tunnel:/etc/credstore.encrypted/iemmixer-tunnel"])
        for key in ("LoadCredential", "SetCredential", "SetCredentialEncrypted", "ImportCredential", "Environment",
                    "EnvironmentFile", "PassEnvironment"):
            self.assertNotIn(key, self.unit["Service"], key)
        words = self.exec_start()
        self.assertEqual(words, ["/usr/bin/cloudflared", "tunnel", "--no-autoupdate", "--protocol", "http2",
                                 "--metrics", "127.0.0.1:20241", "run", "--token-file", "%d/tunnel"])
        # No --token with a value in any spelling: the file from the
        # credentials folder only.
        self.assertNotIn("--token", words)
        self.assertFalse([w for w in words if w.startswith(("--token=", "-token"))], words)
        self.assertEqual(words[words.index("--token-file") + 1], "%d/tunnel")
        self.assertNotIn("TUNNEL_TOKEN", UNIT.read_text(encoding="utf-8"))

    def test_it_runs_as_a_dynamic_user_without_any_capability(self) -> None:
        service = self.unit["Service"]
        self.assertEqual(one(self.unit, "Service", "DynamicUser"), "yes")
        self.assertNotIn("User", service)
        self.assertNotIn("Group", service)
        self.assertEqual(service.get("CapabilityBoundingSet"), [""])
        self.assertEqual(service.get("AmbientCapabilities"), [""])
        for key, value in (("NoNewPrivileges", "yes"), ("ProtectSystem", "strict"), ("ProtectHome", "yes"),
                           ("PrivateTmp", "yes")):
            self.assertEqual(one(self.unit, "Service", key), value, key)

    def test_a_stop_is_bounded_and_never_force_ended(self) -> None:
        # cloudflared's grace period is up to 30 s (tunnel-watchdog.md).
        self.assertEqual(one(self.unit, "Service", "TimeoutStopSec"), "45")
        self.assertEqual(one(self.unit, "Service", "SendSIGKILL"), "no")
        for key in ("KillSignal", "KillMode", "FinalKillSignal", "ExecStop"):
            self.assertNotIn(key, self.unit["Service"], key)

    def test_it_restarts_with_the_stack_s_backoff_and_keeps_trying(self) -> None:
        server = parse_unit(UNITS["iem-server"])
        self.assertEqual(one(self.unit, "Service", "Restart"), "on-failure")
        for key in ("RestartSec", "RestartSteps", "RestartMaxDelaySec"):
            self.assertEqual(one(self.unit, "Service", key), one(server, "Service", key), key)
        self.assertEqual(one(self.unit, "Unit", "StartLimitIntervalSec"), "0")

    def test_it_is_ready_once_connected_and_can_be_enabled_later(self) -> None:
        # cloudflared's own unit: ready on its first registered connection.
        self.assertEqual(one(self.unit, "Service", "Type"), "notify")
        self.assertEqual(one(self.unit, "Service", "TimeoutStartSec"), "15")
        self.assertEqual(one(self.unit, "Unit", "Wants"), "network-online.target")
        self.assertEqual(one(self.unit, "Unit", "After"), "network-online.target")
        self.assertEqual(one(self.unit, "Install", "WantedBy"), "multi-user.target")
        # Nothing ties it to the stack: the switch tool starts and stops it.
        for key in ("Requires", "BindsTo", "PartOf", "Requisite", "Upholds", "Conflicts", "Before"):
            self.assertNotIn(key, self.unit["Unit"], key)


if __name__ == "__main__":
    unittest.main()
