#!/usr/bin/env python3
"""Scan git content for private site data (program spec P6).

The denylist (one term per line, `#` comments) is private: a local file for
the pre-push hook, the DENYLIST secret in CI. Output never contains a term, a
matched line or an email address — only locations and the entry number, each
finding line starting with `tree` or a commit's short SHA (never with a path,
so a path beginning `::` cannot read as a CI workflow command). A path
component that holds a term, or any non-ASCII character, is printed as
`[redacted]` (the whole path when a term spans components), other components
have their control characters escaped.

Content is read as numbered units in Batches of about CHUNK bytes, so CPU and
memory stay bounded (#32 review m7): the lines of text; the decoded lines of
UTF-32 / UTF-16 text, then its byte runs (a binary may only look like wide
text); for other content holding a NUL byte (binary) its text runs -- byte runs
without control characters, then embedded UTF-16 strings -- located as `run N`.
In a binary run a term shorter than MIN_BINARY_TERM counts only when the run is
LONG_TEXT_RUN bytes of valid UTF-8, since random bytes form short words by
chance. Tree mode reads each blob's own bytes (cat-file applies no
.gitattributes); `--hash PATH N` (or `<rev>:<path>`) numbers units the same way.
A git-lfs pointer blob or a `.gitattributes` `filter=lfs` line is a finding: the
content it stands for is not in the repository to scan.

Commit mode scans each commit's author/committer names and emails together
with its message, its added lines and every added/modified path -- including an
empty or binary file, whose path the unified diff omits, enumerated via
`git diff-tree`. Added lines come from `git show --text --no-textconv`, so a
`binary` / `-diff` attribute or a textconv driver cannot hide them, with the
output pinned against local config (`--src-prefix=a/ --dst-prefix=b/
--no-relative --diff-merges=first-parent --root --no-show-signature`, and the
metadata with `--encoding=UTF-8 --no-show-signature`); a changed blob that is
not plain text is read whole and the units the old blob lacks are reported as
`<sha> <path>:<unit>`. With `--identities FILE` it also rejects every commit
whose author or committer email is not exactly one listed there (read
NUL-separated; an email holding a line separator is never allowed). Lines are
split on `\n` only (not str.splitlines()), so a term after a CR/VT/FF/NEL/U+2028
cannot slip past, and a malformed C-quoted path never crashes the scan
(`unquote_c` keeps a bad escape literal rather than dropping the bytes that
follow).

Text is decoded losslessly (UTF-8, surrogateescape) and matched in every
reading (Views): the escapes decoded (`\\uXXXX`, `\\u{X}`, `\\UXXXXXXXX`, XML /
HTML numeric references, C / Rust / Python byte escapes, percent-encoding);
double-encoded UTF-8 read back; invisible characters removed and compatibility
letters (fullwidth ...) read in NFKC; undecodable bytes re-read as cp1250,
Latin-1, ISO-8859-2 and cp852. Matching is case-insensitive. A term that
starts (ends) with a letter or digit must not be preceded (followed) by one,
where letters include diacritics and `_` is a separator, and an escape sequence
right before it is a boundary too: `kit` does not hit `kitten`, `x_kit_y` and
`\\0kit` are hits, and a term ending in `.` such as `10.0.` hits `10.0.0.5`.
"""
from __future__ import annotations

import argparse
import hashlib
import os
import re
import shlex
import subprocess
import sys
import unicodedata
from collections.abc import Callable, Iterable, Iterator
from dataclasses import dataclass
from pathlib import Path

from denylist_containers import Member, Problem, blob_key, container_kind, expand
from denylist_content import (MIN_BINARY_TERM, OVERLAP_PER_CHARACTER, SEGMENT_OVERLAP, Batch, batches, is_plain_text,
                              line_batches, long_text_run, unit_key)
from denylist_readings import SEP, Views, decode, fold, from_git_latin1, nfc

EXIT_CLEAN = 0
EXIT_HIT = 1
EXIT_USAGE = 2

REDACTED = "[redacted]"
GITLINK = b"160000"  # a submodule entry: its object is a commit, not a blob
# git-lfs keeps a file's content on its server and only a pointer in the repository (#32 review m9)
_LFS_POINTER = (b"version https://git-lfs.github.com/spec/", b"version https://hawser.github.com/spec/")
_LFS_FILTER = re.compile(r"(?<!\S)filter=lfs(?!\S)")
LFS_POINTER = "git-lfs pointer, its content is not in the repository to scan"
LFS_FILTER = "git-lfs filter, the content of the files it matches is never in the repository to scan"
# a commit's metadata (`git show -s`), pinned against local config: --encoding=UTF-8 beats
# i18n.logOutputEncoding (UTF-16 puts a NUL in every character, ISO-8859-2 re-encodes letters that
# no reading decodes back); --no-show-signature beats log.showSignature, whose "No signature"
# would land in the first field
METADATA = ("show", "-s", "--encoding=UTF-8", "--no-show-signature")
# every character str.splitlines() breaks a line at
LINE_SEPARATORS = frozenset("\n\r\x0b\x0c\x1c\x1d\x1e\x85\u2028\u2029")
# git's C-quoting of a path in a diff header (core.quotePath)
C_ESCAPES = {"a": 7, "b": 8, "t": 9, "n": 10, "v": 11, "f": 12, "r": 13, '"': 34, "\\": 92}


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


@dataclass(frozen=True)
class Unscannable:
    where: str
    what: str

    def render(self) -> str:
        return f"{self.where}: {self.what}"


Finding = Hit | IdentityProblem | Unscannable


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


# an escape sequence right before a term is a word boundary even though it ends in a letter or a
# digit: `\t`, `\0`, `\101`, `\x41`, `\u0041`, `\U00000041` (byte-string fixtures: b"\0Program 1\0")
_ESCAPE_BEFORE = (r"(?<=\\[0-7abfnrtv])", r"(?<=\\[0-7]{2})", r"(?<=\\[0-7]{3})",
                  r"(?<=\\x[0-9A-Fa-f]{2})", r"(?<=\\u[0-9A-Fa-f]{4})", r"(?<=\\U[0-9A-Fa-f]{8})")


# between the words of a multi-word term: any run of whitespace (a no-break space, a tab, a line
# break in a wrapped commit message) and batch separators, so a term wrapped onto the next line of
# a file is found too, on the line it starts on (#32 F5 m1)
_GAP = rf"[\s{SEP}]+"


def spelled(term: str) -> str:
    """The regex of the bare term: its words, any whitespace between them."""
    return _GAP.join(re.escape(word) for word in term.split())


def compile_term(term: str) -> re.Pattern[str]:
    left = "(?:(?<![^\\W_])|" + "|".join(_ESCAPE_BEFORE) + ")" if term[:1].isalnum() else ""
    right = r"(?![^\W_])" if term[-1:].isalnum() else ""
    return re.compile(left + spelled(term) + right, re.IGNORECASE)


def line_key(path: str, line: str) -> str:
    # surrogateescape: an undecodable byte (see decode) hashes as that exact byte
    return hashlib.sha256(f"{path}\n{line}".encode("utf-8", "surrogateescape")).hexdigest()


def load_allow(path: Path | None) -> set[str]:
    if path is None or not path.exists():
        return set()
    keys: set[str] = set()
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if line and not line.startswith("#"):
            keys.add(line.split()[0])
    return keys


def printable(text: str) -> str:
    """Control characters escaped, so a path cannot inject lines (a CI `::error` command) into
    the log."""
    return "".join(char if char.isprintable() else f"\\x{ord(char):02x}" if ord(char) < 0x100
                   else f"\\u{ord(char):04x}" for char in text)


_ASCII_WORD = re.compile("[a-z0-9]{2,}")
# Latin letters NFKD keeps whole, with their usual ASCII spellings
_ASCII_LETTERS = str.maketrans({"ł": "l", "Ł": "L", "đ": "d", "Đ": "D", "ø": "o", "Ø": "O", "ß": "ss", "ẞ": "SS",
                                "æ": "ae", "Æ": "AE", "œ": "oe", "Œ": "OE", "ħ": "h", "Ħ": "H", "ŧ": "t", "Ŧ": "T",
                                "ı": "i", "ð": "d", "Ð": "D", "þ": "th", "Þ": "TH"})


def ascii_spelling(term: str) -> str:
    """The term as a name is written in a path, an e-mail address, an identifier or a host name: its
    diacritics dropped (NFKD, combining marks removed) and the Latin letters NFKD keeps whole spelled
    in ASCII (`ł` l, `ß` ss). An entry is matched in this spelling too (#32 F5 MAJOR)."""
    decomposed = unicodedata.normalize("NFKD", term.translate(_ASCII_LETTERS))
    return nfc("".join(char for char in decomposed if unicodedata.category(char) != "Mn"))


@dataclass(frozen=True)
class Term:
    entry: int
    literal: re.Pattern[str]  # the bare term, same flags: searched ~15x faster than with lookarounds
    pattern: re.Pattern[str]  # the term with its word boundaries, tried at each literal hit
    short: bool               # under MIN_BINARY_TERM characters (see LONG_TEXT_RUN)
    ascii: bool
    folded: str               # an ASCII term's longest word folded; else its longest folded ASCII word, or ""

    @classmethod
    def of(cls, entry: int, term: str) -> Term:
        term = nfc(term)
        if term.isascii():  # the longest word: any whitespace may stand between the words
            folded = max(fold(term).split(), key=len)
        else:
            folded = max(_ASCII_WORD.findall(fold(term)), key=len, default="")
        return cls(entry, re.compile(spelled(term), re.IGNORECASE), compile_term(term),
                   len(term) < MIN_BINARY_TERM, term.isascii(), folded)

    def starts(self, view: str) -> Iterator[int]:
        """Every position the term matches at, overlapping occurrences included."""
        found = self.literal.search(view)
        while found:
            if self.pattern.match(view, found.start()):
                yield found.start()
            found = self.literal.search(view, found.start() + 1)


class Scanner:
    def __init__(self, terms: list[str], allow: set[str]) -> None:
        # each entry's term, and its ASCII spelling when that differs (#32 F5 MAJOR)
        self.terms = [Term.of(entry, spelling) for entry, term in enumerate(terms, start=1)
                      for spelling in dict.fromkeys((nfc(term), ascii_spelling(nfc(term)))) if spelling.strip()]
        self.allow = allow
        self.overlap = max(SEGMENT_OVERLAP, OVERLAP_PER_CHARACTER * max(map(len, terms), default=0))

    def batches(self, data: bytes) -> Iterator[Batch]:
        """batches(data), a long unit's segments overlapping by at least the longest term."""
        return batches(data, self.overlap)

    def entries_in(self, text: str) -> list[int]:
        """The entries found in any reading of the text (other encodings, escapes decoded)."""
        views = Views(text)
        return sorted({term.entry for term in self.terms
                       if any(next(term.starts(view), None) is not None for view in views.for_term(term))})

    def batch_hits(self, batch: Batch) -> list[tuple[int, int]]:
        """(unit position in the batch, entry number) of every term found in a batch, sorted.

        One search per term and reading over the whole batch -- a binary file has ~10^5 runs per
        MiB -- with each match mapped to the unit it starts in by the SEPs before it (a SEP is a word
        boundary like the end of a unit, and only the gap between the words of a multi-word term
        crosses one: such a term wrapped onto the next line counts on the line it starts on)."""
        views = Views(batch.text())
        per_view: dict[int, tuple[str, list[tuple[int, int]]]] = {}
        for index, term in enumerate(self.terms):
            for view in views.for_term(term):
                starts = [(start, index) for start in term.starts(view)]
                if starts:
                    per_view.setdefault(id(view), (view, []))[1].extend(starts)
        found: set[tuple[int, int]] = set()
        for view, starts in per_view.values():
            unit = last = 0
            for start, index in sorted(starts):
                unit += view.count(SEP, last, start)
                last = start
                found.add((unit, index))
        if batch.runs and found:  # a short term counts only in a long text run (MIN_BINARY_TERM)
            texts = batch.texts()
            found = {(unit, index) for unit, index in found
                     if not self.terms[index].short or long_text_run(texts[unit])}
        return sorted({(unit, self.terms[index].entry) for unit, index in found})

    def findings(self, path: str, batches: Iterable[Batch]) -> list[tuple[str, str, int]]:
        """(unit label, unit key, entry number) of every term found and not allowlisted, once per
        unit and entry (the segments of a long unit overlap)."""
        found: dict[tuple[str, int], str] = {}
        for batch in batches:
            hits = self.batch_hits(batch)
            if hits:
                keys = batch.keys()
                for unit, entry in hits:
                    label = f"{batch.label}{batch.first + unit}"
                    if (label, entry) not in found and line_key(path, keys[unit]) not in self.allow:
                        found[label, entry] = keys[unit]
        return [(label, key, entry) for (label, entry), key in found.items()]

    def shown(self, path: str) -> str:
        """The path as printed: each component holding a term (in any reading) or any non-ASCII
        character is redacted -- no set of readings can be proven complete, and printed in a
        reading the scanner lacks, the ASCII tail of a term (`ĺˇqxwzy`, a cp1250 `\\xefqxwzy`)
        would reach the log -- and the others have their control characters escaped; the whole
        path is redacted when a term spans components (a term without `/` always matches inside
        one component) or the printed form holds one."""
        whole = set(self.entries_in(path))
        parts = path.split("/")
        part_hits = [set(self.entries_in(part)) if whole else set() for part in parts]
        if not whole <= set().union(*part_hits):
            return REDACTED
        kept = "/".join(REDACTED if hit or not part.isascii() else printable(part)
                        for part, hit in zip(parts, part_hits, strict=True))
        return REDACTED if self.entries_in(kept) else kept

    def scan_path(self, path: str, prefix: str) -> list[Hit]:
        return [Hit(f"{prefix}{self.shown(path)}: path", entry) for entry in self.entries_in(path)]


# Local repository state must not redirect what the scan reads (#32 F5 m7): a replace ref swaps an
# object for another in cat-file / ls-tree / show / rev-list (--no-replace-objects), and a grafts file
# gives commits other parents, cutting history out of rev-list (an empty GIT_GRAFT_FILE)
GIT_ENV = {"GIT_GRAFT_FILE": os.devnull, "GIT_NO_REPLACE_OBJECTS": "1"}


def git(repo: Path, *args: str) -> bytes:
    return subprocess.run(["git", "--no-replace-objects", "-C", str(repo), *args], check=True,
                          capture_output=True, env={**os.environ, **GIT_ENV}).stdout


_OCTAL = frozenset(b"01234567")


def unquote_c(quoted: bytes) -> bytes:
    """A path git wrote C-quoted (`"b/Kl\\303\\241vor"`) back to its exact bytes.

    Defensive: a malformed escape (a lone trailing backslash, an unknown escape
    letter, or an out-of-range octal such as `\\777`) never crashes and never
    drops the bytes that follow — the backslash is kept literal so a term cannot
    hide behind a crafted escape. Well-formed octal (`\\NNN`, and a 1-2 digit
    leading run) and the letter escapes decode exactly as before.
    """
    body = quoted[1:-1] if len(quoted) >= 2 and quoted[:1] == b'"' and quoted[-1:] == b'"' else quoted
    out, i, n = bytearray(), 0, len(body)
    while i < n:
        if body[i] != 0x5C:  # not a backslash
            out.append(body[i])
            i += 1
            continue
        nxt = body[i + 1:i + 2]
        if nxt and nxt[0] in _OCTAL:
            j = i + 1  # the leading run of up to three octal digits (git emits exactly three)
            while j < n and j < i + 4 and body[j] in _OCTAL:
                j += 1
            value = int(body[i + 1:j], 8)
            if value <= 0xFF:
                out.append(value)
                i = j
                continue
        elif (letter := nxt.decode("ascii", "replace")) in C_ESCAPES:
            out.append(C_ESCAPES[letter])
            i += 2
            continue
        # lone trailing backslash, unknown escape or out-of-range octal: keep the backslash literal
        # so the bytes that follow are re-read and a term cannot hide behind a crafted escape
        out.append(0x5C)
        i += 1
    return bytes(out)


def diff_path(label: str) -> str:
    """The path of a `+++ ` header label: git adds a tab when the label has a space, and
    C-quotes (core.quotePath) a label with a control or non-ASCII byte. Decode both back to
    the real path, so a term hiding behind an octal escape or a trailing tab is still redacted."""
    label = label.removesuffix("\t")
    if label.startswith('"'):  # a C-quoted label is pure ASCII; unquote to the real bytes
        return decode(unquote_c(label.encode("ascii")).removeprefix(b"b/"))
    return label.removeprefix("b/")


def lfs_problems(scanner: Scanner, where: str, path: str, data: bytes, numbered: bool) -> list[Unscannable]:
    """A git-lfs pointer blob, and each `.gitattributes` line that sets `filter=lfs`: the content
    they stand for lives on the LFS server, which no scan reads, so each is a finding."""
    problems = []
    if data.startswith(_LFS_POINTER):
        problems.append(Unscannable(f"{where}{scanner.shown(path)}", LFS_POINTER))
    if path.rsplit("/", 1)[-1] == ".gitattributes":
        for number, line in enumerate(decode(data).split("\n"), start=1):
            if _LFS_FILTER.search(line) and not line.lstrip().startswith("#"):
                problems.append(Unscannable(f"{where}{scanner.shown(path)}" + (f":{number}" if numbered else ""),
                                            LFS_FILTER))
    return problems


def scan_tree(scanner: Scanner, repo: Path, rev: str) -> list[Finding]:
    # every location starts with a fixed word, never with a path: a path starting with `::`
    # would otherwise read as a GitHub workflow command in the CI log
    hits: list[Finding] = []
    for entry in git(repo, "ls-tree", "-r", "-z", "--full-tree", rev).split(b"\0"):
        if not entry:
            continue
        meta, _, raw_path = entry.partition(b"\t")
        _mode, kind, obj = meta.split()
        path = decode(raw_path)
        hits += scanner.scan_path(path, "tree ")
        if kind != b"blob":
            continue
        # the blob's own bytes: cat-file applies no .gitattributes (binary, -diff, textconv)
        data = git(repo, "cat-file", "blob", decode(obj))
        hits += lfs_problems(scanner, "tree ", path, data, numbered=True)
        hits += blob_findings(scanner, "tree ", path, data)
    return hits


def blob_findings(scanner: Scanner, prefix: str, path: str, data: bytes,
                  old: Callable[[], bytes] | None = None) -> list[Finding]:
    """The findings of a blob and of the members of the container it is (expand, #32 F5 m6): each
    member name holding a term, as a path; each unit holding one, at `<path>[!/<member>]:<unit>` --
    the location `--hash` takes to allowlist it -- and, given `old` (the blob before a commit, read
    only when needed), only the units that version lacks, so a commit reports what it adds; and each
    part that cannot be scanned, unless the blob's own key (`--hash PATH blob`) is allowlisted."""
    findings: list[Finding] = []
    found: list[tuple[str, str, str, int]] = []  # (part path, unit label, unit key, entry)
    problems: list[Problem] = []
    for part in expand(path, data):
        if isinstance(part, Problem):
            problems.append(part)
            continue
        if part.name:
            findings += [Hit(f"{prefix}{scanner.shown(part.path)}: path", entry) for entry in scanner.entries_in(part.name)]
        found += [(part.path, label, key, entry) for label, key, entry in scanner.findings(part.path, scanner.batches(part.data))]
    if found and old is not None:
        present = present_units(path, old(), {(part_path, key) for part_path, _label, key, _entry in found})
        found = [hit for hit in found if (hit[0], hit[2]) not in present]
    findings += [Hit(f"{prefix}{scanner.shown(part_path)}:{label}", entry) for part_path, label, _key, entry in found]
    if problems and line_key(path, blob_key(data)) not in scanner.allow:
        findings += [Unscannable(f"{prefix}{scanner.shown(problem.path)}", problem.what) for problem in problems]
    return findings


def changed_blobs(repo: Path, sha: str) -> list[tuple[bytes, str, bytes, str, bytes]]:
    """(old mode, old blob, new mode, new blob, raw path) of every added, modified or type-changed
    path of a commit: a root commit against the empty tree (--root), a merge against every parent
    (-m, a safe over-scan that never misses a path any parent introduces). Raw bytes via -z, so
    there is no C-quoting to undo."""
    fields = git(repo, "diff-tree", "--no-commit-id", "-r", "-z", "--root", "--no-renames", "-m",
                 "--diff-filter=AMT", sha).split(b"\0")
    changes = []
    for meta, raw_path in zip(fields[0::2], fields[1::2]):
        old_mode, new_mode, old, new, _status = meta.lstrip(b":").split()
        changes.append((old_mode, old.decode("ascii"), new_mode, new.decode("ascii"), raw_path))
    return changes


def present_units(path: str, old: bytes, wanted: set[tuple[str, str]]) -> set[tuple[str, str]]:
    """The (part path, unit key) pairs of `wanted` that the old version of a blob has too -- its own
    units or its members' -- read a batch at a time, only the wanted keys looked up; the old
    container is expanded only when a member is wanted."""
    present: set[tuple[str, str]] = set()
    parts = expand(path, old) if {part_path for part_path, _key in wanted} != {path} else iter([Member(path, "", old)])
    for part in parts:
        keys = {key for part_path, key in wanted if isinstance(part, Member) and part_path == part.path}
        if keys:
            for batch in batches(part.data):
                present |= {(part.path, key) for key in keys.intersection(batch.keys())}
    return present


def scan_commit_blobs(scanner: Scanner, repo: Path, sha: str, seen: set[str]) -> tuple[list[Finding], set[str]]:
    """Scan every changed path of a commit, every changed blob for git-lfs (lfs_problems), and each
    changed blob git's line diff cannot show (it holds a NUL byte or is UTF-16 / UTF-32) or that is a
    container (container_kind): its findings the old version lacks (blob_findings). Returns the
    findings and those blob paths, whose line diff is then not scanned.

    Every path is enumerated here, not only from the unified diff's `+++` headers: an empty or
    binary file has no such header, so its term-bearing name would otherwise slip past."""
    short = sha[:12]
    hits: list[Finding] = []
    blob_paths: set[str] = set()
    reported: set[Finding] = set()
    for old_mode, old, new_mode, new, raw_path in changed_blobs(repo, sha):
        path = decode(raw_path)
        if path not in seen:
            seen.add(path)
            hits += scanner.scan_path(path, f"{short} ")
        if new_mode == GITLINK:
            continue
        data = git(repo, "cat-file", "blob", new)
        found: list[Finding] = list(lfs_problems(scanner, f"{short} ", path, data, numbered=False))
        if not is_plain_text(data) or container_kind(data) is not None:
            blob_paths.add(path)
            earlier = old.strip("0") and old_mode != GITLINK  # not an added path, not a submodule
            found += blob_findings(scanner, f"{short} ", path, data,
                                   (lambda old=old: git(repo, "cat-file", "blob", old)) if earlier else None)
        for finding in found:  # a merge repeats a path per parent
            if finding not in reported:
                reported.add(finding)
                hits.append(finding)
    return hits, blob_paths


def scan_commit_diff(scanner: Scanner, repo: Path, sha: str, seen: set[str], blob_paths: set[str]) -> list[Hit]:
    """Scan the added lines of a commit's unified diff, except those of the blob_paths."""
    short = sha[:12]
    # force quotePath=true so a `+++ ` label is always pure-ASCII octal regardless of the local git
    # config; diff_path/unquote_c decode it back (a raw non-ASCII byte in a quoted label under
    # quotePath=false would otherwise fail encode("ascii")). --text --no-textconv: a `binary` or
    # `-diff` attribute would print "Binary files differ" and a textconv driver would replace the
    # content, hiding the added lines; with --text a NUL file's lines appear too, but those paths
    # are blob_paths, scanned from their blobs instead. The rest pins the output shape against a
    # developer's local config: --src-prefix/--dst-prefix beat diff.noprefix / mnemonicPrefix /
    # srcPrefix / dstPrefix (without them `+++ b/x` under noprefix is the path `b/x` read as `x`);
    # --no-relative beats diff.relative (from a subdirectory it hides every change outside it);
    # --diff-merges=first-parent (what `-m --first-parent` gave by default) beats log.diffMerges,
    # whose `combined` leaves a merge's diff empty for a file only one parent changed; --root beats
    # log.showRoot=false (no diff at all for a root commit); --no-show-signature beats
    # log.showSignature
    diff = git(repo, "-c", "core.quotePath=true", "show", "--format=", "--unified=0", "--no-color",
               "--no-ext-diff", "--text", "--no-textconv", "--no-renames", "--src-prefix=a/",
               "--dst-prefix=b/", "--no-relative", "--diff-merges=first-parent", "--root",
               "--no-show-signature", sha)
    hits: list[Hit] = []
    added: dict[str, list[bytes]] = {}
    # `+++`/`---` count as headers only before a file's first hunk; inside a hunk an added line
    # beginning with `++ ` renders as `+++ ...` and is content, not a new header path
    path, in_hunk = "", False
    # split on `\n` only (as in scan_tree): splitlines() would break an added line at an embedded
    # CR/VT/FF/NEL/U+2028, dropping its `+` prefix so the term-bearing tail is skipped
    for line in diff.split(b"\n"):
        if line.startswith(b"diff --git "):
            path, in_hunk = "", False
        elif line.startswith(b"@@"):
            in_hunk = True
        elif in_hunk and line.startswith(b"+"):
            if path not in blob_paths:
                added.setdefault(path, []).append(line[1:])
        elif not in_hunk and line.startswith(b"+++ "):
            path = diff_path(decode(line[4:]))
            if path not in seen:  # a type change or a path diff-tree did not list
                seen.add(path)
                hits += scanner.scan_path(path, f"{short} ")
    for path, lines in added.items():
        found = scanner.findings(path, line_batches(b"\n".join(lines), 1, scanner.overlap))
        hits += [Hit(f"{short} {scanner.shown(path)}", entry) for _label, _key, entry in found]
    return hits


def identity_problems(repo: Path, sha: str, identities: set[str]) -> list[IdentityProblem]:
    """The author and committer emails that are not exactly (case aside) an allowed identity.

    The two emails are read NUL-terminated -- git never stores a NUL in an ident -- because a
    line split cannot tell them apart: str.splitlines() also breaks at U+2028 / U+2029 / NEL, so an
    author email "allowed<U+2028>allowed" would read as two allowed lines and push the committer
    email out of the check. An email holding any such separator is never allowed, and there is no
    strip(): a trailing separator is whitespace to strip() and would make a stranger's email equal
    an allowed one."""
    author, committer, _end = decode(git(repo, *METADATA, "--format=%ae%x00%ce%x00", sha)).split("\0")
    return [IdentityProblem(sha[:12], role) for role, email in (("author", author), ("committer", committer))
            if LINE_SEPARATORS.intersection(email) or email.lower() not in identities]


def scan_commits(
    scanner: Scanner, repo: Path, revlist_args: list[str], identities: set[str] | None = None
) -> list[Finding]:
    hits: list[Finding] = []
    for sha in decode(git(repo, "rev-list", *revlist_args)).split():
        short = sha[:12]
        metadata = decode(git(repo, *METADATA, "--format=%an%n%ae%n%cn%n%ce%n%B", sha))
        entries = {entry for reading in [metadata, *from_git_latin1(metadata)] for entry in scanner.entries_in(reading)}
        hits += [Hit(f"{short} commit metadata", entry) for entry in sorted(entries)]
        if identities is not None:
            hits += identity_problems(repo, sha, identities)
        seen: set[str] = set()
        blob_hits, blob_paths = scan_commit_blobs(scanner, repo, sha, seen)
        hits += blob_hits
        hits += scan_commit_diff(scanner, repo, sha, seen, blob_paths)
    return hits


def committed_blob(repo: Path, spec: str) -> tuple[str, bytes, str] | None:
    """(blob path, its bytes, the part path) for `--hash`: `<path>` in HEAD, else `<rev>:<path>` (a
    blob only history holds); a path `<blob>!/<member>` names a member of a container blob (expand),
    its part path the whole spec. Never the working-tree file: an uncommitted edit, its line endings,
    its encoding or a smudge filter can make it differ from the blob the scan read (#32 F5 m8)."""
    targets = [("HEAD", spec)]
    if ":" in spec:  # `HEAD:<path>` first: a path may hold a colon itself
        rev, path = spec.split(":", 1)
        targets.append((rev, path))
    for rev, path in targets:
        for blob_path in [path, *(path[:member.start()] for member in re.finditer("!/", path))]:
            try:
                return blob_path, git(repo, "cat-file", "blob", f"{rev}:{blob_path}"), path
            except subprocess.CalledProcessError:
                continue
    return None


def hash_key(repo: Path, spec: str, number: str) -> str | None:
    """The allow key `--hash` prints: of unit `number` (N or `run N`) of a blob or a member of it,
    or of the blob itself (`blob`, for a finding that cannot be scanned), or None when there is no
    such blob, member or unit."""
    target = committed_blob(repo, spec)
    if target is None:
        return None
    blob_path, data, part_path = target
    if number == "blob":
        return line_key(blob_path, blob_key(data))
    part = next((part for part in expand(blob_path, data) if isinstance(part, Member) and part.path == part_path), None)
    try:
        # the units the scan numbers, from the blob's bytes: a text read would translate CR / CRLF
        # to \n and diverge the allow key from the scanner; a UTF-16 line or a binary run (`run N`)
        # is keyed exactly as the scanner keys it
        return None if part is None else line_key(part_path, unit_key(part.data, int(number.removeprefix("run").strip())))
    except (IndexError, ValueError):
        return None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Scan git content for private site data.")
    parser.add_argument("--denylist", type=Path)
    parser.add_argument("--allow", type=Path)
    parser.add_argument("--identities", type=Path, help="allowed author/committer emails (commit mode)")
    parser.add_argument("--repo", type=Path, default=Path("."))
    parser.add_argument("--tree", action="append", default=[], metavar="REV")
    parser.add_argument("--commits", action="append", default=[], metavar="REVLIST")
    parser.add_argument("--hash", nargs=2, metavar=("PATH", "N"),
                        help="print the allow key of line N (or `run N`) of PATH as committed in HEAD, of "
                             "<rev>:<path>, of a container member <path>!/<member>; N `blob` keys the blob itself")
    args = parser.parse_args(argv)

    if args.hash:
        key = hash_key(args.repo, *args.hash)
        if key is None:
            print("--hash: no such committed blob, member or unit (give <path> in HEAD, or <rev>:<path>; "
                  "<path>!/<member> for a container member)", file=sys.stderr)
            return EXIT_USAGE
        print(key)
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
    hits: list[Finding] = []
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
