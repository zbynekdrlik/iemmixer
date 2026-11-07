"""Fixtures for the iempc tests (not a test module: discover never collects it).
A fake gh stands in for GitHub; every value is synthetic, and the private env file
is never read (a temp file takes its place)."""
from __future__ import annotations

import contextlib
import io
import json
import os
import shutil
import sys
import tempfile
import types
import unittest
import warnings
from pathlib import Path
from typing import Iterator
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc as ip  # noqa: E402

REAL_GH = ip.gh  # Base puts a FakeGh in its place for every test
SHA = "1234567890abcdef1234567890abcdef12345678"
ENV = {"IEMPC_TEST": "1"}


class FakeGh:
    def __init__(self) -> None:
        self.calls: list[list[str]] = []
        self.runner_reply = "A" * 29

    def __call__(self, args, timeout=ip.GH_S):
        args = list(args)
        self.calls.append(args)
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


class GuardedIempc(types.ModuleType):
    """iempc's module type in the tests (#36): a name set on iempc itself would
    shadow the binding the moved code reads, and `mock.patch.object(ip, ...)`
    removes its name again when the block ends, so any write or delete fails
    at once. Base.patch writes through `swap`."""

    def __setattr__(self, name: str, value) -> None:
        raise AssertionError(f"iempc.{name} set directly: patch it with Base.patch (#36)")

    def __delattr__(self, name: str) -> None:
        raise AssertionError(f"iempc.{name} deleted directly: patch it with Base.patch (#36)")


ip.__class__ = GuardedIempc


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
        self.patch(EVENT_NOW=self.tmp / "config" / "EVENT-NOW", STATE_DIR=self.tmp / "state", POLL_S=0.05)
        envfile = self.tmp / "iem-pc.env"
        envfile.write_text("".join(f"{k}={v}\n" for k, v in ENV.items()), encoding="utf-8")
        self.patch(env_path=lambda: envfile)
        self.gh = FakeGh()
        self.patch(gh=self.gh)

    @staticmethod
    def swap(values: dict) -> dict:
        """Sets each name in its owner module; returns the values it replaced."""
        saved = {}
        for name, value in values.items():
            module = owner(name)
            saved[name] = getattr(module, name)
            types.ModuleType.__setattr__(module, name, value)   # past GuardedIempc for iempc's own names
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

    def run_main(self, *argv: str) -> tuple[int, list[dict], str]:
        out, err = io.StringIO(), io.StringIO()
        # unittest turns ResourceWarning on: a call `guarded` left running warns
        # with its pid when collected, and the pid would land in the captured
        # stderr (a test's look for a private number once failed on pid 9427).
        with warnings.catch_warnings(), contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            warnings.simplefilter("ignore", ResourceWarning)
            code = ip.main(list(argv))
        return code, [json.loads(line) for line in out.getvalue().splitlines() if line.strip()], err.getvalue()
