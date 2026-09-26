---
paths:
  - "crates/iem-rpp/**"
  - "scripts/golden/**"
  - "goldens/**"
---

# Golden renders (S1b, D7)

- REAPER runs only on the IEM PC, only in dev time (the owner's "event skončil" quoted in `golden_window.py new --signal`), one step at a time in the order of the S1b design note §4. "ide event" → `golden_window.py preempt` immediately, then the event runbook.
- The render instance never uses audio mode 3 (ASIO); every render is watched with `tasklist /m`; any holder stops the queue. Never end any process by force: dialogs are closed via MCP or `close-render`.
- Only the `golden-bundle-<sha>` artifact of a reviewed `dev` push may reach the PC; `check_bundle.py` and the PC stager repeat the hash and plug-in allowlist checks. A new plug-in needs all three allowlists changed together (`fx.rs`, `check_bundle.py`, `GoldenPc.psm1`).
- A window succeeds only with `verify-restore` identical. PC backups stay on the PC (they hold the predecessor's secrets); raw renders live in `~/.local/share/iemmixer/golden-raw/`, never in the repo or `/tmp`.
- `RENDER_CFG` / `RENDER_STEMS` values are only trusted after the `cal` family confirmed them; analysis refuses non-float renders and compares at 1e-9 only on 64-bit.
- Site values (paths, names, host, module) are only in `~/.config/iemmixer/golden.env`, `golden-trees.json` and the ops runbook.
- A window may keep the predecessor app running (`new --keep-app`, when its tray Exit is not reachable): `app-stopped` is skipped, bring-back never restarts it, and its trees are `PC_VOLATILE_TREES` (best-effort backup, reported, never restored). REAPER's trees stay strict.
- PowerShell: a function that `return ,$array`s is assigned (`$x = Get-GoldenAsioHolders …`), never wrapped in `@(…)` — the wrap nests the array and an empty holder list becomes a false ASIO alarm. Run `Test-GoldenPc.ps1` on real Windows PowerShell 5.1 (CI `windows` job) after any module change.
- The reviewed SHA of a downloaded bundle is the sidecar `<bundle>.source-sha`, never a file inside the bundle.
