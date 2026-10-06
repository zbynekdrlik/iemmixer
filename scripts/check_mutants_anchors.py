#!/usr/bin/env python3
"""Line anchors in `.cargo/mutants.toml` still point at the code they name (#32 D1).

An `exclude_re` entry may pin an excluded mutant to one source line, e.g.
`"iem-engine/src/rt\\\\.rs:1204:.*replace > with >= in …"`, when the same
function holds other mutants of the same kind that are NOT equivalent and must
stay mutated. cargo-mutants matches the regex against mutant names such as
`crates/iem-engine/src/rt.rs:1204:13: replace > with >= in …`, so a line anchor
goes stale without a sound when code above it moves: it then excludes nothing
(the equivalent mutant comes back as a survivor) or a different mutant that
now sits on that line.

So every anchored entry carries, in the comment block right above it, one
comment per anchored line with that line's source text:

    # anchor 1204: if budget > 0
    "iem-engine/src/rt\\\\.rs:1204:.*replace > with >= in …",

and this check fails when
- an anchor (`<path>\\\\.rs:<N>:` or `<path>\\\\.rs:(<N>|<M>…):`) has no such
  comment, or an `anchor` comment names a line its entry does not anchor, or
  stands anywhere else;
- an entry writes a line anchor in any other form (an unescaped `.rs:`, …);
- the anchor's path is not exactly one tracked `.rs` file ending in `<path>.rs`;
- source line N, stripped, differs from the comment's text (the report names
  the line(s) where that text is now, if any).

Lines are counted by `\\n` only, as cargo-mutants numbers them (never
`str.splitlines()`, which also splits on U+2028 and friends).
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONFIG = Path(".cargo/mutants.toml")

BLOCK_START = re.compile(r"^exclude_re\s*=\s*\[\s*$")
ANCHOR_COMMENT = re.compile(r"^#\s*anchor\s+(\d+):(.*)$")
# In the regex text (TOML already decoded): `path\.rs:N:` or `path\.rs:(N|M):`.
ANCHOR = re.compile(r"([A-Za-z0-9_./-]+)\\\.rs:(\d+|\(\d+(?:\|\d+)*\)):")
# Anything that looks like a line anchor, escaped or not.
LOOSE = re.compile(r"\.rs:[(\d]")


@dataclass(frozen=True)
class Entry:
    line: int  # 1-based line of the entry in mutants.toml
    regex: str
    comments: tuple[int, ...]  # 1-based lines of the comment block right above


@dataclass(frozen=True)
class Parsed:
    entries: tuple[Entry, ...]
    anchor_comments: dict[int, tuple[int, str]]  # config line -> (source line, text)
    problems: tuple[str, ...]


def decode_string(line: str) -> str | None:
    """The TOML string of an `exclude_re` entry line (`"…",`), else None."""
    body = line.strip()
    if body.endswith(","):
        body = body[:-1].rstrip()
    try:
        value = tomllib.loads(f"v = {body}").get("v")
    except tomllib.TOMLDecodeError:
        return None
    return value if isinstance(value, str) else None


def parse_config(text: str) -> Parsed:
    lines = text.split("\n")
    problems: list[str] = []
    anchor_comments: dict[int, tuple[int, str]] = {}
    for n, raw in enumerate(lines, start=1):
        m = ANCHOR_COMMENT.match(raw.strip())
        if m:
            anchor_comments[n] = (int(m[1]), m[2].strip())
    start = next((i for i, raw in enumerate(lines) if BLOCK_START.match(raw.strip())), None)
    if start is None:
        return Parsed((), anchor_comments, ("no `exclude_re = [` block",))
    entries: list[Entry] = []
    block: list[int] = []
    closed = False
    for i in range(start + 1, len(lines)):
        body = lines[i].strip()
        n = i + 1
        if body == "]":
            closed = True
            break
        if not body:
            block = []
            continue
        if body.startswith("#"):
            block.append(n)
            continue
        value = decode_string(body)
        if value is None:
            problems.append(f"line {n}: not one quoted string per line in exclude_re: {body}")
            block = []
            continue
        entries.append(Entry(n, value, tuple(block)))
        block = []
    if not closed:
        problems.append("the exclude_re block has no closing `]` line")
    try:
        declared = tomllib.loads(text).get("exclude_re", [])
    except tomllib.TOMLDecodeError as e:
        problems.append(f"not valid TOML: {e}")
        declared = []
    if [e.regex for e in entries] != declared:
        problems.append(
            "the entries read line by line differ from exclude_re as TOML reads it "
            "(write one quoted string per line, comments on their own lines)"
        )
    return Parsed(tuple(entries), anchor_comments, tuple(problems))


def anchored_lines(spec: str) -> list[int]:
    return [int(x) for x in spec.strip("()").split("|")]


def resolve(path: str, tracked: list[str]) -> list[str]:
    name = f"{path}.rs"
    return [f for f in tracked if f == name or f.endswith("/" + name)]


def source_lines(text: str) -> list[str]:
    """The file's lines as cargo-mutants numbers them: split on `\\n` only."""
    lines = [s.rstrip("\r") for s in text.split("\n")]
    if lines and lines[-1] == "":
        lines.pop()
    return lines


def check(text: str, tracked: list[str], read: Callable[[str], str]) -> tuple[list[str], int]:
    """Problems with the config's line anchors, and how many anchors held."""
    parsed = parse_config(text)
    problems = list(parsed.problems)
    used: set[int] = set()
    held = 0
    sources: dict[str, list[str]] = {}
    for entry in parsed.entries:
        anchors = ANCHOR.findall(entry.regex)
        if len(LOOSE.findall(entry.regex)) != len(anchors):
            problems.append(
                f"line {entry.line}: a line anchor in an unsupported form "
                "(write <path>\\\\.rs:<N>: or <path>\\\\.rs:(<N>|<M>):): " + entry.regex
            )
            continue
        mine = {n: parsed.anchor_comments[n] for n in entry.comments if n in parsed.anchor_comments}
        wanted: list[tuple[str, int]] = [(p, line) for p, spec in anchors for line in anchored_lines(spec)]
        for path, line in wanted:
            found = [n for n, (src, _) in mine.items() if src == line]
            if len(found) != 1:
                problems.append(
                    f"line {entry.line}: anchor {path}.rs:{line} needs exactly one "
                    f"`# anchor {line}: <source text>` comment right above it (found {len(found)})"
                )
                continue
            used.add(found[0])
            files = resolve(path, tracked)
            if len(files) != 1:
                problems.append(
                    f"line {entry.line}: {path}.rs matches {len(files)} tracked files, not one"
                    + (f": {', '.join(files)}" if files else "")
                )
                continue
            src = sources.get(files[0])
            if src is None:
                src = source_lines(read(files[0]))
                sources[files[0]] = src
            want = mine[found[0]][1]
            if line > len(src):
                problems.append(f"line {entry.line}: {files[0]} has {len(src)} lines, the anchor names {line}")
                continue
            if src[line - 1].strip() != want:
                now = [str(k) for k, s in enumerate(src, start=1) if s.strip() == want]
                where = f"that text is now at line {', '.join(now)}" if now else "that text is not in the file"
                problems.append(
                    f"line {entry.line}: {files[0]}:{line} reads `{src[line - 1].strip()}`, "
                    f"the anchor expects `{want}` ({where})"
                )
                continue
            held += 1
    for n in sorted(set(parsed.anchor_comments) - used):
        src_line, _ = parsed.anchor_comments[n]
        problems.append(
            f"line {n}: `# anchor {src_line}:` belongs to no line anchor of the entry right below it"
        )
    return problems, held


def tracked_rs(root: Path) -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", "-z", "--", "*.rs"],
        cwd=root,
        check=True,
        capture_output=True,
    ).stdout
    return [p for p in out.decode("utf-8").split("\0") if p]


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", type=Path, default=ROOT, help="the repository (default: this one)")
    args = ap.parse_args(argv)
    root: Path = args.root
    text = (root / CONFIG).read_text(encoding="utf-8")
    problems, held = check(text, tracked_rs(root), lambda rel: (root / rel).read_text(encoding="utf-8"))
    for p in problems:
        print(f"{CONFIG}: {p}", file=sys.stderr)
    if problems:
        return 1
    print(f"{CONFIG}: {held} line anchor(s) point at their code")
    return 0


if __name__ == "__main__":
    sys.exit(main())
