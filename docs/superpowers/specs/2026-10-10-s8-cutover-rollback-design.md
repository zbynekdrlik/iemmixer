# S8: trials, cutover and rollback (design note)

**Ticket:** iemmixer#11. **Program:** `2026-09-24-iemmixer-gen2-program.md` §4.3, D4, R8, R9. **Status:** design, 2026-10-10.

## 1. What the owner asked for (2026-10-08/10)

- One question matters: do the in-ears sound the same as with REAPER, and does a band member feel iemmixer is transparent? The blind A/B with a member is the acceptance. The agent does not track rehearsal dates.
- No padding gates: an event lasts at most 2 h and the PC has short windows between events (the 72 h soak was dropped, #10). Evidence accrues from real use, never from waiting on a calendar.
- D4 stands: trials (rehearsals and a service) on the band's usual address, the engineer's and the band's agreement, then iemmixer becomes the boot default with a rollback window. The owner says when.

## 2. Where the code is (mapped 2026-10-10)

Built by S6/S7: `live --build` with the main + green-HIL gate (`bundle::may_go_live`), trials (`--trial`, fresh import each entry via `pc.toml` `data_live`), `Pins {current, previous}`, the prod crash-loop branch (`crash::after_exit` → `PreviousPin`), the self-checked export to a new RPP (`iem-migrate export`), VAPID import, the "Back to REAPER" endpoint (`iemmode event`), switch timing and HIL/live runs on the PC.

Missing: a persisted cutover state (`SiteConf.prod` is always false, every boot resets to `event`), the cutover command, the rollback command and its drill, report-only shadow imports, and the pin promotes too early (on every entry, before any HIL result).

## 3. Design

### 3.1 One persisted lifecycle state

`GuardState.lifecycle: Trial | Prod { since, pin } | RollingBack` (persisted, additive, default `Trial`).
- `Trial` (today): every boot is `event` (G1); `live` only with `--trial`.
- `Prod`: the boot restores `live` on the pin; `SiteConf.prod` is true (the crash-loop branch becomes live); a dev entry is maintenance (§3.4).
- `RollingBack`: set first by rollback, cleared to `Trial` when it ends; a boot in it continues the rollback to `event`.
A pure `lifecycle` module decides the boot mode, the entry gates and the crash rule (mutated, unit-tested), the daemon has call sites only.

### 3.2 `iemmode cutover --build <main sha>` (owner's message only)

Refuses unless: `Trial`, the build is the active main bundle with green `hil/iem-pc` and `live/iem-pc` recorded, the guard is in `live --trial` on that build or in dev. Steps, each read back, any failure unwinds to `Trial` + `event`:
1. final import of the saved REAPER project (as `data_live`), state saved as a generation;
2. export the predecessor's autostart values (task XML, run keys) to `<root>\cutover\autostarts-<ts>`, then disable them;
3. the guard task gets its logon trigger;
4. `pin_changes = true` allowed in the server's config;
5. lifecycle `Prod { since, pin = build }`, persisted and read back;
6. post-cutover checks: identity (LAN, public host), engine on the card at 32, a member page loads.
`iempc cutover --sha` wraps it (EVENT-NOW refusal, dev-box lock).

### 3.3 `iemmode rollback` and the drill

Refuses unless `Prod` (or `RollingBack`). Steps, logged, each read back:
1. lifecycle `RollingBack` persisted;
2. export band data to a **new** RPP (`iem-migrate export`, self-checked; the original is never overwritten);
3. stop iemmixer (the usual graceful stops), remove the guard's logon trigger;
4. re-enable the exported autostarts (from step 2 of cutover);
5. start the predecessor and REAPER on the verified export (fallback: the original project, plus an alarm);
6. handover checks (as the event path), lifecycle `Trial`, mode `event`, persisted and read back.
The "Back to REAPER" button calls `iemmode event` in `Trial` and `iemmode rollback` in `Prod` (same endpoint, the guard decides).
**Drill:** before the first cutover, on the PC in dev time, with a scratch lifecycle: cutover onto the current main, then rollback, then a reboot that must come back in `event` with REAPER on the export. Recorded on #11.

### 3.4 Pin and maintenance (fixes the early promotion)

- An entry never promotes the pin. `Prod` keeps `pin`; a maintenance dev entry `dev --build SHA` runs that build; when it ends (`live` again), the pin becomes SHA only if its HIL is green, else the old pin returns.
- A crash loop in maintenance stops iemmixer and goes back to `live` on the pin; a crash loop in prod reverts to the previous pin and, if that loops too, stops and alarms naming rollback.

### 3.5 Shadow imports without waiting

Every dev and live entry already imports the REAPER project. Add a report-only shadow import (no state written) at each `event → dev`/`event → live` entry: the project's state diffed against the engine's last live state and against `site.toml`, the counts and any difference appended to `<root>\shadow\history.jsonl`. `iempc shadow-report` summarises it for sign-off. No calendar gate: the evidence is whatever real entries happened before the owner's cutover message.

### 3.6 Decommissioning

After the rollback window the owner decides; a runbook in the ops repo lists the predecessor's tasks, ports and files to retire. No code beyond what cutover already exports.

## 4. Not in S8

D5 (no loopback, decided). Any new feature (program §2 scope freeze). The rollback window's length is the owner's (D4: 8 weeks).

## 5. Proof

Unit tests for `lifecycle` (every mode × lifecycle × request), the pin rules and the cutover/rollback step order with a fake PC; the PowerShell autostart export/disable/re-enable against a test task folder (windows job); on the PC: the drill (§3.3) and a switch test in `Prod` (event path from live).

## 6. Lanes (one at a time)

1. `lifecycle` state, boot rule, gates, pin fix (~250 Rust).
2. `cutover` (guard + iempc + the autostart PowerShell) (~350).
3. `rollback` + button routing + the drill script (~350).
4. Shadow imports + report (~150).
Then the drill on the PC, then the owner's trials and cutover message.
