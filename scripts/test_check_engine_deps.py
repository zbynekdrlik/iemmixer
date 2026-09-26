import tempfile
import unittest
from pathlib import Path

import check_engine_deps as ced


def pkg(pid: str) -> dict:
    return {"id": pid, "name": pid}


def node(pid: str, *deps: tuple[str, str | None]) -> dict:
    return {"id": pid, "deps": [{"pkg": d, "dep_kinds": [{"kind": k}]} for d, k in deps]}


META = {
    "packages": [pkg(p) for p in ["iem-engine", "serde", "serde_derive", "cc", "tempfile", "rand", "other"]],
    "resolve": {
        "nodes": [
            node("iem-engine", ("serde", None), ("cc", "build"), ("tempfile", "dev")),
            node("serde", ("serde_derive", "normal")),
            node("serde_derive"),
            node("cc"),
            node("tempfile", ("rand", None)),
            node("rand"),
            node("other"),
        ]
    },
}


class ClosureTests(unittest.TestCase):
    def test_normal_and_build_edges_count_dev_edges_do_not(self) -> None:
        self.assertEqual(ced.closure(META), {"serde", "serde_derive", "cc"})

    def test_differences_are_named_both_ways(self) -> None:
        errors = ced.compare({"serde", "cc"}, {"serde", "old"})
        self.assertEqual(len(errors), 2)
        self.assertIn("cc: in the engine's dependency closure but not allowlisted", errors)
        self.assertIn("old: allowlisted but no longer a dependency of the engine", errors)
        self.assertEqual(ced.compare({"a"}, {"a"}), [])

    def test_the_allowlist_ignores_comments_and_blank_lines(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "allow.txt"
            p.write_text("# header\nserde  # serialisation\n\ncc\n")
            self.assertEqual(ced.read_allowlist(p), {"serde", "cc"})


if __name__ == "__main__":
    unittest.main()
