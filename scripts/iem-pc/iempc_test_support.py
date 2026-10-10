"""Fixtures for the iempc tests (not a test module: discover never collects it).
A fake ssh runner stands in for the PC and a fake gh for GitHub; every value is
synthetic, and the private env file is never read (a temp file takes its place)."""
from __future__ import annotations

import contextlib
import hashlib
import io
import json
import os
import re
import shutil
import sys
import tempfile
import time
import unittest
import zipfile
from pathlib import Path
from typing import Iterator
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc as ip  # noqa: E402

REAL_GH = ip.gh  # Base puts a FakeGh in its place for every test
SHA ="1234567890abcdef1234567890abcdef12345678"
SHA2 = "abcdefabcdefabcdefabcdefabcdefabcdefabcd"
RUN = 987654
ENV = {"PC_SSH": "tester@pc.test", "PC_ROOT": "X:\\root", "PC_ROOT_SCP": "/X:/root", "PC_BIN": "X:\\root\\bin"}
OK = json.dumps({"ok": True, "alarms": []})


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def make_zip(path: Path, *, sha: str = SHA, branch: str = "dev", run: int = RUN, drop: tuple[str, ...] = (),
             tamper: str | None = None, unlisted: str | None = None, rename: dict | None = None,
             manifest: dict | None = None, sums_extra: str = "", manifest_raw: bytes | None = None,
             sums_raw: bytes | None = None, extra: dict[str, bytes] | None = None) -> Path:
    """A bundle zip shaped like the CI `bundle` job's (plan Task 12); `extra`: more listed files."""
    files = {n: f"synthetic {n}".encode() for n in ip.BUNDLE_REQUIRED if n != "manifest.json"}
    files["tuning/state.ps1"] = b"synthetic tuning"
    files.update(extra or {})
    doc = manifest if manifest is not None else {"sha": sha, "branch": branch, "version": "2.0.0-dev.9", "run": run}
    files["manifest.json"] = json.dumps(doc).encode() if manifest_raw is None else manifest_raw
    for name in drop:
        files.pop(name)
    sums = "".join(f"{sha256(b)}  {n}\n" for n, b in sorted(files.items())) + sums_extra
    if tamper:
        files[tamper] = b"changed after the sums"
    if unlisted:
        files[unlisted] = b"not in the sums"
    path.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("tuning/", b"")  # a directory entry, as Compress-Archive writes one
        for name, data in files.items():
            z.writestr((rename or {}).get(name, name), data)
        z.writestr("SHA256SUMS", sums if sums_raw is None else sums_raw)
    return path


def unquote(text: str) -> str:
    return text[1:-1].replace("''", "'")


class FakePc:
    """Stands in for `ssh_ps` and `scp`: records each native call (program,
    arguments, flag mode) and module call, answers with scripted replies, and
    ends a watched call on the flag the way `guarded` does."""

    NATIVE = re.compile(r"\$x = ('(?:[^']|'')*') ; \$a = @\(((?:'(?:[^']|'')*'(?:, )?)*)\) ; .*?\$r = @\(& \$x @a")
    WANT = re.compile(r"\$iemH -cne '([0-9a-f]{64})'")

    def __init__(self) -> None:
        self.calls: list[tuple[str, list[str], str]] = []
        self.timeouts: list[float] = []
        self.native_scripts: list[str] = []
        self.modules: list[tuple[str, str]] = []
        self.scps: list[tuple[str, str, str]] = []
        self.replies: dict = {}  # args -> (exit, stdout[, stderr]) or a callable returning one
        self.module_result = "ok"
        # A module script holding the text gets this result (or a callable's) instead: the
        # elevated tuning folder's profile check after every activate (iempc_tuning) finds none,
        # and the admin-only bin's install (#15) reads back the hash it was given.
        self.texts: dict = {"profile.json": False, "$iemDst": self.bin_installed}
        # An iemmode call's note (#15): the admin-only copy did not read back, PC_BIN ran.
        self.bin_note = None

    def bin_installed(self) -> str:
        return self.WANT.findall(self.modules[-1][0])[-1]

    def ssh_ps(self, env, script, timeout, event):
        m = self.NATIVE.search(script)
        if m:
            exe = unquote(m.group(1))
            args = [unquote(a) for a in re.findall(r"'(?:[^']|'')*'", m.group(2))]
            self.calls.append((exe.rsplit("\\", 1)[-1], args, event))
            self.timeouts.append(timeout)
            self.native_scripts.append(script)
            reply = self.replies.get(tuple(args), (0, OK))
            code, out, err = (*(reply() if callable(reply) else reply), "")[:3]
            doc = {"exit": code, "out": out, "err": err, "note": self.bin_note if "$iemUse" in script else None}
        else:
            self.modules.append((script, event))
            r = next((v for k, v in self.texts.items() if k in script), self.module_result)
            doc = {"ok": True, "r": r() if callable(r) else r}
        if event != "ignore" and ip.event_now():
            raise ip.EventNow()
        return "PowerShell noise\n" + json.dumps(doc) + "\n"

    def scp(self, src, dst, event):
        self.scps.append((src, dst, event))


class FakeGh:
    def __init__(self, artifact: Path) -> None:
        self.calls: list[list[str]] = []
        self.heads = {"dev": SHA, "main": SHA2}
        self.runs = [{"databaseId": RUN, "headSha": SHA, "event": "push", "headBranch": "dev", "conclusion": "success"}]
        self.jobs = {RUN: [{"name": "bundle", "conclusion": "success"}, {"name": "attest", "conclusion": "success"}]}
        self.artifact = artifact
        self.attest_ok = True
        self.runner_reply = "A" * 29
        self.on_list = None
        self.on_download = None

    def __call__(self, args, timeout=ip.GH_S):
        args = list(args)
        self.calls.append(args)
        if args[0] == "api" and args[1].startswith(f"repos/{ip.REPO}/git/ref/heads/"):
            return self.heads[args[1].rsplit("/", 1)[1]] + "\n"
        if args[:2] == ["run", "list"]:
            if self.on_list:
                self.on_list()
            sha = args[args.index("--commit") + 1]
            return json.dumps([r for r in self.runs if r["headSha"] == sha])
        if args[:2] == ["run", "view"]:
            return json.dumps({"jobs": self.jobs.get(int(args[2]), [])})
        if args[:2] == ["run", "download"]:
            dest = Path(args[args.index("-D") + 1])
            dest.mkdir(parents=True, exist_ok=True)
            shutil.copy(self.artifact, dest / self.artifact.name)
            if self.on_download:
                self.on_download()
            return ""
        if args[:2] == ["attestation", "verify"]:
            if not self.attest_ok:
                raise ip.StepError("gh attestation verify failed (exit 1): no attestation matched")
            return ""
        if args[:2] == ["workflow", "run"]:
            return ""
        if args[:3] == ["api", "-X", "POST"]:
            return self.runner_reply + "\n"
        raise AssertionError(f"unexpected gh call {args}")

    def named(self, *prefix: str) -> list[list[str]]:
        return [c for c in self.calls if c[:len(prefix)] == list(prefix)]


class FakeClock:
    """A monotonic clock that moves only when a fake reply waits (`sleep`)."""

    def __init__(self) -> None:
        self.t = 1024.0   # binary fractions below stay exact

    def now(self) -> float:
        return self.t

    def sleep(self, seconds: float) -> None:
        self.t += seconds


# The modules iempc is made of (#36): a test patches a name in the one module that
# holds it, where the code that reads it looks it up. `ip.<name>` reads it live.
MODULES = (ip, *ip.SPLIT)
IEMPC_NAMES = frozenset(vars(ip))


def owner(name: str):
    """The one module of MODULES that holds `name`. A second holder would be
    a copy that a patch never reaches (#36), so it fails the test."""
    holders = [m for m in MODULES if name in vars(m)]
    if len(holders) != 1:
        raise AssertionError(f"{name} is held by {[m.__name__ for m in holders]}, not by exactly one iempc module")
    return holders[0]


class Base(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.addCleanup(self.nothing_set_on_iempc)
        self.patch(EVENT_NOW=self.tmp / "config" / "EVENT-NOW", STATE_DIR=self.tmp / "state",
                   SPIKE_STATE=self.tmp / "spike-window.json", POLL_S=0.05)
        self.spike_log = self.tmp / "spike.log"
        self.spike_done = self.tmp / "spike.done"
        self.patch(SPIKE=self.write_spike(0))
        envfile = self.tmp / "iem-pc.env"
        envfile.write_text("".join(f"{k}={v}\n" for k, v in ENV.items()), encoding="utf-8")
        self.patch(env_path=lambda: envfile)
        self.pc = FakePc()
        self.patch(ssh_ps=self.pc.ssh_ps, scp=self.pc.scp)
        self.artifact = make_zip(self.tmp / "artifact" / f"iemmixer-{SHA}.zip")
        self.gh = FakeGh(self.artifact)
        self.patch(gh=self.gh)

    @staticmethod
    def swap(values: dict) -> dict:
        """Sets each name in its owner module; returns the values it replaced."""
        saved = {}
        for name, value in values.items():
            module = owner(name)
            saved[name] = getattr(module, name)
            setattr(module, name, value)
        return saved

    def patch(self, **values) -> None:
        """Patches names of iempc's modules for the rest of the test."""
        self.addCleanup(self.swap, self.swap(values))

    @contextlib.contextmanager
    def patched(self, **values) -> Iterator[None]:
        """Patches names of iempc's modules inside a `with` block."""
        saved = self.swap(values)
        try:
            yield
        finally:
            self.swap(saved)

    def nothing_set_on_iempc(self) -> None:
        """A name set on `ip` itself never reaches a moved module's code (#36):
        use `patch`."""
        added = set(vars(ip)) - IEMPC_NAMES
        for name in added:
            delattr(ip, name)
        self.assertEqual(added, set(), "set on iempc itself: patch it with Base.patch")

    def write_spike(self, code: int, delay: float = 0.0) -> Path:
        """A stand-in spike_window.py: logs its arguments at start, takes
        `delay` seconds, marks its end in spike_done, exits with `code`."""
        p = self.tmp / "spike_window.py"
        p.write_text(f"import sys, time\nopen({str(self.spike_log)!r}, 'a').write(' '.join(sys.argv[1:]) + '\\n')\n"
                     f"time.sleep({delay})\nopen({str(self.spike_done)!r}, 'a').write('ended\\n')\n"
                     f"print('preempted')\nsys.exit({code})\n", encoding="utf-8")
        return p

    def wait_for_spike_end(self) -> None:
        """A spike stand-in left running ends by itself (never force-ended)."""
        deadline = time.monotonic() + 15
        while not self.spike_done.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertTrue(self.spike_done.exists(), "the spike stand-in never ended")

    def route_to_real_gh(self, *prefix: str) -> None:
        """gh calls starting with `prefix` go through the real wrapper (and a
        stand-in gh program on PATH); every other call stays with FakeGh."""
        fake = self.gh

        def mixed(args, timeout=ip.GH_S):
            if list(args[:len(prefix)]) == list(prefix):
                return REAL_GH(args, timeout)
            return fake(args, timeout)

        self.patch(gh=mixed)

    def gh_program(self, body: str) -> None:
        """A stand-in `gh` program first on PATH, running `body` (sys, time imported)."""
        d = self.tmp / "bin"
        d.mkdir(exist_ok=True)
        p = d / "gh"
        p.write_text(f"#!{sys.executable}\nimport sys, time\n{body}\n", encoding="utf-8")
        p.chmod(0o755)
        self.set_path(f"{d}{os.pathsep}{os.environ.get('PATH', '')}")

    def set_path(self, path: str) -> None:
        patcher = mock.patch.dict(os.environ, {"PATH": path})
        patcher.start()
        self.addCleanup(patcher.stop)

    def flag(self) -> None:
        ip.EVENT_NOW.parent.mkdir(parents=True, exist_ok=True)
        ip.EVENT_NOW.write_text("2026-09-27T20:00:00+02:00\n", encoding="utf-8")

    def open_window(self, **kw) -> None:
        state = {"id": "w1", "card": "free", "pref_original": 64, "pref_current": None, "closed": False}
        state.update(kw)
        ip.SPIKE_STATE.write_text(json.dumps(state), encoding="utf-8")

    def run_main(self, *argv: str) -> tuple[int, list[dict], str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = ip.main(list(argv))
        return code, [json.loads(line) for line in out.getvalue().splitlines() if line.strip()], err.getvalue()

    def fetched(self) -> dict:
        code, docs, err = self.run_main("fetch-bundle", "--sha", SHA)
        self.assertEqual(code, 0, err)
        return docs[-1]

