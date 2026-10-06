"""The readings of decoded text that the denylist terms are matched in (scripts/denylist_scan.py, #32).

Text is decoded losslessly (UTF-8, surrogateescape) and read as it is and with its escapes decoded
(unescape: HTML named and numeric character references, `\\uXXXX`, `\\u{X}`, `\\UXXXXXXXX`, C / Rust /
Python byte escapes, percent-encoding), each also with its double-encoded UTF-8 read back (unmojibake:
cp1250, and Latin-1 / cp1252), compacted (compact: Unicode's default-ignorable characters removed,
compatibility letters in NFKC), and with its undecodable bytes re-read in FALLBACK_CODECS. Views holds
the readings of one text. No reading creates or removes SEP, the separator of a batch's units.
"""
from __future__ import annotations

import html
import html.entities
import re
import unicodedata
from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from denylist_scan import Term

# the separator of a batch's units: a private-use, non-word character that no reading creates or
# removes, so a match maps to its unit by the separators before it
SEP = "\ue000"
# the other readings of bytes that are not valid UTF-8: Windows Central European, Latin-1, ISO
# Central European (ISO-8859-2), DOS Central European (cp852)
FALLBACK_CODECS = ("cp1250", "latin-1", "iso-8859-2", "cp852")
# UTF-8 shown in one of these code pages and encoded again (double-encoded mojibake) is read back
# (unmojibake): Windows Central European; and Western, where Latin-1 and Windows-1252 differ only in
# 0x80-0x9F (C1 controls against punctuation and a few letters), so one reading takes both (#32 F5 m3)
MOJIBAKE_CODECS = {"cp1250": ("cp1250",), "western": ("latin-1", "cp1252")}


def nfc(text: str) -> str:
    """One form for letters with diacritics, so a decomposed `á` cannot hide a term."""
    return text if unicodedata.is_normalized("NFC", text) else unicodedata.normalize("NFC", text)


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
