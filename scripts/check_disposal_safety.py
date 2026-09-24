#!/usr/bin/env python3
"""CI gate: forbid unsafe Leptos signal writes anywhere in iem-ui sources.

Motivation (#153): Leptos 0.7 panics with "tried to access a reactive value
that has already been disposed" when `.set()` / `.update()` (or their
untracked variants) is called on a signal whose owning scope has been
disposed. The original trigger was a `spawn_local` task or a
`Closure::wrap(...)` that outlived the component that owned the signal.

The first version of this scanner only flagged writes syntactically
inside a `spawn_local` or `Closure::wrap` block. That was too narrow:
helper functions called *from* those blocks can write a plain `.set()`,
and the scanner missed them because the write site was in the helper's
body, not inside a danger-zone closure. A production panic on the
Android PWA (url="/" after navigating back) was caused by exactly this
pattern — a helper called from a navigate-back handler wrote a signal
whose owning scope had already been disposed.

The fix is to make the rule CONTEXT-FREE. Any call of the form

    <receiver>.set(...)
    <receiver>.update(...)
    <receiver>.set_untracked(...)
    <receiver>.update_untracked(...)

where `<receiver>` matches the Leptos writer convention `set_\\w+` is a
violation, anywhere in the file. Helper functions are covered for free
because the regex fires on the write site regardless of the enclosing
scope.

This is safe because:

1. Web_sys setters like `window.set_interval_with_callback(...)`,
   `opts.set_body(...)`, `socket.set_onclose(...)`, `element.set_onclick(...)`,
   and `headers.set(...)` do NOT match — their receivers are not named
   `set_*`, so the `\\bset_\\w+\\s*\\.` prefix of the regex fails.

2. The `try_` prefixed forms (`try_set`, `try_update`, `try_set_untracked`,
   `try_update_untracked`) do NOT match — the `\\b` word boundary before
   `(?:set|update)` requires the preceding character to NOT be a word
   character, and the underscore in `try_set` IS a word character, so
   the boundary fails inside `try_set`.

3. Rustfmt-split writes like

       set_alert_data
           .set(Some(x));

   are handled by a secondary pass: when the current line ends with
   `set_\\w+` and the next line starts with `.set(` / `.update(`, we
   flag the current line so fixers land on the writer name.

Allowed alternatives:

    let _ = set_x.try_set(value);
    let _ = set_x.try_update(|v| *v = value);
    let _ = set_x.try_update(|v| *v += 1);
    let _ = set_x.try_set_untracked(value);
    let _ = set_x.try_update_untracked(|v| *v = value);

Escape hatch: append `// disposal-safe: <reason>` to the flagged line and
the check will ignore it. Use this only when you can prove the write
cannot race with dispose (e.g. the write happens during initial mount
before any async work can suspend).

Usage:
    python3 scripts/check_disposal_safety.py

Exits 0 if clean, 1 if any violation is found.
"""

from __future__ import annotations

import pathlib
import re
import sys

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
SCAN_ROOT = REPO_ROOT / "crates" / "iem-ui" / "src"

# Match identifiers that follow the Leptos writer convention (`set_*`)
# calling `.set(`, `.update(`, `.set_untracked(`, or `.update_untracked(`
# as a method.
#
# Why the word boundaries matter:
#   * The leading `\b` before `set_\w+` ensures we don't match inside
#     identifiers like `try_set_foo` — the character before `set` in
#     `try_set_foo` is `_`, which is a word character, so `\bset_` fails.
#   * The `\b` before `(?:set|update)` ensures we don't match
#     `.try_set(...)` / `.try_update(...)` — same reasoning: the
#     character before `set` in `try_set` is `_`.
#
# Why `\.` is escaped: we want a literal dot (method call), not "any
# character".
#
# Why web_sys methods are not matched:
#   `window.set_interval_with_callback(...)` → receiver is `window`, not
#   `set_*`, so `\bset_\w+\s*\.` fails on the receiver.
UNSAFE_WRITE = re.compile(
    r"\bset_\w+\s*\.\s*\b(?:set|update)(?:_untracked)?\b\s*\("
)

# Read-side danger-zone check.
#
# Leptos 0.7 `.get_untracked()` / `.with_untracked()` / `.read_untracked()`
# also panic on a disposed signal, with the exact same message as the
# write side ("tried to access a reactive value that has already been
# disposed"). Async tasks (`spawn_local`) and JS callbacks
# (`Closure::wrap`) can fire AFTER the component's scope has been
# disposed — e.g. a WebSocket onmessage handler delivering a frame that
# arrived during teardown — and any plain untracked read inside such a
# closure then panics.
#
# The tracked variants (`.get()`, `.with()`, `.read()`) are intentionally
# NOT targeted here: they are pervasive in reactive render code (view!
# macros, Memo bodies, on:click handlers) where the burn radius of a
# blanket rule is too big. The `_untracked` variants are exactly the
# ones used from async/closure contexts (tracked reads inside async
# would cause runaway reactivity), so they're a clean target.
#
# The safe alternatives are `try_get_untracked()`, `try_with_untracked()`,
# and `try_read_untracked()`, all of which return `Option<T>` and silently
# yield `None` on a disposed signal instead of panicking. The natural
# fix at a call site is:
#
#     let Some(v) = sig.try_get_untracked() else { return; };
#
# or, when a sensible default exists:
#
#     let v = sig.try_get_untracked().unwrap_or_default();
#
# The check is scoped to DANGER ZONES only — syntactic regions that can
# outlive the component's disposal:
#   * `spawn_local(async move { ... })`
#   * `Closure::wrap(Box::new(move |...| { ... }) ...)`
#   * `Effect::new(move |_| { ... })`
#
# Plain untracked reads outside a danger zone are considered safe: their
# execution context is scope-bound and runs synchronously during mount /
# render, so they cannot race with disposal.

DANGER_ZONE_START = re.compile(
    r"\bspawn_local\s*\(\s*async(?:\s+move)?\s*\{"
    r"|\bClosure::wrap\s*\(\s*Box::new\s*\(\s*move\s*\|[^|]*\|\s*\{"
    r"|\bEffect::new\s*\(\s*move\s*\|[^|]*\|\s*\{"
)

# Match plain `.get_untracked(` / `.with_untracked(` / `.read_untracked(`.
# The `\b` word boundary before each name prevents `try_get_untracked`
# etc. from matching, because the character before `get` inside
# `try_get_untracked` is `_` (a word char) and `\bget` therefore fails.
UNSAFE_READ = re.compile(
    r"\w+\s*\.\s*\b(?:get_untracked|with_untracked|read_untracked)\s*\("
)

# Second-pass detection: track locally-bound RwSignal names.
#
# Leptos `RwSignal::new(x)` is commonly bound to arbitrary identifiers
# that do NOT follow the `set_*` convention (e.g.
# `let local_value = RwSignal::new(0.0)`). The primary UNSAFE_WRITE regex
# misses these because its receiver anchor is `set_\w+`. To cover them,
# we do a first pass over each file to collect the set of identifiers
# bound to `RwSignal::new(...)`, then a second pass flags any
# `<name>.set(` / `<name>.update(` call on one of those tracked names.
#
# Two binding shapes are tracked:
#   1. `let <name> = RwSignal::new(...)` (and `let mut <name> = ...`)
#   2. Struct field initializers of the form `<field>: RwSignal::new(...)`
#      inside struct literals. This is conservative — the field name is
#      added to the tracked set even though the scanner cannot tell
#      which *receiver expression* owns that field. In practice, any
#      `foo.<field>.set(...)` call on a struct with such a field is a
#      disposal-race candidate and deserves `try_set` anyway.
#
# Tradeoff — known limitation of the field-tracking pass:
#
#   Field names are added to a file-wide set with no knowledge of which
#   struct they belong to. If the same file defines two struct literals
#   where one has a Leptos-signal field `foo: RwSignal::new(...)` and
#   the other has a non-signal field named `foo` (e.g. `foo: 42`), any
#   `bar.foo.set(...)` call in the file will be flagged regardless of
#   which `bar` is the receiver. iem-ui does not currently have such a
#   collision, but if one appears the correct fix is the
#   `// disposal-safe:` escape hatch on the specific line, not a wider
#   relaxation of the tracking. Keeping the false-positive bias over a
#   false-negative one is deliberate: a spurious try_set conversion is
#   harmless, a missed disposal race is a production panic.
RWSIGNAL_BINDING = re.compile(
    r"^\s*let\s+(?:mut\s+)?(\w+)\s*=\s*RwSignal::new\s*\("
)
RWSIGNAL_FIELD_BINDING = re.compile(
    r"^\s*(\w+)\s*:\s*RwSignal::new\s*\("
)

# Rustfmt-split writes:
#     set_alert_data
#         .set(Some(x));
#
# MULTILINE_WRITER_TAIL matches a line that ENDS with a writer name and
# no method call (so the method lives on the next line). MULTILINE_METHOD_HEAD
# matches a line that STARTS with `.set(` / `.update(` etc. When both fire
# on consecutive lines, we flag the writer line.
MULTILINE_WRITER_TAIL = re.compile(r"\bset_\w+\s*$")
MULTILINE_METHOD_HEAD = re.compile(
    r"^\.\s*\b(?:set|update)(?:_untracked)?\b\s*\("
)

ESCAPE_HATCH = "// disposal-safe:"


def _strip_strings(line: str) -> str:
    """Remove double-quoted string contents from a line.

    Very naive — does not handle escaped quotes, raw strings, or multi-line
    strings. Good enough for scanning single-line .rs source where string
    literals that would trigger the regex are rare. False negatives are
    acceptable; the goal is to avoid false positives on string content.
    """
    result = []
    in_string = False
    for ch in line:
        if ch == '"':
            in_string = not in_string
            result.append('"')
        elif in_string:
            result.append(" ")
        else:
            result.append(ch)
    return "".join(result)


def _collect_rwsignal_names(lines: list[str]) -> set[str]:
    """First pass: find every identifier in `lines` bound to `RwSignal::new(...)`.

    Scans both `let <name> = RwSignal::new(...)` and struct-field
    initializers `<field>: RwSignal::new(...)`. Comment-only lines are
    skipped, and string literals are stripped before matching so a
    literal like `"let x = RwSignal::new(0)"` does not pollute the set.

    Field names are added to a single file-wide set with no knowledge
    of which struct they belong to. If a file defines both a signal
    field `foo: RwSignal::new(...)` and an unrelated non-signal field
    named `foo` on a different struct, any `bar.foo.set(...)` in the
    file will be flagged regardless of which `bar` owns which `foo`.
    Use the `// disposal-safe:` escape hatch on the specific line if
    this ever produces a false positive; see the tracked-name tradeoff
    note near RWSIGNAL_FIELD_BINDING above.
    """
    names: set[str] = set()
    for line in lines:
        stripped = line.strip()
        if stripped.startswith("//"):
            continue
        code = _strip_strings(line)
        m = RWSIGNAL_BINDING.match(code)
        if m:
            names.add(m.group(1))
            continue
        mf = RWSIGNAL_FIELD_BINDING.match(code)
        if mf:
            names.add(mf.group(1))
    return names


def _build_tracked_name_regex(names: set[str]) -> re.Pattern[str] | None:
    r"""Build a single regex that flags `.set(` / `.update(` calls on
    any of the tracked identifiers.

    Returns None if `names` is empty (nothing to scan for).

    Example output for names={"local_value", "curve_trigger"}:
        \b(?:local_value|curve_trigger)\s*\.\s*\b(?:set|update)(?:_untracked)?\b\s*\(
    """
    if not names:
        return None
    alt = "|".join(sorted(re.escape(n) for n in names))
    return re.compile(
        r"\b(?:" + alt + r")\s*\.\s*\b(?:set|update)(?:_untracked)?\b\s*\("
    )


def scan_file(path: pathlib.Path) -> list[tuple[int, str]]:
    """Return (line_number, line_text) for every violation in `path`.

    The rule is context-free: any `set_*.set(...)`, `set_*.update(...)`,
    `set_*.set_untracked(...)`, or `set_*.update_untracked(...)` call
    is a violation anywhere in the file, unless the line has the
    `// disposal-safe: ...` escape hatch.

    In addition, a second-pass check flags `.set(` / `.update(` calls
    on any identifier locally bound to `RwSignal::new(...)` — this
    catches Leptos locals whose names don't follow the `set_*`
    convention (e.g. `let local_value = RwSignal::new(0.0)`).
    """
    violations: list[tuple[int, str]] = []
    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()

    # First pass: collect every identifier in this file bound to an
    # RwSignal. Second pass will flag writes on these names.
    tracked_names = _collect_rwsignal_names(lines)
    tracked_re = _build_tracked_name_regex(tracked_names)

    # Brace-depth stack for danger zones (read-side rule only).
    #
    # We maintain a stack of integers, each element being the brace depth
    # at which a danger zone started. When the running depth drops back
    # to that level, we pop the zone off the stack. "Inside a danger
    # zone" = stack is non-empty.
    #
    # String-literal stripping before brace counting keeps false positives
    # low on lines like `let s = "}";` — the `}` inside the string is
    # blanked out by `_strip_strings`.
    danger_stack: list[int] = []
    brace_depth = 0

    for lineno, line in enumerate(lines, start=1):
        stripped = line.strip()

        # Ignore pure line comments so a `// foo.set(bar)` example in a
        # doc comment doesn't trigger the gate. Trailing comments like
        # `set_foo.set(1); // note` are NOT skipped — the regex still
        # fires on the code before the comment. The `// disposal-safe:`
        # escape hatch is handled separately below.
        if stripped.startswith("//"):
            continue

        # Strip inline string literals so regexes don't fire on string
        # content like `let s = "set_foo.set(1)";`.
        stripped_code = _strip_strings(line)

        # Single-line write: `set_foo.set(1)` / `set_foo.update(...)` etc.
        matched_primary = False
        if UNSAFE_WRITE.search(stripped_code) and ESCAPE_HATCH not in line:
            violations.append((lineno, line.rstrip()))
            matched_primary = True

        # Rustfmt-split write: current line ends in `set_\w+`, next line
        # starts with `.set(` / `.update(` etc. Flag at the current line
        # so fixers land on the writer name.
        if MULTILINE_WRITER_TAIL.search(stripped_code) and lineno < len(lines):
            next_line = lines[lineno]  # lines is 0-indexed, lineno is 1-indexed
            next_code = _strip_strings(next_line).lstrip()
            if (
                MULTILINE_METHOD_HEAD.match(next_code)
                and ESCAPE_HATCH not in line
                and ESCAPE_HATCH not in next_line
            ):
                violations.append((lineno, line.rstrip()))

        # Second-pass: writes on locally-tracked RwSignal names. Only
        # fire if the primary regex didn't already match on this line,
        # to avoid double-reporting. Respects the `// disposal-safe:`
        # escape hatch just like the primary pass.
        if (
            not matched_primary
            and tracked_re is not None
            and tracked_re.search(stripped_code)
            and ESCAPE_HATCH not in line
        ):
            violations.append((lineno, line.rstrip()))

        # Read-side danger-zone check.
        #
        # Flag plain `.get_untracked(` / `.with_untracked(` /
        # `.read_untracked(` calls anywhere the danger_stack is
        # non-empty. The line-in-danger check must be done BEFORE the
        # brace-depth update so that the opening `{` of a zone counts
        # the current line as inside the zone — but that is handled
        # naturally below because danger zones are detected on their
        # opening line and pushed BEFORE the read scan (see order).

        # Detect danger-zone opener(s) on this line. A line can open
        # multiple zones (rare but possible with chained calls). Push a
        # marker for each opener at the current brace depth (pre-line).
        # We then count braces for this line and pop any zone whose
        # depth equals the new depth once closed.
        opener_count = len(DANGER_ZONE_START.findall(stripped_code))
        for _ in range(opener_count):
            # Each opener introduces exactly one `{` that also counts
            # toward the brace tally below. Mark the zone as starting
            # at current `brace_depth` (before we process this line's
            # braces) so the pop condition `brace_depth == marker` is
            # the same brace balance as before the opener's `{`.
            danger_stack.append(brace_depth)

        # Now scan reads on this line — danger_stack reflects whether
        # we are inside any open zone.
        in_danger = len(danger_stack) > 0
        if (
            in_danger
            and UNSAFE_READ.search(stripped_code)
            and ESCAPE_HATCH not in line
        ):
            violations.append((lineno, line.rstrip()))

        # Update brace_depth for this line. Count raw `{` and `}` in
        # the string-stripped code. Line-comment tails are NOT stripped
        # here, but that's acceptable — braces inside trailing
        # comments are rare in .rs source, and the worst-case effect
        # is a slightly off brace_depth, not a crash.
        brace_depth += stripped_code.count("{") - stripped_code.count("}")
        # Pop any zones whose start-depth is >= current brace_depth
        # (i.e. their surrounding `{` has been closed).
        while danger_stack and brace_depth <= danger_stack[-1]:
            danger_stack.pop()

    return violations


def main() -> int:
    if not SCAN_ROOT.is_dir():
        print(f"::error::Scan root does not exist: {SCAN_ROOT}", file=sys.stderr)
        return 2

    all_violations: list[tuple[pathlib.Path, int, str]] = []
    for rs_path in sorted(SCAN_ROOT.rglob("*.rs")):
        for lineno, line in scan_file(rs_path):
            rel = rs_path.relative_to(REPO_ROOT)
            all_violations.append((rel, lineno, line))

    if all_violations:
        print(
            "::error::Unsafe Leptos signal accesses (disposal race, #153):"
        )
        for rel, lineno, line in all_violations:
            print(f"  {rel}:{lineno}: {line.strip()}")
        print()
        print("Writes: replace `.set(x)` / `.update(|v| ...)` with")
        print("        `.try_set(x)` / `.try_update(|v| ...)`. Use")
        print("        `let _ =` to discard the Option<...> result.")
        print()
        print("Reads (inside spawn_local / Closure::wrap / Effect::new):")
        print("        replace `.get_untracked()` / `.with_untracked(...)` /")
        print("        `.read_untracked()` with `.try_get_untracked()` /")
        print("        `.try_with_untracked(...)` / `.try_read_untracked()`,")
        print("        then handle the `None` case (usually `return;` or a")
        print("        safe default).")
        print()
        print("Rules:")
        print("  * Writes: context-free — any `set_*.set(...)` anywhere is a")
        print("    violation, because helpers called from danger zones can")
        print("    race with scope disposal.")
        print("  * Reads: only flagged inside `spawn_local(async move {…})`,")
        print("    `Closure::wrap(Box::new(move |…| {…}))`, or")
        print("    `Effect::new(move |_| {…})` — these are the contexts that")
        print("    can outlive the owning scope.")
        print()
        print("Escape hatch: append `// disposal-safe: <reason>` to the line")
        print("if you can prove the access cannot race with scope disposal.")
        return 1

    print(
        f"OK: no unsafe signal writes or danger-zone reads found "
        f"({SCAN_ROOT.relative_to(REPO_ROOT)}/**/*.rs)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
