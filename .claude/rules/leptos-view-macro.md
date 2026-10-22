---
paths:
  - "crates/iem-ui/**/*.rs"
---

# Leptos `view!` macro gotchas (iem-ui)

## Never put a bare comparison inline in a `view!` attribute or `when=`

The `view!` macro tokenizes `>` / `>=` / `<` as tag boundaries. An inline comparison such as `<Show when=move || snapshots.get().len() >= 50>` does not fail to parse — it mis-tokenizes and surfaces as an unrelated `E0308 mismatched types` deep in the expansion (reaperiem#206, `snapshot_modal.rs`). Bind the predicate to a named closure first (`let at_limit = move || snapshots.get().len() >= MAX_SNAPSHOTS;`), then write `<Show when=at_limit>`. Same for `class:=`, `style:=` and any attribute expression with `<`, `>`, `>=`, `<=`.

## Signal writes after an await or in a JS callback use `try_*`

`scripts/check_disposal_safety.py` (the `integrity` CI job) rejects `.set()` / `.update()` inside `spawn_local` blocks and `Closure::wrap` callbacks — the component may already be disposed. Use `try_set` / `try_update`.

## Write `connected` before a view change that unmounts the mixer

Effects run after the turn, in the order they were woken. A write that unmounts a subtree (the notice's view takes the mixer's place, `<Show>` drops its children) followed in the same turn by a write the subtree reads (`connected`) wakes the subtree's render effects after the unmount was scheduled: they run on disposed locals (`is_fader_active`) and panic, "Tried to access a reactive value that has already been disposed", then "RefCell already borrowed" (#61 second review C1, CI run 39700306928). So write `connected` before the view (`pages/mixer/helpers.rs` `lineup_writes`, pinned by its test), and make no write that changes nothing: `try_set` notifies even when the value is the same. That order is what reactive_graph 0.1.8 does, not a documented guarantee, so the fader class closures also read their own `is_fader_active` with `try_get().unwrap_or(false)`: a closure that outlives its fader reads nothing instead of panicking. Re-check both whenever Leptos is upgraded; the 4003 E2E (`lineups.spec.ts`, `expectNotice`) catches a recurrence.
