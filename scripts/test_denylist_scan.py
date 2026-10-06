"""Tests for scripts/denylist_scan.py (run: python3 -m unittest discover -s scripts)."""
from __future__ import annotations

import contextlib
import io
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time
import tracemalloc
import unicodedata
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import denylist_scan as ds  # noqa: E402

TERMS = ["zyxname", "10.9.", "ghost-host.example"]
REDACTED_MARKER = "[redacted]"
# #32 review m7: the stated budget for binary content (public-repo-hygiene.md) -- a 4 MiB blob of
# pseudo-random bytes against 40 invented terms, in tree mode through main()
BUDGET_BLOB = 4 << 20
BUDGET_CPU_PER_MIB = 1.0          # seconds of this process's CPU per MiB of blob
BUDGET_MEMORY_BEYOND_BLOB = 24 << 20  # peak Python allocation on top of two copies of the blob
BUDGET_TERMS = ([f"qz{letter}xw{letter}k" for letter in "abcdefghijklmnopqrstuvwxyz"]  # 7 characters
                + [f"ďq{letter}zyx" for letter in "abcdefgh"] + ["qxv", "zqk", "xwq", "qzzx", "kqxz", "zxqv"])


def git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)


def git_out(repo: Path, *args: str, stdin: bytes = b"") -> str:
    return subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True,
                          input=stdin).stdout.decode().strip()


class DenylistScanTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.repo = self.tmp / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-q", "-b", "main")
        git(self.repo, "config", "user.email", "test@example.org")
        git(self.repo, "config", "user.name", "test")
        git(self.repo, "config", "commit.gpgsign", "false")
        self.deny = self.tmp / "deny.txt"
        self.deny.write_text("# test terms\n" + "\n".join(TERMS) + "\n", encoding="utf-8")

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp)

    def commit(self, files: dict[str, str | bytes], message: str = "change") -> None:
        for rel, content in files.items():
            path = self.repo / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content if isinstance(content, bytes) else content.encode("utf-8"))
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", message)

    def scan(self, *extra: str) -> tuple[int, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = ds.main(["--denylist", str(self.deny), "--repo", str(self.repo), *extra])
        return code, out.getvalue() + err.getvalue()

    def test_clean_tree_passes(self) -> None:
        self.commit({"a.txt": "nothing private here\n"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 0)

    def test_term_in_content_is_reported_without_revealing_it(self) -> None:
        self.commit({"a.txt": "hello ZyxName!\n"})
        code, out = self.scan("--tree", "HEAD")
        self.assertEqual(code, 1)
        self.assertIn("a.txt:1", out)
        self.assertIn("denylist entry 1", out)
        self.assertNotIn("zyxname", out.lower())

    def test_term_inside_a_longer_word_is_not_a_hit(self) -> None:
        self.commit({"a.txt": "prezyxnamed zyxnameless\n"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 0)

    def test_underscore_does_not_hide_a_term(self) -> None:
        self.commit({"a.rs": "let x_zyxname_y = 1;\n"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 1)

    def test_a_string_escape_does_not_hide_a_term(self) -> None:
        # Fixture strings such as "TRACK\t1\tNAME mic": the `t` of `\t` is
        # an escape, not part of the word.
        self.commit({"a.rs": 'let l = "TRACK\\t1\\tZyxName mic";\n'})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 1)

    def test_letters_with_diacritics_count_as_word_characters(self) -> None:
        self.commit({"a.md": "šzyxname\n"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 0)

    def test_term_ending_in_a_dot_matches_an_address(self) -> None:
        self.commit({"a.txt": "addr 10.9.3.4\n"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 1)

    def test_term_starting_with_a_digit_needs_a_left_boundary(self) -> None:
        self.commit({"a.txt": "addr 110.9.3.4\n"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 0)

    def test_term_in_a_path_is_reported(self) -> None:
        self.commit({"docs/zyxname-notes.md": "x\n"})
        code, out = self.scan("--tree", "HEAD")
        self.assertEqual(code, 1)
        self.assertIn("docs/", out)
        self.assertIn("path", out)

    def test_history_hit_is_found_by_commits_mode_only(self) -> None:
        self.commit({"a.txt": "zyxname\n"})
        (self.repo / "a.txt").write_text("clean\n", encoding="utf-8")
        git(self.repo, "commit", "-q", "-am", "clean up")
        self.assertEqual(self.scan("--tree", "HEAD")[0], 0)
        self.assertEqual(self.scan("--commits", "HEAD")[0], 1)

    def test_commit_message_hit(self) -> None:
        self.commit({"a.txt": "clean\n"}, message="fix for ghost-host.example")
        code, out = self.scan("--commits", "HEAD")
        self.assertEqual(code, 1)
        self.assertIn("commit metadata", out)

    def test_author_identity_is_scanned_against_the_denylist(self) -> None:
        git(self.repo, "config", "user.email", "zyxname@example.org")
        self.commit({"a.txt": "clean\n"})
        code, out = self.scan("--commits", "HEAD")
        self.assertEqual(code, 1)
        self.assertIn("commit metadata", out)
        self.assertNotIn("zyxname", out.lower())

    def test_identity_outside_the_allowed_set_is_rejected_without_printing_it(self) -> None:
        ids = self.tmp / "ids.txt"
        ids.write_text("# allowed\ntest@example.org\n", encoding="utf-8")
        git(self.repo, "config", "user.email", "someone.private@example.net")
        self.commit({"a.txt": "clean\n"})
        code, out = self.scan("--identities", str(ids), "--commits", "HEAD")
        self.assertEqual(code, 1)
        self.assertIn("author email is not an allowed identity", out)
        self.assertIn("committer email is not an allowed identity", out)
        self.assertNotIn("someone.private", out)

    def test_allowed_identities_pass(self) -> None:
        ids = self.tmp / "ids.txt"
        ids.write_text("TEST@example.org\n", encoding="utf-8")
        self.commit({"a.txt": "clean\n"})
        self.assertEqual(self.scan("--identities", str(ids), "--commits", "HEAD")[0], 0)

    def test_allowlisted_line_is_skipped(self) -> None:
        self.commit({"a.txt": "keep zyxname here\n"})
        allow = self.tmp / "allow.txt"
        allow.write_text(ds.line_key("a.txt", "keep zyxname here") + "  a.txt reviewed\n", encoding="utf-8")
        self.assertEqual(self.scan("--allow", str(allow), "--tree", "HEAD", "--commits", "HEAD")[0], 0)

    def test_binary_content_and_its_path_are_both_scanned(self) -> None:
        # #32 E2: this test used to assert that a term inside NUL-containing content is SKIPPED
        # (exit 0) -- the very bypass E2 reports; binary content is now scanned (its text runs).
        self.commit({"bin.dat": b"\0zyxname"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 1)
        self.commit({"bin.dat": b"\0x", "zyxname.bin": b"\0x"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 1)

    def test_empty_denylist_is_a_usage_error(self) -> None:
        self.commit({"a.txt": "x\n"})
        self.deny.write_text("# nothing\n\n", encoding="utf-8")
        self.assertEqual(self.scan("--tree", "HEAD")[0], 2)

    def test_hash_mode_prints_the_line_key(self) -> None:
        self.commit({"a.txt": "one\ntwo\n"})
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = ds.main(["--repo", str(self.repo), "--hash", "a.txt", "2"])
        self.assertEqual(code, 0)
        self.assertEqual(out.getvalue().strip(), ds.line_key("a.txt", "two"))

    # --- #27: the scan must never print a private term into the (public) CI log ---

    def test_a_term_inside_a_path_component_is_redacted_not_printed(self) -> None:
        # Vector 1: a file whose name holds a listed term must not put the term in the log.
        self.commit({"docs/zyxname-notes.md": "x\n"})
        code, out = self.scan("--tree", "HEAD")
        self.assertEqual(code, 1)
        self.assertNotIn("zyxname", out.lower())  # the leak the ticket is about
        self.assertIn("[redacted]", out)          # the term-bearing component is redacted
        self.assertIn("docs/", out)               # the clean component is still shown
        self.assertIn("denylist entry", out)

    def test_a_component_named_exactly_as_a_term_is_redacted(self) -> None:
        self.commit({"zyxname/readme.md": "x\n"})
        code, out = self.scan("--tree", "HEAD")
        self.assertEqual(code, 1)
        self.assertNotIn("zyxname", out.lower())
        self.assertIn("[redacted]", out)

    def test_a_finding_line_never_starts_with_a_raw_path(self) -> None:
        # A path starting with `::` would read as a GitHub workflow command in the CI log;
        # every finding location starts with a fixed word (`tree` / a commit SHA) instead.
        self.commit({"zyxname.txt": "x\n"})
        code, out = self.scan("--tree", "HEAD")
        self.assertEqual(code, 1)
        finding_lines = [ln for ln in out.splitlines() if "denylist entry" in ln]
        self.assertTrue(finding_lines)
        for ln in finding_lines:
            self.assertTrue(ln.startswith("tree "), ln)

    def test_an_added_line_rendered_as_a_plus_plus_header_does_not_leak(self) -> None:
        # Vector 2: an added line whose content starts with `++ ` renders as `+++ ...` under
        # --unified=0 and must be read as content, not a diff file-header, so its text (the
        # private term) never reaches the log.
        self.commit({"note.txt": "clean\n"})
        (self.repo / "note.txt").write_text("++ zyxname secret marker\n", encoding="utf-8")
        git(self.repo, "commit", "-q", "-am", "add a line beginning with ++")
        code, out = self.scan("--commits", "HEAD~1..HEAD")
        self.assertEqual(code, 1)
        self.assertNotIn("zyxname", out.lower())    # the leak the ticket is about
        self.assertNotIn("secret marker", out)      # no line content in the log at all
        self.assertIn("note.txt", out)              # the real (clean) path is reported
        self.assertIn("denylist entry", out)

    def test_a_term_only_in_a_diff_header_path_is_redacted(self) -> None:
        # Commit mode reports the added-file path; a term in it must be redacted, not printed.
        self.commit({"a.txt": "clean\n"})
        self.commit({"zyxname-new.txt": "harmless\n"}, message="add a file named after a term")
        code, out = self.scan("--commits", "HEAD~1..HEAD")
        self.assertEqual(code, 1)
        self.assertNotIn("zyxname", out.lower())
        self.assertIn("[redacted]", out)

    # --- #27 round 2: commit-mode C-quoting, NFC, and shown() coverage ---

    def test_a_c_quoted_commit_path_with_a_term_does_not_leak(self) -> None:
        # git C-quotes a `+++` header path holding a non-ASCII byte (default core.quotePath):
        # `note<U+00A0>zyxname.txt` -> `+++ "b/note\302\240zyxname.txt"`. A content hit prints
        # the location, so the quoted path must be decoded and redacted, never printed raw.
        self.commit({"base.txt": "base\n"})
        name = "note zyxname.txt"  # no-break space before the term forces C-quoting
        (self.repo / name).write_text("a line that also holds zyxname\n", encoding="utf-8")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "non-ascii path plus a content hit")
        code, out = self.scan("--commits", "HEAD~1..HEAD")
        self.assertEqual(code, 1)
        self.assertNotIn("zyxname", out.lower())  # the leak this round fixes
        self.assertNotIn("\\302", out)            # no octal-escaped bytes of the term either
        self.assertIn("[redacted]", out)

    def test_a_space_in_a_commit_path_aligns_with_the_tree_path(self) -> None:
        # git appends a TAB to a `+++` label that has a space; the commit-mode path must equal
        # the tree-mode path so one allowlist key (made with --hash) works in both modes.
        self.commit({"my note.txt": "keep zyxname here\n"})
        allow = self.tmp / "allow.txt"
        allow.write_text(ds.line_key("my note.txt", "keep zyxname here") + "  reviewed\n", encoding="utf-8")
        self.assertEqual(self.scan("--allow", str(allow), "--tree", "HEAD", "--commits", "HEAD")[0], 0)

    def test_a_decomposed_diacritic_path_is_matched_and_redacted(self) -> None:
        # A term with a diacritic and a path holding it in NFD (decomposed) form: NFC
        # normalization must still match and redact it, so it cannot hide in the log.
        term = "ďqzywx"  # an invented word with a diacritic, precomposed (NFC)
        scanner = ds.Scanner([term], set())
        nfd_path = unicodedata.normalize("NFD", f"docs/{term}-notes.md")
        self.assertNotEqual(nfd_path, f"docs/{term}-notes.md")  # genuinely decomposed
        self.assertEqual(scanner.shown(nfd_path), "docs/[redacted]")

    def test_shown_redacts_a_component_that_holds_a_term(self) -> None:
        scanner = ds.Scanner(["zyxname"], set())
        self.assertEqual(scanner.shown("docs/zyxname-notes.md"), "docs/[redacted]")

    def test_shown_redacts_the_whole_path_for_a_term_spanning_components(self) -> None:
        scanner = ds.Scanner(["rack/mixer"], set())
        self.assertEqual(scanner.shown("rack/mixer/config.txt"), REDACTED_MARKER)

    def test_shown_escapes_control_chars_in_a_kept_component(self) -> None:
        # a raw control char in a kept component could inject a log line / a CI ::command
        scanner = ds.Scanner(["zyxname"], set())
        self.assertEqual(scanner.shown("a\x01b/zyxname.txt"), "a\\x01b/[redacted]")

    def test_shown_redacts_a_component_whose_redacted_form_still_matches(self) -> None:
        # the post-redaction re-check: a term equal to the literal marker text
        scanner = ds.Scanner(["redacted"], set())
        self.assertEqual(scanner.shown("x/redacted/y"), REDACTED_MARKER)

    def test_printable_escapes_control_and_non_ascii_characters(self) -> None:
        self.assertEqual(ds.printable("a\x01b"), "a\\x01b")
        self.assertEqual(ds.printable("x y"), "x\\u2028y")
        self.assertEqual(ds.printable("plain-ok"), "plain-ok")

    def test_commit_path_under_local_quotepath_false_does_not_crash(self) -> None:
        # A developer's local core.quotePath=false leaves a non-ASCII byte raw inside a quoted
        # label (git still quotes for the control char); the scan must force quotePath=true so
        # the label is pure-ASCII octal and decode it, never crash on encode("ascii").
        git(self.repo, "config", "core.quotePath", "false")
        self.commit({"base.txt": "base\n"})
        name = "n\x07ote zyxname.txt"  # a control char forces quoting; a non-ASCII byte too
        (self.repo / name).write_text("content that also holds zyxname\n", encoding="utf-8")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "control + non-ascii path under quotePath=false")
        code, out = self.scan("--commits", "HEAD~1..HEAD")
        self.assertEqual(code, 1)
        self.assertNotIn("zyxname", out.lower())
        self.assertIn("[redacted]", out)

    def test_commit_mode_redacts_tricky_path_shapes(self) -> None:
        # git C-quotes a +++ label with a control char, backslash or quote, and TAB-suffixes a
        # label with a space: every shape must decode and redact, with a content hit present.
        names = ["y\x07zyxname.txt", "y\\zyxname.txt", 'y"zyxname.txt', "my zyxname.txt"]
        self.commit({"base.txt": "base\n"})
        for name in names:
            (self.repo / name).write_text("body has zyxname in it\n", encoding="utf-8")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "tricky path shapes, each with a content hit")
        code, out = self.scan("--commits", "HEAD~1..HEAD")
        self.assertEqual(code, 1)
        self.assertNotIn("zyxname", out.lower())
        self.assertNotIn("\\302", out)

    # --- #29: harden commit-mode DETECTION (empty/binary paths, non-LF splits, unquote_c) ---

    def test_empty_added_file_path_with_a_term_is_caught_in_commit_mode(self) -> None:
        # Vector 1: an added EMPTY file has no `+++` diff header, so commit mode never scanned its
        # path; a term in the path must still be caught (via git diff-tree) and redacted, never
        # printed.
        self.commit({"base.txt": "base\n"})
        (self.repo / "zyxname-empty.txt").write_bytes(b"")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "add an empty file named after a term")
        code, out = self.scan("--commits", "HEAD~1..HEAD")
        self.assertEqual(code, 1)
        self.assertNotIn("zyxname", out.lower())
        self.assertIn("[redacted]", out)
        self.assertIn("denylist entry", out)

    def test_binary_added_file_path_with_a_term_is_caught_in_commit_mode(self) -> None:
        # Vector 1: an added BINARY file shows `Binary files … differ`, no `+++` header; its
        # term-bearing path must still be caught and redacted in commit mode.
        self.commit({"base.txt": "base\n"})
        (self.repo / "zyxname.bin").write_bytes(b"\x00\x01\x02content")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "add a binary file named after a term")
        code, out = self.scan("--commits", "HEAD~1..HEAD")
        self.assertEqual(code, 1)
        self.assertNotIn("zyxname", out.lower())
        self.assertIn("[redacted]", out)

    def test_a_text_added_path_is_reported_exactly_once_in_commit_mode(self) -> None:
        # The added diff-tree path source must not double-report a text file already seen via its
        # `+++` header (dedup guard).
        self.commit({"base.txt": "base\n"})
        self.commit({"zyxname-new.txt": "harmless\n"}, message="add a text file named after a term")
        code, out = self.scan("--commits", "HEAD~1..HEAD")
        self.assertEqual(code, 1)
        path_findings = [ln for ln in out.splitlines() if ": path: denylist entry" in ln]
        self.assertEqual(len(path_findings), 1, path_findings)

    def test_non_lf_separators_do_not_hide_a_term_in_commit_mode(self) -> None:
        # Vector 2: an added line with CR / VT / FF / NEL / U+2028 before a term. str.splitlines()
        # breaks the diff line at that character, and the tail (holding the term) loses its `+`
        # prefix and is skipped. Split on `\n` only → the whole added line is scanned, so the term
        # is caught; and it is never printed (redaction contract).
        self.commit({"note.txt": "base\n"})
        for i, sep in enumerate(("\r", "\x0b", "\x0c", "\x85", " ")):
            with self.subTest(sep=hex(ord(sep))):
                (self.repo / "note.txt").write_text(f"safe{sep}ZyxName line {i}\n", encoding="utf-8")
                git(self.repo, "commit", "-q", "-am", f"line with separator {i}")
                code, out = self.scan("--commits", "HEAD~1..HEAD")
                self.assertEqual(code, 1, f"term after {hex(ord(sep))} not caught")
                self.assertNotIn("zyxname", out.lower())

    def test_non_lf_separator_lines_share_one_allow_key_across_modes(self) -> None:
        # Vector 2 (allow-key parity): a line containing VT must split identically (`\n` only) in
        # tree, commit and --hash mode, so a single allow key covers all three. str.splitlines()
        # would break it and the key would not match.
        line = "keep\x0bzyxname here"
        self.commit({"a.txt": line + "\n"})
        allow = self.tmp / "allow.txt"
        allow.write_text(ds.line_key("a.txt", line) + "  reviewed\n", encoding="utf-8")
        self.assertEqual(self.scan("--allow", str(allow), "--tree", "HEAD", "--commits", "HEAD")[0], 0)

    def test_unquote_c_is_robust_to_a_malformed_escape(self) -> None:
        # Vector 3: a crafted / malformed C-quoted `+++` label must never crash the scan. A
        # malformed backslash is kept literal (the well-formed octal / letter escapes are
        # unchanged). Current code raises KeyError / ValueError.
        self.assertEqual(ds.unquote_c(b'"trailing\\"'), b"trailing\\")   # lone trailing backslash
        self.assertEqual(ds.unquote_c(b'"a\\zb"'), b"a\\zb")             # unknown escape letter
        self.assertEqual(ds.unquote_c(b'"o\\9"'), b"o\\9")               # \9 is not octal
        self.assertEqual(ds.unquote_c(b'"big\\777"'), b"big\\777")       # octal > 255, kept literal
        # well-formed escapes still decode exactly (no regression)
        self.assertEqual(ds.unquote_c(b'"b/\\303\\241"'), b"b/\xc3\xa1")
        self.assertEqual(ds.unquote_c(b'"a\\tb\\\\c\\"d"'), b"a\tb\\c\"d")

    def test_unquote_c_keeps_a_term_visible_after_a_malformed_escape(self) -> None:
        # Vector 3: bytes after a malformed escape must survive so a term cannot hide behind it.
        out = ds.unquote_c(b'"b/x\\qzyxname.txt"')  # \q is not a valid C-escape
        self.assertIn(b"zyxname", out)

    def test_a_malformed_quoted_label_still_exposes_and_redacts_a_term(self) -> None:
        # Vector 3 end-to-end: a term behind a malformed escape in a `+++` label is still caught
        # by the scanner and still redacted (never printed).
        scanner = ds.Scanner(["zyxname"], set())
        path = ds.diff_path('"b/dir\\qname/zyxname.txt"')  # \q malformed; term in its own component
        self.assertTrue(scanner.entries_in(path))
        self.assertIn("[redacted]", scanner.shown(path))
        self.assertNotIn("zyxname", scanner.shown(path))

    def test_hash_key_matches_the_scanner_for_a_line_containing_a_cr(self) -> None:
        # Vector 2 regression: --hash must yield the SAME allow key the scanner computes for a line
        # with a CR (and every line of a CRLF file). Path.read_text() translates \r / \r\n to \n,
        # so the key diverges from tree/commit mode and the allowlist workflow silently fails.
        self.commit({"a.txt": "keep\rzyxname here\n"})  # a CR inside the added line
        out = io.StringIO()
        with contextlib.redirect_stdout(out):  # the developer's --hash step
            ds.main(["--repo", str(self.repo), "--hash", "a.txt", "1"])
        allow = self.tmp / "allow.txt"
        allow.write_text(out.getvalue().strip() + "  reviewed ordinary prose\n", encoding="utf-8")
        self.assertEqual(self.scan("--allow", str(allow), "--tree", "HEAD", "--commits", "HEAD")[0], 0)

    def test_empty_term_named_file_in_a_root_commit_is_caught(self) -> None:
        # Vector 1 (root commit): the diff-tree scan passes --root, so a term-named empty file added
        # in the very first commit (which has no parent and no `+++` header) is still caught.
        self.commit({"zyxname-empty.txt": b""})  # the root commit itself
        code, out = self.scan("--commits", "HEAD")
        self.assertEqual(code, 1)
        self.assertNotIn("zyxname", out.lower())
        self.assertIn("[redacted]", out)

    def test_empty_term_file_merged_from_a_side_branch_is_caught(self) -> None:
        # Vector 1 (merge): -m makes diff-tree diff a merge against its parents, so a term-named
        # empty file brought in by a merge is still caught in commit mode.
        self.commit({"base.txt": "base\n"})
        git(self.repo, "checkout", "-q", "-b", "feature")
        (self.repo / "zyxname-merge.txt").write_bytes(b"")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "side branch adds an empty term-named file")
        git(self.repo, "checkout", "-q", "main")
        git(self.repo, "merge", "-q", "--no-ff", "-m", "merge feature", "feature")
        code, out = self.scan("--commits", "-1 HEAD")  # just the merge commit
        self.assertEqual(code, 1)
        self.assertNotIn("zyxname", out.lower())
        self.assertIn("[redacted]", out)

    # --- #32 E2: content holding a NUL byte (UTF-16 text, binary files) is scanned too ---

    def assert_found_in_both_modes(self, name: str, content: bytes) -> str:
        self.commit({"base.txt": f"base for {name}\n"})
        self.commit({name: content}, message="add content holding a NUL byte")
        code, tree_out = self.scan("--tree", "HEAD")
        self.assertEqual(code, 1, "tree mode missed the term")
        code, commit_out = self.scan("--commits", "HEAD~1..HEAD")
        self.assertEqual(code, 1, "commit mode missed the term")
        for out in (tree_out, commit_out):
            self.assertNotIn("zyxname", out.lower())
        return tree_out

    def test_utf16_text_with_a_bom_is_decoded_and_scanned(self) -> None:
        out = self.assert_found_in_both_modes("u.txt", "first line\nhello ZyxName\n".encode("utf-16"))
        self.assertIn("tree u.txt:2: denylist entry 1", out)

    def test_utf16_text_without_a_bom_is_detected_by_its_nul_pattern(self) -> None:
        for codec in ("utf-16-le", "utf-16-be"):
            with self.subTest(codec=codec):
                self.assert_found_in_both_modes(f"{codec}.txt", "a zyxname line\n".encode(codec))

    def test_a_term_in_a_text_run_of_binary_content_is_found(self) -> None:
        content = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x01author zyxname\x00\x02\xff"
        out = self.assert_found_in_both_modes("img.png", content)
        self.assertIn("tree img.png:run ", out)

    def test_a_utf16_string_inside_binary_content_is_found(self) -> None:
        content = b"\x00\x01\x02\x03" + "C:\\Users\\zyxname\\trace".encode("utf-16-le") + b"\x00\x00\xfe"
        self.assert_found_in_both_modes("trace.etl", content)

    def test_a_short_term_needs_a_long_text_run_in_binary_content(self) -> None:
        # random bytes (the f64 goldens) form short words by chance, so in binary content a term
        # under MIN_BINARY_TERM characters counts only inside a long valid-UTF-8 text run
        self.deny.write_text("qxv\n", encoding="utf-8")
        self.commit({"noise.f64": b"\x00\x07\x91qxv\x00\x93"})
        self.assertEqual(self.scan("--tree", "HEAD", "--commits", "HEAD")[0], 0)
        self.commit({"text.bin": b"\x00\x01" + b"a long line of ordinary text that names qxv here" + b"\x00"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 1)
        self.assertEqual(self.scan("--commits", "HEAD~1..HEAD")[0], 1)

    def test_commit_mode_reports_only_the_binary_runs_a_commit_added(self) -> None:
        self.commit({"blob.bin": b"\x00zyxname\x00one"})
        self.commit({"blob.bin": b"\x00zyxname\x00two"}, message="change another run")
        self.assertEqual(self.scan("--commits", "HEAD~1..HEAD")[0], 0)
        self.assertEqual(self.scan("--commits", "HEAD")[0], 1)

    def test_hash_key_allowlists_a_utf16_line_and_a_binary_run(self) -> None:
        self.commit({"u.txt": "keep zyxname here\n".encode("utf-16"), "b.bin": b"\x00\x01keep zyxname\x00"})
        keys = []
        for path, number in (("u.txt", "1"), ("b.bin", "1")):
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(ds.main(["--repo", str(self.repo), "--hash", path, number]), 0)
            keys.append(out.getvalue().strip() + "  reviewed ordinary prose")
        self.assertEqual(self.scan("--tree", "HEAD")[0], 1)
        allow = self.tmp / "allow.txt"
        allow.write_text("\n".join(keys) + "\n", encoding="utf-8")
        self.assertEqual(self.scan("--allow", str(allow), "--tree", "HEAD", "--commits", "HEAD")[0], 0)

    # --- #32 E1: .gitattributes (binary, -diff, a textconv driver) never hide content ---

    def assert_history_term_found_despite(self, attributes: str, name: str) -> None:
        # the term is added and removed again, so only commit mode can find it (tree mode sees HEAD)
        self.commit({".gitattributes": attributes, name: "a,zyxname\n"}, message="add under an attribute")
        self.commit({name: "a,clean\n"}, message="remove the term again")
        self.assertEqual(self.scan("--tree", "HEAD")[0], 0)
        code, out = self.scan("--tree", "HEAD", "--commits", "HEAD")
        self.assertEqual(code, 1, f"commit mode honoured {attributes.strip()!r}")
        self.assertNotIn("zyxname", out.lower())

    def test_a_minus_diff_attribute_does_not_hide_content_in_commit_mode(self) -> None:
        self.assert_history_term_found_despite("*.csv -diff\n", "data.csv")

    def test_a_binary_attribute_does_not_hide_content_in_commit_mode(self) -> None:
        self.assert_history_term_found_despite("*.dat binary\n", "table.dat")

    def test_a_textconv_driver_does_not_hide_content_in_commit_mode(self) -> None:
        git(self.repo, "config", "diff.hide.textconv", "true")  # converts every version to nothing
        self.assert_history_term_found_despite("*.txt diff=hide\n", "note.txt")

    def test_tree_mode_reads_the_bytes_of_an_attribute_marked_file(self) -> None:
        self.commit({".gitattributes": "*.csv -diff\n*.dat binary\n", "a.csv": "zyxname\n", "b.dat": "zyxname\n"})
        code, out = self.scan("--tree", "HEAD")
        self.assertEqual(code, 1)
        self.assertIn("tree a.csv:1: denylist entry 1", out)
        self.assertIn("tree b.dat:1: denylist entry 1", out)

    # --- #32 E3: non-UTF-8 text, JSON \u and percent escapes, and undecodable paths ---

    def add_terms(self, *terms: str) -> None:
        self.deny.write_text("\n".join([*TERMS, *terms]) + "\n", encoding="utf-8")

    def assert_found_in_both_modes_as(self, files: dict[str, str | bytes], term_letters: str) -> str:
        self.bases = getattr(self, "bases", 0) + 1  # a fresh base commit per subTest
        self.commit({"base.txt": f"base {self.bases}\n"})
        self.commit(files, message="add content in another encoding")
        code, tree_out = self.scan("--tree", "HEAD")
        self.assertEqual(code, 1, "tree mode missed the term")
        code, commit_out = self.scan("--commits", "HEAD~1..HEAD")
        self.assertEqual(code, 1, "commit mode missed the term")
        for out in (tree_out, commit_out):
            self.assertNotIn(term_letters, out.lower())
        return tree_out

    def test_a_cp1250_term_is_found_in_both_modes(self) -> None:
        self.add_terms("ďqxwzy")
        out = self.assert_found_in_both_modes_as({"c.txt": "meno: Ďqxwzy\n".encode("cp1250")}, "qxwzy")
        self.assertIn("tree c.txt:1: denylist entry 4", out)

    def test_a_latin1_term_is_found_in_both_modes(self) -> None:
        self.add_terms("qñzyxw")
        self.assert_found_in_both_modes_as({"l.txt": "x qñzyxw y\n".encode("latin-1")}, "zyxw")

    def test_a_json_unicode_escape_does_not_hide_a_term(self) -> None:
        self.add_terms("ďqxwzy")
        for name, text in (("j.json", '{"n": "\\u010fqxwzy"}\n'), ("u.json", '{"n": "\\u010Fqxwzy"}\n'),
                           ("r.rs", 'let n = "\\u{10f}qxwzy";\n')):
            with self.subTest(name=name):
                self.assert_found_in_both_modes_as({name: text}, "qxwzy")

    def test_percent_encoding_does_not_hide_a_term(self) -> None:
        self.add_terms("ďqxwzy")
        for name, text in (("p.txt", "see /x/%C4%8Fqxwzy\n"), ("q.txt", "see /x/%EFqxwzy\n")):  # UTF-8, cp1250
            with self.subTest(name=name):
                self.assert_found_in_both_modes_as({name: text}, "qxwzy")

    def test_a_non_utf8_commit_message_is_scanned_in_its_encoding(self) -> None:
        self.add_terms("ďqxwzy")
        self.commit({"a.txt": "clean\n"})
        message = self.tmp / "message.txt"
        message.write_bytes("fix for Ďqxwzy".encode("cp1250"))  # raw cp1250, no encoding header
        (self.repo / "a.txt").write_text("clean 2\n", encoding="utf-8")
        git(self.repo, "commit", "-q", "-a", "-F", str(message))
        code, out = self.scan("--commits", "HEAD~1..HEAD")
        self.assertEqual(code, 1)
        self.assertIn("commit metadata: denylist entry 4", out)

    def test_valid_utf8_is_not_rescanned_as_mojibake(self) -> None:
        # the cp1250 / Latin-1 readings apply only to bytes that are not valid UTF-8: read as
        # Latin-1, `č` (C4 8D) would end in a control character and split `čqxv` into a word `qxv`
        self.deny.write_text("qxv\n", encoding="utf-8")
        self.commit({"sk.txt": "čqxv\n"})
        self.assertEqual(self.scan("--tree", "HEAD", "--commits", "HEAD")[0], 0)

    def test_a_cp1250_path_holding_a_term_never_reaches_the_output(self) -> None:
        # #27 leak class: the undecodable letter used to print as U+FFFD with the rest of the term
        # after it (`docs/\ufffdqxwzy-notes.md`) in the public CI log
        self.add_terms("ďqxwzy")
        path = os.fsdecode(b"docs/\xefqxwzy-notes.md")  # `ďqxwzy` in cp1250
        out = self.assert_found_in_both_modes_as({path: "x zyxname\n"}, "qxwzy")
        self.assertIn("tree docs/[redacted]:1: denylist entry 1", out)
        self.assertIn("tree docs/[redacted]: path: denylist entry 4", out)

    def test_an_undecodable_path_component_is_redacted_even_without_a_term(self) -> None:
        path = os.fsdecode(b"\xe1bcde/a.txt")  # Latin-1 `ábcde`: not valid UTF-8, holds no term
        out = self.assert_found_in_both_modes_as({path: "x zyxname\n"}, "bcde")
        self.assertIn("tree [redacted]/a.txt:1: denylist entry 1", out)
        self.assertNotIn("\ufffd", out)

    def test_a_percent_encoded_path_holding_a_term_is_matched_and_redacted(self) -> None:
        scanner = ds.Scanner(["ďqxwzy"], set())
        self.assertEqual(scanner.entries_in("docs/%C4%8Fqxwzy.md"), [1])
        self.assertEqual(scanner.shown("docs/%C4%8Fqxwzy.md"), "docs/[redacted]")

    def test_hash_key_allowlists_a_cp1250_line(self) -> None:
        self.add_terms("ďqxwzy")
        self.commit({"c.txt": "keep ďqxwzy here\n".encode("cp1250")})
        # the allowlist must be what clears it (F5 m12: without this the test passed even where
        # the scan never found the cp1250 term at all)
        self.assertEqual(self.scan("--tree", "HEAD")[0], 1)
        self.assertEqual(self.scan("--commits", "HEAD")[0], 1)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(ds.main(["--repo", str(self.repo), "--hash", "c.txt", "1"]), 0)
        allow = self.tmp / "allow.txt"
        allow.write_text(out.getvalue().strip() + "  reviewed ordinary prose\n", encoding="utf-8")
        self.assertEqual(self.scan("--allow", str(allow), "--tree", "HEAD", "--commits", "HEAD")[0], 0)

    # --- #32 review m3, m4: content read as UTF-16 keeps its byte runs; UTF-32 is decoded ---

    def test_a_binary_that_looks_like_utf16_keeps_its_byte_runs(self) -> None:
        # m3: quiet 16-bit PCM has a NUL high byte in nearly every sample, so the blob read as
        # UTF-16LE text and its ASCII LIST/INFO chunk (artist, title) was never scanned; any blob
        # starting FF FE read as UTF-16 the same way
        samples = b"".join(bytes([1 + index % 3, 0]) for index in range(4000))
        info = b"LIST\x1a\x00\x00\x00INFOIART\x0e\x00\x00\x00zyxname band\x00\x00"
        wav = b"RIFF\x00\x00\x00\x00WAVEfmt \x10\x00\x00\x00" + bytes(16) + b"data\x40\x1f\x00\x00" + samples + info
        for name, content in (("quiet.wav", wav), ("marked.bin", b"\xff\xfe\x00\x01\x02 zyxname here\x03\x04")):
            with self.subTest(name=name):
                out = self.assert_found_in_both_modes_as({name: content}, "zyxname")
                self.assertIn(f"tree {name}:run ", out)

    def test_utf32_text_is_decoded_and_scanned(self) -> None:
        # m4: UTF-32 (with or without a byte-order mark) read as UTF-16 or as binary runs of one
        # character, so no term was ever found in it
        text = "first line\na zyxname line\n"
        for name, content in (("bom.txt", text.encode("utf-32")),
                              ("bom-be.txt", b"\x00\x00\xfe\xff" + text.encode("utf-32-be")),
                              ("le.txt", text.encode("utf-32-le")), ("be.txt", text.encode("utf-32-be"))):
            with self.subTest(name=name):
                out = self.assert_found_in_both_modes_as({name: content}, "zyxname")
                self.assertIn(f"tree {name}:2: denylist entry 1", out)

    # --- #32 review m5: more single-byte readings, double-encoded UTF-8, non-ASCII path components ---

    def test_iso_8859_2_cp852_and_double_encoded_terms_are_found(self) -> None:
        # `š` is B9 in ISO-8859-2 and E7 in cp852 -- neither cp1250 (9A) nor Latin-1 reads it; UTF-8
        # decoded as cp1250 or Latin-1 and encoded again (mojibake) spells it `Ĺˇ` / `Å¡`
        self.add_terms("šqxwzy")
        texts = {"l2.txt": "meno: šqxwzy\n".encode("iso-8859-2"), "dos.txt": "meno: šqxwzy\n".encode("cp852"),
                 "moji1250.txt": "meno: " + "šqxwzy".encode().decode("cp1250") + "\n",
                 "moji1.txt": "meno: " + "šqxwzy".encode().decode("latin-1") + "\n"}
        for name, content in texts.items():
            with self.subTest(name=name):
                out = self.assert_found_in_both_modes_as({name: content}, "qxwzy")
                self.assertIn(f"tree {name}:1: denylist entry 4", out)

    def test_a_non_ascii_path_component_is_never_printed(self) -> None:
        # a reading the scanner lacks (double-encoded UTF-8 here) left a valid UTF-8 component
        # printed with the ASCII tail of a term in it; now any non-ASCII component is redacted
        self.add_terms("šqxwzy")
        moji = "šqxwzy".encode().decode("cp1250")
        out = self.assert_found_in_both_modes_as({f"docs/{moji}-notes.md": "x zyxname\n"}, "qxwzy")
        self.assertIn("tree docs/[redacted]:1: denylist entry 1", out)
        self.assertIn("tree docs/[redacted]: path: denylist entry 4", out)
        out = self.assert_found_in_both_modes_as({"docs/résumé/a.txt": "x zyxname\n"}, "sum")
        self.assertIn("tree docs/[redacted]/a.txt:1: denylist entry 1", out)

    # --- #32 review m6: character references, \U escapes, invisible characters, compatibility forms ---

    def test_references_escapes_invisibles_and_fullwidth_do_not_hide_a_term(self) -> None:
        self.add_terms("ďqxwzy")
        texts = {"ref.html": ("<b>&#271;qxwzy</b>", "qxwzy"), "hexref.xml": ("<n>&#x10F;qxwzy</n>", "qxwzy"),
                 "ascii.xml": ("<n>&#x7a;&#121;xname</n>", "xname"), "py.py": ('n = "\\U0000010fqxwzy"', "qxwzy"),
                 "shy.md": ("zyx\u00adname", "name"), "zwsp.md": ("zyx\u200bname", "name"),
                 "zwj.md": ("zyx\u200dname", "name"), "bom.md": ("zyx\ufeffname", "name"),
                 "wj.md": ("zyx\u2060name", "name"), "wide.md": ("\uff5a\uff59\uff58\uff4e\uff41\uff4d\uff45", "zyx")}
        for name, (text, letters) in texts.items():
            with self.subTest(name=name):
                self.assert_found_in_both_modes_as({name: f"x {text} y\n"}, letters)

    def test_a_zero_width_space_between_words_still_separates_them(self) -> None:
        # a zero-width space is also a word break: the term after one is still found
        self.commit({"a.md": "abc\u200bzyxname\n"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 1)

    # --- #32 review m9: git-lfs content is not in the repository, so its use is a finding ---

    def test_a_git_lfs_filter_and_pointer_are_findings(self) -> None:
        # with filter=lfs the repository holds only a pointer; the real content lives on the LFS
        # server, where no scan ever reads it
        pointer = "version https://git-lfs.github.com/spec/v1\noid sha256:" + "0" * 64 + "\nsize 12\n"
        self.commit({"base.txt": "base\n"})
        self.commit({"assets/.gitattributes": "# art\n*.psd filter=lfs diff=lfs merge=lfs -text\n",
                     "assets/art.psd": pointer})
        code, out = self.scan("--tree", "HEAD")
        self.assertEqual(code, 1)
        self.assertIn("tree assets/.gitattributes:2: git-lfs filter", out)
        self.assertIn("tree assets/art.psd: git-lfs pointer", out)
        code, out = self.scan("--commits", "HEAD~1..HEAD")
        self.assertEqual(code, 1)
        self.assertIn(" assets/.gitattributes: git-lfs filter", out)
        self.assertIn(" assets/art.psd: git-lfs pointer", out)

    # --- #32 review m10: a finding only history holds can be located and allowlisted ---

    def test_commit_mode_locates_a_binary_run_and_a_wide_line(self) -> None:
        self.commit({"base.txt": "base\n"})
        self.commit({"b.bin": b"\x00\x01keep zyxname\x00", "u.txt": "first\nkeep zyxname\n".encode("utf-16")})
        code, out = self.scan("--commits", "HEAD~1..HEAD")
        self.assertEqual(code, 1)
        self.assertIn(" b.bin:run 1: denylist entry 1", out)
        self.assertIn(" u.txt:2: denylist entry 1", out)

    def test_hash_reads_a_blob_of_history_by_rev_and_path(self) -> None:
        # the file is gone from the working tree, so a history-only finding needs `<rev>:<path>`
        self.commit({"b.bin": b"\x00\x01keep zyxname\x00", "a.txt": "keep zyxname\n"})
        git(self.repo, "rm", "-q", "b.bin", "a.txt")
        git(self.repo, "commit", "-q", "-m", "remove them")
        self.assertEqual(self.scan("--tree", "HEAD")[0], 0)
        keys = []
        for target, number in (("HEAD~1:b.bin", "run 1"), ("HEAD~1:a.txt", "1")):
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(ds.main(["--repo", str(self.repo), "--hash", target, number]), 0)
            keys.append(out.getvalue().strip() + "  reviewed ordinary prose")
        self.assertEqual(self.scan("--commits", "HEAD")[0], 1)
        allow = self.tmp / "allow.txt"
        allow.write_text("\n".join(keys) + "\n", encoding="utf-8")
        self.assertEqual(self.scan("--allow", str(allow), "--commits", "HEAD")[0], 0)

    # --- #32 review m7: binary content is scanned in bounded CPU and memory ---

    def test_a_large_binary_blob_stays_within_the_cpu_and_memory_budget(self) -> None:
        # a 20 MB blob took ~65 s and ~1 GB: per-run objects cost ~50 B of RAM per input byte, so a
        # blob of ~70 MB would time out the 10-minute CI secrets job
        self.deny.write_text("\n".join(BUDGET_TERMS) + "\n", encoding="utf-8")
        self.assertEqual(len(BUDGET_TERMS), 40)
        self.commit({"noise.bin": random.Random(32).randbytes(BUDGET_BLOB)})
        started = time.process_time()
        code, out = self.scan("--tree", "HEAD")
        cpu = time.process_time() - started
        self.assertEqual(code, 0, out)
        self.assertLess(cpu, BUDGET_CPU_PER_MIB * BUDGET_BLOB / (1 << 20), f"{cpu:.1f} s CPU")
        tracemalloc.start()
        try:
            self.assertEqual(self.scan("--tree", "HEAD")[0], 0)
            peak = tracemalloc.get_traced_memory()[1]
        finally:
            tracemalloc.stop()
        self.assertLess(peak, 2 * BUDGET_BLOB + BUDGET_MEMORY_BEYOND_BLOB, f"{peak / (1 << 20):.0f} MiB peak")

    # --- #32 review M1: a C-style escape right before a term, or spelling it, does not hide it ---

    def test_a_c_escape_does_not_hide_a_term(self) -> None:
        # byte-string fixtures such as b"\0Program 1\0..." put an escape right before a word; the
        # boundary accepted only \n \t \r, and escapes were never decoded
        texts = ('let b = b"\\0zyxname\\0";', 'let s = "\\x00zyxname";', 'char s[] = "\\101zyxname";',
                 'x = "\\azyxname"', 'let s = "\\x7a\\x79\\x78name";', 'char s[] = "\\172\\171\\170name";')
        for number, text in enumerate(texts):
            with self.subTest(text=text):
                self.assert_found_in_both_modes_as({f"escape{number}.rs": text + "\n"}, "zyxname")

    def test_a_submodule_entry_is_scanned_by_path_not_read_as_a_blob(self) -> None:
        # commit mode reads changed blobs whole; a gitlink's object is a commit, which cat-file
        # blob would fail on -- its path is still scanned, its object is not read
        self.commit({"base.txt": "base\n"})
        head = subprocess.run(["git", "-C", str(self.repo), "rev-parse", "HEAD"], check=True,
                              capture_output=True, text=True).stdout.strip()
        git(self.repo, "update-index", "--add", "--cacheinfo", f"160000,{head},vendor/zyxname-lib")
        git(self.repo, "commit", "-q", "-m", "add a submodule entry")
        code, out = self.scan("--tree", "HEAD", "--commits", "HEAD~1..HEAD")
        self.assertEqual(code, 1)
        self.assertIn("vendor/[redacted]: path: denylist entry 1", out)
        self.assertNotIn("zyxname", out.lower())

    # --- #32 review: a developer's local diff/log config cannot change the diff commit mode parses ---

    def test_local_prefix_config_cannot_misparse_a_diff_path(self) -> None:
        # diff.noprefix drops the `b/` of `+++ b/<path>`, so a path under a directory `b/` lost its
        # first component: its allow key and printed location diverged from tree mode.
        # mnemonicPrefix and srcPrefix/dstPrefix leave `git show` alone on git 2.43 (the latter two
        # exist from 2.45) -- guards for newer git
        line = "keep zyxname here"
        allow = self.tmp / "allow.txt"
        allow.write_text(ds.line_key("b/note.txt", line) + "  reviewed ordinary prose\n", encoding="utf-8")
        for number, settings in enumerate(([("diff.noprefix", "true")], [("diff.mnemonicPrefix", "true")],
                                            [("diff.srcPrefix", "S/"), ("diff.dstPrefix", "D/")])):
            with self.subTest(settings=settings):
                for key, value in settings:
                    git(self.repo, "config", key, value)
                try:
                    self.commit({"b/note.txt": f"{line}\n", "b/other.txt": f"zyxname {number}\n"})
                    code, out = self.scan("--allow", str(allow), "--tree", "HEAD", "--commits", "-1 HEAD")
                    self.assertEqual(code, 1)
                    self.assertNotIn("note.txt", out)                     # the allowlisted line stays allowed
                    self.assertIn(" b/other.txt: denylist entry 1", out)  # the full path, not `other.txt`
                finally:
                    for key, _value in settings:
                        git(self.repo, "config", "--unset", key)

    def test_local_diff_relative_config_cannot_hide_changes_outside_the_directory(self) -> None:
        # with diff.relative, `git show` run from a subdirectory (--repo inside the repository)
        # shows only that subdirectory's changes, so a term added elsewhere was missed
        git(self.repo, "config", "diff.relative", "true")
        self.commit({"sub/clean.txt": "clean\n", "top.txt": "zyxname\n"})
        code, out = self.scan("--repo", str(self.repo / "sub"), "--commits", "HEAD")
        self.assertEqual(code, 1)
        self.assertIn(" top.txt: denylist entry 1", out)

    def test_local_diff_merges_config_cannot_change_how_a_merge_is_diffed(self) -> None:
        # log.diffMerges=combined turned the merge's first-parent diff into a combined diff that
        # holds nothing for a file only one parent changed, so the lines the merge brings in vanished
        self.commit({"base.txt": "base\n"})
        git(self.repo, "checkout", "-q", "-b", "side")
        self.commit({"side.txt": "brought in zyxname\n"})
        git(self.repo, "checkout", "-q", "main")
        self.commit({"main.txt": "main\n"})
        git(self.repo, "merge", "-q", "--no-ff", "-m", "merge side", "side")
        git(self.repo, "config", "log.diffMerges", "combined")
        code, out = self.scan("--commits", "-1 HEAD")  # just the merge commit
        self.assertEqual(code, 1)
        self.assertIn(" side.txt: denylist entry 1", out)

    def test_local_show_root_config_cannot_hide_the_root_commit(self) -> None:
        # m1: with log.showRoot=false `git show` prints no diff for a root commit
        git(self.repo, "config", "log.showRoot", "false")
        self.commit({"a.txt": "zyxname\n"})
        code, out = self.scan("--commits", "HEAD")
        self.assertEqual(code, 1)
        self.assertIn(" a.txt: denylist entry 1", out)

    def test_local_output_encoding_cannot_hide_a_message_term_or_break_identities(self) -> None:
        # m2: i18n.logOutputEncoding re-encodes the metadata git show prints: UTF-16LE put a NUL in
        # every character (the identity split crashed), ISO-8859-2 turned `š` into a byte no reading
        # of the scanner decoded back
        self.add_terms("šqxwzy")
        ids = self.tmp / "ids.txt"
        ids.write_text("test@example.org\n", encoding="utf-8")
        for number, encoding in enumerate(("UTF-16LE", "ISO-8859-2")):
            with self.subTest(encoding=encoding):
                git(self.repo, "config", "i18n.logOutputEncoding", encoding)
                try:
                    self.commit({"a.txt": f"clean {number}\n"}, message=f"fix for zyxname and šqxwzy {number}")
                    code, out = self.scan("--identities", str(ids), "--commits", "-1 HEAD")
                    self.assertEqual(code, 1)
                    self.assertIn("commit metadata: denylist entry 1", out)
                    self.assertIn("commit metadata: denylist entry 4", out)
                    self.assertNotIn("identity", out)
                finally:
                    git(self.repo, "config", "--unset", "i18n.logOutputEncoding")

    def test_local_show_signature_config_cannot_break_the_identity_check(self) -> None:
        # m2: log.showSignature prints the signature check ("No signature") ahead of the format,
        # inside the author field of a signed commit
        key = self.tmp / "signing-key"
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)], check=True, capture_output=True)
        git(self.repo, "config", "gpg.format", "ssh")
        git(self.repo, "config", "user.signingkey", f"{key}.pub")
        git(self.repo, "commit", "-q", "-S", "--allow-empty", "-m", "signed")
        git(self.repo, "config", "log.showSignature", "true")
        ids = self.tmp / "ids.txt"
        ids.write_text("test@example.org\n", encoding="utf-8")
        code, out = self.scan("--identities", str(ids), "--commits", "HEAD")
        self.assertEqual((code, out.strip()), (0, "denylist: clean"))

    # --- #32 E4: the identity check reads author and committer as separate fields ---

    def commit_as(self, author_email: str, committer_email: str) -> None:
        env = {**os.environ, "GIT_AUTHOR_EMAIL": author_email, "GIT_COMMITTER_EMAIL": committer_email}
        subprocess.run(["git", "-C", str(self.repo), "commit", "-q", "--allow-empty", "-m", "identity"],
                       check=True, capture_output=True, env=env)

    def test_a_line_separator_in_the_author_email_cannot_push_out_the_committer(self) -> None:
        # `%ae%n%ce` split with str.splitlines() broke the author email at U+2028 into two allowed
        # halves, and the committer email fell off the end unchecked
        ids = self.tmp / "ids.txt"
        ids.write_text("test@example.org\n", encoding="utf-8")
        self.commit_as("test@example.org\u2028test@example.org", "outsider@example.net")
        code, out = self.scan("--identities", str(ids), "--commits", "HEAD")
        self.assertEqual(code, 1)
        self.assertIn("author email is not an allowed identity", out)
        self.assertIn("committer email is not an allowed identity", out)
        self.assertNotIn("outsider", out)

    def test_an_email_holding_a_line_separator_is_never_allowed(self) -> None:
        ids = self.tmp / "ids.txt"
        ids.write_text("test@example.org\n", encoding="utf-8")
        # (git itself strips a trailing ASCII control such as VT from an email; these it keeps)
        for separator in ("\u2028", "\u2029", "\x85"):
            with self.subTest(separator=hex(ord(separator))):
                self.commit_as(f"test@example.org{separator}", "test@example.org")
                code, out = self.scan("--identities", str(ids), "--commits", "-1 HEAD")
                self.assertEqual(code, 1)
                self.assertIn("author email is not an allowed identity", out)
                self.assertNotIn("committer email", out)

    # --- #32 F5 m7: local repository state cannot redirect what the scan reads ---

    def test_a_replace_ref_cannot_swap_a_blob_for_a_clean_one(self) -> None:
        # `git replace` makes cat-file, show and ls-tree read another object in place of the real
        # one: a local replace ref turned the term-bearing blob into a clean one in both modes
        self.commit({"base.txt": "base\n"})
        self.commit({"a.txt": "keep zyxname\n"})
        clean = git_out(self.repo, "hash-object", "-w", "--stdin", stdin=b"clean\n")
        git(self.repo, "replace", git_out(self.repo, "rev-parse", "HEAD:a.txt"), clean)
        code, out = self.scan("--tree", "HEAD", "--commits", "HEAD~1..HEAD")
        self.assertEqual(code, 1)
        self.assertIn("tree a.txt:1: denylist entry 1", out)
        self.assertIn(" a.txt: denylist entry 1", out)

    def test_a_graft_cannot_cut_commits_out_of_the_history_scan(self) -> None:
        # a grafts file gives a commit other parents: listing HEAD alone as a root hid every
        # commit before it from rev-list
        self.commit({"a.txt": "zyxname\n"})
        self.commit({"a.txt": "clean\n"})
        (self.repo / ".git" / "info" / "grafts").write_text(git_out(self.repo, "rev-parse", "HEAD") + "\n",
                                                            encoding="utf-8")
        code, out = self.scan("--commits", "HEAD")
        self.assertEqual(code, 1)
        self.assertIn(" a.txt: denylist entry 1", out)

    # --- #32 F5 m8: --hash keys the committed blob, not the working-tree file ---

    def hash_key(self, target: str, number: str) -> str:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(ds.main(["--repo", str(self.repo), "--hash", target, number]), 0)
        return out.getvalue().strip()

    def test_hash_reads_the_committed_blob_not_the_working_tree(self) -> None:
        # the working-tree file differs from the scanned blob (an uncommitted edit; eol, encoding or
        # a smudge filter on checkout), so its key matched nothing the scan reported
        self.commit({"a.txt": "keep zyxname here\n", "a:b.txt": "keep zyxname too\n"})
        (self.repo / "a.txt").write_text("edited since\n", encoding="utf-8")
        (self.repo / "a:b.txt").write_text("edited since\n", encoding="utf-8")
        self.assertEqual(self.hash_key("a.txt", "1"), ds.line_key("a.txt", "keep zyxname here"))
        self.assertEqual(self.hash_key("a:b.txt", "1"), ds.line_key("a:b.txt", "keep zyxname too"))

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
        long = "x" * (ds.CHUNK - 4) + " zyxname " + "y" * ds.CHUNK
        noise = random.Random(9).randbytes(4 * ds.CHUNK).replace(b"\x00", b"\x01")  # keeps a wide string binary
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

    def test_a_named_reference_never_makes_a_batch_separator(self) -> None:
        # a reading that created U+E000 would shift every later unit: the hit must stay on line 3
        self.commit({"a.html": "&dcaron;\n&#xE000;&#57344;\nkeep zyxname\n"})
        code, out = self.scan("--tree", "HEAD")
        self.assertEqual((code, out.count("denylist entry")), (1, 1), out)
        self.assertIn("tree a.html:3: denylist entry 1", out)


if __name__ == "__main__":
    unittest.main()
