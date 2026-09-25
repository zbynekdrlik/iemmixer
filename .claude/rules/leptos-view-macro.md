---
paths:
  - "crates/iem-ui/**/*.rs"
---

# Leptos `view!` macro gotchas (iem-ui)

## Never put a bare comparison inline in a `view!` attribute or `when=`

The `view!` macro tokenizes `>` / `>=` / `<` as tag boundaries. An inline comparison such as `<Show when=move || snapshots.get().len() >= 50>` does not fail to parse — it mis-tokenizes and surfaces as an unrelated `E0308 mismatched types` deep in the expansion (reaperiem#206, `snapshot_modal.rs`). Bind the predicate to a named closure first (`let at_limit = move || snapshots.get().len() >= MAX_SNAPSHOTS;`), then write `<Show when=at_limit>`. Same for `class:=`, `style:=` and any attribute expression with `<`, `>`, `>=`, `<=`.

## Signal writes after an await or in a JS callback use `try_*`

`scripts/check_disposal_safety.py` (the `integrity` CI job) rejects `.set()` / `.update()` inside `spawn_local` blocks and `Closure::wrap` callbacks — the component may already be disposed. Use `try_set` / `try_update`.
