"""Tests of what of a blob the denylist scanner reads (#32 F5): embedded UTF-16 strings, long lines
and runs in segments, the members of containers."""
from __future__ import annotations

import random
import sys
import tracemalloc
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import denylist_content as dc  # noqa: E402
from denylist_test_support import BUDGET_BLOB, BUDGET_MEMORY_BEYOND_BLOB, BUDGET_TERMS, ScanTestCase  # noqa: E402


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


if __name__ == "__main__":
    unittest.main()
