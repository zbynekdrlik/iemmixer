# S8: trials, cutover and rollback (design note)

**Ticket:** iemmixer#11. **Program:** `2026-09-24-iemmixer-gen2-program.md` §4.3, D4, R8, R9. **Status:** design, 2026-10-10.

## 1. What the owner asked for (2026-10-08/10)

- One question matters: do the in-ears sound the same as with REAPER, and does a band member feel iemmixer is transparent? The blind A/B with a member is the acceptance. The agent does not track rehearsal dates.
- No padding gates: an event lasts at most 2 h and the PC has short windows between events (the 72 h soak was dropped, #10). Evidence accrues from real use, never from waiting on a calendar.
- D4 stands: trials (rehearsals and a service) on the band's usual address, the engineer's and the band's agreement, then iemmixer becomes the boot default. The owner says when. (The rollback window was dropped on 2026-10-10, §3.3: going back is the event switch.)

## 2. Where the code is (mapped 2026-10-10)

Built by S6/S7: `live --build` with the main + green-HIL gate (`bundle::may_go_live`), trials (`--trial`, fresh import each entry via `pc.toml` `data_live`), `Pins {current, previous}`, the prod crash-loop branch (`crash::after_exit` → `PreviousPin`), the self-checked export to a new RPP (`iem-migrate export`), VAPID import, the "Back to REAPER" endpoint (`iemmode event`), switch timing and HIL/live runs on the PC.

Missing: a persisted cutover state (`SiteConf.prod` is always false, every boot resets to `event`), the cutover command, the rollback command and its drill, report-only shadow imports, and the pin promotes too early (on every entry, before any HIL result).

## 3. Design

### 3.1 One persisted lifecycle state

`GuardState.lifecycle: Trial | Prod { since, pin } | RollingBack` (persisted, additive, default `Trial`).
- `Trial` (today): every boot is `event` (G1); `live` only with `--trial`.
- `Prod`: the boot restores `live` on the pin; `SiteConf.prod` is true (the crash-loop branch becomes live); a dev entry is maintenance (§3.4).
- `RollingBack`: set first by rollback, cleared to `Trial` when it ends; a boot in it continues the rollback to `event`. (Removed in lane 5 with the rollback, §3.3: a state that holds it reads as `Trial`, alarmed.)
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

### 3.3 No rollback (the owner's decision, 2026-10-10)

**ROZHODNUTÉ on #11: no rollback machinery.** REAPER and the predecessor stay installed on the PC and in git; going back to REAPER after the cutover is the same event switch as today (the event plan: iemmixer stopped, REAPER and the predecessor app started), run by hand when needed. Program spec D4's rollback window is dropped. Lane 3 built a rollback (`iemmode rollback`/`iempc rollback`, a persisted record and `RollingBack`, an export of the band's data swapped into the REAPER project's place, the button rolling back in prod, the drill); lane 5 removed it, with the review's findings 2(b), 3 and 5 on it.
- **`iemmode event`:** the button (`iemmode event`, no flag) is the event plan in every lifecycle; in prod the lifecycle stays prod and the next boot goes live on the pin. "Ide event" (`--signal`, `iempc event`) keeps lane 3's prod behaviour: maintenance ends live on the pin (REAPER when the pin may not go live), a healthy live stays, and an engine that does not play or a PC already in event gets the event plan, still prod.
- **A state lane 3's guard saved** while rolling back reads as trial (every boot event) and its load alarms it.
- **What a lost prod leaves** (an older guard took over: `pin_changes` open, the predecessor's autostarts disabled) is put back by hand; the next cutover refuses until it is (§3.5a).
- `iempc switch-test` runs in trial and in prod: its plain event leg is the event plan.

### 3.4 Pin and maintenance (fixes the early promotion)

- An entry never promotes the pin. `Prod` keeps `pin`; a maintenance dev entry `dev --build SHA` runs that build; when it ends (`live` again), the pin becomes SHA only if its HIL is green, else the old pin returns.
- A crash loop in maintenance stops iemmixer and goes back to `live` on the pin; a crash loop in prod reverts to the previous pin and, if that loops too, stops and alarms naming the way back to REAPER (`iemmode event`, the PC staying prod).

### 3.5 Shadow imports without waiting

Every dev and live entry already imports the REAPER project. Add a report-only shadow import (no state written) at each `event → dev`/`event → live` entry: the project's state diffed against the engine's last live state and against `site.toml`, the counts and any difference appended to `<root>\shadow\history.jsonl`. `iempc shadow-report` summarises it for sign-off. No calendar gate: the evidence is whatever real entries happened before the owner's cutover message.

(Lane 4 as built; `.claude/rules/guard.md` and `.claude/rules/migration.md` hold the detail.)
- **The command:** `iem-migrate shadow --rpp --aliases --site --state-dir` (pure comparison `iem-migrate/src/shadow.rs`): one JSON object, the import's verdict (`writes`, `refuses_topology`, `refuses_fit`, `refuses_doubts`, `unmappable`), the counts, the topology against `site.toml`, the values the engine would drop or cap, and the state an import would write against the saved one as the engine would load it; each difference `{kind, id, field}`, never a value. It writes nothing: no state, no recovery, no `engine.lock`, no state directory created.
- **Where it runs:** `pc.toml`'s optional `shadow` key names the command (no key, no shadow); the guard plans the step `Shadow` right after `TuningEnter` of an entry from event (REAPER saved and quit, the engine's state not yet refreshed), before `Data`.
- **Prod (decided in the lane):** it runs there too, as a report. No entry refreshes the data in prod (lane 3), so the line is how far REAPER's project has drifted from what iemmixer serves, the drift a switch back to REAPER would meet; it never feeds anything back.
- **Never fails or delays:** a wait bounded by 5 s ("ide event" ends it at once; past the bound it is asked to stop, Ctrl-Break as every wait, never force-ended); any failure is one `error` line (a fixed code and why) and a log line, never an alarm or a step failure. Its time is part of the entry's silence and shows as the step `shadow` in the switch record.
- **The history format:** one JSON line per entry, `at` (Unix ms), `entry`, `bundle`, then the command's report or `error` and `why`; the guard is its one writer. `iempc shadow-report` prints counts and kinds only (clean entries, verdicts, errors by code, differences by `site|state.<kind>.<field>`).

### 3.5a The cross-lane review's fixes (lane 5)

The review of lanes 1–4 together found five gaps; each is fixed with a RED test first (`.claude/rules/guard.md` holds the detail).
- **"Ide event" never cancels a switch to live in prod.** A second owner's signal (an in-flight iempc command's own `iemmode event --signal`) routed while the first one's dev → live entry runs waits for it and counts live as done; the first one claims the view before its first step, so one routed earlier waits too. The button still pre-empts.
- **A prod lost to trial** (an older guard took over, an unreadable state) left `pin_changes` open and the autostarts disabled. (a) The cutover refuses what such a prod leaves (an export never restored, `pin_changes = true`), and the cutover task refuses a listed task already disabled, a Run value already absent or an export never restored before writing anything; `Enable-IemAutostarts` marks an export restored (`restored.json`), so an earlier cutover that was undone cleanly does not block the next. (Decided in the lane: "an earlier export exists" counts only when it was never restored; counted as written, any earlier export, a cleanly undone one too, would refuse every later cutover.) (c) `manifest.json` names `guard_lifecycle: 1`; `activate` in prod refuses a bundle without it, and `iempc activate --offline` (whose step is the bundle's own guard) refuses such a bundle unless the guard says trial. (b), the repair rollback, was built and then removed with the rollback (§3.3).
- **A boot after a power loss mid-cutover**: the lifecycle back to trial and `pin_changes` first (local), the start's event plan, then the elevated undos.
- **Findings 3 (the drill) and 5 (the rollback's project swap)** were fixed and then removed with the rollback (§3.3).

### 3.6 Decommissioning

When the owner retires the predecessor (no rollback window, §3.3); a runbook in the ops repo lists the predecessor's tasks, ports and files to retire. No code beyond what cutover already exports.

## 4. Not in S8

D5 (no loopback, decided). Any new feature (program §2 scope freeze). The rollback (dropped by the owner, §3.3).

## 5. Proof

Unit tests for `lifecycle` (every mode × lifecycle × request), the pin rules and the cutover step order with a fake PC; the PowerShell autostart export/disable/re-enable against a test task folder (windows job); on the PC: a switch test in `Prod` (event path from live; the drill went with the rollback, §3.3).

## 6. Lanes (one at a time)

1. `lifecycle` state, boot rule, gates, pin fix (~250 Rust).
2. `cutover` (guard + iempc + the autostart PowerShell) (~350).
3. `rollback` + button routing + the drill script (~350; removed in lane 5, §3.3).
4. Shadow imports + report (~150).
5. The cross-lane review's five fixes (§3.5a).
Then the owner's trials and cutover message (the drill went with the rollback, §3.3).
