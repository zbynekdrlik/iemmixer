import contextlib
import io
import tempfile
import unittest
from pathlib import Path

import check_parity_manifest as cpm

RUST = """\
fn helper() {}

#[cfg(test)]
mod tests {
    #[test]
    fn plain_test() {}

    /// A doc comment between the attribute run and the fn.
    #[tokio::test(flavor = "multi_thread")]
    #[should_panic]
    async fn async_test() {}

    #[tracing_test::traced_test]
    // a comment line
    fn traced() {}

    #[cfg(test)]
    fn cfg_only() {}

    #[test]
    const X: u8 = 1;
    fn after_a_const() {}
}
"""

SPEC = """\
import { test, expect } from "@playwright/test";

test.describe("A describe title", () => {
  test("a plain title", async () => {});
  test(
    'a title; with a semicolon and ::colons',
    async () => {},
  );
  test(`${member}: a template title`, async () => {});
  test("it\\'s escaped", async () => {});
});
function mytest(t: string) {}
mytest("not a test");
"""

PY = """\
def helper():
    pass


def test_one():
    pass


    async def test_two(self):
        pass
"""


def manifest(rows: list[str], total: int | None = None) -> str:
    head = "# a comment\n"
    if total is not None:
        head += f"# total: {total}\n"
    return head + "\t".join(cpm.COLUMNS) + "\n" + "".join(r + "\n" for r in rows)


def row(status: str = "PORTED", gen2: str = "crates/x/src/lib.rs::plain_test", reason: str = "", name: str = "t") -> str:
    return "\t".join(["gen1/file.rs", name, status, gen2, reason])


class TreeTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        for rel, text in {"crates/x/src/lib.rs": RUST, "e2e/tests/a.spec.ts": SPEC, "scripts/test_y.py": PY}.items():
            (self.root / rel).parent.mkdir(parents=True, exist_ok=True)
            (self.root / rel).write_text(text, encoding="utf-8")

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def run_check(self, rows: list[str], total: int | None = None, cutover: bool = False, expected: int | None = None):
        declared, parsed, errors = cpm.parse(manifest(rows, len(rows) if total is None else total))
        self.assertEqual(errors, [])
        return cpm.check(parsed, declared, self.root, cutover=cutover, expected=len(rows) if expected is None else expected)


class Gen2IndexTests(TreeTest):
    def test_rust_test_fns_are_those_under_a_test_attribute(self) -> None:
        self.assertEqual(cpm.rust_tests(RUST), {"plain_test", "async_test", "traced"})

    def test_playwright_titles_are_test_calls_only(self) -> None:
        self.assertEqual(
            cpm.playwright_titles(SPEC),
            {"a plain title", "a title; with a semicolon and ::colons", "${member}: a template title", "it's escaped"},
        )

    def test_python_tests_are_test_functions(self) -> None:
        self.assertEqual(cpm.Gen2Tests(self.root).names("scripts/test_y.py"), {"test_one", "test_two"})

    def test_a_missing_or_escaping_path_has_no_names(self) -> None:
        tests = cpm.Gen2Tests(self.root)
        self.assertIsNone(tests.names("crates/x/src/missing.rs"))
        (self.root / "outside.rs").write_text(RUST, encoding="utf-8")
        self.assertIsNone(tests.names("crates/../outside.rs"))


class RefTests(TreeTest):
    def test_refs_split_only_where_a_path_starts(self) -> None:
        field = "e2e/tests/a.spec.ts::a title; with a semicolon and ::colons; crates/x/src/lib.rs::plain_test;scripts/test_y.py::test_one"
        self.assertEqual(
            cpm.split_refs(field),
            ("e2e/tests/a.spec.ts::a title; with a semicolon and ::colons", "crates/x/src/lib.rs::plain_test",
             "scripts/test_y.py::test_one"),
        )
        self.assertEqual(cpm.split_refs("  "), ())

    def test_every_kind_of_existing_ref_passes(self) -> None:
        gen2 = "; ".join([
            "crates/x/src/lib.rs::plain_test", "crates/x/src/lib.rs::async_test", "crates/x/src/lib.rs::traced",
            "e2e/tests/a.spec.ts::a title; with a semicolon and ::colons", "e2e/tests/a.spec.ts::${member}: a template title",
            "e2e/tests/a.spec.ts::it's escaped", "scripts/test_y.py::test_two",
        ])
        errors, waiting = self.run_check([row(gen2=gen2)])
        self.assertEqual(errors, [])
        self.assertEqual(waiting, {})

    def test_a_missing_or_non_test_fn_fails(self) -> None:
        for name in ("helper", "cfg_only", "after_a_const", "gone"):
            errors, _ = self.run_check([row(gen2=f"crates/x/src/lib.rs::{name}")])
            self.assertEqual(errors, [f"line 4 (gen1/file.rs::t): no test '{name}' in crates/x/src/lib.rs"], name)

    def test_a_test_fn_counts_only_in_the_file_cited(self) -> None:
        (self.root / "crates/x/src/other.rs").write_text("fn nothing() {}\n", encoding="utf-8")
        errors, _ = self.run_check([row(gen2="crates/x/src/other.rs::plain_test")])
        self.assertEqual(errors, ["line 4 (gen1/file.rs::t): no test 'plain_test' in crates/x/src/other.rs"])

    def test_a_playwright_title_must_match_exactly(self) -> None:
        for title in ("a plain", "a plain title.", "A describe title", "not a test", "A PLAIN TITLE"):
            errors, _ = self.run_check([row(gen2=f"e2e/tests/a.spec.ts::{title}")])
            self.assertEqual(errors, [f"line 4 (gen1/file.rs::t): no test '{title}' in e2e/tests/a.spec.ts"], title)

    def test_a_python_ref_must_be_a_test_function(self) -> None:
        errors, _ = self.run_check([row(gen2="scripts/test_y.py::helper")])
        self.assertEqual(errors, ["line 4 (gen1/file.rs::t): no test 'helper' in scripts/test_y.py"])

    def test_a_missing_file_and_an_unsupported_ref_fail(self) -> None:
        errors, _ = self.run_check([row(gen2="crates/x/src/nope.rs::plain_test"), row(name="u", gen2="docs/x.md::y")])
        self.assertEqual(errors, [
            "line 4 (gen1/file.rs::t): crates/x/src/nope.rs does not exist",
            "line 5 (gen1/file.rs::u): 'docs/x.md::y' is not crates/..rs::fn, e2e/tests/..ts::title or scripts/..py::test",
        ])


class StatusTests(TreeTest):
    def test_obsolete_needs_a_reason_and_no_ref(self) -> None:
        errors, _ = self.run_check([row("OBSOLETE", gen2="", reason="REAPER-only"), row("OBSOLETE", gen2="", name="u")])
        self.assertEqual(errors, ["line 5 (gen1/file.rs::u): OBSOLETE needs a reason"])

    def test_ported_and_transformed_need_a_gen2_test(self) -> None:
        errors, _ = self.run_check([row("PORTED", gen2=""), row("TRANSFORMED", gen2="", name="u")])
        self.assertEqual(errors, [
            "line 4 (gen1/file.rs::t): PORTED needs a gen2 test",
            "line 5 (gen1/file.rs::u): TRANSFORMED needs a gen2 test",
        ])

    def test_open_rows_are_allowed_only_while_waiting_for_s7_or_s6(self) -> None:
        rows = [
            row("PENDING", gen2="", reason="the PC half runs in S7 #10", name="a"),
            row("FEATURE-GAP", gen2="", reason="waits for S6 #9 F30", name="b"),
            row("PENDING", gen2="crates/x/src/lib.rs::plain_test", reason="S7 #10 PC half", name="c"),
            row("PENDING", gen2="", reason="not written yet", name="d"),
            row("FEATURE-GAP", gen2="", reason="", name="e"),
            row("PENDING", gen2="", reason="S7 #100 is another ticket", name="f"),
            row("PENDING", gen2="", reason="S6 #90 and XS7 #10", name="g"),
        ]
        errors, waiting = self.run_check(rows)
        self.assertEqual(errors, [
            "line 7 (gen1/file.rs::d): PENDING without 'S7 #10' or 'S6 #9' in its reason",
            "line 8 (gen1/file.rs::e): FEATURE-GAP without 'S7 #10' or 'S6 #9' in its reason",
            "line 9 (gen1/file.rs::f): PENDING without 'S7 #10' or 'S6 #9' in its reason",
            "line 10 (gen1/file.rs::g): PENDING without 'S7 #10' or 'S6 #9' in its reason",
        ])
        self.assertEqual(waiting, {"S7 #10": 2, "S6 #9": 1})

    def test_cutover_allows_no_open_row(self) -> None:
        rows = [row("PENDING", gen2="", reason="S7 #10", name="a"), row("FEATURE-GAP", gen2="", reason="S6 #9", name="b"), row()]
        errors, waiting = self.run_check(rows, cutover=True)
        self.assertEqual(errors, [
            "line 4 (gen1/file.rs::a): PENDING at cutover",
            "line 5 (gen1/file.rs::b): FEATURE-GAP at cutover",
        ])
        self.assertEqual(waiting, {})

    def test_an_unknown_status_fails(self) -> None:
        errors, _ = self.run_check([row("DONE", gen2="crates/x/src/lib.rs::gone")])
        self.assertEqual(errors, ["line 4 (gen1/file.rs::t): unknown status 'DONE'"])


class ShapeTests(TreeTest):
    def test_the_row_count_must_match_the_header_and_the_inventory(self) -> None:
        errors, _ = self.run_check([row(), row(name="u")], total=3, expected=3)
        self.assertEqual(errors, ["the header declares 3 rows, the manifest has 2"])
        errors, _ = self.run_check([row(), row(name="u")], total=2, expected=3)
        self.assertEqual(errors, ["the header declares 2 rows, the gen 1 inventory has 3"])
        self.assertEqual(self.run_check([row(), row(name="u")])[0], [])

    def test_a_repeated_or_nameless_row_fails(self) -> None:
        errors, _ = self.run_check([
            row(), row(), "\t".join(["", "", "PORTED", "crates/x/src/lib.rs::plain_test", ""]), row(name=""),
            "\t".join(["", "n", "PORTED", "crates/x/src/lib.rs::plain_test", ""]),
        ])
        self.assertEqual(errors, [
            "line 5 (gen1/file.rs::t): repeats line 4",
            "line 6 (::): gen1_file and gen1_test are required",
            "line 7 (gen1/file.rs::): gen1_file and gen1_test are required",
            "line 8 (::n): gen1_file and gen1_test are required",
        ])

    def test_format_errors_are_reported(self) -> None:
        declared, rows, errors = cpm.parse("# total: 1\n# total: 2\ngen1_file\tgen1_test\n\na\tb\tPORTED\n")
        self.assertEqual(declared, 2)
        self.assertEqual(rows, [])
        self.assertEqual(errors, [
            "line 2: a second '# total:' line",
            "line 3: the column header must be gen1_file / gen1_test / status / gen2 / reason",
            "line 5: 3 fields, expected 5",
        ])
        self.assertEqual(cpm.parse("# nothing\n")[2], ["no '# total: N' line", "no column header line"])

    def test_fields_are_parsed_and_stripped(self) -> None:
        declared, rows, errors = cpm.parse(manifest([" f \t n \tPENDING\t\t S7 #10 "], 1))
        self.assertEqual((declared, errors), (1, []))
        self.assertEqual(rows, [cpm.Row(4, "f", "n", "PENDING", (), "S7 #10")])


class MainTests(TreeTest):
    def write(self, rows: list[str], total: int) -> None:
        path = self.root / cpm.MANIFEST
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(manifest(rows, total), encoding="utf-8")

    def main(self, *args: str) -> tuple[int, str]:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = cpm.main(["--root", str(self.root), *args])
        return code, out.getvalue()

    def test_a_clean_manifest_passes_and_counts_the_waiting_rows(self) -> None:
        rows = [row(name=str(i)) for i in range(cpm.GEN1_TESTS - 1)] + [row("PENDING", gen2="", reason="S7 #10", name="p")]
        self.write(rows, cpm.GEN1_TESTS)
        code, out = self.main()
        self.assertEqual(code, 0, out)
        self.assertIn(f"parity manifest: {cpm.GEN1_TESTS} rows (PORTED {cpm.GEN1_TESTS - 1}, TRANSFORMED 0, OBSOLETE 0, "
                      "PENDING 1, FEATURE-GAP 0); allowed until cutover: 1 (S7 #10: 1)", out)
        code, out = self.main("--cutover")
        self.assertEqual(code, 1)
        self.assertIn("::error::docs/parity/gen1-tests.tsv: line ", out)
        self.assertIn("PENDING at cutover", out)
        self.assertIn("parity manifest: 1 problem(s)", out)

    def test_a_short_manifest_fails(self) -> None:
        self.write([row()], 1)
        code, out = self.main()
        self.assertEqual(code, 1)
        self.assertIn(f"the header declares 1 rows, the gen 1 inventory has {cpm.GEN1_TESTS}", out)

    def test_a_missing_manifest_fails(self) -> None:
        code, out = self.main()
        self.assertEqual(code, 1)
        self.assertIn("::error::docs/parity/gen1-tests.tsv is missing", out)

    def test_the_inventory_is_the_pinned_gen1_count(self) -> None:
        self.assertEqual(cpm.GEN1_TESTS, 543 + 276 + 89)


if __name__ == "__main__":
    unittest.main()
