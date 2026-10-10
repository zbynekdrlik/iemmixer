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

(Lane 2 as built, its review: steps 2 and 3 run the other way round, the logon trigger first, so at every moment the next boot starts the predecessor or the guard; the export lives in the elevated root, `<elevated root>\cutover\autostarts-<since>`, `<since>` being `Prod.since`; `.claude/rules/guard.md` holds its format.)
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

(Lane 3 as built; `.claude/rules/guard.md` holds the detail.)
- **Order:** stop, export, the export in the project's place, the event plan (REAPER), then the autostarts back, the guard's logon trigger off (only once the autostarts are back: lane 2's rule that the next boot always starts the predecessor or the guard), `pin_changes = false`, trial. The export runs after the engine stopped, so it reads the engine's last save; the elevated tasks run after REAPER is back, so the in-ears are silent only for the stops, the export, the swap and the event plan.
- **The export's path:** a new file in the REAPER project's own folder, `<stem>.rollback-<at>.<ext>`. Not the elevated root: the guard runs as the user and may not write there, and REAPER saves into the project it runs.
- **REAPER "on the export":** the export takes the project's path by renames, the original kept beside it as `<stem>.before-rollback-<at>.<ext>` (never overwritten). The site's StartREAPER task, the guard's save check, the trial's import and the predecessor's autostarts all name the project's path, so a REAPER started on a file elsewhere would split the band's data from all of them. REAPER that cannot open the export is quit, the original goes back and REAPER starts on it, with an alarm.
- **The button and "ide event":** plain `iemmode event` (the button's command, the server's route unchanged) is the rollback in prod; `iemmode event --signal` is the owner's "ide event" (`iempc event` sends it). After the cutover iemmixer serves the band at an event, so "ide event" in prod never rolls back: maintenance ends with live on the pin (REAPER if the pin may not go live), a healthy live stays, and an engine that does not play (or a PC already in event) gets the event plan, REAPER for this event, the PC still in prod.
- **Robustness:** a step left keeps `RollingBack` and the record; a guard restart (its stops and event plan first) or another `iemmode rollback` continues it. A healthy engine that does not stop keeps serving and a dead one may hold the card: either stops the rollback with no REAPER, as the event plan does.
- **The drill:** `scripts/iem-pc/iempc_drill.py`; the reboot is S1c's graceful one (`spike_window`/`tuning_window`), after which REAPER must come back by itself from the restored autostarts.

### 3.4 Pin and maintenance (fixes the early promotion)

- An entry never promotes the pin. `Prod` keeps `pin`; a maintenance dev entry `dev --build SHA` runs that build; when it ends (`live` again), the pin becomes SHA only if its HIL is green, else the old pin returns.
- A crash loop in maintenance stops iemmixer and goes back to `live` on the pin; a crash loop in prod reverts to the previous pin and, if that loops too, stops and alarms naming rollback.

### 3.5 Shadow imports without waiting

Every dev and live entry already imports the REAPER project. Add a report-only shadow import (no state written) at each `event → dev`/`event → live` entry: the project's state diffed against the engine's last live state and against `site.toml`, the counts and any difference appended to `<root>\shadow\history.jsonl`. `iempc shadow-report` summarises it for sign-off. No calendar gate: the evidence is whatever real entries happened before the owner's cutover message.

(Lane 4 as built; `.claude/rules/guard.md` and `.claude/rules/migration.md` hold the detail.)
- **The command:** `iem-migrate shadow --rpp --aliases --site --state-dir` (pure comparison `iem-migrate/src/shadow.rs`): one JSON object, the import's verdict (`writes`, `refuses_topology`, `refuses_fit`, `unmappable`), the counts, the topology against `site.toml`, the values the engine would drop or cap, and the state an import would write against the saved one as the engine would load it; each difference `{kind, id, field}`, never a value. It writes nothing: no state, no recovery, no `engine.lock`, no state directory created.
- **Where it runs:** `pc.toml`'s optional `shadow` key names the command (no key, no shadow); the guard plans the step `Shadow` right after `TuningEnter` of an entry from event (REAPER saved and quit, the engine's state not yet refreshed), before `Data`.
- **Prod (decided in the lane):** it runs there too, as a report. No entry refreshes the data in prod (lane 3), so the line is how far REAPER's project has drifted from what iemmixer serves, which is what a rollback's export would replace; it never feeds anything back.
- **Never fails or delays:** a wait bounded by 5 s ("ide event" ends it at once, past the bound it is left to end by itself, never ended by force); any failure is one `error` line (a fixed code and why) and a log line, never an alarm or a step failure. Its time is part of the entry's silence and shows as the step `shadow` in the switch record.
- **The history format:** one JSON line per entry, `at` (Unix ms), `entry`, `bundle`, then the command's report or `error` and `why`; the guard is its one writer. `iempc shadow-report` prints counts and kinds only (clean entries, verdicts, errors by code, differences by `site|state.<kind>.<field>`).

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
