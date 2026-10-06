"""What of a blob the denylist scan reads (scripts/denylist_scan.py, #32): numbered units in batches.

A blob is read as numbered units: the lines of text; the decoded lines of UTF-32 / UTF-16 text, then
its byte runs (a binary may only look like wide text); for other content holding a NUL byte (binary)
its text runs -- byte runs without control characters, then embedded UTF-16 strings -- labelled
`run N`. Units are scanned in Batches of at most CHUNK (a unit longer than that in overlapping
segments), so CPU and memory stay bounded, and tree mode, commit mode and `--hash` number and key
them alike.
"""
from __future__ import annotations

import hashlib
import re
from collections.abc import Generator, Iterator
from dataclasses import dataclass

from denylist_readings import SEP, decode, undecodable

# In binary content random bytes form short words by chance: in this repository's f64 goldens 28 %
# of all 3-letter and 0.5 % of all 4-letter words occur as words of their text runs, 0.003 % of the
# 5-letter ones. So in a binary text run a term shorter than MIN_BINARY_TERM characters counts only
# when the run is LONG_TEXT_RUN or more bytes of valid UTF-8 -- text, which random bytes never form.
MIN_BINARY_TERM = 5
LONG_TEXT_RUN = 32
# Content is scanned in batches -- whole lines of at most CHUNK bytes, binary runs up to twice that,
# a longer line or run in segments -- joined by SEP, so CPU and memory stay bounded (#32 review m7).
CHUNK = 1 << 18
# A line or run longer than CHUNK is read in segments of CHUNK, each reaching back this far into the
# one before (#32 F5 m9) -- and at least OVERLAP_PER_CHARACTER times the longest term, the longest
# spelling of one character in a reading being four percent-encoded bytes (`%F0%9F%98%80`) -- so
# no match is split and memory stays bounded however long the line
SEGMENT_OVERLAP = 1 << 12
OVERLAP_PER_CHARACTER = 12
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


@dataclass(frozen=True)
class Batch:
    """Consecutive units of one blob or diff -- lines of text, or text runs of binary content --
    scanned together, or one segment of a unit longer than CHUNK. A batch holds at most CHUNK of
    content plus an overlap, so memory stays bounded whatever the size of the blob or of a line."""
    source: str             # the units, decoded (decode), separated by `sep`, which no unit holds
    sep: str
    first: int              # the number of its first unit (`--hash` numbers units the same way)
    label: str = ""         # a unit's label in a location: "" (line N) or "run " (text run N)
    runs: bool = False      # byte runs of binary content: a short term needs a long text run
    key: str | None = None  # a segment of a unit longer than CHUNK: that unit's key (long_key)

    def texts(self) -> list[str]:
        """Each unit's text (a segment's own)."""
        return self.source.split(self.sep)

    def keys(self) -> list[str]:
        """Each unit's key, which its allow key is made of: its text, or a long unit's long_key."""
        return [self.key] if self.key is not None else self.texts()

    def text(self) -> str:
        """The units joined by SEP; a unit's own U+E000 is read as U+FFFD, also a non-word character."""
        return self.source.replace(SEP, "\ufffd").replace(self.sep, SEP)


def long_text_run(text: str) -> bool:
    return not undecodable(text) and len(text.encode("utf-8")) >= LONG_TEXT_RUN


def long_key(data: bytes | str, start: int, end: int) -> str:
    """The key of a unit longer than CHUNK, data[start:end]: its SHA-256, hashed in place. Tree mode,
    commit mode and `--hash` read such a unit in the same segments, so its allow key is the same in
    all three (#32 F5 m9); the leading NUL keeps it apart from any line of text."""
    digest = hashlib.sha256()
    if isinstance(data, bytes):
        digest.update(memoryview(data)[start:end])
    else:
        for at in range(start, end, CHUNK):
            digest.update(data[at:min(end, at + CHUNK)].encode("utf-8", "surrogateescape"))
    return "\0sha256:" + digest.hexdigest()


# a segment edge goes right after one of these: no reading changes them or spans them (no escape,
# reference or double-encoded character holds one but `;`, which ends a reference), so the edge
# reads as the non-word character the content has there
_SEGMENT_CUTS = (" ", "\t", ",", ";")
_SEGMENT_CUT_BYTES = tuple(cut.encode() for cut in _SEGMENT_CUTS)


def _segment_edge(data: bytes | str, low: int, at: int) -> int:
    """A segment edge in data[low:at]: right after its last _SEGMENT_CUTS character; if it has none
    (a word longer than the window, base64 say), `at` itself moved past UTF-8 continuation bytes, so
    no character is split -- where a term may then seem to start or end at the edge."""
    found = max(data.rfind(cut, low, at) for cut in (_SEGMENT_CUT_BYTES if isinstance(data, bytes) else _SEGMENT_CUTS))
    if found >= 0:
        return found + 1
    if isinstance(data, bytes):
        for _ in range(3):
            if at < len(data) and 0x80 <= data[at] < 0xC0:
                at += 1
    return at


def long_unit_batches(data: bytes | str, start: int, end: int, number: int, label: str, runs: bool,
                      overlap: int) -> Iterator[Batch]:
    """Unit `number`, data[start:end], longer than CHUNK, in segments (#32 F5 m9): steps of about
    CHUNK (at least six overlaps), and each segment reaching back at least `overlap` before its step,
    so a match up to `overlap` long that crosses a step or a segment's start lies whole in the segment
    before or after it. Edges sit right after a _SEGMENT_CUTS character where the content has one,
    so a term at an edge is matched as in the whole unit. Each segment is a batch of its own carrying
    the whole unit's key; a match inside an overlap is found twice, and findings reports it once."""
    key = long_key(data, start, end)
    step = max(CHUNK, 6 * overlap)
    sep = "\x00" if label else "\n"
    segment_start = step_start = start
    while step_start < end:
        step_end = end if end - step_start <= step else _segment_edge(data, step_start + step // 2, step_start + step)
        piece = data[segment_start:step_end]
        yield Batch(decode(piece) if isinstance(piece, bytes) else piece, sep, number, label, runs, key)
        segment_start = max(start, _segment_edge(data, step_end - 2 * overlap, step_end - overlap))
        step_start = step_end


def line_batches(data: bytes | str, first: int = 1, overlap: int = SEGMENT_OVERLAP) -> Generator[Batch, None, int]:
    """The lines of text (split on `\\n` only): whole lines of at most CHUNK at a time, and a line
    longer than that in overlapping segments (long_unit_batches). Returns the number the next unit
    gets."""
    newline = b"\n" if isinstance(data, bytes) else "\n"
    start, size = 0, len(data)
    while True:
        # the rest of the content, or the last line break within CHUNK
        cut = size if size - start <= CHUNK else data.rfind(newline, start, start + CHUNK + 1)
        if cut >= 0:
            piece = data[start:cut]
            source = decode(piece) if isinstance(piece, bytes) else piece
            yield Batch(source, "\n", first)
            first += source.count("\n") + 1
            if cut == size:
                return first
            start = cut + 1
        else:  # a line longer than CHUNK
            end = data.find(newline, start + CHUNK)
            end = size if end < 0 else end
            yield from long_unit_batches(data, start, end, first, "", False, overlap)
            first += 1
            if end == size:
                return first
            start = end + 1


def _run_piece(data: bytes, start: int, end: int, first: int) -> Generator[Batch, None, int]:
    """The runs of data[start:end] (which ends at a control byte or the content's end) as one batch."""
    piece = _SHORT_RUN.sub(b"\x00", b"\x00" + data[start:end].translate(_CONTROL_TO_NUL) + b"\x00")
    piece = _NUL_RUN.sub(b"\x00", piece).strip(b"\x00")
    if piece:
        yield Batch(decode(piece), "\x00", first, "run ", runs=True)
        first += piece.count(b"\x00") + 1
    return first


def run_batches(data: bytes, first: int, overlap: int = SEGMENT_OVERLAP) -> Generator[Batch, None, int]:
    """The byte runs of binary content -- bytes without control characters, so a UTF-8 / cp1250
    letter stays in its run -- of MIN_BINARY_TERM or more bytes (a shorter one can never count: a
    short term needs a LONG_TEXT_RUN), NUL-separated, a piece of about CHUNK at a time: a piece ends
    at a control byte, which no run holds, and no Python object is made per run. A run longer than
    CHUNK is read in overlapping segments (long_unit_batches). Returns the number the next unit
    gets."""
    start, size = 0, len(data)
    while start < size:
        cut = _CONTROL.search(data, start + CHUNK)
        end, run_end = (cut.end(), cut.start()) if cut else (size, size)
        if run_end - start <= 2 * CHUNK:
            first = yield from _run_piece(data, start, end, first)
        else:  # the run that crosses start + CHUNK is longer than CHUNK: it starts after the last
            # control byte before start + CHUNK and ends at run_end
            run_start = start + data[start:start + CHUNK].translate(_CONTROL_TO_NUL).rfind(b"\x00") + 1
            first = yield from _run_piece(data, start, run_start, first)
            yield from long_unit_batches(data, run_start, run_end, first, "run ", True, overlap)
            first += 1
        start = end
    return first


def wide_runs(data: bytes) -> list[str]:
    """The UTF-16 strings inside binary content, in the order they occur (few: random bytes rarely
    form one). A string read in the other byte order one byte later spells the same letters, so of
    two readings that overlap, the one starting first -- at the string's first byte -- is kept, and
    the other only when it runs on more than a byte past it (#32 F5 m11: one finding per string)."""
    size = len(data)
    spans = sorted([(size - run.end(), size - run.start(), "utf-16-le") for run in _UTF16_RUN.finditer(data[::-1])]
                   + [(run.start(), run.end(), "utf-16-be") for run in _UTF16_RUN.finditer(data)])
    kept: list[tuple[int, int, str]] = []
    last_end = {"utf-16-le": -1, "utf-16-be": -1}  # where the last kept reading in each byte order ends
    for start, end, codec in spans:
        other = last_end["utf-16-be" if codec == "utf-16-le" else "utf-16-le"]
        if start < other and end <= other + 1:
            continue
        kept.append((start, end, codec))
        last_end[codec] = end
    return [data[start:end].decode(codec, errors="replace") for start, end, codec in kept]


def wide_run_batches(data: bytes, first: int, overlap: int = SEGMENT_OVERLAP) -> Iterator[Batch]:
    """The UTF-16 strings inside binary content (wide_runs), about CHUNK characters at a time, and a
    string longer than that in overlapping segments (long_unit_batches). A decoded UTF-16 run holds no
    NUL: its characters are U+0009 and U+0020-01FF."""
    pending: list[str] = []
    size = 0
    for run in wide_runs(data):
        if pending and (size + len(run) > CHUNK or len(run) > CHUNK):
            yield Batch("\x00".join(pending), "\x00", first, "run ")
            first += len(pending)
            pending, size = [], 0
        if len(run) > CHUNK:
            yield from long_unit_batches(run, 0, len(run), first, "run ", False, overlap)
            first += 1
        else:
            pending.append(run)
            size += len(run) + 1
    if pending:
        yield Batch("\x00".join(pending), "\x00", first, "run ")


def batches(data: bytes, overlap: int = SEGMENT_OVERLAP) -> Iterator[Batch]:
    """What of a blob is scanned, numbered on unit after unit: text as its lines; UTF-32 / UTF-16
    text as its decoded lines and then its byte runs too (a binary may only look like it: quiet
    16-bit PCM has a NUL high byte in nearly every sample, and any blob may start FF FE -- genuine
    UTF-16 / UTF-32 Latin text has no byte run a term could match); other content holding a NUL
    byte (binary) as its text runs -- byte runs, then UTF-16 strings. A run is labelled `run N`.
    `overlap` only places the segments of a long unit; units, their numbers and keys never depend
    on it."""
    codec = wide_codec(data)
    if codec is not None:
        first = yield from line_batches(data.decode(codec, errors="replace"), 1, overlap)
        yield from run_batches(data, first, overlap)
    elif b"\0" not in data:
        yield from line_batches(data, 1, overlap)
    else:
        first = yield from run_batches(data, 1, overlap)
        yield from wide_run_batches(data, first, overlap)


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
