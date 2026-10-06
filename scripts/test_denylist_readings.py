"""Tests of the readings and term spellings the denylist scanner matches (#32 F5): ASCII spellings,
whitespace between words, code pages, character references, ignorable characters."""
from __future__ import annotations

import os
import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from denylist_test_support import ScanTestCase, git  # noqa: E402


class ReadingTests(ScanTestCase):
    # --- #32 F5 m10: the two non-ASCII characters whose normal form is ASCII punctuation ---

    def test_the_greek_question_mark_and_varia_read_as_their_ascii_forms(self) -> None:
        # NFC maps U+037E to `;` and U+1FEF to a backtick, so an ASCII term holding either could be
        # spelled with them past the ASCII-only readings
        self.add_terms("qxv;zyxw", "zyx`qwvn")
        for name, text in (("greek.txt", "qxv\u037ezyxw"), ("varia.txt", "zyx\u1fefqwvn")):
            with self.subTest(name=name):
                self.assert_found_in_both_modes_as({name: f"x {text} y\n"}, "zyx")

    # --- #32 F5 m5: named character references and the other default-ignorable characters ---

    def test_named_references_and_ignorable_characters_do_not_hide_a_term(self) -> None:
        self.add_terms("ďqxwzy")
        texts = {"dcaron.html": "<b>&dcaron;qxwzy</b>", "shy.html": "zyx&shy;name", "zwsp.html": "zyx&ZeroWidthSpace;name",
                 "double.html": "&amp;#271;qxwzy"}
        texts |= {f"ignorable-{number}.md": f"zyx{char}name" for number, char in enumerate(
            ("\u200e", "\u200f", "\u034f", "\ufe0f", "\ufe00", "\u2061", "\u2064", "\u2066", "\u202a",
             "\u061c", "\u180e", "\U000e0020"))}
        for name, text in texts.items():
            with self.subTest(name=name):
                self.assert_found_in_both_modes_as({name: f"x {text} y\n"}, "qxwzy" if "qxwzy" in text else "name")

    def test_a_named_reference_never_makes_a_batch_separator(self) -> None:
        # a reading that created U+E000 would shift every later unit: the hit must stay on line 3
        self.commit({"a.html": "&dcaron;\n&#xE000;&#57344;\nkeep zyxname\n"})
        code, out = self.scan("--tree", "HEAD")
        self.assertEqual((code, out.count("denylist entry")), (1, 1), out)
        self.assertIn("tree a.html:3: denylist entry 1", out)

    # --- #32 F5 m3: UTF-8 shown as Windows-1252 (or with a byte cp1250 / cp1252 leave undefined) ---

    def test_utf8_double_encoded_through_windows_code_pages_is_read_back(self) -> None:
        # Windows reads UTF-8 as cp1252 (`ň` C5 88 shows as `Åˆ`) -- neither cp1250 nor Latin-1 reads
        # 0x88 that way -- and it reads a byte a code page leaves undefined as the C1 control of the same
        # number (`Á` C3 81 through cp1250 shows as `Ă` and U+0081)
        self.add_terms("ňqxwzy", "ŕqxwzy", "áqxwzy", "čqxwzy")
        texts = {"cp1252-n.txt": "ňqxwzy".encode().decode("cp1252"), "cp1252-r.txt": "ŕqxwzy".encode().decode("cp1252"),
                 "cp1250-hole.txt": b"\xc3".decode("cp1250") + "\x81qxwzy",
                 "cp1252-hole.txt": b"\xc4".decode("cp1252") + "\x8dqxwzy"}
        for name, text in texts.items():
            with self.subTest(name=name):
                self.assert_found_in_both_modes_as({name: f"meno: {text}\n"}, "qxwzy")

    # --- #32 F5 m4: commit metadata in the other Central European encodings ---

    def test_commit_metadata_in_iso_8859_2_or_cp852_is_read_back(self) -> None:
        # git stores a message or a name that is not valid UTF-8 with each such byte converted as if
        # it were Latin-1; only cp1250 was read back from that, so `š` (B9 in ISO-8859-2, E7 in cp852)
        # hid the term in a message and in an author name
        self.add_terms("šqxwzy")
        self.commit({"a.txt": "base\n"})
        for number, codec in enumerate(("iso-8859-2", "cp852")):
            for field in ("message", "author"):
                with self.subTest(codec=codec, field=field):
                    message = self.tmp / "message.txt"
                    raw = f"fix for Šqxwzy {number}".encode(codec)
                    message.write_bytes(raw if field == "message" else b"clean message")
                    env = {**os.environb, b"GIT_AUTHOR_NAME": raw if field == "author" else b"test"}
                    subprocess.run(["git", "-C", str(self.repo), "commit", "-q", "--allow-empty", "-F", str(message)],
                                   check=True, capture_output=True, env=env)
                    code, out = self.scan("--commits", "-1 HEAD")
                    self.assertEqual(code, 1, out)
                    self.assertIn("commit metadata: denylist entry 4", out)

    # --- #32 F5 MAJOR: a term with diacritics is found in its plain ASCII spelling too ---

    def test_a_diacritic_term_is_found_in_its_ascii_spelling(self) -> None:
        # names lose their diacritics in paths, e-mail addresses, identifiers and host names, so
        # `ďqxwzy` written `dqxwzy` passed; letters NFKD keeps whole are spelled out (ł l, ß ss)
        self.add_terms("ďqxwzy", "łqzxwv ßqzv")
        texts = {"mail.txt": ("contact dqxwzy@example.org", "dqxwzy"), "ident.rs": ("let DQXWZY_HOST = 1;", "dqxwzy"),
                 "host.txt": ("https://dqxwzy.example.org/", "dqxwzy"), "other.txt": ("by lqzxwv ssqzv", "qzxwv")}
        for name, (text, letters) in texts.items():
            with self.subTest(name=name):
                self.assert_found_in_both_modes_as({name: f"{text}\n"}, letters)

    def test_an_ascii_term_is_found_where_the_text_adds_diacritics(self) -> None:
        # the reverse of the ASCII spelling: an entry the list holds without its diacritics (a name
        # typed in ASCII) was missed where the text writes them (#32 lane G3 follow-up)
        self.add_terms("zyxqwvn")
        texts = {"name.txt": "by z\u00fdxqwv\u0148 today", "decomposed.txt": "zyx q z\u0301yxqwvn",
                 "upper.txt": "Z\u00ddXQWV\u0147"}
        for name, text in texts.items():
            with self.subTest(name=name):
                self.assert_found_in_both_modes_as({name: f"{text}\n"}, "qwv")

    def test_the_ascii_spelling_of_a_term_is_redacted_in_paths_and_found_in_metadata(self) -> None:
        self.add_terms("ďqxwzy")
        out = self.assert_found_in_both_modes_as({"docs/dqxwzy-notes.md": "x zyxname\n"}, "qxwzy")
        self.assertIn("tree docs/[redacted]: path: denylist entry 4", out)
        git(self.repo, "config", "user.email", "dqxwzy@example.org")
        self.commit({"a.txt": "clean\n"})
        code, out = self.scan("--commits", "-1 HEAD")
        self.assertEqual(code, 1)
        self.assertIn("commit metadata: denylist entry 4", out)
        self.assertNotIn("qxwzy", out.lower())

    # --- #32 F5 m1: the words of a multi-word term may be split by any whitespace, a line break too ---

    def test_any_whitespace_between_the_words_of_a_term_is_matched(self) -> None:
        self.add_terms("zyxa qwvb")
        texts = {"nbsp.txt": "zyxa\xa0qwvb", "tab.txt": "zyxa\tqwvb", "double.txt": "zyxa  qwvb",
                 "entity.html": "zyxa&nbsp;qwvb", "em.txt": "zyxa\u2003qwvb", "crlf.txt": "zyxa \r qwvb"}
        for name, text in texts.items():
            with self.subTest(name=name):
                self.assert_found_in_both_modes_as({name: f"by {text} here\n"}, "qwvb")

    def test_a_term_split_across_two_lines_is_found_on_its_first_line(self) -> None:
        # a wrapped paragraph or commit message puts a line break between the words
        self.add_terms("zyxa qwvb")
        out = self.assert_found_in_both_modes_as({"wrap.md": "intro\nnamed zyxa\nqwvb and more\n"}, "qwvb")
        self.assertIn("tree wrap.md:2: denylist entry 4", out)
        self.commit({"a.txt": "clean\n"}, message="fix for the zyxa\nqwvb case")
        code, out = self.scan("--commits", "-1 HEAD")
        self.assertEqual(code, 1)
        self.assertIn("commit metadata: denylist entry 4", out)

    def test_a_term_split_across_two_lines_is_allowlisted_by_its_first_line(self) -> None:
        self.add_terms("zyxa qwvb")
        self.commit({"wrap.md": "named zyxa\nqwvb and more\n"})
        self.assertEqual(self.scan("--tree", "HEAD", "--commits", "HEAD")[0], 1)
        allow = self.tmp / "allow.txt"
        allow.write_text(self.hash_key("wrap.md", "1") + "  reviewed ordinary prose\n", encoding="utf-8")
        self.assertEqual(self.scan("--allow", str(allow), "--tree", "HEAD", "--commits", "HEAD")[0], 0)


if __name__ == "__main__":
    unittest.main()
