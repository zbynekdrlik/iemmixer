#!/usr/bin/env python3
"""Unit tests for `scripts/check_disposal_safety.py` (context-free rule).

Run: python3 scripts/test_check_disposal_safety.py

The tests work on synthetic source snippets written to temporary files,
so they do not touch the real iem-ui tree. Each test creates a file,
runs `scan_file`, and asserts on the violations list.

The rule under test is context-free: any `.set()` / `.update()` /
`.set_untracked()` / `.update_untracked()` call whose receiver matches
`set_\\w+` is a violation, regardless of whether it is inside a
spawn_local, a Closure::wrap, an event handler, or a plain top-level
function. Helper functions called from inside a danger zone can still
panic with "reactive value has already been disposed", so the only
safe rule is "never write a Leptos signal unconditionally anywhere in
the file". The safe alternatives are the `try_` prefixed forms.
"""

from __future__ import annotations

import pathlib
import sys
import tempfile
import traceback

SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

# Import the module under test. This relies on check_disposal_safety.py
# being importable — it is when run as a script by file path.
import check_disposal_safety as cds  # noqa: E402


def scan_src(src: str) -> list[tuple[int, str]]:
    """Write `src` to a temp .rs file and run scan_file on it."""
    with tempfile.NamedTemporaryFile("w", suffix=".rs", delete=False) as f:
        f.write(src)
        path = pathlib.Path(f.name)
    try:
        return cds.scan_file(path)
    finally:
        path.unlink(missing_ok=True)


# ---------------------------------------------------------------------------
# Test helpers
# ---------------------------------------------------------------------------

_failures: list[tuple[str, str]] = []
_passed = 0


def check(name: str, cond: bool, detail: str = "") -> None:
    global _passed
    if cond:
        _passed += 1
        print(f"  ok   {name}")
    else:
        _failures.append((name, detail))
        print(f"  FAIL {name}{(' — ' + detail) if detail else ''}")


def assert_violation_at(name: str, src: str, expected_line: int) -> None:
    vs = scan_src(src)
    lines = [ln for ln, _ in vs]
    check(name, expected_line in lines, f"expected line {expected_line} in {lines}")


def assert_no_violations(name: str, src: str) -> None:
    vs = scan_src(src)
    check(name, not vs, f"unexpected violations: {vs}")


# ---------------------------------------------------------------------------
# True positives — must be flagged (context-free)
# ---------------------------------------------------------------------------


def test_plain_set_at_top_level_fn_is_flagged() -> None:
    src = """
fn helper() {
    set_foo.set(1);
}
"""
    assert_violation_at("plain set at top-level fn is flagged", src, 3)


def test_plain_update_at_top_level_fn_is_flagged() -> None:
    src = """
fn helper() {
    set_foo.update(|v| *v = 1);
}
"""
    assert_violation_at("plain update at top-level fn is flagged", src, 3)


def test_plain_set_untracked_is_flagged() -> None:
    src = """
fn helper() {
    set_foo.set_untracked(1);
}
"""
    assert_violation_at("plain set_untracked at top-level fn is flagged", src, 3)


def test_plain_update_untracked_is_flagged() -> None:
    src = """
fn helper() {
    set_foo.update_untracked(|v| *v = 1);
}
"""
    assert_violation_at("plain update_untracked at top-level fn is flagged", src, 3)


def test_plain_set_inside_spawn_local_is_flagged() -> None:
    src = """
fn f() {
    spawn_local(async move {
        set_foo.set(1);
    });
}
"""
    assert_violation_at("plain set inside spawn_local is flagged", src, 4)


def test_plain_set_inside_closure_wrap_is_flagged() -> None:
    src = """
fn f() {
    let cb = Closure::wrap(Box::new(move |_: Event| {
        set_state.set(Idle);
    }) as Box<dyn FnMut(_)>);
}
"""
    assert_violation_at("plain set inside Closure::wrap is flagged", src, 4)


def test_plain_set_inside_on_click_is_flagged() -> None:
    src = """
fn f() -> impl IntoView {
    view! {
        <button on:click=move |_| {
            set_foo.set(1);
        }>
            "click"
        </button>
    }
}
"""
    assert_violation_at("plain set inside on:click handler is flagged", src, 5)


def test_plain_set_inside_callback_new_is_flagged() -> None:
    src = """
fn f() {
    let cb = Callback::new(move |_: ()| {
        set_foo.set(false);
    });
}
"""
    assert_violation_at("plain set inside Callback::new is flagged", src, 4)


def test_rustfmt_split_multiline_is_flagged() -> None:
    src = """
fn f() {
    set_alert_data
        .set(Some(x));
}
"""
    # The violation is reported at the WRITER line (first half), so fixers
    # land on the variable name.
    assert_violation_at("rustfmt-split multi-line is flagged", src, 3)


# ---------------------------------------------------------------------------
# True negatives — must NOT be flagged
# ---------------------------------------------------------------------------


def test_try_set_is_ok() -> None:
    src = """
fn f() {
    let _ = set_foo.try_set(1);
}
"""
    assert_no_violations("try_set is OK", src)


def test_try_update_is_ok() -> None:
    src = """
fn f() {
    let _ = set_foo.try_update(|v| *v = 1);
}
"""
    assert_no_violations("try_update is OK", src)


def test_try_set_untracked_is_ok() -> None:
    src = """
fn f() {
    let _ = set_foo.try_set_untracked(1);
}
"""
    assert_no_violations("try_set_untracked is OK", src)


def test_web_sys_setters_are_not_flagged() -> None:
    src = """
fn f() {
    window.set_interval_with_callback(cb, 1000);
    opts.set_body(&value);
    socket.set_onclose(Some(closure));
    headers.set("content-type", "application/json");
    element.set_onclick(handler);
}
"""
    assert_no_violations(
        "web_sys setters (set_interval, set_body, set_onclose, headers.set, set_onclick) are not flagged",
        src,
    )


def test_disposal_safe_escape_hatch_respected() -> None:
    src = """
fn f() {
    set_foo.set(1); // disposal-safe: only runs during mount
}
"""
    assert_no_violations("// disposal-safe: escape hatch respected", src)


def test_line_comment_set_is_not_flagged() -> None:
    src = """
fn f() {
    // set_foo.set(1) — example in a comment
    let _ = set_foo.try_set(1);
}
"""
    assert_no_violations("signal write inside a // comment is not flagged", src)


def test_string_literal_set_is_not_flagged() -> None:
    src = '''
fn f() {
    let s = "set_foo.set(1)";
    let _ = set_foo.try_set(1);
}
'''
    assert_no_violations("signal write inside a string literal is not flagged", src)


# ---------------------------------------------------------------------------
# Second-pass: locally-bound RwSignal names must be flagged
# ---------------------------------------------------------------------------


def test_rwsignal_bound_local_name_is_flagged() -> None:
    src = """
fn f() {
    let local_value = RwSignal::new(0.0);
    local_value.set(1.0);
}
"""
    assert_violation_at("plain set on RwSignal-bound local is flagged", src, 4)


def test_rwsignal_bound_update_is_flagged() -> None:
    src = """
fn f() {
    let curve_trigger = RwSignal::new(0u32);
    curve_trigger.update(|n| *n += 1);
}
"""
    assert_violation_at("plain update on RwSignal-bound local is flagged", src, 4)


def test_rwsignal_bound_try_set_is_ok() -> None:
    src = """
fn f() {
    let local_value = RwSignal::new(0.0);
    let _ = local_value.try_set(1.0);
}
"""
    assert_no_violations("try_set on RwSignal-bound local is OK", src)


def test_rwsignal_bound_inside_spawn_local_is_flagged() -> None:
    src = """
fn f() {
    let local_state_created = RwSignal::new(false);
    spawn_local(async move {
        local_state_created.set(true);
    });
}
"""
    assert_violation_at(
        "plain set on RwSignal-bound local inside spawn_local is flagged",
        src,
        5,
    )


def test_non_rwsignal_set_on_plain_identifier_is_not_flagged() -> None:
    src = """
fn f() {
    let cell = std::cell::Cell::new(0.0);
    cell.set(1.0);
}
"""
    assert_no_violations("plain .set on non-RwSignal identifier is not flagged", src)


# ---------------------------------------------------------------------------
# Read-side rule: plain get_untracked / with_untracked / read_untracked
# inside danger zones (spawn_local, Closure::wrap, Effect::new) is a
# violation. `.get()` / `.with()` / `.read()` (tracked) are NOT targeted
# — those only appear in reactive render code, and the burn radius is
# too big. The `_untracked` variants are what async/closure code uses.
# ---------------------------------------------------------------------------


def test_plain_get_untracked_inside_spawn_local_is_flagged() -> None:
    src = """
fn f() {
    spawn_local(async move {
        let v = some_sig.get_untracked();
    });
}
"""
    assert_violation_at(
        "plain get_untracked inside spawn_local is flagged",
        src,
        4,
    )


def test_plain_with_untracked_inside_closure_wrap_is_flagged() -> None:
    src = """
fn f() {
    let cb = Closure::wrap(Box::new(move |_: Event| {
        some_sig.with_untracked(|v| do_something(v));
    }) as Box<dyn FnMut(_)>);
}
"""
    assert_violation_at(
        "plain with_untracked inside Closure::wrap is flagged",
        src,
        4,
    )


def test_plain_get_untracked_inside_effect_new_is_flagged() -> None:
    src = """
fn f() {
    Effect::new(move |_| {
        let v = some_sig.get_untracked();
    });
}
"""
    assert_violation_at(
        "plain get_untracked inside Effect::new is flagged",
        src,
        4,
    )


def test_try_get_untracked_inside_spawn_local_is_ok() -> None:
    src = """
fn f() {
    spawn_local(async move {
        if let Some(v) = some_sig.try_get_untracked() {
            do_something(v);
        }
    });
}
"""
    assert_no_violations(
        "try_get_untracked inside spawn_local is OK",
        src,
    )


def test_plain_get_untracked_outside_danger_zone_is_ok() -> None:
    src = """
fn f() {
    let v = some_sig.get_untracked();
}
"""
    assert_no_violations(
        "plain get_untracked in top-level fn is OK (synchronous read only)",
        src,
    )


def test_plain_get_untracked_inside_on_click_is_ok() -> None:
    src = """
fn f() {
    view! {
        <button on:click=move |_| {
            let v = some_sig.get_untracked();
        }>{"click"}</button>
    }
}
"""
    assert_no_violations(
        "plain get_untracked inside on:click is OK (synchronous, DOM-gated)",
        src,
    )


# ---------------------------------------------------------------------------
# Pinned invariant: std interior-mutability types do NOT match UNSAFE_READ.
#
# The `_untracked` suffix on get/with/read is Leptos-specific; std `Cell`,
# `RefCell`, thread-local `LocalKey`, `AtomicUsize`, and `OnceCell` do not
# expose methods by those names, so even a broad `\w+\.\bget_untracked\b`
# regex inside a danger zone cannot accidentally match them. These tests
# pin that invariant so a future scanner change can't silently start
# flagging Cell/RefCell accesses inside spawn_local or Closure::wrap.
# ---------------------------------------------------------------------------


def test_cell_get_inside_spawn_local_is_not_flagged() -> None:
    src = """
fn f() {
    let cell = std::cell::Cell::new(0.0);
    spawn_local(async move {
        let v = cell.get();
        do_something(v);
    });
}
"""
    assert_no_violations(
        "Cell::get() inside spawn_local is not flagged (Cell has no get_untracked)",
        src,
    )


def test_refcell_borrow_inside_closure_wrap_is_not_flagged() -> None:
    src = """
fn f() {
    let rc = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let rc2 = rc.clone();
    let cb = Closure::wrap(Box::new(move |_: Event| {
        let borrow = rc2.borrow();
        for item in borrow.iter() { use_item(item); }
    }) as Box<dyn FnMut(_)>);
}
"""
    assert_no_violations(
        "RefCell::borrow() inside Closure::wrap is not flagged",
        src,
    )


def test_thread_local_with_inside_spawn_local_is_not_flagged() -> None:
    src = """
thread_local! { static COUNTER: std::cell::Cell<u32> = std::cell::Cell::new(0); }

fn f() {
    spawn_local(async move {
        COUNTER.with(|c| c.set(c.get() + 1));
    });
}
"""
    assert_no_violations(
        "thread-local LocalKey::with() is not flagged (`with`, not `with_untracked`)",
        src,
    )


def test_rc_cell_get_inside_effect_is_not_flagged() -> None:
    src = """
fn f() {
    let state = std::rc::Rc::new(std::cell::Cell::new(0));
    let state_e = state.clone();
    Effect::new(move |_| {
        let v = state_e.get();
        do_something(v);
    });
}
"""
    assert_no_violations(
        "Rc<Cell>::get() inside Effect::new is not flagged",
        src,
    )


def test_atomic_load_inside_spawn_local_is_not_flagged() -> None:
    src = """
fn f() {
    let counter = std::sync::atomic::AtomicUsize::new(0);
    spawn_local(async move {
        let v = counter.load(std::sync::atomic::Ordering::Relaxed);
        do_something(v);
    });
}
"""
    assert_no_violations(
        "AtomicUsize::load() inside spawn_local is not flagged",
        src,
    )


# ---------------------------------------------------------------------------
# Runner
# ---------------------------------------------------------------------------


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    print(f"Running {len(tests)} tests for check_disposal_safety.py")
    for t in tests:
        try:
            t()
        except Exception as e:
            _failures.append((t.__name__, f"exception: {e}\n{traceback.format_exc()}"))
            print(f"  CRASH {t.__name__}: {e}")
    print()
    print(f"Passed: {_passed}")
    print(f"Failed: {len(_failures)}")
    if _failures:
        print()
        print("Failures:")
        for name, detail in _failures:
            print(f"  {name}: {detail}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
