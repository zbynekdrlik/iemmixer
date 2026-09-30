"""Tests for scripts/denylist_scan.py (run: python3 -m unittest discover -s scripts)."""
from __future__ import annotations

import contextlib
import io
import shutil
import subprocess
import sys
import tempfile
import unicodedata
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import denylist_scan as ds  # noqa: E402

TERMS = ["zyxname", "10.9.", "ghost-host.example"]
REDACTED_MARKER = "[redacted]"


def git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)


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

    def test_binary_content_is_skipped_but_its_path_is_scanned(self) -> None:
        self.commit({"bin.dat": b"\0zyxname"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 0)
        self.commit({"zyxname.bin": b"\0x"})
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
        term = "ďurica"  # 'ďurica' precomposed (NFC)
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


if __name__ == "__main__":
    unittest.main()
