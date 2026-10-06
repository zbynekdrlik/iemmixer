"""The members of container blobs, for the denylist scan (scripts/denylist_scan.py, #32 F5 m6).

A zip-based file (zip, docx, xlsx, odt, jar ...), a gzip, bzip2 or xz stream and a tar archive keep
their text in members, compressed in all but tar, so the raw bytes the scan reads hold none of it.
expand() yields the blob itself and then each member, decompressed and expanded in turn, as
`<path>!/<member name>` (a stream's one member has no name: `a.txt.gz!/`; a tar inside it is
`b.tar.gz!/!/dir/note.txt`). Decompression is bounded: a member over MEMBER_LIMIT, over RATIO_LIMIT
times its compressed size (past RATIO_FLOOR), past EXPANSION_LIMIT for the whole blob, past
MEMBER_COUNT_LIMIT members or DEPTH_LIMIT levels, an encrypted member and a broken one are each a
Problem, never a silent skip. A container no stdlib module reads (7z, RAR, zstd ...), a PDF with
compressed or encrypted streams and a PNG with a compressed text chunk are a Problem too. A Problem
is allowlisted by its blob's key (blob_key, `--hash PATH blob`).
"""
from __future__ import annotations

import bz2
import hashlib
import io
import lzma
import re
import tarfile
import zipfile
import zlib
from collections.abc import Callable, Iterator
from dataclasses import dataclass

MEMBER_LIMIT = 32 << 20        # bytes of one decompressed member
EXPANSION_LIMIT = 128 << 20    # bytes decompressed from one blob, every member and level together
RATIO_LIMIT = 100              # a member's size over its compressed size ...
RATIO_FLOOR = 1 << 20          # ... checked once the member is larger than this
MEMBER_COUNT_LIMIT = 10_000    # members of one container
DEPTH_LIMIT = 4                # containers inside containers
# containers no stdlib module reads, by their magic bytes
_UNREADABLE = ((b"7z\xbc\xaf\x27\x1c", "a 7z archive"), (b"Rar!\x1a\x07", "a RAR archive"),
               (b"\x28\xb5\x2f\xfd", "a zstd stream"), (b"LZIP\x01", "an lzip stream"),
               (b"\x04\x22\x4d\x18", "an LZ4 frame"), (b"MSCF\x00\x00\x00\x00", "a cabinet archive"),
               (b"\x1f\x9d", "a compress (.Z) stream"), (b"!<arch>\n", "an ar archive"))
_BZIP2 = re.compile(rb"BZh[1-9](?:1AY&SY|\x17rE8P\x90)")
_ZIP_MAGIC = (b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08")
_PNG = b"\x89PNG\r\n\x1a\n"


@dataclass(frozen=True)
class Member:
    path: str   # the blob's path, or `<container path>!/<member name>`
    name: str   # its name inside its container: scanned like a path ("" for the blob and a stream)
    data: bytes


@dataclass(frozen=True)
class Problem:
    path: str
    what: str   # "cannot be scanned: <why>"


class _Budget:
    """The bytes one blob may still expand to (EXPANSION_LIMIT)."""

    def __init__(self) -> None:
        self.left = EXPANSION_LIMIT

    def problem(self, size: int, compressed: int) -> str | None:
        """Why a member of `size` bytes (`compressed` stored) is not decompressed, or None."""
        if size > MEMBER_LIMIT:
            return f"cannot be scanned: a member over {MEMBER_LIMIT >> 20} MiB"
        if size > RATIO_FLOOR and size > RATIO_LIMIT * max(compressed, 1):
            return f"cannot be scanned: a member compressed more than {RATIO_LIMIT}:1"
        if size > self.left:
            return f"cannot be scanned: more than {EXPANSION_LIMIT >> 20} MiB expanded from one blob"
        return None


def blob_key(data: bytes) -> str:
    """The key that allowlists the Problems of a blob (`--hash PATH blob`): its SHA-256, after a NUL
    that keeps it apart from any line of text."""
    return "\0blob sha256:" + hashlib.sha256(data).hexdigest()


def _png_compressed_text(data: bytes) -> bool:
    """A zTXt chunk, or an iTXt chunk with its compression flag set."""
    at = len(_PNG)
    while at + 8 <= len(data):
        length, kind = int.from_bytes(data[at:at + 4], "big"), data[at + 4:at + 8]
        if kind == b"zTXt":
            return True
        if kind == b"iTXt":
            keyword_end = data.find(b"\0", at + 8, at + 8 + length)
            if keyword_end >= 0 and data[keyword_end + 1:keyword_end + 2] == b"\x01":
                return True
        if kind == b"IEND":
            break
        at += 12 + length
    return False


def _is_tar(data: bytes) -> bool:
    """A POSIX / GNU tar header: `ustar` at 257 and the header's own checksum right (so text that
    happens to hold `ustar` there is not read as a broken archive)."""
    if len(data) < 512 or data[257:262] != b"ustar":
        return False
    try:
        stored = int(data[148:156].strip(b"\0 ") or b"x", 8)
    except ValueError:
        return False
    return stored == sum(data[:148]) + 8 * 0x20 + sum(data[156:512])


def _is_zip(data: bytes) -> bool:
    """Zip magic at the start, or an end-of-central-directory record in the last 64 KiB that zipfile
    opens (a zip after other content: a self-extracting archive)."""
    if data.startswith(_ZIP_MAGIC):
        return True
    if data.rfind(b"PK\x05\x06", max(0, len(data) - (1 << 16) - 22)) < 0:
        return False
    try:
        zipfile.ZipFile(io.BytesIO(data)).close()
    except (zipfile.BadZipFile, ValueError, OSError, EOFError):
        return False
    return True


def container_kind(data: bytes) -> str | None:
    """The kind of container a blob is -- one of _READERS, or the description of one that cannot
    be scanned -- or None."""
    for magic, kind in _UNREADABLE:
        if data.startswith(magic):
            return kind
    if data.startswith(b"\x1f\x8b\x08"):
        return "gzip"
    if _BZIP2.match(data):
        return "bzip2"
    if data.startswith(b"\xfd7zXZ\x00"):
        return "xz"
    if _is_tar(data):
        return "tar"
    if data.startswith(b"%PDF-") and (b"/Filter" in data or b"/Encrypt" in data):
        return "a PDF with compressed or encrypted streams"
    if data.startswith(_PNG) and _png_compressed_text(data):
        return "a PNG with a compressed text chunk"
    if _is_zip(data):
        return "zip"
    return None


_ZIP_ERRORS = (zipfile.BadZipFile, zlib.error, lzma.LZMAError, EOFError, OSError, ValueError, RuntimeError)


def _zip_members(path: str, data: bytes, budget: _Budget) -> Iterator[Member | Problem]:
    try:
        archive = zipfile.ZipFile(io.BytesIO(data))
        infos = archive.infolist()
    except _ZIP_ERRORS:
        yield Problem(path, "cannot be scanned: a broken zip archive")
        return
    if len(infos) > MEMBER_COUNT_LIMIT:
        yield Problem(path, f"cannot be scanned: more than {MEMBER_COUNT_LIMIT} members")
        return
    for info in infos:
        member = f"{path}!/{info.filename}"
        if info.is_dir():
            yield Member(member, info.filename, b"")
            continue
        if info.flag_bits & 0x1:
            yield Problem(member, "cannot be scanned: an encrypted zip member")
            continue
        problem = budget.problem(info.file_size, info.compress_size)
        if problem is None:
            try:
                with archive.open(info) as stream:
                    content = stream.read(MEMBER_LIMIT + 1)
            except NotImplementedError:
                problem = f"cannot be scanned: zip compression method {info.compress_type}"
            except _ZIP_ERRORS:
                problem = "cannot be scanned: a broken zip member"
            else:  # the sizes a zip declares are read back, not trusted
                problem = budget.problem(len(content), info.compress_size)
        if problem is not None:
            yield Problem(member, problem)
            continue
        budget.left -= len(content)
        yield Member(member, info.filename, content)


# a compressed stream's decompressor, `max_length` bounded: `decompress(data, limit)`
_DECOMPRESSORS: dict[str, Callable[[], object]] = {
    "gzip": lambda: zlib.decompressobj(wbits=31), "bzip2": bz2.BZ2Decompressor, "xz": lzma.LZMADecompressor}


def _stream_member(path: str, data: bytes, kind: str, budget: _Budget) -> Iterator[Member | Problem]:
    """The one member of a gzip, bzip2 or xz stream, concatenated streams joined (bytes after the
    last stream are left to the raw scan of the blob)."""
    out, rest, limit, first = bytearray(), data, min(MEMBER_LIMIT, budget.left) + 1, True
    try:
        while rest and (first or rest.startswith(data[:3])):
            first = False
            decompressor = _DECOMPRESSORS[kind]()
            out += decompressor.decompress(rest, limit - len(out))
            if len(out) >= limit:
                break
            if not decompressor.eof:
                yield Problem(f"{path}!/", f"cannot be scanned: a broken {kind} stream")
                return
            rest = decompressor.unused_data
    except (zlib.error, OSError, EOFError, lzma.LZMAError, ValueError):
        yield Problem(f"{path}!/", f"cannot be scanned: a broken {kind} stream")
        return
    problem = budget.problem(len(out), len(data) - len(rest))
    if problem is not None:
        yield Problem(f"{path}!/", problem)
        return
    budget.left -= len(out)
    yield Member(f"{path}!/", "", bytes(out))


def _tar_members(path: str, data: bytes, budget: _Budget) -> Iterator[Member | Problem]:
    try:
        archive = tarfile.open(fileobj=io.BytesIO(data), mode="r:", encoding="utf-8", errors="surrogateescape")
        infos = archive.getmembers()
    except (tarfile.TarError, EOFError, OSError, ValueError):
        yield Problem(path, "cannot be scanned: a broken tar archive")
        return
    if len(infos) > MEMBER_COUNT_LIMIT:
        yield Problem(path, f"cannot be scanned: more than {MEMBER_COUNT_LIMIT} members")
        return
    for info in infos:
        member = f"{path}!/{info.name}"
        if not info.isfile():
            yield Member(member, info.name, b"")
            continue
        problem = budget.problem(info.size, info.size)
        if problem is None:
            stream = archive.extractfile(info)
            content = stream.read() if stream is not None else b""
            if len(content) != info.size:
                problem = "cannot be scanned: a broken tar member"
        if problem is not None:
            yield Problem(member, problem)
            continue
        budget.left -= len(content)
        yield Member(member, info.name, content)


_READERS: dict[str, Callable[[str, bytes, _Budget], Iterator[Member | Problem]]] = {
    "zip": _zip_members, "tar": _tar_members,
    **{kind: (lambda path, data, budget, kind=kind: _stream_member(path, data, kind, budget)) for kind in _DECOMPRESSORS}}


def expand(path: str, data: bytes) -> Iterator[Member | Problem]:
    """The blob itself (a Member named ""), then the members of the container it is, each expanded
    in turn, depth first; a part that cannot be scanned is a Problem."""
    yield from _expand(Member(path, "", data), 0, _Budget())


def _expand(part: Member, depth: int, budget: _Budget) -> Iterator[Member | Problem]:
    yield part
    kind = container_kind(part.data)
    if kind is None:
        return
    if kind not in _READERS:
        yield Problem(part.path, f"cannot be scanned: {kind}")
    elif depth == DEPTH_LIMIT:
        yield Problem(part.path, f"cannot be scanned: containers nested more than {DEPTH_LIMIT} deep")
    else:
        for item in _READERS[kind](part.path, part.data, budget):
            yield from _expand(item, depth + 1, budget) if isinstance(item, Member) else (item,)
