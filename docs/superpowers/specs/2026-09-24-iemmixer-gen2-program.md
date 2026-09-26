# iemmixer Gen 2 — program specification

**Status:** APPROVED by the owner 2026-09-24 (D1–D4 as recommended, plus the switching amendment in §4.3), ticket iemmixer#1; supersedes the archived 22.6k-word draft where they conflict (§9).

**Predecessor:** `reaperiem` (private, frozen) serves every event until cutover. It is maintained separately and is out of scope here; this work never pushes to it and never changes its code, config or deployment.

**Placeholders** (real values only in the private ops repo): `<public-host>` (the band's hostname), `<build-box>` (Linux render machine), `MEMBER_1`…`MEMBER_9`, `ELEVATED_MEMBER` (= `MEMBER_1`), `ENGINEER`, `TRANSLATOR`.

**Terms:** *golden* = REAPER reference render; *HIL* = automated test on the real PC; *cutover* = the day iemmixer permanently replaces REAPER; *dev time* = the time between the owner's "event skončil" and the next "ide event"; *Method B / C* = one live capture / live probes (§3.5).

**Owner reading path:** §0, §4, §8 (~2k words); the rest is for design review.

---

## 0. Zhrnutie pre vlastníka

- **Čo robíme.** iemmixer nahradí program REAPER vlastným zvukovým programom. Kapela má v ušiach počuť presne to isté ako dnes. Nové funkcie pridáme až po prechode (deň, keď iemmixer natrvalo nahradí REAPER).
- **Kým nie je hotový, všetko ide po starom:** každé zapnutie počítača spustí REAPER.
- **Tri režimy:** `event` = REAPER ako dnes; `dev` = vývoj a testy bez kapely; `live` = iemmixer posiela kapele zvuk do uší, najprv len na skúškach, po prechode natrvalo.
- **Počítač riadiš ty dvoma správami.** „Ide event“ → agent hneď vypne iemmixer, korektne spustí REAPER a reaperiem a potvrdí ti to. „Event skončil“ → agent uloží a korektne ukončí REAPER aj reaperiem, spustí iemmixer a pokračuje vo vývoji. Medzi eventmi patrí počítač vývoju. Sám nikdy neprepína. Po reštarte sa počítač vždy spustí s REAPERom a čaká na tvoje „event skončil“. Ak počas vývoja počítač počuje hrať kapelu, pošle ti upozornenie a zvukár má tlačidlo „Späť na REAPER“. (Schválené ako D2.)
- **Ochrana sluchu je povinná:** limiter na každom výstupe (vypnúť sa dá len vedome, s nápisom HEARING PROTECTION OFF), žiadne skoky hlasitosti, skúšobný tón je tichý a sám sa vypne.
- **Nič sa nevypína násilím** — to platí pre REAPER aj pre nový program. Pri chybe program najprv bezpečne uvoľní zvukovú kartu.
- **Verejný kód začína načisto:** bez histórie starého systému, bez mien, adries a hesiel.
- **Zhodu so starým systémom dokazujú testy:** nahrávky z REAPERa, rozdiel najviac 0,01 dB (nepočuteľné). Čo testy nezmerajú, porovnáme ušami na skúškach.
- **Núdzová brzda:** pred prechodom reštart počítača vždy vráti REAPER. Zvukár má v aplikácii tlačidlo „Späť na REAPER“, na skúškach aj 8 týždňov po prechode (D4). Prepnutie stíši slúchadlá asi na minútu, preto nie uprostred piesne.
- **Plán:** časti S0 až S8, spolu asi 22–25 týždňov práce a potom 8 týždňov poistky; začíname S0. Tempo určuje hlavne to, kedy je počítač voľný.
- **Starý systém sa nás netýka:** reaperiem má vlastnú údržbu; iemmixer do neho nič nepushuje a nič na ňom nemení.
- **Rozhodnuté 24. 9. 2026** (podrobne v §8): D1–D4 podľa odporúčania; navyše prepnutie korektne uloží a ukončí REAPER aj reaperiem a pri návrate ich znova spustí.
  - **D1** licencia: kód voľný (MIT/Apache), limiter a tým aj zvukový engine pod GPL.
  - **D2** zdieľanie počítača: okná otváraš ty správou (vyššie).
  - **D3** zvláštnosti REAPERa: mix presne ako dnes, 4 zvláštnosti opraviť.
  - **D4** prechod: 2 skúšky a 1 ostrá akcia na dočasnej adrese, potom súhlas zvukára a kapely, hlavná adresa a 8 týždňov možnosť návratu.
  - D5 príde neskôr, pri príslušnej časti. D7 rozhodnuté: referenčné rendery robí REAPER na iem PC (záloha → render na kópii → obnova).
- **Pre kapelu sa nič nemení:** rovnaká adresa, rovnaké PINy, rovnaká appka v telefóne. Pri skúškach a pri prechode nemusia nič riešiť.

---

## 1. Goal, scope, non-goals, principles

**Goal.** Replace REAPER and everything around it (ReaScripts, JSFX limiter, two VST3 plugins, HTTP polling) with a native Rust engine giving the band exactly today's in-ear mix. Both share one PC whose card takes one ASIO client, so the owner signals every event and development uses the PC in between.

**In scope:** engine, guard (`iemmode` CLI), server, web UI, tray, importer/exporter, mode switching, public CI plus a private ops repo for HIL, parity harness, cutover, rollback, decommissioning.

**Non-goals:**
- Nothing new before cutover: own-mic EQ, LUFS, channel reordering, hearing test, OSC trim sync, several talkers, absolute output ceiling, several member listen taps, Linux host, recording, auto-updater.
- No Dante network change (owner rule; the only candidate exception is D5).
- No REAPER features; RPP serves only the importer, exporter and golden generator.
- Deviations only per §3.4 and D3.

**MVP means limited scope, never lower quality.** Live-audio safety, hearing safety and public-repo security are non-negotiable.

- **P1** Parity is measured, not assumed: every uncertain REAPER behaviour gets a golden or probe before its code freezes.
- **P2** The audio process is small and isolated (I1, I9).
- **P3** Nothing cuts into an event (G1–G3); iemmixer holds the card only between the owner's "event skončil" and the next "ide event".
- **P4** Nothing is force-killed, neither REAPER nor the engine.
- **P5** Only hosted-CI builds from `dev`/`main` with verified provenance run on the PC; `live` only a reviewed `main` bundle (G8). Fork PRs never yield deployable artifacts; provenance does not prove code benign.
- **P6** Site data never enters the public repo: names, hosts, Dante channel numbers and real track names, PINs, keys. EQ and limiter values are not site data.
- **P7** The PC is self-contained at runtime: no dependency on any other machine we run.
- **P8** Framework first; reuse predecessor app code that does not depend on REAPER.
- **P9** **Band members notice nothing.** Same address, same PINs, same app on their phones, same controls and sound; switching between REAPER and iemmixer (trials included) and the cutover need no action from them. Internal fixes (e.g. how PINs are stored) never change what they type or see.

Numbers are tunable defaults unless they are parity requirements, tolerances (§3.5) or safety invariants (§2.5, §4.4).

---

## 2. Architecture decisions and hard invariants

### 2.1 Processes (all audio in the interactive console session)

| Process | Started by | Role | Restart effect |
|---|---|---|---|
| engine | guard | ASIO, DSP, mix state, taps, listen-path limiter; local pipes only; HIGH priority, QPC time | 1–3 s gap, 500 ms fade-in |
| guard (~1.2–1.5k LoC) | Interactive task, restart on failure, single-instance mutex; on demand before cutover, at logon after | `iemmode`, modes, interlock, band-activity alarm, handover checks, bundle install/pin/revert, crash loop, alarms; no listening sockets | none; reconnects |
| server (existing binary) | guard | HTTPS, auth, Opus, push, photos, backups, tunnel health | none on audio |
| tray (Tauri) | guard, iemmixer modes only | status, Open Mixer, Copy URL, alarms | none |
| ops runner (ops repo only) | guard, `dev` only | HIL, topology deploy | — |

- **Launch:** outside callers start the guard or REAPER via `schtasks /Run` of an Interactive task; children use job breakaway. Tasks: no time limit (`PT0S`; the 72 h default silently kills), `IgnoreNew`, no idle or battery stop.
- **Engine:** no power throttling; the driver's callback thread is never re-prioritised.

### 2.2 Crates

`iem-core` (WASM-safe), `iem-engine-proto`, `iem-dsp`, `iem-limiter-mga`, `iem-audio-io` (ASIO, `Offline`, paced `NullRt`), `iem-engine`, `iem-guard`, `iem-rpp`, `iem-server` (~60 % reused), `iem-ui` and `iem-tray` (reused).

**Licensing:** MIT OR Apache-2.0, except `iem-limiter-mga` (GPL-3.0-or-later, D1(a)); the engine links it, so its binary is GPL with licence text and source link. Server, UI, guard and tray are separate processes and stay permissive.

**ASIO:** azo 0.2.1, vendored and pinned behind the backend trait. Fallbacks: patch the fork, our own IASIO host, then `asio-sys`.

### 2.3 IPC and trust

- **Pipes:** control (JSON), media (20 ms 48 kHz frames), guard. Hardening: reject remote clients, a current-user DACL, the first-instance flag.
- **No pipe tokens** (any same-user process could read them): the engine caps every field from any sender. Test signal and fault injection are per-mode launch flags; a mode change restarts the engine.
- **Protocol N and N−1:** a reverted engine works with a newer server.

### 2.4 State, persistence, crash model

- **Owners:** engine (topology, `MixState`, limiter counters), server (PINs, presets, snapshots, photos, push, backups), guard (mode, bundles, pin, alarms), ops repo (`site.toml`).
- **Saves:** atomic, checksummed, 1 s after the last change, ≤ 5 s after the first. Kept: `current.json`, 20 generations, `baseline.json` (at each import and `live` entry); F19 backups give time points.
- **Schema changes stay additive-only until the rollback window closes.** Load chain: current → generations → baseline → all outputs silent plus alarm.
- **Topology authority:** before cutover, the REAPER project; an import differing from `site.toml` refuses `live` and emits a diff for an owner-merged ops PR.
- **Crash model:** panics unwind (`catch_unwind`); a fault zeroes outputs, the STA thread stops the driver, the engine emits `DriverReleased` and exits, and the guard respawns it (1→10 s backoff). An SEH filter waits ≤ 1 s for release, else parks the thread. At end-session the engine stops the driver first and the guard stops respawning. No `abort`, no C libraries, preallocated memory.
- **Parked engine:** the guard alarms; an owner reboot recovers, though the engine may block it or Windows may hard-kill it (S1a tests both). Last resort: a power cycle.
- **Fault injection:** one `dev`-only flag; automation injects panics only. `seh_ctl` and the hard-kill test (at most once, owner-approved, owner at the PC) are manual S1a steps.

### 2.5 Hard invariants

- **I1** The engine opens no socket, links no codec, parses no browser data.
- **I2** **Lowest stable latency (owner requirement 2026-09-26: in-ear system, target 32 samples ≈ 0.33 ms per buffer, as REAPER originally ran).** The buffer is the card driver's preferred size (one setting shared by REAPER and iemmixer); S1a measures 32/48/64 under load and the lowest size with 0 missed periods becomes the site setting for both, with owner-visible numbers. The engine never changes the rate or the buffer at runtime and refuses any rate but 96 kHz.
- **I3** One ASIO host at a time: the engine refuses while `reaper.exe` exists; the guard never starts REAPER while an engine exists.
- **I4** The graph is compiled once per run from `site.toml` and validated (acyclic, unique TX, channels in map, one send per pair); topology change = controlled engine restart.
- **I5** f64, plain summing, no bus pan law, zero added latency, no delay-compensation emulation.
- **I6** The engine is the single writer; clients get revisioned `State`/`Delta`, and any gap forces a resync.
- **I7** RT contract: lock-free rings, atomics and fixed messages only; ≤ 512 commands per block; no allocation, lock, syscall or log; CI checks with `assert_no_alloc` and rtsan.
- **I8** Nothing is force-killed: the guard stops the engine only with `Shutdown`; no `TerminateProcess`, `taskkill`, `Stop-Process` or `shutdown /f` anywhere (CI scans).
- **I9** A server restart, crash or deploy never touches audio.
- **I10** Edits lost: none on a graceful restart, ≤ 5 s on a crash.

---

## 3. Parity

### 3.1 The system reproduced

- **Card:** Yamaha AIC128-D, ASIO only, 96 kHz, buffer today B = 64 (667 µs; originally 32), target the lowest stable B (I2), treated as single-client.
- **32 RX, 24 inputs.** Direct: `MIC_1`…`MIC_10`, `HAND_1`…`HAND_3`, `ENG_MIC` (mono), `KEYS`, `IEMONLY`, `CONTENT` (stereo). Stems group: `CLICK`, `GUIDE` (mono), `DRUMS`, `BASS`, `INST`, `OTHER`, `BGVS` (stereo).
- **23 TX:** 9 member buses and `ENGINEER` (stereo, with EQ, limiter, fader and mute), `TRANSLATOR` (mono), master; plus 10 stems buses without TX.
- **268 sends:**
  - 170 from the 17 direct inputs to the 10 output buses (mode 3, A6);
  - 70 from the 7 stems-group inputs to the 10 stems buses (mode 3);
  - `HAND_1`→`TRANSLATOR` (mode 3);
  - 10, one from each stems bus to its output bus (mode 0);
  - 17 bus-to-bus (mode 0, muted by default): 8 into `ELEVATED_MEMBER`, 9 into `ENGINEER`.
- **Processing:** 44 five-band EQs, 24 trims, 10 limiters.
- **Roles:** `ELEVATED_MEMBER` views and controls the other member mixes. `ENGINEER` has talkback, listen, Mute All, backups and admin.

### 3.2 Features

Proof codes: **M** mock E2E, **L** live E2E/HIL, **S** server tests, **E** engine/DSP tests, **O** oracle, **G** goldens.

- **F1** Login grid with photo or initial; auto-redirect to last member (M)
- **F2** PIN numpad (4 digits, as today), 7-day JWT, engineer PIN from any member login, cross-member guard (S,M)
- **F3** Own PIN change; engineer PIN reset; logout drops push subscription (S,M)
- **F4** Tabs Main/Mics/Stems/Tech/Mixes/Hidden, categories from topology (M)
- **F5** Send fader −60 (= −∞)…+12 dB, 0.2 dB grid, 150 ms hold, relative drag, double-tap 0 dB; pan (double-tap centre); mute; stereo = one strip (M,L)
- **F6** Solo: server-synced, transient, separate from mute (E,M)
- **F7** IEM VOL: bus fader ≤ +12 dB and mute; stems fader and mute (O,M)
- **F8** Per-member pin/hide (M)
- **F9** Peak meters with decay and hold, limiter GR, header meters (E,L)
- **F10** Header: version, LAN/WAN badge, connection dot, 3 s reconnect banner (M)
- **F11** EQ modal: 5 bands, response curve (the engine's), per-band on/off/reset, ±12 dB, 20 Hz–24 kHz, 0.01–4 oct (E,G,M)
- **F12** Limiter −6…0 dB, on/off with "HEARING PROTECTION OFF", active-seconds counter with reset (E,M)
- **F13** Presets: load, overwrite, delete, save-as, confirms, max 20, content per Q2 (S,M)
- **F14** History: daily auto-snapshot on first change, save, restore, pin/unpin, delete, prune at 50 (S,M)
- **F15** Engineer page: Mute All, Listen, Talk; member pages: Listen (M,L)
- **F16** Mixes tab for `ENGINEER` and `ELEVATED_MEMBER`; visibility from `mix_view` (O,M)
- **F17** Listen: Opus, jitter buffer, stats, 0–24 dB boost; own bus or one member (L)
- **F18** Talkback: push-to-talk, one-talker lock, "ENGINEER SPEAKING" (S,L)
- **F19** Backups at 13:00, 21:00 and manual; 60 days; engineer restore (S,L)
- **F20** SOS: persistent engineer alert, vibration, 10 s chime, system notification, re-sent on reconnect (M)
- **F21** Web Push (L)
- **F22** Photos: 128 px JPEG ≤ 256 KB, public view (M)
- **F23** Browser settings: double-tap toggle, listen boost (M)
- **F24** Tunnel health watchdog, banner, LAN hint (S)
- **F25** `/api/version` label; client crash reports (M)
- **F26** PWA: HTTPS, redirect, network-first `index.html` (M)
- **F27** Tray: Open Mixer, Copy URL, Exit (tray only) (S6 checklist)
- **F28** Diagnostics endpoints with today's JSON fields (S,L)
- **F29** **New engineer surface** for REAPER-GUI-only controls: input mute, trim, processing on/off, `TRANSLATOR` controls, master fader, limiter-stats reset (O,M,L)
- **F30** **Topology change** procedure (add, rename, re-route, add member) in `dev` after cutover; no UI (S6 test)
- **F31** Restore with preview as a diff of two states (S,L)
- **F32** Limiter active-seconds counted in the engine, persistence per Q4 (E)

### 3.3 Audio behaviour

- **A1** Audio only on the 23 site TX channels, zeros elsewhere; sole exception: the D5(b) loopback pair during a `dev` test signal.
- **A2** Mono inputs: L = R = x at unity, no input pan law (O, Method B).
- **A3** Input mute zeroes every tap incl. pre-fader sends; on `ENG_MIC` it also silences talkback (G,O).
- **A4** With processing on: trim → EQ → (`ENG_MIC` only) `+= 0.379934·talkback` (≈ −8.4 dB), before the mute gate (E,G).
- **A5** Send gain `g = v·(1−m)·[1−max(p,0), 1+min(p,0)]`; taper confirmed by a golden pan sweep (G,O).
- **A6** Mode 3 reads pre-fader post-FX; mode 0 reads post-fader post-mute (O,G).
- **A7** Stems bus: Σ of the 7 stems-group sends → EQ → fader → mute → mode 0 into its output bus (O,G).
- **A8** Output bus: Σ → EQ → limiter → fader ≤ +12 dB → mute → safety stage (Q1) → clamp ±1.0 (G).
- **A9** Bus-to-bus taps read after mute, before safety stage and clamp, unclipped as in REAPER; graph acyclic (O).
- **A10** `TRANSLATOR`: `HAND_1` → fader → mute → mono downmix (law from a golden) → safety stage → clamp (G).
- **A11** Master: Σ post-fader outputs of inputs and stems buses (O).
- **A12** EQ: ReaEQ band types as an SVF fed RBJ parameters; shelf bandwidth and HPF gain from goldens (G).
- **A13** Limiter: faithful MGA port (no lookahead, instant attack, 50 ms release, 75 % link, hold sr/128, ceiling −6…0 dB) (E,G).

### 3.4 Deviations and quirks

**Always applied** (not subject to D3):
- **X1** Sanitiser on every input and after every node: non-finite or > 1e6 → count, reset node, silence its block, alarm.
- **X2** Solo is a transient engine mask that clears 10 s after the member's last connection or the controller link drops.
- **X3** Side-effect-free listen taps, at most two: `ENGINEER` (after its limiter, before fader and mute) plus one member (after mute, via a listen-path limiter); today allows one target at a time. A second distinct member gets `no_source`.
- **X4** Listen: anti-aliased 96→48 kHz in the engine; Opus CELT-only, FEC off, in the server.
- **X5** Talkback receive: 40 ms pre-roll, concealment, 120 ms cap, 5 ms fades.
- **X6** The talk socket binds to a talk id (UI change).
- **X7** Input EQ can be edited by its owner or the engineer, bus EQ by its member or the engineer.
- **X8–X10** Gen 2 security baseline (§5.3); not parity items.
- **X11** −1 dBFS AudioWorklet limiter after the browser boost (UI change).
- **X12** Diagnostics flags reset after 2 s without frames.
- **X13** Test signal: TTL, TX cap, `dev` only (§4.4).
- **X14** Limiter active-seconds = exact samples with GR < −1 dB.
- **X15** Own ramps (gain/pan 10 ms, mute 5 ms, EQ 20 ms, presets 50 ms, fade-in 500 ms); parity asserted in steady state.

**D3 quirks.** Until answered, each flag defaults to the (recommended) fix. The mix math is the same under every answer.

- **Q1** Fader after the limiter can exceed 0 dBFS; the driver hard-clips. *Replicate:* clamp ±1.0. *Fix:* stereo-linked 0 dBFS safety limiter before the clamp; fader stays after the bus limiter.
- **Q2** Preset/snapshot load rewrites shared input EQ. *Replicate:* only for inputs the user may edit (X7). *Fix:* stop applying input EQ; keep it as metadata.
- **Q3** EQ/trim on `BASS`/`INST` stems inert. *Replicate:* bypassed; EQ still offered. *Fix:* shown "bypassed"; engineer can enable (F29).
- **Q4** Limiter counter resets at REAPER start. *Replicate:* reset at engine cold start. *Fix:* persist until reset.

**Not reproduced:** delay compensation from fake plugin latency reports, plus three REAPER-session artifacts (S2 note). **Removed:** unused routes, `/poll`, the MCP server, ReaScripts, JSFX, VST3, legacy pages, the UDP audio loopback, the nightly git backup (P6), the batch `Reset`.

**Migration:** the importer only reads files (the project saved at switch time, the newest backup taken while REAPER was connected, legacy data re-keyed per era) and fails loudly on unmappable names. It imports the predecessor's LAN HTTPS certificate into the band directory, so LAN phones keep trusting it. VAPID keys and the JWT secret move with the band data, so nobody logs in again or re-subscribes to push (P9). **PINs:** imported with their current values (stored hashed); a later import never overwrites an iemmixer-set PIN.

### 3.5 How parity is proven (path: proof → tolerance)

- **Linear mix:** impulse oracle over `test-site.toml`; goldens → 1e-9; ≤ 0.01 dB, residual < −120 dBFS.
- **EQ:** SVF vs RBJ; goldens of the 44 site EQs (anonymised) and a synthetic matrix at 44.1/48/96 kHz → 1e-9; ≤ 0.01 dB, 20 Hz–20 kHz.
- **Limiter:** literal EEL2 translation; goldens on hot material → ≤ 1e-12; residual < −100 dBFS, GR ≤ 0.05 dB.
- **Block size:** 32/64/97/256, commands at identical sample indices → ≤ 1e-12.
- **Real time:** HIL, ≥ 8 h PC soak → 0 missed periods (≥ 2·B/sr); late ≤ 0.2 % vs S1a baseline p99.9.
- **CPU:** `process()` on the PC → p99.9 ≤ 25 % of period; alarm 50 %.
- **Talkback:** HIL tone at the `ENG_MIC` meter → −8.4 ± 0.3 dB.
- **Listen:** HIL test signal via Opus to the browser → ± 1 Hz, ± 0.5 dB, CELT-only.
- **Features:** hosted mock E2E, live E2E via HIL → green, zero console errors.
- **Data:** importer round trip → lossless; counts = `site.toml`.
- **Live-only** (mono level, buffer, delay compensation): Method B (owner-approved) and C → documented.
- **Remaining:** A/B in trials → recorded in D3.
- **Human acceptance:** 2 rehearsals + 1 service in trial (D4) → engineer and band sign-off.

**Goldens:** Method A renders with the IEM PC's own REAPER in dev time, from `iem-rpp`-generated project copies, offline without opening the card (D7): back up REAPER's and the predecessor's state first, restore and verify it afterwards; the original project is never touched; fallbacks: a Windows VM, a ReaPlugs harness, Method B plus analytic references, A/B. Stored compactly (≤ 20 MB); site EQ and limiter settings are anonymised, rendered once, compared in public CI.

---

## 4. Modes and safety

### 4.1 Three modes

| Mode | Runs | Data | Entered |
|---|---|---|---|
| `event` | REAPER, predecessor app | — | Every boot before cutover and after rollback; `iemmode event` |
| `dev` | engine, server, tray, runner; test signal, fault injection | test | `iemmode dev [--build SHA]` on the owner's "event skončil" |
| `live` | engine, server, tray; no runner, installs, HIL, test signal or faults | band | Before cutover `iemmode live --build SHA` on the owner's rehearsal message, fresh import each time; after cutover every boot |

- **Trial** = `live` before cutover: not persisted; a crash loop (3 abnormal exits in 10 min) goes to `event`.
- **Prod** = `live` after cutover, persisted. A crash loop reverts only the engine, to the previous pin's (protocol N−1, G8 exception); if there is none or it loops too, the guard stops respawning and alarms, naming rollback.
- **Maintenance** = `dev` after cutover. `dev` is never restored after a reboot; a crash loop in it alarms and stops iemmixer (`event` before cutover, `live` on the current pin after).
- **Bundles and pin:** a `live` bundle (engine, server, guard, tray) is one `main` SHA; on entry the guard switches itself first. Leaving `dev` restores the target mode's bundle, data and engine flags. The guard records {sha, branch, HIL result} per installed bundle (runner-reported) and refuses `live --build` unless it is `main` with green `hil/iem-pc`. After cutover only a maintenance session entered with `--build SHA` changes the pin: when it ends, if that SHA's HIL is green, it becomes the pin and the old one the previous pin; otherwise nothing changes.
- **Predecessor:** its app runs in `event` only. Switching away from `event` stops it gracefully after REAPER is saved and quit; switching back starts REAPER, then the app (§4.3). Its code, config and data are never modified.
- **Identity (P9):** whenever iemmixer runs it serves the band's usual address — `<public-host>` and LAN 80/443 — because the predecessor app is stopped outside `event`; the tunnel ingress and the phones' installed app stay unchanged. Its tunnel repair is active whenever it runs.

### 4.2 Event signals (D2, owner model approved 2026-09-24)

- **Owner messages drive the PC.** "ide event" (an event is coming) → the agent immediately runs `iemmode event` (graceful iemmixer stop, then REAPER and the predecessor app, handover checks) and confirms to the owner. "event skončil" → the agent runs `iemmode dev` (save and quit REAPER, graceful predecessor-app stop, interlock, iemmixer start) and development continues. Between the two messages the PC stays in `dev`, open-ended.
- **Rehearsals with iemmixer (trial, before cutover):** the owner's rehearsal message (plus the build) → `iemmode live --build SHA`; the owner's end message → `dev`.
- **The agent never switches on its own**, except the safety fallbacks below. It never asks whether an event is running; the owner says so.
- **Reboot** is always `event` (G1); after an unplanned reboot the PC stays in `event` until the owner's next "event skončil".
- **Activity interlock:** before `event`→`dev` (and `live`→`dev` after cutover) the guard samples stage inputs for 60 s; any peak above −50 dBFS refuses and alarms the owner. `--force` only on explicit owner instruction. Trials skip it: the band is there on purpose.
- **Band-activity alarm in `dev`:** sustained stage-input activity (peaks above −50 dBFS for ≥ 2 min within 5 min) alarms the owner and shows a banner with the "Back to REAPER" button on the engineer page; switching stays an owner (or engineer-button) decision.
- **Alarms:** a persistent file (shown by the tray and every `iemmode` call) plus Web Push to `alarm_recipients` (the owner) and `ENGINEER` subscriptions, from the server or, if none runs, the server binary run once in `notify` mode. Entering `dev` or `live` requires ≥ 1 alarm subscription; the owner-present S6 bootstrap registers the owner's.
- **HIL and soak jobs** start only in `dev` with a quiet interlock. An "ide event" cancels running jobs gracefully (through `iemmode`, never force) before the switch. Jobs change state only via `iemmode install|activate|test-signal|report`.

### 4.3 Switching, handover, cutover, rollback

- **Into iemmixer:** save REAPER (verify the file changed), quit it (verify gone within 30 s), then stop the predecessor app through its own graceful exit path (never force; verify gone and its data files closed); any failure aborts, restarting whatever was stopped. The graceful-exit path of the predecessor app is verified in S1a. If the import then refuses, the back-to-`event` path (with handover checks) runs. `live` imports band data; `dev` only reports a shadow import. The engine fades in after 10 s at 96 kHz with 0 missed periods.
- **Back to `event`:** engine `Shutdown`, then ≤ 10 s for `DriverReleased` (timeout: alarm, no REAPER start). Start REAPER, then the predecessor app, and within 90 s run the **handover checks**: engine gone, REAPER alive, no REAPER dialog, control plane up, predecessor app up and connected to REAPER, input peaks not all −∞. Unconfirmed audio is `UNCONFIRMED-AUDIO`, never success; failures alarm.
- **"Back to REAPER" button** (engineer PIN plus confirm; in `dev`, trials and the rollback window): `iemmode event` before cutover, the full rollback after. It silences every in-ear for about a minute (target ≤ 60 s, S7 measures it): never "the safe direction" while the band plays, and agents never request it during `live` without an owner instruction.
- **Cutover** (owner message, outside slots): final import, guard logon task, prod pin, old autostarts disabled with their values exported, ports and tunnel repair switched, VAPID imported, post-deploy checks.
- **Rollback:** export band data to a **new** RPP and self-check it (the original is never overwritten); stop iemmixer, disable the guard task, persist `event` (read back), start the predecessor, and start REAPER on the verified export (fallback: the original plus an alarm) with autostart pointed at it. iemmixer-only changes are not carried back. S8 drills it, ending with a reboot.

### 4.4 Safety invariants

- **G1** Every boot is `event` before cutover and after a rollback.
- **G2** **Procedural, not technical:** an agent *can* run `iemmode`; the band is protected by the owner-message rule ("ide event" / "event skončil"), the interlock, the band-activity alarm, G1 and owner notification.
- **G3** "ide event" is executed immediately and confirmed back to the owner; any failure alarms at once. A hung guard fails the switch loudly; the fallback is an owner reboot (= `event`).
- **G4** No force-kill of REAPER or the engine (P4, I8), OS shutdown included.
- **G5** HIL, installs, test signal and fault injection only in `dev`; the runner lives only there, so CI never decides about events.
- **G6** One ASIO host (I3); handover verified, never assumed.
- **G7** Boundary with the predecessor: iemmixer never changes its code, config or deployment; it only saves/quits/starts REAPER through the verified switching procedure (§4.3) and never starts it while the engine holds the card (I3).
- **G8** `live` runs only a reviewed `main` bundle from one SHA with green HIL (sole exception: the prod crash-loop engine revert, §4.1).
- **Hearing:** a limiter on every member and `ENGINEER` bus (disabling explicit and labelled) whose detector always runs; enabling or lowering the ceiling is instant, disabling or raising it ramps over 10 ms. Plus sanitiser, ramps, fade-in, zeros when faulted; 0 dBFS safety stage (Q1) then clamp; listen-path and browser limiters; solo auto-clear; driver resets after a 2 s stall, ≤ 1 per 5 min and ≤ 3 per process.
- **Test signal** (`dev` engine flag only): ≤ −20 dBFS, TTL ≤ 120 s, never persisted; while active, every TX reachable from the injection point is hard-capped at −20 dBFS.

---

## 5. Repo, CI and security baseline

### 5.1 Fresh import and split

- **Archive:** the old working folder goes whole to a read-only archive outside any repo, remotes removed, blocking hook kept.
- **Fill:** the new repo starts empty with a gitleaks and denylist pre-push hook; only `git archive` of a pinned predecessor SHA through a TAKE manifest fills it, scrubbed of site data (P6; it moves to `site.toml`) and anything the private denylist matches.
- **First push:** one provenance-noted import commit plus the security baseline, after D1. Commits use the noreply identity; the PC never commits.
- **Public repo:** code, `test-site.toml` (the real shape with synthetic values), public goldens.
- **Private ops repo:** `site.toml`, `eras.toml`, the denylist, `hil.yml`, topology deploy, site appendix.
- **Rules:** PR-only `main` with hosted checks, no force-push or deletion, SHA-pinned actions, read-only default token, fork-PR approval, push protection.
- **No self-hosted runner is ever registered on the public repo.**

### 5.2 CI, provenance, deploy

- **Hosted CI:** lint, tests (oracle, goldens, importer, UI, server on the real engine under NullRt), rt-safety, per-PR fuzzing plus a nightly shard, mock E2E with zero console errors, diff-scoped mutation ≤ 20 min, supply chain (cargo-deny, engine dependency allowlist, `--locked`; cargo-vet optional), secrets scan, integrity.
- **Attest:** the build job has no `id-token`; a separate attest job without checkout or build steps attests by digest on `dev`/`main` pushes.
- **Verify:** the ops runner's `hil.yml` runs `gh attestation verify` (this repo's CI, `dev`/`main`, hosted runner); the guard does not re-verify.
- **Dispatch:** the public dispatch token can trigger only `hil.yml` (the ops repo's sole `workflow_dispatch` workflow); topology deploys and maintenance run only on owner-merged pushes.
- **Branch heads only:** a HIL job whose SHA is no longer its branch head exits skipped; no scheduled catch-up; entering `dev` dispatches the newest `dev` and `main` heads once.
- **Required check:** `hil/iem-pc` on `main`, posted only by the ops GitHub App. HIL activates the whole bundle; green requires `Hello.engine_build`, `ServerHello.build` and the guard's reported version all = SHA.
- **Install:** `iemmixer-guard install <zip>` (versioned whole-bundle zip plus a `current` pointer). The owner-present bootstrap runs it by hand after a manual `gh attestation verify` of the zip.

### 5.3 Secrets, login, talk lock, handshake

- **Secrets never enter git:** JWT, VAPID and TLS secrets are generated or imported on the PC; PINs are argon2id hashes with a DPAPI pepper never backed up; test PINs and the denylist are CI or ops secrets.
- **Login protection:** backoff, never lockout, capped at 60 s. Per-client budgets count **failures only**, separately for LAN and tunnel (client IP trusted only from a loopback peer); engineer-PIN failures from any member login also hit an **engineer budget**. Limits apply before hashing (bounded concurrency, else 429). Issued JWTs are untouched; the edge rate-limits too.
- **Talk lock:** held by the mixer *session*; the talkback socket must bind to the 128-bit talk id within 1 s or is closed; released on stop, disconnect or 2 s of silence.
- **Handshake:** `ServerHello{proto, build, min_client_proto}`; out of range the UI reloads (≤ once per 60 s), as it does once without a hello in 3 s; the server closes `/ws` without a proto.

---

## 6. Sub-projects

**Each sub-project gets a design note and implementation plan before any code.**

| # | Deliverable | PC | Size | Needs |
|---|---|---|---|---|
| **S0** | Fresh import, LICENSE, repo settings, CI skeleton, security baseline incl. complete login limiter, ops repo skeleton | No | ~1k, 1.5 wk | D1 (push only) |
| S1a | Owner-present ASIO spike (interim switch script): driver, duplex, callback baseline, resets, clock role, fault injections incl. `seh_ctl`, OS restart, parked-engine reboot; optional hard-kill | Yes | ~0.5k, 1 wk | S0 |
| S1b | Golden spike on the IEM PC's REAPER (backup → offline render of project copies → restore + verify): generator, taper, shelf, HPF, downmix | Yes (dev time) | ~0.6k, 1.5 wk | S0 |
| S2 | DSP, limiter, golden harness, vectors | No | ~2.5k, 2 wk | S1b, D1 |
| S3 | Engine on Offline/NullRt: graph, RT, persistence, pipes, crash handling, test cap, oracle | No | ~3.5k, 3 wk | S2 |
| S4 | Importer, exporter, legacy data, certificate, PIN rule | No | ~0.9k, 1.5 wk | S3 |
| S5 | Server and UI on the engine: Opus, taps, browser limiter, talk bind, handshake, `notify`, engineer-page login-failure counters, band-activity banner, button, F29, F31, mock E2E | No | +2k/−3.6k, 0.8k UI, 4 wk | S3 |
| S6 | ASIO backend, guard (modes, event signals, pin, alarms), bootstrap with alarm subscription, runner, `hil.yml`, F30 | Yes | ~1.5k + ops, 2–3 wk | S1a, S3, D2 |
| S7 | Full HIL, live specs, ≥ 8 h PC soak, one manual 72 h NullRt soak, switch timing | Yes | ~1k + 3k TS, 2–3 wk | S5, S6 |
| S8 | Shadow imports (≥ 2 weeks), D5 loopback, rollback drill, trials, sign-off, cutover, rollback window, decommissioning | Yes | ~0.5k; 3–4 wk + 8 wk | all; D3–D5 |

**Totals:** ~15k LoC Rust (0.8k of it UI) plus 3k TypeScript; ~22–25 weeks for one stream, then the 8-week rollback window. PC availability sets the calendar.

**Critical path:** S1b → S2 → S3 → S6 (needs S1a) → S7 → S8; S0 runs in parallel and gates the first push. S4 and S5 follow S3; S1a waits only on the owner.

**Scope freeze** until S8 completes.

---

## 7. Risks

- **R1** azo fails on this driver → S1a first; fallbacks (§2.2).
- **R2** REAPER behaviours stay unknown or goldens are blocked → goldens before DSP freeze; D7 fallback chain; Methods B/C; A/B.
- **R3** Dropouts at the lowest buffer (target B = 32) → S1a buffer sweep; CPU gate; late-callback telemetry; soak; fall back only to the next stable size.
- **R4** Development or a HIL job cuts into an event (the owner's signal comes late, or an agent switches unasked) → owner-message rule; immediate "ide event" with job cancel; band-activity alarm; G1.
- **R5** Engine bug harms hearing → §4.4; fuzzing.
- **R6** Engine death or parking hangs the PC → graceful paths; S1a reboot test; power cycle last.
- **R7** Public-repo exposure or PIN guessing → §5.1; §5.3.
- **R8** Migration mis-keys values or loses state → per-era mapping; assertions; shadow imports; generations.
- **R9** Rollback or handover fails silently → self-checked export; handover checks; drill.
- **R10** Guard hangs → the switch fails loudly (G3); owner reboot.

---

## 8. Owner decisions (D1–D4 approved 2026-09-24, D7 2026-09-26; D5 pending)

D1–D4: the **bold** option was approved. D5 waits for S8; **bold** = recommended. (Former D6 and D8 are settled by P9 and the agent: PINs stay 4 digits; a renamed member's old history is imported archived and read-only.)

- **D1 Licence** (blocks the first push): **(a) MIT OR Apache-2.0 with a GPL limiter crate (engine binary GPL).** (b) All GPL. (c) Permissive with a clean-room limiter: behavioural parity only, weak legal footing.
- **D2 Sharing the PC — approved (owner model, 2026-09-24):** **the owner signals every event by message — "ide event" → `event`, "event skončil" → `dev`; the PC belongs to development in between; reboot = `event`; interlock and band-activity alarm kept; G2 procedural.** (Superseded alternatives: 12 h owner-granted windows, signed grants, calendar-autonomous, unrestricted, owner-present only.)
- **D3 REAPER quirks Q1–Q4** (blocks S8): **(a) exact mix math; fix all four (§3.4).** (b) Replicate any chosen item.
- **D4 Cutover** (blocks S8): **(a) all gates green, 2 trial rehearsals and 1 service on the band's usual address, engineer and band sign-off, then iemmixer becomes the boot default; 8-week rollback window.** (b) 1 rehearsal, 12 weeks. (c) 4 rehearsals and 2 services, 6 weeks. The band's address never changes; trial mixes are discarded.
- **D5 Dante self-loopback** (S8): (a) none. **(b) The owner subscribes a spare TX pair to a spare RX pair on the same card (the A1 exception).** (c) The same on a band pair.
- **D7 Golden renders — decided 2026-09-26:** REAPER is never installed on a dev box; renders run on the IEM PC's REAPER in dev time, with a full backup of REAPER's and the predecessor's state before and a verified restore after.

---

## 9. Detailed design notes

Mechanisms, constants, protocols, runbooks and all UNVERIFIED lists live in the per-sub-project design notes, seeded from the archived draft (kept in the private ops repo).

**Cut, not deferred:** grants, trust store, calendar, lock file, leases (incl. the "no other mixer session" check), pipe tokens, spool CLI, network-logon deny ACE, integrity label, PID check, guard-side Sigstore, known-good ≥ 1 h registry, hourly/daily buckets, per-major state directories, `SetProcessShutdownParameters` ordering, second fault flag and `owner_present`, 4 listen taps, NSIS, catch-up workflow, private golden runner and `golden/site` check, nightly 4×5 h fuzz, 72 h build-box soak as a gate, mandatory cargo-vet, 426 exemptions, five modes.
