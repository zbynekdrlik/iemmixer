"""Shared fixtures of the denylist scanner's tests (scripts/denylist_scan.py): a scratch repository per
test, the invented TERMS, and helpers to commit into it and scan it through main(). Not a test module."""
from __future__ import annotations

import contextlib
import io
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import denylist_scan as ds  # noqa: E402

TERMS = ["zyxname", "10.9.", "ghost-host.example"]
REDACTED_MARKER = "[redacted]"
# #32 review m7: the stated budget for binary content (public-repo-hygiene.md) -- a 4 MiB blob of
# pseudo-random bytes against 40 invented terms, in tree mode through main()
BUDGET_BLOB = 4 << 20
BUDGET_CPU_PER_MIB = 1.0          # seconds of this process's CPU per MiB of blob
BUDGET_MEMORY_BEYOND_BLOB = 24 << 20  # peak Python allocation on top of two copies of the blob
BUDGET_TERMS = ([f"qz{letter}xw{letter}k" for letter in "abcdefghijklmnopqrstuvwxyz"]  # 7 characters
                + [f"ďq{letter}zyx" for letter in "abcdefgh"] + ["qxv", "zqk", "xwq", "qzzx", "kqxz", "zxqv"])


def git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)


def git_out(repo: Path, *args: str, stdin: bytes = b"") -> str:
    return subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True,
                          input=stdin).stdout.decode().strip()


class ScanTestCase(unittest.TestCase):
    """A scratch repository and a denylist of TERMS (entries 1-3) per test."""

    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.repo = self.tmp / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-q", "-b", "main")
        git(self.repo, "config", "user.email", "test@example.org")
        git(self.repo, "config", "user.name", "test")
        git(self.repo, "config", "commit.gpgsign", "false")
        self.deny = self.tmp / "deny.txt"
        self.deny.write_text("# test terms\n" + "\n".join(TERMS) + "\n", encoding="utf-8")

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp)

    def commit(self, files: dict[str, str | bytes], message: str = "change") -> None:
        for rel, content in files.items():
            path = self.repo / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content if isinstance(content, bytes) else content.encode("utf-8"))
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", message)

    def scan(self, *extra: str) -> tuple[int, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = ds.main(["--denylist", str(self.deny), "--repo", str(self.repo), *extra])
        return code, out.getvalue() + err.getvalue()

    def add_terms(self, *terms: str) -> None:
        self.deny.write_text("\n".join([*TERMS, *terms]) + "\n", encoding="utf-8")

    def assert_found_in_both_modes_as(self, files: dict[str, str | bytes], term_letters: str) -> str:
        self.bases = getattr(self, "bases", 0) + 1  # a fresh base commit per subTest
        self.commit({"base.txt": f"base {self.bases}\n"})
        self.commit(files, message="add content in another encoding")
        code, tree_out = self.scan("--tree", "HEAD")
        self.assertEqual(code, 1, "tree mode missed the term")
        code, commit_out = self.scan("--commits", "HEAD~1..HEAD")
        self.assertEqual(code, 1, "commit mode missed the term")
        for out in (tree_out, commit_out):
            self.assertNotIn(term_letters, out.lower())
        return tree_out

    def hash_key(self, target: str, number: str) -> str:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(ds.main(["--repo", str(self.repo), "--hash", target, number]), 0)
        return out.getvalue().strip()
