---
paths:
  - "crates/iem-server/src/*.rs"
  - "crates/iem-core/src/{types,snapshot,preset}.rs"
---

# Pan domains + send_index on restore (REAPER-era server code, deleted in S5)

**Pan:** the poller converts REAPER→UI on read, so `Channel.pan`, cache, snapshots and presets ALL hold **0..1 (0.5 = center)**. REAPER `SET/…/SEND/M/PAN` expects **−1..1** → call `ui_pan_to_reaper` at the REAPER write only (WS `SetPan`, `restore_send_pan` in both REST restores). reaperiem#203: a raw write panned every mix half-right. The backup path is raw −1..1 end to end — leave it.

**send_index:** bulk writes (restore/replay) resolve per track via `resolve_send_index` (discovered `mix_send_index`; `Err` if missing — no fallback). Never hardcode 0 or the member's own index for mix channels (reaperiem#204).

**Elevated member:** the REAPER-era code hardcodes the ELEVATED_MEMBER id as the placeholder `member1`; S5 replaces it with `mix_view` from the site config.
