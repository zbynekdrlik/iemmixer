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
import html
import html.entities
import os
import re
import shlex
import subprocess
import sys
import unicodedata
from collections.abc import Generator, Iterable, Iterator
from dataclasses import dataclass
from pathlib import Path

EXIT_CLEAN = 0
EXIT_HIT = 1
EXIT_USAGE = 2

REDACTED = "[redacted]"
# the other readings of bytes that are not valid UTF-8: Windows Central European, Latin-1, ISO
# Central European (ISO-8859-2), DOS Central European (cp852)
FALLBACK_CODECS = ("cp1250", "latin-1", "iso-8859-2", "cp852")
# UTF-8 shown in one of these code pages and encoded again (double-encoded mojibake) is read back
# (unmojibake): Windows Central European; and Western, where Latin-1 and Windows-1252 differ only in
# 0x80-0x9F (C1 controls against punctuation and a few letters), so one reading takes both (#32 F5 m3)
MOJIBAKE_CODECS = {"cp1250": ("cp1250",), "western": ("latin-1", "cp1252")}
# In binary content random bytes form short words by chance: in this repository's f64 goldens 28 %
# of all 3-letter and 0.5 % of all 4-letter words occur as words of their text runs, 0.003 % of the
# 5-letter ones. So in a binary text run a term shorter than MIN_BINARY_TERM characters counts only
# when the run is LONG_TEXT_RUN or more bytes of valid UTF-8 -- text, which random bytes never form.
MIN_BINARY_TERM = 5
LONG_TEXT_RUN = 32
# Content is scanned in batches of about CHUNK bytes, joined by SEP -- a private-use, non-word
# character that no form creates or removes -- so CPU and memory stay bounded (#32 review m7).
CHUNK = 1 << 18
SEP = "\ue000"
# a control byte ends a text run of binary content (a tab does not)
_CONTROLS = bytes([*range(0x09), *range(0x0A, 0x20), 0x7F])
_CONTROL = re.compile(b"[" + re.escape(_CONTROLS) + b"]")
_CONTROL_TO_NUL = bytes.maketrans(_CONTROLS, bytes(len(_CONTROLS)))
# a run shorter than MIN_BINARY_TERM after its NUL (a leading literal, so the regex engine skips
# from NUL to NUL); the NUL that ends it is left for the next match
_SHORT_RUN = re.compile(rb"\x00[^\x00]{1,%d}(?=\x00)" % (MIN_BINARY_TERM - 1))
_NUL_RUN = re.compile(rb"\x00{2,}")
# a UTF-16 string inside binary content (Windows wide strings: an .etl trace, a .lnk, PE resources):
# a Latin character is its low byte next to a 0x00 (U+0000-00FF) or 0x01 (U+0100-017F) high byte
# (big-endian; a little-endian run is this pattern's match in the reversed bytes -- a leading 0x00 or
# 0x01 lets the regex engine skip ahead, which the low byte first would not)
_UTF16_RUN = re.compile(rb"(?:\x00[\t\x20-\x7e\xa0-\xff]|\x01[\x00-\xff]){2,}")
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


def nfc(text: str) -> str:
    """One form for letters with diacritics, so a decomposed `á` cannot hide a term."""
    return text if unicodedata.is_normalized("NFC", text) else unicodedata.normalize("NFC", text)


def printable(text: str) -> str:
    """Control characters escaped, so a path cannot inject lines (a CI `::error` command) into
    the log."""
    return "".join(char if char.isprintable() else f"\\x{ord(char):02x}" if ord(char) < 0x100
                   else f"\\u{ord(char):04x}" for char in text)


def decode(data: bytes) -> str:
    """UTF-8, lossless: a byte that is not valid UTF-8 becomes a lone surrogate U+DC80-DCFF
    (surrogateescape), so a path or a line keeps its exact bytes -- for its allow key, for the
    cp1250 / Latin-1 re-reading in Views, and to redact an undecodable path component."""
    return data.decode("utf-8", errors="surrogateescape")


_UNDECODABLE = re.compile("[\udc80-\udcff]+")


def undecodable(text: str) -> bool:
    return _UNDECODABLE.search(text) is not None


def _byte_table(first: int, codec: str) -> dict[int, str]:
    """Code points first+0x80 .. first+0xFF -> byte 0x80..0xFF read in a single-byte codec."""
    return {first + byte: bytes([byte]).decode(codec, "replace") for byte in range(0x80, 0x100)}


_REREAD = {codec: _byte_table(0xDC00, codec) for codec in FALLBACK_CODECS}  # surrogateescape bytes
# U+0080-00FF read as the other code pages' bytes, see from_git_latin1
_GIT_LATIN1_AS = {codec: _byte_table(0, codec) for codec in FALLBACK_CODECS if codec != "latin-1"}
_GIT_LATIN1_CHARS = re.compile("[\u0080-\u00ff]")


def reread(text: str, codec: str) -> str:
    """The text with each undecodable byte read in a single-byte codec instead; the valid UTF-8
    around it stays as it is (re-reading valid UTF-8 would be mojibake that splits words: read as
    Latin-1, the `č` of `čqxv` ends in a control character, leaving a word `qxv`)."""
    return text.translate(_REREAD[codec])


# JSON / JS / Python `\uXXXX`, Rust / JS `\u{X}`, Python `\UXXXXXXXX`, XML / HTML `&#N;` / `&#xN;`
_UNICODE_ESCAPE = re.compile(r"\\u(?:([0-9A-Fa-f]{4})|\{([0-9A-Fa-f]{1,6})\})|\\U([0-9A-Fa-f]{8})"
                             r"|&#(?:([0-9]{1,7})|[xX]([0-9A-Fa-f]{1,6}));")
# an HTML named character reference: `&` and a name of up to 32 characters, `;` optional (HTML5 keeps
# a legacy set without it; html.unescape reads a longer name as such a prefix and the rest)
_NAMED_REFERENCE = re.compile(r"&[A-Za-z][A-Za-z0-9]{1,31};?")
_HTML5 = html.entities.html5
_SURROGATE_PAIR = re.compile("[\ud800-\udbff][\udc00-\udfff]")
# a run of escapes that each stand for one byte: C / Rust / Python `\xNN`, octal `\NNN` and `\0`,
# the letter escapes, and URL percent-encoding
_BYTE_ESCAPES = re.compile(r"(?:\\(?:x[0-9A-Fa-f]{2}|[0-7]{1,3}|[abfnrtv\\'\"?])|%[0-9A-Fa-f]{2})+")
_ONE_BYTE_ESCAPE = re.compile(r"\\x([0-9A-Fa-f]{2})|%([0-9A-Fa-f]{2})|\\([0-7]{1,3})|\\(.)")
_LETTER_ESCAPES = {"a": 7, "b": 8, "f": 12, "n": 10, "r": 13, "t": 9, "v": 11, "\\": 92, "'": 39, '"': 34, "?": 63}


def _escaped_char(match: re.Match[str]) -> str:
    four, braced, eight, decimal, hexadecimal = match.groups()
    value = int(decimal, 10) if decimal else int(four or braced or eight or hexadecimal, 16)
    return match.group() if value > 0x10FFFF else "\ufffd" if value == ord(SEP) else chr(value)


def _escaped_bytes(match: re.Match[str]) -> str:
    out = bytearray()
    for escape in _ONE_BYTE_ESCAPE.finditer(match.group()):
        hex_digits, percent, octal, letter = escape.groups()
        if hex_digits or percent:
            out.append(int(hex_digits or percent, 16))
        elif octal:
            out.append(int(octal, 8) & 0xFF)
        else:
            out.append(_LETTER_ESCAPES[letter])
    return decode(bytes(out)).replace(SEP, "\ufffd")  # an escape never makes a batch separator


def _named_reference(match: re.Match[str]) -> str:
    # no named reference stands for SEP (U+E000): html5 holds no private-use character
    return _HTML5.get(match.group()[1:]) or html.unescape(match.group())


def unescape(text: str) -> str:
    """The text with its escapes decoded: HTML named character references first (`&dcaron;`,
    `&shy;`, `&nbsp;`, HTML5's legacy ones without `;`, so a double-escaped `&amp;#271;` is decoded
    too -- #32 F5 m5), then `\\uXXXX`, `\\u{X}`, `\\UXXXXXXXX` and XML / HTML numeric character
    references (a UTF-16 surrogate pair joined), and each run of byte escapes (C / Rust / Python
    `\\xNN`, octal, `\\0`, the letter escapes, percent-encoding) as the bytes it stands for, decoded
    like raw bytes -- undecodable ones stay lone surrogates that Views re-reads."""
    if "&" in text:
        text = _NAMED_REFERENCE.sub(_named_reference, text)
    if"\\u" in text or "\\U" in text or "&#" in text:
        text = _UNICODE_ESCAPE.sub(_escaped_char, text)
        text = _SURROGATE_PAIR.sub(
            lambda pair: pair.group().encode("utf-16-le", "surrogatepass").decode("utf-16-le"), text)
    if "\\" in text or "%" in text:
        text = _BYTE_ESCAPES.sub(_escaped_bytes, text)
    return text


@dataclass(frozen=True)
class CodePage:
    """How a single-byte code page shows UTF-8's bytes 0x80-0xFF, for reading double-encoded UTF-8 back."""
    hint: re.Pattern[str]       # a lead byte's character and one continuation's: ~10x faster to find
    sequences: re.Pattern[str]  # whole UTF-8 sequences as the code page shows them
    to_bytes: dict[int, str]    # each of those characters -> the Latin-1 character of its byte

    @classmethod
    def of(cls, *codecs: str) -> CodePage:
        """The characters bytes 0x80-0xFF show as in any of `codecs` (Windows shows a byte its code
        page leaves undefined as the C1 control of the same number, so that is one of them)."""
        chars: dict[str, int] = {}
        for codec in codecs:
            for byte in range(0x80, 0x100):
                try:
                    chars.setdefault(bytes([byte]).decode(codec), byte)
                except UnicodeDecodeError:
                    chars.setdefault(chr(byte), byte)

        def of_bytes(first: int, last: int) -> str:
            return "[" + re.escape("".join(char for char, byte in chars.items() if first <= byte <= last)) + "]"
        cont = of_bytes(0x80, 0xBF)
        sequences = (f"(?:{of_bytes(0xC2, 0xDF)}{cont}|{of_bytes(0xE0, 0xEF)}{cont}{{2}}"
                     f"|{of_bytes(0xF0, 0xF4)}{cont}{{3}})+")
        return cls(re.compile(of_bytes(0xC2, 0xF4) + cont), re.compile(sequences),
                   {ord(char): chr(byte) for char, byte in chars.items()})


_MOJIBAKE = {reading: CodePage.of(*codecs) for reading, codecs in MOJIBAKE_CODECS.items()}


def unmojibake(text: str, reading: str) -> str:
    """The text with each double-encoded stretch -- UTF-8 once shown in a code page (MOJIBAKE_CODECS)
    and encoded again, `ď` shown as `ÄŹ` -- read back as the UTF-8 it was; a stretch that is not
    valid UTF-8 stays."""
    page = _MOJIBAKE[reading]

    def undo(match: re.Match[str]) -> str:
        try:
            return match.group().translate(page.to_bytes).encode("latin-1").decode("utf-8").replace(SEP, "\ufffd")
        except UnicodeError:
            return match.group()
    return page.sequences.sub(undo, text) if page.hint.search(text) else text


# characters no reader sees -- Unicode's Default_Ignorable_Code_Point set (#32 F5 m5): soft hyphen,
# combining grapheme joiner, Arabic letter mark, Hangul fillers, Khmer inherent vowels, Mongolian
# variation selectors and vowel separator, zero-width space / non-joiner / joiner, left-to-right and
# right-to-left marks, bidi embeddings and overrides, word joiner, invisible operators, bidi isolates,
# variation selectors, zero-width no-break space (BOM), the reserved U+FFF0-FFF8, shorthand format
# controls, musical symbol formats, tag characters
_INVISIBLE = re.compile("[\u00ad\u034f\u061c\u115f\u1160\u17b4\u17b5\u180b-\u180f\u200b-\u200f\u202a-\u202e"
                        "\u2060-\u206f\u3164\ufe00-\ufe0f\ufeff\uffa0\ufff0-\ufff8"
                        "\U0001bca0-\U0001bca3\U0001d173-\U0001d17a\U000e0000-\U000e0fff]")
# compatibility characters whose NFKC form holds Latin letters or digits: ª ² ³ ¹ º, the ligature and
# digraph letters (Ĳ Ŀ ŉ ſ Ǆ-ǌ Ǳ-ǳ), modifier letters, super- and subscripts, letterlike symbols
# and Roman numerals, enclosed alphanumerics, Latin ligatures, fullwidth forms, mathematical
# alphanumerics, the enclosed alphanumeric supplement; and the two characters besides the Kelvin
# sign (which fold covers) whose NFC form is ASCII: the Greek question mark (`;`) and the Greek varia
# (a backtick) -- checked against every code point (#32 F5 m10)
_COMPATIBLE = re.compile("[\u00aa\u00b2\u00b3\u00b9\u00ba\u0132\u0133\u013f\u0140\u0149\u017f"
                         "\u01c4-\u01cc\u01f1-\u01f3\u02b0-\u02b8\u037e\u1d2c-\u1d6a\u1fef\u2070-\u209c"
                         "\u2100-\u2189\u2460-\u24ff\ufb00-\ufb06\uff01-\uff5e"
                         "\U0001d400-\U0001d7ff\U0001f100-\U0001f1aa]")


def compact(text: str) -> str:
    """The text with its invisible characters removed and each compatibility character that stands
    for Latin letters or digits in NFKC (_COMPATIBLE): a fullwidth z reads as `z`, a word split by an
    invisible character reads whole. (The raw reading keeps a zero-width space as the word break it
    also is.) Only those characters are normalized -- NFKC of the whole text costs ~10x more."""
    text = _INVISIBLE.sub("", text)
    return _COMPATIBLE.sub(lambda match: unicodedata.normalize("NFKC", match.group()), text)


def from_git_latin1(text: str) -> list[str]:
    """git stores a commit message or name that is not valid UTF-8 with each such byte converted
    as if it were Latin-1 (commit.c verify_utf8), so a cp1250 `ď` (0xEF) arrives as `ï`: the text
    with its U+0080-00FF characters read back as the bytes they were, in each other FALLBACK_CODECS
    code page -- cp1250, ISO-8859-2, cp852 (#32 F5 m4) -- or nothing when it has none."""
    if not _GIT_LATIN1_CHARS.search(text):
        return []
    return [text.translate(table) for table in _GIT_LATIN1_AS.values()]


# re.IGNORECASE matches an ASCII letter against these non-ASCII characters too (checked against every
# code point): İ and ı against i, ſ against s, the Kelvin sign against k
_FOLD = (("\u0130", "i"), ("\u0131", "i"), ("\u017f", "s"), ("\u212a", "k"))
_ASCII_WORD = re.compile("[a-z0-9]{2,}")


def fold(text: str) -> str:
    """The text case-folded so that `fold(term) in fold(text)` holds wherever re.IGNORECASE finds an
    ASCII term (a C-level pre-filter, far faster than the regex)."""
    for char, ascii_char in _FOLD:
        text = text.replace(char, ascii_char)
    return text.lower()


class Views:
    """The readings of a decoded text that the terms are matched against, each made on first need.

    The text and the text with its escapes decoded (unescape), each also with any double-encoded
    UTF-8 read back (unmojibake), and the decoded text compacted (compact: NFKC, invisible characters
    removed) serve an ASCII term as they are: a re-reading only turns lone
    surrogates -- non-word characters -- into letters or symbols, and NFC turns no character into
    an ASCII one but U+037E, U+1FEF (compact reads both as their ASCII forms) and the Kelvin sign
    (fold), so neither can add an ASCII term's match. Their case folds pre-filter
    every term. A term with a non-ASCII character is matched in their NFC forms and in the NFC
    re-readings of the first two with FALLBACK_CODECS (undecodable bytes only) -- made only when the
    term's longest ASCII word occurs in a fold, since those readings keep every ASCII character."""

    def __init__(self, text: str) -> None:
        decoded = unescape(text)
        self.bases = [text] if decoded == text else [text, decoded]
        self.raw = list(self.bases)
        for base in self.bases:
            for reading in MOJIBAKE_CODECS:
                fixed = unmojibake(base, reading)
                if fixed not in self.raw:
                    self.raw.append(fixed)
        compacted = compact(self.bases[-1])
        if compacted not in self.raw:
            self.raw.append(compacted)
        self.folded = [fold(view) for view in self.raw]
        self._normal: list[str] | None = None

    def normal(self) -> list[str]:
        if self._normal is None:
            readings = list(self.raw)
            for base in self.bases:
                if undecodable(base):
                    readings += [reread(base, codec) for codec in FALLBACK_CODECS]
            found: list[str] = []
            for reading in readings:
                form = nfc(reading)
                if form not in found:
                    found.append(form)
            self._normal = found
        return self._normal

    def for_term(self, term: Term) -> list[str]:
        if term.ascii:
            return [view for view, folded in zip(self.raw, self.folded, strict=True) if term.folded in folded]
        if term.folded and not any(term.folded in folded for folded in self.folded):
            return []
        return self.normal()


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
            keys = batch.keys()
            found = {(unit, index) for unit, index in found
                     if not self.terms[index].short or long_text_run(keys[unit])}
        return sorted({(unit, self.terms[index].entry) for unit, index in found})

    def findings(self, path: str, batches: Iterable[Batch]) -> list[tuple[str, str, int]]:
        """(unit label, unit key, entry number) of every term found and not allowlisted."""
        found: list[tuple[str, str, int]] = []
        for batch in batches:
            hits = self.batch_hits(batch)
            if hits:
                keys = batch.keys()
                found += [(f"{batch.label}{batch.first + unit}", keys[unit], entry) for unit, entry in hits
                          if line_key(path, keys[unit]) not in self.allow]
        return found

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


@dataclass(frozen=True)
class Batch:
    """Consecutive units of one blob or diff -- lines of text, or text runs of binary content --
    scanned together. A batch holds about CHUNK of content, so memory stays bounded whatever the
    size of the blob."""
    source: str             # the units, decoded (decode), separated by `sep`, which no unit holds
    sep: str
    first: int              # the number of its first unit (`--hash` numbers units the same way)
    label: str = ""         # a unit's label in a location: "" (line N) or "run " (text run N)
    runs: bool = False      # byte runs of binary content: a short term needs a long text run

    def keys(self) -> list[str]:
        """Each unit's text, which its allow key is made of."""
        return self.source.split(self.sep)

    def text(self) -> str:
        """The units joined by SEP; a unit's own U+E000 is read as U+FFFD, also a non-word character."""
        return self.source.replace(SEP, "\ufffd").replace(self.sep, SEP)


def long_text_run(key: str) -> bool:
    return not undecodable(key) and len(key.encode("utf-8")) >= LONG_TEXT_RUN


def line_batches(data: bytes | str, first: int = 1) -> Generator[Batch, None, int]:
    """The lines of text (split on `\\n` only), a piece of about CHUNK at a time. Returns the
    number the next unit gets."""
    newline = b"\n" if isinstance(data, bytes) else "\n"
    start = 0
    while True:
        end = data.find(newline, start + CHUNK)
        piece = data[start:] if end < 0 else data[start:end]
        source = decode(piece) if isinstance(piece, bytes) else piece
        yield Batch(source, "\n", first)
        first += source.count("\n") + 1
        if end < 0:
            return first
        start = end + 1


def run_batches(data: bytes, first: int) -> Generator[Batch, None, int]:
    """The byte runs of binary content -- bytes without control characters, so a UTF-8 / cp1250
    letter stays in its run -- of MIN_BINARY_TERM or more bytes (a shorter one can never count: a
    short term needs a LONG_TEXT_RUN), NUL-separated, a piece of about CHUNK at a time: a piece ends
    at a control byte, which no run holds, and no Python object is made per run. Returns the number
    the next unit gets."""
    start = 0
    while start < len(data):
        cut = _CONTROL.search(data, start + CHUNK)
        end = cut.end() if cut else len(data)
        piece = _SHORT_RUN.sub(b"\x00", b"\x00" + data[start:end].translate(_CONTROL_TO_NUL) + b"\x00")
        piece = _NUL_RUN.sub(b"\x00", piece).strip(b"\x00")
        start = end
        if piece:
            yield Batch(decode(piece), "\x00", first, "run ", runs=True)
            first += piece.count(b"\x00") + 1
    return first


def wide_run_batches(data: bytes, first: int) -> Iterator[Batch]:
    """The UTF-16 strings inside binary content (few: random bytes rarely form one)."""
    little = [run[::-1].decode("utf-16-le", errors="replace") for run in reversed(_UTF16_RUN.findall(data[::-1]))]
    runs = little + [run.decode("utf-16-be", errors="replace") for run in _UTF16_RUN.findall(data)]
    if runs:  # a decoded UTF-16 run holds no NUL: its characters are U+0009 and U+0020-01FF
        yield Batch("\x00".join(runs), "\x00", first, "run ")


def batches(data: bytes) -> Iterator[Batch]:
    """What of a blob is scanned, numbered on unit after unit: text as its lines; UTF-32 / UTF-16
    text as its decoded lines and then its byte runs too (a binary may only look like it: quiet
    16-bit PCM has a NUL high byte in nearly every sample, and any blob may start FF FE -- genuine
    UTF-16 / UTF-32 Latin text has no byte run a term could match); other content holding a NUL
    byte (binary) as its text runs -- byte runs, then UTF-16 strings. A run is labelled `run N`."""
    codec = wide_codec(data)
    if codec is not None:
        first = yield from line_batches(data.decode(codec, errors="replace"))
        yield from run_batches(data, first)
    elif b"\0" not in data:
        yield from line_batches(data)
    else:
        first = yield from run_batches(data, 1)
        yield from wide_run_batches(data, first)


def unit_key(data: bytes, number: int) -> str:
    """The key of unit `number` of a blob, numbered as the scan numbers it (`--hash`)."""
    for batch in batches(data):
        keys = batch.keys()
        if number < batch.first + len(keys):
            return keys[number - batch.first]
    raise IndexError(f"the content has no unit {number}")


def wide_codec(data: bytes) -> str | None:
    """The codec of UTF-32 or UTF-16 text: from its byte-order mark (UTF-32's FF FE 00 00 before
    UTF-16's FF FE), or -- without one -- from the NUL bytes of Latin text: in UTF-32 the upper two
    bytes of every character and the second of most, in UTF-16 the high byte of most, against
    almost no NUL in the low byte."""
    if data.startswith((b"\xff\xfe\x00\x00", b"\x00\x00\xfe\xff")):
        return "utf-32"
    if data.startswith((b"\xff\xfe", b"\xfe\xff")):
        return "utf-16"
    if len(data) < 8:
        return None
    nul = [column.count(0) / len(column) for column in (data[offset::4] for offset in range(4))]
    if nul[0] <= 0.05 and nul[1] >= 0.5 and min(nul[2:]) >= 0.95:
        return "utf-32-le"
    if min(nul[:2]) >= 0.95 and nul[2] >= 0.5 and nul[3] <= 0.05:
        return "utf-32-be"
    even, odd = (nul[0] + nul[2]) / 2, (nul[1] + nul[3]) / 2
    if odd >= 0.5 and even <= 0.05:
        return "utf-16-le"
    if even >= 0.5 and odd <= 0.05:
        return "utf-16-be"
    return None


def is_plain_text(data: bytes) -> bool:
    """Content git's line diff shows faithfully: no NUL byte and not UTF-16 / UTF-32."""
    return b"\0" not in data and wide_codec(data) is None


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
        found = scanner.findings(path, batches(data))
        if found:
            shown = scanner.shown(path)
            hits += [Hit(f"tree {shown}:{label}", entry) for label, _key, entry in found]
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


def not_in(found: list[tuple[str, str, int]], old: bytes) -> list[tuple[str, str, int]]:
    """The findings whose unit the old blob does not have: only those are the commit's own. The old
    blob is read a batch at a time, and only the found keys are looked up."""
    wanted = {key for _label, key, _entry in found}
    present: set[str] = set()
    for batch in batches(old):
        present |= wanted.intersection(batch.keys())
    return [finding for finding in found if finding[1] not in present]


def scan_commit_blobs(scanner: Scanner, repo: Path, sha: str, seen: set[str]) -> tuple[list[Finding], set[str]]:
    """Scan every changed path of a commit, every changed blob for git-lfs (lfs_problems), and the
    content of each changed blob git's line diff cannot show (it holds a NUL byte or is UTF-16 /
    UTF-32): the units its new blob adds over the old one. Returns the findings and those blob
    paths, whose line diff is then not scanned.

    Every path is enumerated here, not only from the unified diff's `+++` headers: an empty or
    binary file has no such header, so its term-bearing name would otherwise slip past."""
    short = sha[:12]
    hits: list[Finding] = []
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
        hits += [problem for problem in lfs_problems(scanner, f"{short} ", path, data, numbered=False)
                 if problem not in hits]  # a merge repeats a path per parent
        if is_plain_text(data):
            continue
        blob_paths.add(path)
        found = scanner.findings(path, batches(data))
        if found and old.strip("0") and old_mode != GITLINK:  # not an added path, not a submodule
            found = not_in(found, git(repo, "cat-file", "blob", old))
        for label, key, entry in found:  # the label lets `--hash <sha>:<path> <N>` allowlist it
            if (path, key, entry) not in reported:  # a merge repeats it per parent
                reported.add((path, key, entry))
                hits.append(Hit(f"{short} {scanner.shown(path)}:{label}", entry))
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
        found = scanner.findings(path, line_batches(b"\n".join(lines)))
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


def committed_blob(repo: Path, spec: str) -> tuple[str, bytes] | None:
    """The path and bytes of the blob `--hash` keys: `<path>` in HEAD, else `<rev>:<path>` (a blob
    only history holds). Never the working-tree file: an uncommitted edit, its line endings, its
    encoding or a smudge filter can make it differ from the blob the scan read (#32 F5 m8)."""
    targets = [("HEAD", spec)]
    if ":" in spec:  # `HEAD:<path>` first: a path may hold a colon itself
        rev, path = spec.split(":", 1)
        targets.append((rev, path))
    for rev, path in targets:
        try:
            return path, git(repo, "cat-file", "blob", f"{rev}:{path}")
        except subprocess.CalledProcessError:
            continue
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
                        help="print the allow key of line N (or `run N`) of PATH as committed in HEAD, "
                             "or of <rev>:<path>")
    args = parser.parse_args(argv)

    if args.hash:
        spec, number = args.hash
        blob = committed_blob(args.repo, spec)
        if blob is None:
            print("--hash: no such committed blob (give <path> in HEAD, or <rev>:<path>)", file=sys.stderr)
            return EXIT_USAGE
        path, data = blob
        # the units the scan numbers, from the blob's bytes: a text read would translate CR / CRLF
        # to \n and diverge the allow key from the scanner; a UTF-16 line or a binary run (`run N`)
        # is keyed exactly as the scanner keys it
        print(line_key(path, unit_key(data, int(number.removeprefix("run").strip()))))
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
