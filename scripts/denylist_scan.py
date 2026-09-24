#!/usr/bin/env python3
"""Scan git content for private site data (program spec P6).

The denylist (one term per line, `#` comments) is private: a local file for
the pre-push hook, the DENYLIST secret in CI. Output never contains a term, a
matched line or an email address — only locations and the entry number.

Commit mode scans each commit's author/committer names and emails together
with its message and added lines; with `--identities FILE` it also rejects
every commit whose author or committer email is not listed there.

Matching is case-insensitive. A term that starts (ends) with a letter or digit
must not be preceded (followed) by one, where letters include diacritics and
`_` is a separator: `kit` does not hit `kitten`, `x_kit_y` is a hit, and a
term ending in `.` such as `10.0.` hits `10.0.0.5`.
"""
from __future__ import annotations

import argparse
import hashlib
import re
import shlex
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

EXIT_CLEAN = 0
EXIT_HIT = 1
EXIT_USAGE = 2


@dataclass(frozen=True)
class Hit:
    where: str
    entry: int

    def render(self) -> str:
        return f"{self.where}: denylist entry {self.entry}"


@dataclass(frozen=True)
class IdentityProblem:
    where: str
    role: str

    def render(self) -> str:
        return f"{self.where}: {self.role} email is not an allowed identity"


def load_identities(path: Path | None) -> set[str] | None:
    if path is None:
        return None
    return {
        line.strip().lower()
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.strip().startswith("#")
    }


def load_terms(path: Path) -> list[str]:
    terms: list[str] = []
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if line and not line.startswith("#"):
            terms.append(line)
    return terms


def compile_term(term: str) -> re.Pattern[str]:
    # a `\t`, `\n` or `\r` string escape right before the term is a boundary too
    left = r"(?:(?<![^\W_])|(?<=\\[ntr]))" if term[:1].isalnum() else ""
    right = r"(?![^\W_])" if term[-1:].isalnum() else ""
    return re.compile(left + re.escape(term) + right, re.IGNORECASE)


def line_key(path: str, line: str) -> str:
    return hashlib.sha256(f"{path}\n{line}".encode("utf-8")).hexdigest()


def load_allow(path: Path | None) -> set[str]:
    if path is None or not path.exists():
        return set()
    keys: set[str] = set()
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if line and not line.startswith("#"):
            keys.add(line.split()[0])
    return keys


class Scanner:
    def __init__(self, terms: list[str], allow: set[str]) -> None:
        self.patterns = [compile_term(term) for term in terms]
        self.allow = allow

    def entries_in(self, text: str) -> list[int]:
        return [number for number, pattern in enumerate(self.patterns, start=1) if pattern.search(text)]

    def scan_path(self, path: str, prefix: str) -> list[Hit]:
        return [Hit(f"{prefix}{path}: path", entry) for entry in self.entries_in(path)]

    def scan_line(self, path: str, line: str, where: str) -> list[Hit]:
        entries = self.entries_in(line)
        if not entries or line_key(path, line) in self.allow:
            return []
        return [Hit(where, entry) for entry in entries]


def git(repo: Path, *args: str) -> bytes:
    return subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True).stdout


def decode(data: bytes) -> str:
    return data.decode("utf-8", errors="replace")


def scan_tree(scanner: Scanner, repo: Path, rev: str) -> list[Hit]:
    hits: list[Hit] = []
    for entry in git(repo, "ls-tree", "-r", "-z", "--full-tree", rev).split(b"\0"):
        if not entry:
            continue
        meta, _, raw_path = entry.partition(b"\t")
        _mode, kind, obj = meta.split()
        path = decode(raw_path)
        hits += scanner.scan_path(path, "")
        if kind != b"blob":
            continue
        data = git(repo, "cat-file", "blob", decode(obj))
        if b"\0" in data:
            continue
        for number, line in enumerate(decode(data).splitlines(), start=1):
            hits += scanner.scan_line(path, line, f"{path}:{number}")
    return hits


def scan_commits(
    scanner: Scanner, repo: Path, revlist_args: list[str], identities: set[str] | None = None
) -> list[Hit | IdentityProblem]:
    hits: list[Hit | IdentityProblem] = []
    for sha in decode(git(repo, "rev-list", *revlist_args)).split():
        short = sha[:12]
        metadata = decode(git(repo, "show", "-s", "--format=%an%n%ae%n%cn%n%ce%n%B", sha))
        hits += [Hit(f"{short} commit metadata", entry) for entry in scanner.entries_in(metadata)]
        if identities is not None:
            emails = decode(git(repo, "show", "-s", "--format=%ae%n%ce", sha)).splitlines()
            for role, email in zip(("author", "committer"), emails):
                if email.strip().lower() not in identities:
                    hits.append(IdentityProblem(short, role))
        diff = decode(git(repo, "show", "--format=", "--unified=0", "--no-color", "--no-ext-diff",
                          "--no-renames", "-m", "--first-parent", sha))
        path = ""
        for line in diff.splitlines():
            if line.startswith("+++ "):
                target = line[4:]
                path = target[2:] if target.startswith("b/") else target
                hits += scanner.scan_path(path, f"{short} ")
            elif line.startswith("+"):
                hits += scanner.scan_line(path, line[1:], f"{short} {path}")
    return hits


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Scan git content for private site data.")
    parser.add_argument("--denylist", type=Path)
    parser.add_argument("--allow", type=Path)
    parser.add_argument("--identities", type=Path, help="allowed author/committer emails (commit mode)")
    parser.add_argument("--repo", type=Path, default=Path("."))
    parser.add_argument("--tree", action="append", default=[], metavar="REV")
    parser.add_argument("--commits", action="append", default=[], metavar="REVLIST")
    parser.add_argument("--hash", nargs=2, metavar=("PATH", "LINE"))
    args = parser.parse_args(argv)

    if args.hash:
        path, number = args.hash
        lines = (args.repo / path).read_text(encoding="utf-8").splitlines()
        print(line_key(path, lines[int(number) - 1]))
        return EXIT_CLEAN
    if args.denylist is None or not (args.tree or args.commits):
        parser.error("--denylist and at least one --tree or --commits are required")

    terms = load_terms(args.denylist)
    if not terms:
        print(f"denylist {args.denylist} has no terms", file=sys.stderr)
        return EXIT_USAGE
    identities = load_identities(args.identities)
    if identities is not None and not identities:
        print(f"identity list {args.identities} is empty", file=sys.stderr)
        return EXIT_USAGE
    scanner = Scanner(terms, load_allow(args.allow))
    hits: list[Hit | IdentityProblem] = []
    for rev in args.tree:
        hits += scan_tree(scanner, args.repo, rev)
    for spec in args.commits:
        hits += scan_commits(scanner, args.repo, shlex.split(spec), identities)
    for hit in hits:
        print(hit.render())
    if hits:
        print(f"{len(hits)} finding(s)", file=sys.stderr)
        return EXIT_HIT
    print("denylist: clean")
    return EXIT_CLEAN


if __name__ == "__main__":
    sys.exit(main())
