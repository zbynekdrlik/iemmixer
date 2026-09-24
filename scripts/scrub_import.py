#!/usr/bin/env python3
"""Scrub an imported tree of site data (program spec P6).

Used for every import from the private predecessor at a pinned SHA. The map
file is private (tab-separated `kind private public`, applied in order):

  literal   case-insensitive substring
  word      case-insensitive whole word (letters incl. diacritics and digits;
            `_` separates); the case style of each match is kept
  ts-ident  in *.ts only: "private" / 'private' -> IDENT, plus
            `import { IDENT } from "./support/pins";` at the top of the file

Also rewrites predecessor issue references (#123 -> reaperiem#123) in *.md
files and in the `//` comment part of *.rs, *.ts and *.js lines (rule 0).
Binary files are skipped; a path matching a literal/word rule aborts (fix the
TAKE manifest). The report lists path, rule number and count, never a value.
"""
from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path

KINDS = {"literal", "word", "ts-ident"}
ISSUE_REF = re.compile(r"(?<![\w/#-])#(\d{1,4})\b")
COMMENT_SUFFIXES = {".rs", ".ts", ".js"}
PINS_MODULE = "./support/pins"


@dataclass(frozen=True)
class Rule:
    number: int
    kind: str
    private: str
    public: str


def load_rules(path: Path) -> list[Rule]:
    rules: list[Rule] = []
    for line_no, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        if not raw.strip() or raw.startswith("#"):
            continue
        parts = raw.split("\t")
        if len(parts) != 3 or parts[0] not in KINDS or not parts[1]:
            raise ValueError(f"map line {line_no}: expected kind<TAB>private<TAB>public")
        rules.append(Rule(len(rules) + 1, parts[0], parts[1], parts[2]))
    return rules


def styled(match: str, public: str) -> str:
    if match.isupper():
        return public.upper()
    if match[:1].isupper():
        return public[:1].upper() + public[1:]
    return public


def word_pattern(private: str) -> re.Pattern[str]:
    # a `\t`, `\n` or `\r` string escape right before the term is a boundary too
    left = r"(?:(?<![^\W_])|(?<=\\[ntr]))" if private[:1].isalnum() else ""
    right = r"(?![^\W_])" if private[-1:].isalnum() else ""
    return re.compile(left + re.escape(private) + right, re.IGNORECASE)


def apply_rule(rule: Rule, text: str, suffix: str) -> tuple[str, int]:
    if rule.kind == "literal":
        return re.subn(re.escape(rule.private), lambda _m: rule.public, text, flags=re.IGNORECASE)
    if rule.kind == "word":
        return word_pattern(rule.private).subn(lambda m: styled(m.group(0), rule.public), text)
    if suffix != ".ts":
        return text, 0
    quoted = re.compile("([\"'])" + re.escape(rule.private) + r"\1")
    return quoted.subn(lambda _m: rule.public, text)


def rewrite_issue_refs(text: str, suffix: str) -> tuple[str, int]:
    if suffix == ".md":
        return ISSUE_REF.subn(r"reaperiem#\1", text)
    if suffix not in COMMENT_SUFFIXES:
        return text, 0
    total = 0
    out: list[str] = []
    for line in text.splitlines(keepends=True):
        code, sep, comment = line.partition("//")
        if sep:
            comment, count = ISSUE_REF.subn(r"reaperiem#\1", comment)
            total += count
            line = code + sep + comment
        out.append(line)
    return "".join(out), total


def scrub_file(path: Path, rules: list[Rule]) -> list[tuple[int, int]]:
    data = path.read_bytes()
    if b"\0" in data:
        return []
    text = data.decode("utf-8")
    counts: list[tuple[int, int]] = []
    idents: set[str] = set()
    for rule in rules:
        text, count = apply_rule(rule, text, path.suffix)
        if count:
            counts.append((rule.number, count))
            if rule.kind == "ts-ident":
                idents.add(rule.public)
    text, count = rewrite_issue_refs(text, path.suffix)
    if count:
        counts.append((0, count))
    if idents:
        text = f'import {{ {", ".join(sorted(idents))} }} from "{PINS_MODULE}";\n' + text
    if counts:
        path.write_text(text, encoding="utf-8")
    return counts


def scrub_tree(root: Path, rules: list[Rule]) -> dict[str, list[tuple[int, int]]]:
    path_rules = [rule for rule in rules if rule.kind in {"literal", "word"}]
    report: dict[str, list[tuple[int, int]]] = {}
    for path in sorted(p for p in root.rglob("*") if p.is_file()):
        rel = path.relative_to(root)
        if ".git" in rel.parts or "node_modules" in rel.parts:
            continue
        rel_text = rel.as_posix()
        for rule in path_rules:
            if apply_rule(rule, rel_text, "")[1]:
                raise SystemExit(f"path {rel_text} matches map rule {rule.number}: drop it from the TAKE manifest")
        counts = scrub_file(path, rules)
        if counts:
            report[rel_text] = counts
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Scrub an imported tree of site data.")
    parser.add_argument("--map", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args(argv)
    rules = load_rules(args.map)
    if not rules:
        print("scrub map has no rules", file=sys.stderr)
        return 2
    report = scrub_tree(args.root, rules)
    with args.report.open("w", encoding="utf-8") as out:
        out.write("path\trule\tcount\n")
        for rel, counts in report.items():
            for number, count in counts:
                out.write(f"{rel}\t{number}\t{count}\n")
    total = sum(count for counts in report.values() for _, count in counts)
    print(f"scrubbed {total} occurrence(s) in {len(report)} file(s); report: {args.report}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
