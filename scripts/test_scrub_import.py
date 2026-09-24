"""Tests for scripts/scrub_import.py."""
from __future__ import annotations

import shutil
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import scrub_import as si  # noqa: E402

MAP = (
    "ts-ident\t4321\tMEMBER_PIN\n"
    "literal\tghost.example\tmixer.example.org\n"
    "word\tzyxname\tmember1\n"
    "word\t4321\t<PIN>\n"
)


class ScrubTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.root = self.tmp / "tree"
        self.root.mkdir()
        self.map = self.tmp / "map.tsv"
        self.map.write_text(MAP, encoding="utf-8")
        self.report = self.tmp / "report.tsv"

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp)

    def write(self, rel: str, content: str | bytes) -> Path:
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content if isinstance(content, bytes) else content.encode("utf-8"))
        return path

    def run_scrub(self) -> int:
        return si.main(["--map", str(self.map), "--root", str(self.root), "--report", str(self.report)])

    def test_word_rule_keeps_case_style_and_boundaries(self) -> None:
        path = self.write("a.rs", 'let t = "ZYXNAME inear"; // zyxname\nlet n = "Zyxname"; let k = prezyxname; let u = x_zyxname;\n')
        self.assertEqual(self.run_scrub(), 0)
        self.assertEqual(
            path.read_text(encoding="utf-8"),
            'let t = "MEMBER1 inear"; // member1\nlet n = "Member1"; let k = prezyxname; let u = x_member1;\n',
        )

    def test_word_rule_sees_through_string_escapes(self) -> None:
        path = self.write("h.rs", 'let l = "TRACK\\t1\\tZYXNAME mic\\nzyxname"; let k = "\\tprezyxname";\n')
        self.run_scrub()
        self.assertEqual(
            path.read_text(encoding="utf-8"),
            'let l = "TRACK\\t1\\tMEMBER1 mic\\nmember1"; let k = "\\tprezyxname";\n',
        )

    def test_literal_rule_is_case_insensitive(self) -> None:
        path = self.write("b.rs", 'const H: &str = "GHOST.example";\n')
        self.run_scrub()
        self.assertEqual(path.read_text(encoding="utf-8"), 'const H: &str = "mixer.example.org";\n')

    def test_ts_ident_replaces_quoted_pin_and_adds_the_import(self) -> None:
        path = self.write("tests/x.spec.ts", 'import { test } from "@playwright/test";\nconst a = "4321"; const b = \'4321\';\n// default PIN 4321\n')
        self.run_scrub()
        self.assertEqual(
            path.read_text(encoding="utf-8"),
            'import { MEMBER_PIN } from "./support/pins";\nimport { test } from "@playwright/test";\n'
            "const a = MEMBER_PIN; const b = MEMBER_PIN;\n// default PIN <PIN>\n",
        )

    def test_quoted_pin_outside_typescript_becomes_the_word_placeholder(self) -> None:
        path = self.write("c.rs", 'let pin = Some("4321".to_string());\n')
        self.run_scrub()
        self.assertEqual(path.read_text(encoding="utf-8"), 'let pin = Some("<PIN>".to_string());\n')

    def test_issue_refs_are_rewritten_in_comments_and_markdown_only(self) -> None:
        rs = self.write("d.rs", 'let c = "#000"; // see #179 and reaperiem#12\n')
        md = self.write("e.md", "Fixed in #202.\n# Heading\n")
        css = self.write("f.css", "a { color: #000; }\n")
        self.run_scrub()
        self.assertEqual(rs.read_text(encoding="utf-8"), 'let c = "#000"; // see reaperiem#179 and reaperiem#12\n')
        self.assertEqual(md.read_text(encoding="utf-8"), "Fixed in reaperiem#202.\n# Heading\n")
        self.assertEqual(css.read_text(encoding="utf-8"), "a { color: #000; }\n")

    def test_binary_files_are_untouched(self) -> None:
        path = self.write("g.bin", b"\0zyxname")
        self.run_scrub()
        self.assertEqual(path.read_bytes(), b"\0zyxname")

    def test_a_private_path_aborts(self) -> None:
        self.write("zyxname-notes.md", "x\n")
        with self.assertRaises(SystemExit):
            self.run_scrub()

    def test_malformed_map_is_rejected(self) -> None:
        self.map.write_text("word\tonly-two-columns\n", encoding="utf-8")
        with self.assertRaises(ValueError):
            self.run_scrub()

    def test_report_names_rules_not_values(self) -> None:
        self.write("a.rs", "zyxname\n")
        self.run_scrub()
        report = self.report.read_text(encoding="utf-8")
        self.assertIn("a.rs\t3\t1", report)
        self.assertNotIn("zyxname", report)


if __name__ == "__main__":
    unittest.main()
