"""Tests for scripts/check_mutants_anchors.py (#32 D1)."""
from __future__ import annotations

import contextlib
import io
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_mutants_anchors as cma  # noqa: E402

RT = "crates/iem-engine/src/rt.rs"
READ = "crates/iem-rpp/src/read.rs"
RT_SRC = "fn process() {\n    let x = 1;\n    if budget > 0\n        && next\n}\n"
READ_SRC = "fn raw_tokens() {\n    while i < n {\n        k += 1;\n        while i < n && b != c {\n        }\n    }\n}\n"
FILES = {RT: RT_SRC, READ: READ_SRC, "crates/iem-engine/src/engine.rs": "fn run() {}\n"}


def config(*entries: str) -> str:
    body = "\n".join(entries)
    return f'profile = "mutants"\n\nexclude_globs = [\n  "crates/iem-tray/**",\n]\n\nexclude_re = [\n{body}\n]\n'


def run(text: str, files: dict[str, str] | None = None) -> tuple[list[str], int]:
    files = FILES if files is None else files
    return cma.check(text, sorted(files), lambda rel: files[rel])


class AnchorTests(unittest.TestCase):
    def test_an_anchor_at_its_line_holds(self) -> None:
        text = config(
            "  # Equivalent: the spent budget.",
            "  # anchor 3: if budget > 0",
            '  "iem-engine/src/rt\\\\.rs:3:.*replace > with >= in process",',
        )
        self.assertEqual(run(text), ([], 1))

    def test_a_moved_line_fails_and_names_where_it_went(self) -> None:
        text = config(
            "  # anchor 2: if budget > 0",
            '  "iem-engine/src/rt\\\\.rs:2:.*replace > with >= in process",',
        )
        problems, held = run(text)
        self.assertEqual(held, 0)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn(f"{RT}:2 reads `let x = 1;`", problems[0])
        self.assertIn("expects `if budget > 0`", problems[0])
        self.assertIn("now at line 3", problems[0])
        # Text that is gone from the file entirely is named as such.
        text = config(
            "  # anchor 3: if spent > 0",
            '  "iem-engine/src/rt\\\\.rs:3:.*replace > with >= in process",',
        )
        problems, _ = run(text)
        self.assertIn("not in the file", problems[0])

    def test_an_anchor_without_its_comment_fails(self) -> None:
        text = config(
            "  # Equivalent: the spent budget.",
            '  "iem-engine/src/rt\\\\.rs:3:.*replace > with >= in process",',
        )
        problems, held = run(text)
        self.assertEqual(held, 0)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("anchor iem-engine/src/rt.rs:3 needs exactly one `# anchor 3:", problems[0])
        # A blank line cuts the comment block: the comment is not "right above".
        text = config(
            "  # anchor 3: if budget > 0",
            "",
            '  "iem-engine/src/rt\\\\.rs:3:.*replace > with >= in process",',
        )
        problems, _ = run(text)
        self.assertEqual(len(problems), 2, problems)
        self.assertIn("needs exactly one", problems[0])
        self.assertIn("belongs to no line anchor", problems[1])
        # Two comments for the same line are refused too.
        text = config(
            "  # anchor 3: if budget > 0",
            "  # anchor 3: if budget > 0",
            '  "iem-engine/src/rt\\\\.rs:3:.*replace > with >= in process",',
        )
        problems, _ = run(text)
        self.assertIn("(found 2)", problems[0])

    def test_an_alternation_needs_a_comment_per_line(self) -> None:
        both = config(
            "  # anchor 2: while i < n {",
            "  # anchor 4: while i < n && b != c {",
            '  "iem-rpp/src/read\\\\.rs:(2|4):.*replace < with <= in raw_tokens$",',
        )
        self.assertEqual(run(both), ([], 2))
        one = config(
            "  # anchor 2: while i < n {",
            '  "iem-rpp/src/read\\\\.rs:(2|4):.*replace < with <= in raw_tokens$",',
        )
        problems, held = run(one)
        self.assertEqual(held, 1)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("read.rs:4 needs exactly one", problems[0])

    def test_a_stray_anchor_comment_fails(self) -> None:
        # Above an entry without that line anchor.
        text = config(
            "  # anchor 9: if budget > 0",
            '  "iem-engine/src/rt\\\\.rs.*replace > with >= in process",',
        )
        problems, _ = run(text)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("line 8: `# anchor 9:` belongs to no line anchor", problems[0])
        # Outside the exclude_re block.
        text = "# anchor 3: if budget > 0\n" + config('  "new_for_test",')
        problems, _ = run(text)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("line 1: `# anchor 3:`", problems[0])

    def test_other_anchor_forms_are_refused(self) -> None:
        for regex in (
            "iem-engine/src/rt.rs:3:.*replace",  # unescaped dot
            "iem-engine/src/rt\\\\.rs:(3|x):.*replace",
        ):
            with self.subTest(regex=regex):
                problems, held = run(config("  # anchor 3: if budget > 0", f'  "{regex}",'))
                self.assertEqual(held, 0)
                self.assertIn("unsupported form", problems[0])

    def test_the_path_must_name_one_tracked_file(self) -> None:
        text = config(
            "  # anchor 3: if budget > 0",
            '  "src/rt\\\\.rs:3:.*replace > with >= in process",',
        )
        # One match by suffix holds.
        self.assertEqual(run(text), ([], 1))
        two = dict(FILES)
        two["crates/other/src/rt.rs"] = RT_SRC
        problems, _ = run(text, two)
        self.assertIn("matches 2 tracked files, not one", problems[0])
        text = config(
            "  # anchor 3: if budget > 0",
            '  "iem-engine/src/gone\\\\.rs:3:.*replace",',
        )
        problems, _ = run(text)
        self.assertIn("gone.rs matches 0 tracked files", problems[0])
        # A suffix must end at a path separator: `t.rs` is not `rt.rs`.
        text = config(
            "  # anchor 3: if budget > 0",
            '  "t\\\\.rs:3:.*replace",',
        )
        problems, _ = run(text)
        self.assertIn("matches 0 tracked files", problems[0])

    def test_a_line_past_the_end_fails(self) -> None:
        text = config(
            "  # anchor 60: if budget > 0",
            '  "iem-engine/src/rt\\\\.rs:60:.*replace",',
        )
        problems, _ = run(text)
        self.assertIn(f"{RT} has 5 lines, the anchor names 60", problems[0])

    def test_lines_count_by_newline_only(self) -> None:
        # CRLF and U+2028 inside a line never shift the numbering (cargo-mutants
        # counts `\n` only).
        files = {RT: "fn a() {\r\n    let s = \"x y\";\r\n    if budget > 0\r\n}\r\n"}
        text = config(
            "  # anchor 3: if budget > 0",
            '  "iem-engine/src/rt\\\\.rs:3:.*replace",',
        )
        self.assertEqual(run(text, files), ([], 1))

    def test_entries_the_line_reader_cannot_see_are_refused(self) -> None:
        for bad in (
            '  "a", "b",',  # two entries on one line
            '  "a", # a trailing comment',
            "  'a',",  # still one string: literal strings are fine
        ):
            with self.subTest(line=bad):
                problems, _ = run(config(bad))
                if bad.strip().startswith("'"):
                    self.assertEqual(problems, [])
                else:
                    self.assertTrue(problems, bad)
        problems, _ = run('profile = "mutants"\n')
        self.assertEqual(problems, ["no `exclude_re = [` block"])
        problems, _ = run('exclude_re = [\n  "a",\n')
        self.assertIn("no closing `]`", problems[0])


class RepositoryTests(unittest.TestCase):
    def test_the_repository_config_holds(self) -> None:
        err = io.StringIO()
        out = io.StringIO()
        with contextlib.redirect_stderr(err), contextlib.redirect_stdout(out):
            code = cma.main([])
        self.assertEqual(code, 0, err.getvalue())
        self.assertIn("line anchor(s) point at their code", out.getvalue())

    def test_main_reports_a_stale_anchor(self) -> None:
        import subprocess
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            (root / ".cargo").mkdir()
            (root / "crates/iem-engine/src").mkdir(parents=True)
            (root / RT).write_text(RT_SRC, encoding="utf-8")
            (root / ".cargo/mutants.toml").write_text(
                config("  # anchor 2: if budget > 0", '  "iem-engine/src/rt\\\\.rs:2:.*replace",'),
                encoding="utf-8",
            )
            subprocess.run(["git", "add", "-A"], cwd=root, check=True)
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                code = cma.main(["--root", str(root)])
        self.assertEqual(code, 1)
        self.assertIn(f".cargo/mutants.toml: line 9: {RT}:2 reads", err.getvalue())


if __name__ == "__main__":
    unittest.main()
