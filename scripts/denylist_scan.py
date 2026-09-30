#!/usr/bin/env python3
"""Scan git content for private site data (program spec P6).

The denylist (one term per line, `#` comments) is private: a local file for
the pre-push hook, the DENYLIST secret in CI. Output never contains a term, a
matched line or an email address — only locations and the entry number, each
finding line starting with `tree` or a commit's short SHA (never with a path,
so a path beginning `::` cannot read as a CI workflow command). A path
component that holds a term is printed as `[redacted]` (the whole path when a
term spans components), other components have their control characters escaped.

Commit mode scans each commit's author/committer names and emails together
with its message, its added lines and every added/modified path -- including an
empty or binary file, whose path the unified diff omits, enumerated via
`git diff-tree`; with `--identities FILE` it also rejects every commit whose
author or committer email is not listed there. Lines are split on `\n` only (not
str.splitlines()), so a term after a CR/VT/FF/NEL/U+2028 cannot slip past, and a
malformed C-quoted path never crashes the scan (`unquote_c` keeps a bad escape
literal rather than dropping the bytes that follow).

Matching is case-insensitive. A term that starts (ends) with a letter or digit
must not be preceded (followed) by one, where letters include diacritics and
`_` is a separator: `kit` does not hit `kitten`, `x_kit_y` is a hit, and a
term ending in `.` such as `10.0.` hits `10.0.0.5`.
"""
from __future__ import annotations

import argparse
import bisect
import hashlib
import re
import shlex
import subprocess
import sys
import unicodedata
from collections import Counter
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path

EXIT_CLEAN = 0
EXIT_HIT = 1
EXIT_USAGE = 2

REDACTED = "[redacted]"
# the other readings of bytes that are not valid UTF-8: Windows Central European, and Latin-1
FALLBACK_CODECS = ("cp1250", "latin-1")
# In binary content random bytes form short words by chance: in this repository's f64 goldens 28 %
# of all 3-letter and 0.5 % of all 4-letter words occur as words of their text runs, 0.003 % of the
# 5-letter ones. So in a binary text run a term shorter than MIN_BINARY_TERM characters counts only
# when the run is LONG_TEXT_RUN or more bytes of valid UTF-8 -- text, which random bytes never form.
MIN_BINARY_TERM = 5
LONG_TEXT_RUN = 32
# a text run of binary content: no control character but tab, so UTF-8 / cp1250 letters stay in it
_BYTE_RUN = re.compile(rb"[\t\x20-\x7e\x80-\xff]+")
# a UTF-16 string inside binary content (Windows wide strings: an .etl trace, a .lnk, PE resources):
# a Latin character is its low byte next to a 0x00 (U+0000-00FF) or 0x01 (U+0100-017F) high byte
_UTF16_RUNS = (
    ("utf-16-le", re.compile(rb"(?:[\t\x20-\x7e\xa0-\xff]\x00|[\x00-\xff]\x01){2,}")),
    ("utf-16-be", re.compile(rb"(?:\x00[\t\x20-\x7e\xa0-\xff]|\x01[\x00-\xff]){2,}")),
)
GITLINK = b"160000"  # a submodule entry: its object is a commit, not a blob
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


def nfc(text: str) -> str:
    """One form for letters with diacritics, so a decomposed `á` cannot hide a term."""
    return unicodedata.normalize("NFC", text)


def printable(text: str) -> str:
    """Control characters escaped, so a path cannot inject lines (a CI `::error` command) into
    the log."""
    return "".join(char if char.isprintable() else f"\\x{ord(char):02x}" if ord(char) < 0x100
                   else f"\\u{ord(char):04x}" for char in text)


def decode(data: bytes) -> str:
    """UTF-8, lossless: a byte that is not valid UTF-8 becomes a lone surrogate U+DC80-DCFF
    (surrogateescape), so a path or a line keeps its exact bytes -- for its allow key, for the
    cp1250 / Latin-1 re-reading in forms(), and to redact an undecodable path component."""
    return data.decode("utf-8", errors="surrogateescape")


_UNDECODABLE = re.compile("[\udc80-\udcff]+")


def undecodable(text: str) -> bool:
    return _UNDECODABLE.search(text) is not None


def _byte_table(first: int, codec: str) -> dict[int, str]:
    """Code points first+0x80 .. first+0xFF -> byte 0x80..0xFF read in a single-byte codec."""
    return {first + byte: bytes([byte]).decode(codec, "replace") for byte in range(0x80, 0x100)}


_REREAD = {codec: _byte_table(0xDC00, codec) for codec in FALLBACK_CODECS}  # surrogateescape bytes
_GIT_LATIN1_AS_CP1250 = _byte_table(0, "cp1250")  # U+0080-00FF, see cp1250_from_git_latin1


def reread(text: str, codec: str) -> str:
    """The text with each undecodable byte read in a single-byte codec instead; the valid UTF-8
    around it stays as it is (re-reading valid UTF-8 would be mojibake that splits words: read as
    Latin-1, the `č` of `čqxv` ends in a control character, leaving a word `qxv`)."""
    return text.translate(_REREAD[codec])


_UNICODE_ESCAPE = re.compile(r"\\u(?:([0-9A-Fa-f]{4})|\{([0-9A-Fa-f]{1,6})\})")  # JSON/JS/Python; Rust/JS
_SURROGATE_PAIR = re.compile("[\ud800-\udbff][\udc00-\udfff]")
_PERCENT = re.compile(r"(?:%[0-9A-Fa-f]{2})+")


def _escaped_char(match: re.Match[str]) -> str:
    value = int(match.group(1) or match.group(2), 16)
    return chr(value) if value <= 0x10FFFF else match.group()


def _percent_bytes(match: re.Match[str]) -> bytes:
    return bytes.fromhex(match.group().replace("%", ""))


def _percent_decoded(text: str, codec: str) -> str:
    return _PERCENT.sub(lambda match: _percent_bytes(match).decode(codec, "replace"), text)


def unescaped(text: str) -> list[str]:
    """The text with its `\\uXXXX` / `\\u{X}` escapes decoded (a UTF-16 surrogate pair joined), and
    that with its percent-encoding decoded as UTF-8 -- and, when some encoded bytes are not valid
    UTF-8, as cp1250 and Latin-1 too (forms() drops a form equal to one it already has)."""
    found = []
    if "\\u" in text:
        text = _UNICODE_ESCAPE.sub(_escaped_char, text)
        text = _SURROGATE_PAIR.sub(
            lambda pair: pair.group().encode("utf-16-le", "surrogatepass").decode("utf-16-le"), text)
        found.append(text)
    runs = [_percent_bytes(match) for match in _PERCENT.finditer(text)]
    if runs:
        codecs = ("utf-8",) if all(is_utf8(run) for run in runs) else ("utf-8", *FALLBACK_CODECS)
        found += [_percent_decoded(text, codec) for codec in codecs]
    return found


def cp1250_from_git_latin1(text: str) -> str:
    """git stores a commit message or name that is not valid UTF-8 with each such byte converted
    as if it were Latin-1 (commit.c verify_utf8), so a cp1250 `ď` (0xEF) arrives as `ï`: read the
    U+0080-00FF characters back as the cp1250 bytes they were."""
    return text.translate(_GIT_LATIN1_AS_CP1250)


def forms(text: str) -> tuple[str, ...]:
    """Every reading of a decoded text that is matched against the terms, NFC-normalized: the text;
    its cp1250 and Latin-1 re-readings when it holds undecodable bytes; and each of those with its
    escapes decoded (unescaped)."""
    readings = [text]
    if undecodable(text):
        readings += [reread(text, codec) for codec in FALLBACK_CODECS]
    found: list[str] = []
    for reading in readings:
        for form in (reading, *unescaped(reading)):
            form = nfc(form)
            if form not in found:
                found.append(form)
    return tuple(found)


class Scanner:
    def __init__(self, terms: list[str], allow: set[str]) -> None:
        self.patterns = [compile_term(nfc(term)) for term in terms]
        # the bare term, same flags: a necessary condition for a match that the regex engine
        # searches ~15x faster than the boundary lookarounds, so a clean text costs one fast pass
        self.literals = [re.compile(re.escape(nfc(term)), re.IGNORECASE) for term in terms]
        # a short term counts in a binary text run only when the run is long text (MIN_BINARY_TERM)
        self.short = [len(nfc(term)) < MIN_BINARY_TERM for term in terms]
        self.allow = allow

    def entries_in(self, text: str) -> list[int]:
        """The entries found in any of the text's forms (other encodings, escapes decoded)."""
        readings = forms(text)
        return [number for number, (literal, pattern) in enumerate(zip(self.literals, self.patterns), start=1)
                if any(literal.search(form) and pattern.search(form) for form in readings)]

    def unit_hits(self, path: str, units: Sequence[Unit]) -> list[tuple[int, int]]:
        """(unit index, entry number) of every term in a unit that is not allowlisted, sorted.

        Every form of every unit is joined into one text, `\\n`-separated (no term holds a `\\n`,
        so no match spans two forms, and a `\\n` is a word boundary like the end of a form), so each
        term is one regex pass over the content rather than one per unit -- a binary file has 10^5
        runs. A short term is searched only in the units it applies to."""
        views: dict[bool, Joined] = {}
        found: set[tuple[int, int]] = set()
        for entry, (literal, pattern, short) in enumerate(zip(self.literals, self.patterns, self.short), start=1):
            if short not in views:
                views[short] = Joined(units, short_terms_only=short)
            view = views[short]
            if literal.search(view.text):
                found.update((view.owner_of(match.start()), entry) for match in pattern.finditer(view.text))
        return sorted(hit for hit in found if line_key(path, units[hit[0]].key) not in self.allow)

    def shown(self, path: str) -> str:
        """The path as printed: each component holding a term (in any of its forms) or an
        undecodable byte is redacted -- printed, the rest of a cp1250 term would follow its
        replaced letter into the log -- and the others have their control characters escaped;
        the whole path is redacted when a term spans components (a term without `/` always
        matches inside one component) or the printed form holds one."""
        whole = set(self.entries_in(path))
        parts = path.split("/")
        part_hits = [set(self.entries_in(part)) if whole else set() for part in parts]
        if not whole <= set().union(*part_hits):
            return REDACTED
        kept = "/".join(REDACTED if hit or undecodable(part) else printable(part)
                        for part, hit in zip(parts, part_hits, strict=True))
        return REDACTED if self.entries_in(kept) else kept

    def scan_path(self, path: str, prefix: str) -> list[Hit]:
        return [Hit(f"{prefix}{self.shown(path)}: path", entry) for entry in self.entries_in(path)]


def git(repo: Path, *args: str) -> bytes:
    return subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True).stdout


@dataclass(frozen=True)
class Unit:
    """One scanned piece of content: a line of text, or a text run of binary content."""
    key: str                  # the text its allow key is made of
    texts: tuple[str, ...]    # every form of it matched against the terms
    short_terms: bool = True  # False: only terms of MIN_BINARY_TERM or more characters count


class Joined:
    """The forms of units joined into one `\\n`-separated text; an offset maps back to its unit."""

    def __init__(self, units: Sequence[Unit], short_terms_only: bool) -> None:
        self.owner: list[int] = []
        self.start: list[int] = []
        forms: list[str] = []
        offset = 0
        for index, unit in enumerate(units):
            if short_terms_only and not unit.short_terms:
                continue
            for text in unit.texts:
                self.owner.append(index)
                self.start.append(offset)
                forms.append(text)
                offset += len(text) + 1
        self.text = "\n".join(forms)

    def owner_of(self, offset: int) -> int:
        return self.owner[bisect.bisect_right(self.start, offset) - 1]


def text_unit(text: str, short_terms: bool = True) -> Unit:
    return Unit(text, forms(text), short_terms)


def byte_unit(raw: bytes, short_terms: bool = True) -> Unit:
    return text_unit(decode(raw), short_terms)


def utf16_codec(data: bytes) -> str | None:
    """The codec of UTF-16 text: from its byte-order mark, or -- without one -- from the NUL high
    byte of nearly every character (Latin text) against almost no NUL low byte."""
    if data.startswith((b"\xff\xfe", b"\xfe\xff")):
        return "utf-16"
    if len(data) < 4:
        return None
    even, odd = data[0::2], data[1::2]
    if odd.count(0) >= len(odd) / 2 and even.count(0) <= len(even) / 20:
        return "utf-16-le"
    if even.count(0) >= len(even) / 2 and odd.count(0) <= len(odd) / 20:
        return "utf-16-be"
    return None


def is_plain_text(data: bytes) -> bool:
    """Content git's line diff shows faithfully: no NUL byte and not UTF-16."""
    return b"\0" not in data and utf16_codec(data) is None


def content_units(data: bytes) -> tuple[str, list[Unit]]:
    """What of a blob is scanned, and the label of a unit number in a tree location.

    Text: its lines (split on `\\n` only). UTF-16 text: its decoded lines. Other content holding a
    NUL byte (binary): its text runs -- byte runs without control characters, then UTF-16 strings
    -- numbered `run N`; a byte run shorter than MIN_BINARY_TERM can never count (a short term
    needs a LONG_TEXT_RUN), so it is not a unit. `--hash` numbers units the same way."""
    codec = utf16_codec(data)
    if codec is not None:
        return "", [text_unit(line) for line in data.decode(codec, errors="replace").split("\n")]
    if b"\0" not in data:
        return "", [byte_unit(line) for line in data.split(b"\n")]
    units = [byte_unit(run, short_terms=len(run) >= LONG_TEXT_RUN and is_utf8(run))
             for run in _BYTE_RUN.findall(data) if len(run) >= MIN_BINARY_TERM]
    for codec, pattern in _UTF16_RUNS:
        units += [text_unit(run.decode(codec, errors="replace")) for run in pattern.findall(data)]
    return "run ", units


def is_utf8(data: bytes) -> bool:
    try:
        data.decode("utf-8")
    except UnicodeDecodeError:
        return False
    return True


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


def scan_tree(scanner: Scanner, repo: Path, rev: str) -> list[Hit]:
    # every location starts with a fixed word, never with a path: a path starting with `::`
    # would otherwise read as a GitHub workflow command in the CI log
    hits: list[Hit] = []
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
        label, units = content_units(git(repo, "cat-file", "blob", decode(obj)))
        found = scanner.unit_hits(path, units)
        if found:
            shown = scanner.shown(path)
            hits += [Hit(f"tree {shown}:{label}{index + 1}", entry) for index, entry in found]
    return hits


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


def added_units(old: list[Unit], new: list[Unit]) -> list[Unit]:
    """The units of `new` that `old` does not have (a multiset difference by allow-key text)."""
    before = Counter(unit.key for unit in old)
    added = []
    for unit in new:
        if before[unit.key]:
            before[unit.key] -= 1
        else:
            added.append(unit)
    return added


def scan_commit_blobs(scanner: Scanner, repo: Path, sha: str, seen: set[str]) -> tuple[list[Hit], set[str]]:
    """Scan every changed path of a commit, and the content of each changed blob git's line diff
    cannot show (it holds a NUL byte or is UTF-16): the units its new blob adds over the old one.
    Returns the hits and those blob paths, whose line diff is then not scanned.

    Every path is enumerated here, not only from the unified diff's `+++` headers: an empty or
    binary file has no such header, so its term-bearing name would otherwise slip past."""
    short = sha[:12]
    hits: list[Hit] = []
    blob_paths: set[str] = set()
    reported: set[tuple[str, str, int]] = set()
    for old_mode, old, new_mode, new, raw_path in changed_blobs(repo, sha):
        path = decode(raw_path)
        if path not in seen:
            seen.add(path)
            hits += scanner.scan_path(path, f"{short} ")
        if new_mode == GITLINK:
            continue
        data = git(repo, "cat-file", "blob", new)
        if is_plain_text(data):
            continue
        blob_paths.add(path)
        before = []
        if old.strip("0") and old_mode != GITLINK:  # not an added path, not a submodule
            before = content_units(git(repo, "cat-file", "blob", old))[1]
        added = added_units(before, content_units(data)[1])
        for index, entry in scanner.unit_hits(path, added):
            if (path, added[index].key, entry) not in reported:  # a merge repeats it per parent
                reported.add((path, added[index].key, entry))
                hits.append(Hit(f"{short} {scanner.shown(path)}", entry))
    return hits, blob_paths


def scan_commit_diff(scanner: Scanner, repo: Path, sha: str, seen: set[str], blob_paths: set[str]) -> list[Hit]:
    """Scan the added lines of a commit's unified diff, except those of the blob_paths."""
    short = sha[:12]
    # force quotePath=true so a `+++ ` label is always pure-ASCII octal regardless of the local git
    # config; diff_path/unquote_c decode it back (a raw non-ASCII byte in a quoted label under
    # quotePath=false would otherwise fail encode("ascii")). --text --no-textconv: a `binary` or
    # `-diff` attribute would print "Binary files differ" and a textconv driver would replace the
    # content, hiding the added lines; with --text a NUL file's lines appear too, but those paths
    # are blob_paths, scanned from their blobs instead
    diff = git(repo, "-c", "core.quotePath=true", "show", "--format=", "--unified=0", "--no-color",
               "--no-ext-diff", "--text", "--no-textconv", "--no-renames", "-m", "--first-parent", sha)
    hits: list[Hit] = []
    added: dict[str, list[Unit]] = {}
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
                added.setdefault(path, []).append(byte_unit(line[1:]))
        elif not in_hunk and line.startswith(b"+++ "):
            path = diff_path(decode(line[4:]))
            if path not in seen:  # a type change or a path diff-tree did not list
                seen.add(path)
                hits += scanner.scan_path(path, f"{short} ")
    for path, units in added.items():
        hits += [Hit(f"{short} {scanner.shown(path)}", entry) for _index, entry in scanner.unit_hits(path, units)]
    return hits


def identity_problems(repo: Path, sha: str, identities: set[str]) -> list[IdentityProblem]:
    """The author and committer emails that are not exactly (case aside) an allowed identity.

    The two emails are read NUL-terminated -- git never stores a NUL in an ident -- because a
    line split cannot tell them apart: str.splitlines() also breaks at U+2028 / U+2029 / NEL, so an
    author email "allowed<U+2028>allowed" would read as two allowed lines and push the committer
    email out of the check. An email holding any such separator is never allowed, and there is no
    strip(): a trailing separator is whitespace to strip() and would make a stranger's email equal
    an allowed one."""
    author, committer, _end = decode(git(repo, "show", "-s", "--format=%ae%x00%ce%x00", sha)).split("\0")
    return [IdentityProblem(sha[:12], role) for role, email in (("author", author), ("committer", committer))
            if LINE_SEPARATORS.intersection(email) or email.lower() not in identities]


def scan_commits(
    scanner: Scanner, repo: Path, revlist_args: list[str], identities: set[str] | None = None
) -> list[Hit | IdentityProblem]:
    hits: list[Hit | IdentityProblem] = []
    for sha in decode(git(repo, "rev-list", *revlist_args)).split():
        short = sha[:12]
        metadata = decode(git(repo, "show", "-s", "--format=%an%n%ae%n%cn%n%ce%n%B", sha))
        entries = set(scanner.entries_in(metadata)) | set(scanner.entries_in(cp1250_from_git_latin1(metadata)))
        hits += [Hit(f"{short} commit metadata", entry) for entry in sorted(entries)]
        if identities is not None:
            hits += identity_problems(repo, sha, identities)
        seen: set[str] = set()
        blob_hits, blob_paths = scan_commit_blobs(scanner, repo, sha, seen)
        hits += blob_hits
        hits += scan_commit_diff(scanner, repo, sha, seen, blob_paths)
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
        # the units scan_tree numbers, from the file's bytes: read_text() would translate CR / CRLF
        # to \n and diverge the allow key from the scanner; a UTF-16 line or a binary run (`run N`)
        # is keyed exactly as the scanner keys it
        units = content_units((args.repo / path).read_bytes())[1]
        print(line_key(path, units[int(number) - 1].key))
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
