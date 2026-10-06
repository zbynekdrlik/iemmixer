"""Tests of what of a blob the denylist scanner reads (#32 F5): embedded UTF-16 strings, long lines
and runs in segments, the members of containers."""
from __future__ import annotations

import bz2
import gzip
import io
import lzma
import random
import sys
import tarfile
import tracemalloc
import unittest
import zipfile
import zlib
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import denylist_content as dc  # noqa: E402
from denylist_test_support import BUDGET_BLOB, BUDGET_MEMORY_BEYOND_BLOB, BUDGET_TERMS, ScanTestCase  # noqa: E402


def zipped(members: dict[str, bytes], method: int = zipfile.ZIP_DEFLATED) -> bytes:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", method) as archive:
        for name, content in members.items():
            archive.writestr(name, content)
    return buffer.getvalue()


def tarred(members: dict[str, bytes]) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as archive:
        for name, content in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(content)
            archive.addfile(info, io.BytesIO(content))
    return buffer.getvalue()


def sparse_tar() -> bytes:
    """An old-GNU sparse tar member whose map reaches far past the data stored for it."""
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", format=tarfile.GNU_FORMAT) as archive:
        payload = b"hello there\n" * 40
        info = tarfile.TarInfo("a.txt")
        info.size = len(payload)
        archive.addfile(info, io.BytesIO(payload))
    header = bytearray(buffer.getvalue())
    header[156] = ord("S")                        # GNUTYPE_SPARSE
    header[386:398] = b"%011o\0" % 0              # sparse entry 0: offset 0 ...
    header[398:410] = b"%011o\0" % 200000         # ... 200000 bytes, more than are stored
    header[482] = 0                               # no extended sparse header
    header[483:495] = b"%011o\0" % 200000         # the real size
    header[148:156] = b"        "
    header[148:156] = b"%06o\0 " % sum(header[:512])
    return bytes(header)


def png_chunk(kind: bytes, body: bytes) -> bytes:
    return len(body).to_bytes(4, "big") + kind + body + zlib.crc32(kind + body).to_bytes(4, "big")


class ContentTests(ScanTestCase):
    # --- #32 F5 m11: an embedded UTF-16 string is one finding, not one per byte order ---

    def test_an_embedded_utf16_string_is_found_once(self) -> None:
        # a little-endian string read big-endian one byte later spells the same letters, so it was
        # reported twice (two allowlist lines for one string); the term may end the string
        for codec in ("utf-16-le", "utf-16-be"):
            for text in ("C:\\Users\\zyxname\\trace", "C:\\data\\zyxname"):
                with self.subTest(codec=codec, text=text):
                    name = f"t{len(text)}-{codec}.etl"
                    content = b"\x07\x01\x02\x03" * 32 + text.encode(codec) + b"\x00\x00\xfe\x07"  # binary, not wide text
                    out = self.assert_found_in_both_modes_as({name: content}, "zyxname")
                    self.assertEqual(out.count(f" {name}:"), 1, out)
                    code, out = self.scan("--commits", "HEAD~1..HEAD")
                    self.assertEqual(out.count(f" {name}:"), 1, out)

    # --- #32 F5 m9: a long line or run is read in bounded, overlapping segments ---

    def assert_within_the_memory_budget(self, content: bytes, terms: list[str]) -> None:
        self.deny.write_text("\n".join(terms) + "\n", encoding="utf-8")
        self.commit({"long.bin": content})
        tracemalloc.start()
        try:
            code, out = self.scan("--tree", "HEAD")
            peak = tracemalloc.get_traced_memory()[1]
        finally:
            tracemalloc.stop()
        self.assertEqual(code, 0, out)
        self.assertLess(peak, 2 * len(content) + BUDGET_MEMORY_BEYOND_BLOB, f"{peak / (1 << 20):.0f} MiB peak")

    def test_one_long_line_stays_within_the_memory_budget(self) -> None:
        # batches were cut only at a line break, so a 4 MiB line was one batch and each of its
        # readings a copy of it (escapes, accents and invisible characters make most of them)
        unit = "abc \\x41 é\xad &amp; %41 ".encode()
        self.assert_within_the_memory_budget(unit * (BUDGET_BLOB // len(unit)), [*BUDGET_TERMS, "ďabc"])

    def test_one_long_binary_run_stays_within_the_memory_budget(self) -> None:
        unit = "abcd é\xad zz ".encode()
        self.assert_within_the_memory_budget(b"\x00" + unit * (BUDGET_BLOB // len(unit)) + b"\x00",
                                             [*BUDGET_TERMS, "ďabc"])

    def test_a_term_across_a_segment_boundary_is_found_once_and_keyed_alike(self) -> None:
        # the term starts 3 characters before the unit's first CHUNK ends; its line or run is
        # allowlisted by one --hash key in tree and commit mode
        long = "x" * (dc.CHUNK - 4) + " zyxname " + "y" * dc.CHUNK
        noise = random.Random(9).randbytes(4 * dc.CHUNK).replace(b"\x00", b"\x01")  # keeps a wide string binary
        cases = {"text.txt": (("first\n" + long + "\n").encode(), "2"),
                 "run.bin": (b"\x00\x01" + long.encode() + b"\x00", "run 1"),
                 "wide.txt": (("first\n" + long + "\n").encode("utf-16"), "2"),
                 "wide-run.bin": (b"\x00\x02" + long.encode("utf-16-le") + b"\x00\x00" + noise, None)}
        for name, (content, unit) in cases.items():
            with self.subTest(name=name):
                self.commit({"base.txt": f"base {name}\n"})
                self.commit({name: content})
                for mode in (("--tree", "HEAD"), ("--commits", "HEAD~1..HEAD")):
                    code, out = self.scan(*mode)
                    self.assertEqual(code, 1, mode)
                    self.assertEqual(out.count(f" {name}"), 1, out)
                    self.assertNotIn("zyxname", out.lower())
                if unit is None:  # the wide run's number depends on the noise's own runs
                    unit = next(line for line in out.splitlines() if name in line).split(":")[1].strip()
                allow = self.tmp / "allow.txt"
                allow.write_text(self.hash_key(name, unit) + "  reviewed ordinary prose\n", encoding="utf-8")
                self.assertEqual(self.scan("--allow", str(allow), "--tree", "HEAD", "--commits", "HEAD~1..HEAD"),
                                 (0, "denylist: clean\n"))
                self.commit({name: b"removed"})

    # --- #32 F5 m6: the members of containers are decompressed and scanned, or the blob is a finding ---

    def assert_findings_in_both_modes(self, files: dict[str, bytes], *expected: str) -> None:
        self.commit({"base.txt": f"base {sorted(files)}\n"})
        self.commit(files)
        for prefix, mode in (("tree ", ("--tree", "HEAD")), ("", ("--commits", "HEAD~1..HEAD"))):
            code, out = self.scan(*mode)
            self.assertEqual(code, 1, out)
            self.assertNotIn("zyxname", out.lower())
            for location in expected:
                self.assertIn(f"{prefix}{location}" if prefix else f" {location}", out)

    def test_the_members_of_zip_based_files_are_scanned(self) -> None:
        # a docx / xlsx / odt is a zip of deflated XML: its raw bytes hold no readable term
        document = b'<w:document><w:t>by zyxname</w:t></w:document>\n'
        inner = zipped({"c.txt": b"first\nkeep zyxname\n"})
        self.assert_findings_in_both_modes(
            {"doc.docx": zipped({"[Content_Types].xml": b"<Types/>", "word/document.xml": document}),
             "sheet.xlsx": zipped({"xl/sharedStrings.xml": b"<sst><si><t>zyxname</t></si></sst>"}),
             "outer.zip": zipped({"b.zip": inner}, zipfile.ZIP_STORED)},
            "doc.docx!/word/document.xml:1: denylist entry 1", "sheet.xlsx!/xl/sharedStrings.xml:1: denylist entry 1",
            "outer.zip!/b.zip!/c.txt:2: denylist entry 1")

    def test_compressed_streams_and_tar_members_are_scanned(self) -> None:
        text = b"line one\nhello zyxname\n"
        self.assert_findings_in_both_modes(
            {"a.txt.gz": gzip.compress(text), "a.txt.bz2": bz2.compress(text), "a.txt.xz": lzma.compress(text),
             "two.gz": gzip.compress(b"clean\n") + gzip.compress(text),
             "b.tar.gz": gzip.compress(tarred({"dir/note.txt": b"by zyxname\n", "dir/clean.txt": b"clean\n"}))},
            "a.txt.gz!/:2: denylist entry 1", "a.txt.bz2!/:2: denylist entry 1", "a.txt.xz!/:2: denylist entry 1",
            "two.gz!/:3: denylist entry 1", "b.tar.gz!/!/dir/note.txt:1: denylist entry 1")

    def test_a_member_name_is_scanned_and_shown_like_a_path(self) -> None:
        self.assert_findings_in_both_modes(
            {"names.zip": zipped({"docs/zyxname-notes.txt": b"clean\n", "résumé/a.txt": b"x zyxname\n"})},
            "names.zip!/docs/[redacted]: path: denylist entry 1", "names.zip!/[redacted]/a.txt:1: denylist entry 1")

    def test_a_member_over_a_limit_or_broken_is_a_finding(self) -> None:
        encrypted = bytearray(zipped({"secret.txt": b"zyxname\n"}))
        central = encrypted.index(b"PK\x01\x02")
        encrypted[central + 8] |= 1  # the central directory's general purpose flag: encrypted
        self.assert_findings_in_both_modes(
            {"bomb.zip": zipped({"zeros.bin": bytes(4 << 20)}), "broken.zip": b"PK\x03\x04" + bytes(40) + b"zyx",
             "secret.zip": bytes(encrypted), "cut.gz": gzip.compress(b"hello zyxname\n" * 50)[:-30]},
            "bomb.zip!/zeros.bin: cannot be scanned: ", "broken.zip: cannot be scanned: ",
            "secret.zip!/secret.txt: cannot be scanned: ", "cut.gz!/: cannot be scanned: ")
        with mock.patch("denylist_containers.MEMBER_LIMIT", 1 << 10):
            self.assert_findings_in_both_modes({"big.zip": zipped({"big.txt": b"clean words " * 200})},
                                               "big.zip!/big.txt: cannot be scanned: ")

    def test_a_tar_member_that_cannot_be_read_is_a_finding_not_a_crash(self) -> None:
        # review of lane G3, finding 1: extractfile().read() raised outside any handler, a traceback
        # that no allowlist line could clear
        self.assert_findings_in_both_modes({"sparse.tar": sparse_tar()},
                                           "sparse.tar!/a.txt: cannot be scanned: a broken tar member")
        self.assertEqual(len(self.hash_key("sparse.tar", "blob")), 64)

    def test_padded_streams_and_crafted_zip_members_are_read_whole(self) -> None:
        # review of lane G3, finding 2: these were skipped silently -- xz's stream padding and NULs
        # between gzip members ended the stream; a zip directory entry carrying data was never read;
        # zipfile reads a member only to its declared size, so a size of 0 hid the deflate data
        text = b"hello zyxname\n" * 20
        directory = io.BytesIO()
        with zipfile.ZipFile(directory, "w") as archive:
            entry = zipfile.ZipInfo("dir/")
            entry.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(entry, text)
        short = bytearray(zipped({"a.txt": text}))
        for header, crc, size in ((b"PK\x03\x04", 14, 22), (b"PK\x01\x02", 16, 24)):  # local, central
            at = short.index(header)
            short[at + crc:at + crc + 4] = bytes(4)
            short[at + size:at + size + 4] = bytes(4)
        self.assert_findings_in_both_modes(
            {"p.xz": lzma.compress(b"clean\n") + bytes(4) + lzma.compress(text),
             "p.gz": gzip.compress(b"clean\n") + bytes(8) + gzip.compress(text),
             "d.zip": directory.getvalue(), "s.zip": bytes(short)},
            "p.xz!/:2: denylist entry 1", "p.gz!/:2: denylist entry 1", "d.zip!/dir/:1: denylist entry 1",
            "s.zip!/a.txt:1: denylist entry 1")

    def test_plain_text_is_never_read_as_a_container(self) -> None:
        # review of lane G3, finding 4: text starting like a bzip2 stream, an ar or lzip archive or
        # a PDF was reported as a container that cannot be scanned
        self.commit({"notes/bz.txt": "BZh91AY&SY then ordinary words\n", "notes/ar.md": "!<arch>\nordinary notes\n",
                     "notes/lz.txt": "LZIP\x01 ordinary words\n", "notes/pdf.txt": "%PDF-1.7 notes on /Filter and /Encrypt\n",
                     "notes/tar.txt": "x" * 257 + "ustar" + "y" * 300 + "\n"})
        self.assertEqual(self.scan("--tree", "HEAD", "--commits", "HEAD"), (0, "denylist: clean\n"))

    def test_a_pdf_with_streams_is_a_finding_whatever_its_filter_is_called(self) -> None:
        # a name object may spell a character as #xx, so `/Fil#74er` is a /Filter the text check missed
        pdf = (b"%PDF-1.4\n1 0 obj << /Length 20 /Fil#74er /FlateDecode >> stream\n" + zlib.compress(b"(zyxname) Tj")
               + b"\nendstream endobj\n%%EOF\n")
        self.assert_findings_in_both_modes({"hex.pdf": pdf}, "hex.pdf: cannot be scanned: a PDF with streams")

    def test_the_members_and_the_expansion_of_one_blob_are_bounded(self) -> None:
        # review of lane G3, finding 3: the member limit was per container, so containers nested in
        # one blob multiplied it; and members just under the ratio floor expanded a small blob ~400x
        many = zipped({f"inner{number}.zip": zipped({f"e{entry}.txt": b"" for entry in range(20)})
                       for number in range(3)}, zipfile.ZIP_STORED)
        dense = zipped({f"m{number}.txt": b"abcd" * (15 << 10) for number in range(30)})
        with mock.patch("denylist_containers.MEMBER_COUNT_LIMIT", 50), \
                mock.patch("denylist_containers.RATIO_FLOOR", 64 << 10):
            self.assert_findings_in_both_modes(
                {"many.zip": many, "dense.zip": dense},
                "many.zip!/inner2.zip: cannot be scanned: more than 50 members in one blob",
                "dense.zip!/m1.txt: cannot be scanned: decompressed to more than 20 times the blob")  # 20 x 5132 bytes
            code, out = self.scan("--tree", "HEAD")
            self.assertEqual(out.count("dense.zip!/"), 1, out)  # the blob's expansion stops at its limit

    def test_other_containers_are_findings_allowlisted_by_their_blob_key(self) -> None:
        pdf = (b"%PDF-1.4\n1 0 obj << /Length 20 /Filter /FlateDecode >> stream\n" + zlib.compress(b"(zyxname) Tj")
               + b"\nendstream endobj\n%%EOF\n")
        png = (b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", bytes(13))
               + png_chunk(b"zTXt", b"Comment\x00\x00" + zlib.compress(b"by zyxname")) + png_chunk(b"IEND", b""))
        files = {"a.7z": b"7z\xbc\xaf\x27\x1c" + bytes(30), "doc.pdf": pdf, "img.png": png,
                 "a.zst": b"\x28\xb5\x2f\xfd" + bytes(20)}
        self.assert_findings_in_both_modes(files, *(f"{name}: cannot be scanned: " for name in files))
        allow = self.tmp / "allow.txt"
        allow.write_text("".join(f"{self.hash_key(name, 'blob')}  reviewed, no site data\n" for name in files),
                         encoding="utf-8")
        self.assertEqual(self.scan("--allow", str(allow), "--tree", "HEAD", "--commits", "HEAD~1..HEAD"),
                         (0, "denylist: clean\n"))

    def test_a_member_line_is_allowlisted_and_only_new_member_units_count_in_commit_mode(self) -> None:
        self.commit({"pack.zip": zipped({"a.txt": b"keep zyxname here\n", "b.txt": b"one\n"})})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 1)
        allow = self.tmp / "allow.txt"
        allow.write_text(self.hash_key("pack.zip!/a.txt", "1") + "  reviewed ordinary prose\n", encoding="utf-8")
        self.assertEqual(self.scan("--allow", str(allow), "--tree", "HEAD", "--commits", "HEAD"), (0, "denylist: clean\n"))
        self.commit({"pack.zip": zipped({"a.txt": b"keep zyxname here\n", "b.txt": b"two\n"})})
        self.assertEqual(self.scan("--commits", "HEAD~1..HEAD"), (0, "denylist: clean\n"))


if __name__ == "__main__":
    unittest.main()
