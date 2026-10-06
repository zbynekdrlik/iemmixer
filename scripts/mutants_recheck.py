#!/usr/bin/env python3
"""Mutation gate post-check (#23): a caught mutant must be caught by a test.

The `mutants` nextest profile (.config/nextest.toml) ends a test at its
slow-timeout and counts it as failed, so a mutant that hangs is caught
quickly. On a loaded runner an ordinary slow test can reach the same bound
and "catch" a mutant it never ran. For every mutant cargo-mutants reports as
caught, this reads the test phase of the mutant's log; the first failing test
decides. A test that failed on its own (FAIL, a leak failure, a crash signal)
proves the catch. A test that nextest ended (TIMEOUT, or SIGTERM/SIGKILL when
fail-fast ends the tests still running), or no failing test at all, does not:
those mutants are tested again, once, with the `mutants-recheck` profile (a
generous per-test bound) and the options given after `--`:

    python3 scripts/mutants_recheck.py [--out mutants.out] -- <cargo mutants options>

The options are the main run's without --in-diff, --shard and --output: the
script selects the mutants by name (--re) and by their files (--file: field-deletion
mutants ignore --re, upstream cargo-mutants#632; one it tests anyway in a selected file
is only noted) and writes to <out>/recheck. Pass
--jobs 1 so no second mutant loads the runner. Exit status, as cargo-mutants:
0 every catch holds (caught again, also by a timeout at the generous bound: a
hang), 2 a mutant survived its recheck (its first catch was false), 3 a
recheck timed out; 1 the recheck did not run, did not test every selected
mutant, or cannot show what caught one.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

RECHECK_PROFILE = "mutants-recheck"
ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
# nextest's result line: `<STATUS> [<secs>s] [counter] <binary-id> <test>`.
# The SLOW and TERMINATING progress lines read `[>…s]` and are not results.
RESULT = re.compile(r"^\s*(?P<status>[A-Z][A-Z0-9 +/-]*?)\s+\[\s*\d+\.\d+s\]\s+(?P<rest>\S.*)$")
# The test failed on its own: an assertion or panic, a leak, a crash.
FAILED = re.compile(r"^(?:TRY \d+ )?(?:FAIL|FAIL \+ LEAK|FL\+LK|LEAK-FAIL|LKFAIL|ABORT|SIG(?:SEGV|ABRT|BUS|ILL|FPE|TRAP|SYS))$")
# nextest ended the test: its slow-timeout, or fail-fast's SIGTERM (SIGKILL after the grace period).
ENDED = re.compile(r"^(?:TRY \d+ )?(?:TIMEOUT|TMT|SIG(?:TERM|KILL|HUP|INT|QUIT))$")
# regex::escape's meta characters (`<` and `>` stay: `\<` is a word boundary there).
REGEX_META = set("\\.+*?()|[]{}^$#&-~")
# A struct-field deletion mutant, which ignores --re (upstream cargo-mutants#632).
FIELD_DELETION = re.compile(r"^[^:]+:\d+:\d+: delete field \S+ from struct .+ expression in ")
# Options that would make the recheck select other mutants or write over the main output.
REFUSED = ("--in-diff", "--shard", "--output", "-o")


@dataclass(frozen=True)
class Failure:
    status: str
    test: str
    by_test: bool  # True: the test failed on its own; False: nextest ended it


def nextest_output(log: str) -> list[str] | None:
    """The lines the test phase's `cargo nextest run` wrote into a cargo-mutants log."""
    lines = ANSI.sub("", log).splitlines()
    start = None
    for i, line in enumerate(lines):
        if line.startswith("*** ") and " nextest run" in line and "--no-run" not in line:
            start = i + 1
    if start is None:
        return None
    end = next((j for j in range(start, len(lines)) if lines[j].startswith("*** result:")), len(lines))
    return lines[start:end]


def first_failure(log: str) -> Failure | None:
    for line in nextest_output(log) or []:
        m = RESULT.match(line)
        if not m:
            continue
        test = " ".join(m["rest"].split()[-2:])
        if FAILED.match(m["status"]):
            return Failure(m["status"], test, True)
        if ENDED.match(m["status"]):
            return Failure(m["status"], test, False)
    return None


def needs_recheck(failure: Failure | None) -> bool:
    return failure is None or not failure.by_test


def describe(failure: Failure | None) -> str:
    if failure is None:
        return "no failing test in the log"
    how = "a failing test" if failure.by_test else "nextest ended the test"
    return f"{failure.status} {failure.test} ({how})"


def rust_regex_literal(text: str) -> str:
    """`text` as a pattern of the regex crate matching it literally (regex::escape)."""
    return "".join("\\" + c if c in REGEX_META else c for c in text)


def mutant_file(name: str) -> str:
    """The source file of a mutant name (`<file>:<line>:<column>: <what>`)."""
    return name.split(":", 1)[0]


def recheck_argv(names: list[str], output: Path, cargo_args: list[str]) -> list[str]:
    """Selects the mutants by name, and by their files too: cargo-mutants 27.1.0 tests every
    struct-field deletion mutant whatever --re says (upstream cargo-mutants#632), so without
    the file limit a recheck of one mutant tested all of the packages' (CI run 37466176804)."""
    argv = ["cargo", "mutants"]
    for name in names:
        argv += ["--re", f"^{rust_regex_literal(name)}$"]
    for path in sorted({mutant_file(name) for name in names}):
        argv += ["--file", path]
    return argv + ["--output", str(output), *cargo_args]


def read_outcomes(out_dir: Path) -> dict | None:
    path = out_dir / "outcomes.json"
    if not path.is_file():
        return None
    return json.loads(path.read_text(encoding="utf-8"))


def mutants(outcomes: dict) -> dict[str, dict]:
    """Mutant name -> its outcome (the baseline, if any, is left out)."""
    return {o["scenario"]["Mutant"]["name"]: o for o in outcomes.get("outcomes", [])
            if isinstance(o.get("scenario"), dict) and "Mutant" in o["scenario"]}


def failure_of(out_dir: Path, outcome: dict) -> Failure | None:
    path = out_dir / outcome["log_path"]
    return first_failure(path.read_text(encoding="utf-8", errors="replace")) if path.is_file() else None


def refused(cargo_args: list[str]) -> list[str]:
    own = cargo_args[:cargo_args.index("--")] if "--" in cargo_args else cargo_args
    return [a for a in own if a in REFUSED or a.startswith(tuple(f"{r}=" for r in REFUSED))]


def main(argv: list[str] | None = None,
         run: Callable[..., subprocess.CompletedProcess] = subprocess.run) -> int:
    argv = sys.argv[1:] if argv is None else argv
    split = argv.index("--") if "--" in argv else len(argv)
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, default=Path("mutants.out"), help="the main run's mutants.out")
    args = parser.parse_args(argv[:split])
    cargo_args = argv[split + 1:]
    if bad := refused(cargo_args):
        print(f"::error::timeout recheck: options that select other mutants or outputs: {' '.join(bad)}")
        return 1

    outcomes = read_outcomes(args.out)
    if outcomes is None:
        print(f"{args.out}/outcomes.json does not exist: no mutants were tested, nothing to recheck")
        return 0
    caught = {n: o for n, o in mutants(outcomes).items() if o.get("summary") == "CaughtMutant"}
    retest = []
    for name, outcome in caught.items():
        failure = failure_of(args.out, outcome)
        if needs_recheck(failure):
            retest.append(name)
            print(f"caught by {describe(failure)}: {name} -> tested again")
        else:
            print(f"caught by {describe(failure)}: {name}")
    if not retest:
        print(f"timeout recheck: {len(caught)} caught mutant(s), each by a failing test")
        return 0

    output = args.out / "recheck"
    print(f"timeout recheck: {len(retest)} of {len(caught)} caught mutant(s) not caught by a failing test; "
          f"testing them again with nextest profile {RECHECK_PROFILE}", flush=True)
    code = run(recheck_argv(retest, output, cargo_args), env=dict(os.environ, NEXTEST_PROFILE=RECHECK_PROFILE),
               check=False).returncode
    if code not in (0, 2, 3):
        print(f"::error::timeout recheck: cargo mutants failed to run (exit {code})")
        return 1

    rechecked = mutants(read_outcomes(output / "mutants.out") or {})
    errors = missed = timeouts = 0
    selected_files = {mutant_file(name) for name in retest}
    for name in [n for n in rechecked if n not in retest]:
        if FIELD_DELETION.match(name) and mutant_file(name) in selected_files:
            print(f"recheck: not selected, tested anyway (cargo-mutants#632, a field deletion in a selected file): {name}")
            continue
        print(f"::error::recheck: tested a mutant it did not select: {name}")
        errors += 1
    for name in retest:
        outcome = rechecked.get(name)
        summary = outcome.get("summary") if outcome else None
        if outcome is None:
            print(f"::error::recheck: not tested again: {name}")
            errors += 1
        elif summary == "CaughtMutant":
            failure = failure_of(output / "mutants.out", outcome)
            if failure is None:
                print(f"::error::recheck: caught, but its log names no failing test: {name}")
                errors += 1
            else:
                hang = "" if failure.by_test else ", a hang"
                print(f"recheck: caught by {describe(failure)}{hang}: {name}")
        elif summary == "MissedMutant":
            print(f"::error::recheck: MISSED (its first catch was not a failing test): {name}")
            missed += 1
        elif summary == "Timeout":
            print(f"::error::recheck: TIMEOUT: {name}")
            timeouts += 1
        else:
            print(f"::error::recheck: {summary}: {name}")
            errors += 1
    if errors:
        return 1
    if timeouts:
        return 3
    return 2 if missed else 0


if __name__ == "__main__":
    sys.exit(main())
