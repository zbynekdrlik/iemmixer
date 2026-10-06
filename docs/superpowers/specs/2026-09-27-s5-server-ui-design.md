# S5 — Server and UI on the engine protocol: design note

**Ticket:** #8 (program #1). **Spec:** `2026-09-24-iemmixer-gen2-program.md` §2.1, §2.3, I1, I6, I9, §3.2 (F1–F32), §3.4 (X2–X7, X11, X15, Q2), §4.2 (band-activity alarm, "Back to REAPER"; the alarm removed by #38), §4.3, §5.3 (talk lock, handshake), P6, P9. **Model:** `2026-09-26-engine-model-rework-design.md` (#20). **Plan:** `docs/superpowers/plans/2026-09-27-s5-server-ui.md`. **Inputs:** the hand-offs from S0, S3, S4 and #20 on #8; the owner rulings on #1/#20 (the members' GUI defines the functionality; no DAW generality; no REAPER concept outside the importer).

## 1. Goal

`iem-server` stops talking to REAPER and becomes the engine's one controller: an `engine_client` over the control and media pipes, a mirror of the engine state behind the unchanged UI features, Opus in the server for Listen and Talkback, and the server-only logic (auth, permissions, Mute All, solo clean-up, presets/snapshots/customizations on band schema 3, backups, photos, push, SOS, tunnel watch, version). The UI moves from REAPER track indices to engine ids **without a visible change** for the band (P9) except the approved items (§8).

Done means: the existing Playwright suites plus new mixer suites run green in hosted CI against the real engine on NullRt with `config/test-site.toml`, zero console errors, `REAPER_ABSENT` deleted; server tests run against the real engine in-process (no mock of internal code); no REAPER code left in `iem-server` or `iem-ui`.

## 2. What goes

`proxy.rs` (REAPER HTTP, EXTSTATE, ReaScript flows), `poller.rs`, `backup_capture.rs` / `backup_restore.rs` (REAPER backup JSON), `audio_stream.rs` (OIEM UDP relay from the VST), the per-track REST controls and `/poll`, `DiscoveredMember` discovery, `reaper_url`, `dante_outputs`, the `member1` placeholder, the REAPER-era types in the UI protocol (`track_index`, ReaEQ norms, `gain_db_min/max`, stereo L/R pairs). The predecessor's data formats (presets/snapshots/customizations keyed by track number, backup JSON v1, ReaEQ bands) survive only in `iem_core::legacy`, used by the importer (`iem-rpp`, `iem-migrate`).

## 3. Site (server part of `site.toml`)

The engine reads `[engine]`; the server reads the rest (`deny_unknown_fields` stays):

```toml
engine_pipe = "iemmixer-engine"        # control pipe (Unix: socket path; Windows: pipe name); media = <pipe>.media
back_to_reaper = ["iemmode", "event"]  # argv of the engineer's switch; empty = no switch (S6 provides iemmode)
[[members]]                            # login tiles, URLs, JWT `sub` — the predecessor's ids (P9)
id = "member1"
name = "Member1"                       # display name, as today
mix = "member1"                        # the engine mix of this listener
[[inputs]]                             # display metadata for engine inputs
id = "mic1"
name = "MEMBER1 mic"                   # the channel label, as today
category = "mics"                      # mics | stems | tech (grouped inputs are always stems)
owner = "member1"                      # X7: may edit this input's EQ; its first owned input is the member's "own" channel
```

`IEMMIXER_ENGINE_PIPE` overrides `engine_pipe` (CI uses a socket path); `IEMMIXER_MODE` = `dev` (default) | `live` is set by the guard (S6). The `[activity]` table this note had (the band-activity alarm) was removed by the owner's decision of 2026-10-06 (#38); an older site's table still loads and is ignored. Validation: unique valid ids, owners are members, categories known. Ids the engine topology does not have are reported at connect and left out of the views (never guessed).

## 4. Engine client and mirror

- **Control pipe** (tokio Unix socket / Windows named pipe — not `interprocess`'s tokio feature, which would enter the engine's dependency closure; u32 LE length + JSON ≤ 1 MiB): `hello{proto: 1, role: control, client: "iem-server <build>"}` → `Hello`, `Topology`, `State`, replayed `Alarm`s. Requests carry an id and `origin` = the WS session that caused them (echo suppression toward that session only). `Reply{rev}` arrives before its `Delta{rev}`; `request_applied` waits until the mirror reached `rev` (≤ 1 s), so an EQ edit reads its predecessor. A `rev` gap sends `GetState`. `Superseded` or a dropped pipe → reconnect with backoff (0.25 → 2 s); the UI sees `ConnectionChanged{connected}` exactly as it saw REAPER's reachability.
- **Mirror** (pure): topology, `MixState`, `Transient` (solo, listen), `rev`; `apply(EngineMsg) → Vec<MirrorEvent>` (state replaced, changes applied, resync needed). The UI view model reads only the mirror; nothing is polled.
- **Media pipe** (`<pipe>.media`): 20-byte header + f32 LE. Streams 0 (engineer tap) and 1 (the member tap) → one Opus encoder each (48 kHz stereo, 20 ms, `RESTRICTED_LOWDELAY` = CELT-only, FEC off, X4), broadcast to `/ws/audio` listeners of that slot. Talkback: `/ws/talkback` Opus (mono, WebCodecs, unchanged wire format) → decoder → jitter buffer (40 ms pre-roll, Opus PLC for a missing frame, playout on a 20 ms clock) → stream 16; the engine keeps its 120 ms cap and 5 ms gate (X5).
- The engine does no authorisation; the server decides who may change which mix (§5), the engine caps values.

## 5. The UI protocol v2 (`iem_core::ws`, same `cmd`/`event` JSON tags)

- Channels are keyed by **id**: an input id or a heard mix id (inputs, groups and mixes share one namespace). `Channel{id, name, level_db, pan, muted, category, eq, own}`: `eq` = the viewer may open its EQ (engineer, or the input's owner; a heard mix's EQ is that mix's output EQ: engineer only, X7); `own` = the page member's first owned input (the "more me" channel). Stereo inputs are one strip (F5); the predecessor's L/R pair fields go.
- The page's mix: `State{…, mix, group}` name the member's mix (IEM VOL meter, limiter, EQ) and the stems strip's group. Meters: `{id → [l, r]}` for inputs (post-mute), mixes (post volume/mute) and the page's group strip under the group id; the engine's 30 Hz peaks are max-merged and sent every 100 ms.
- Commands keep their names with ids (`SetLevel{id, level_db}`, `SetMute`, `SetPan` (0…1), `SetSolo{soloed: [id]}`, `UpdateCustomization{pinned, hidden: [id]}`, `GetEqParams{target}`, `SetEqBand{target, band, param: freq_hz|gain_db|bw_oct|enabled, value}`, `Get/SetLimiter…` for the page's mix). The fader's −60 dB is off (`DB_OFF`); pan 0…1 ↔ engine −1…1.
- **Solo** (F6, X2) is the engine's transient mask; the server shows the mask as the channel's mute (`muted = level.muted || masked`), exactly as the predecessor's REAPER mutes looked, and restores nothing (the engine never touched the level mutes). **Solo clean-up:** 10 s after the member's last WS closed the server sends `SetSolo{sources: []}`.
- **Handshake (§5.3):** the WS URL carries `proto=2`; the first message is `Hello{proto, build, min_client_proto}`. Out of range → the UI reloads (≤ once per 60 s); no hello within 3 s → one reload; `/ws/{page}` without `proto` is closed with code 4001. The new service worker also reloads open tabs when it activates.
- **Talk (X6, §5.3):** `TalkStart` on the mixer WS (engineer) → `TalkAcquired{talk_id}` (128-bit hex); the talkback socket binds by presenting the held id at its upgrade (`/ws/talkback?token=…&talk=<id>`); a socket without the held id is refused (403), one that binds after the 1 s window is closed. The lock is released on `TalkStop`, the holder's mixer WS closing, or 2 s without talkback frames (which also ends a lock whose socket never came).

## 6. Pages, permissions and the server's own logic

- A page is a member (`/member3`) or, for the engineer only, a mix without a member (`/translator`, F29). A member reaches only its own page; the engineer any page. Every command acts on the page's mix; input EQ needs the owner or the engineer; another mix's EQ, input trim/mute/processing (F29) and limiter-stats reset of any mix need the engineer. `mix_view` = the page mix's `MixInfo.mixes` (the Mixes tab; the elevated member is whoever's mix hears other mixes).
- **Mute All** (F15): one engine `Batch` muting every level of the engineer's mix. **Presets** (F13) and **snapshots** (F14) live in the band schema-3 files (`presets/`, `snapshots/`, `customizations/<member>.json`): the server captures the page mix's levels and group faders from the mirror (Q2: input EQ of owned inputs as never-applied metadata), and applies them as a **50 ms ramp** of five `Batch` steps 10 ms apart (linear-amplitude interpolation, mutes at the first step when muting and at the last when unmuting). Daily auto-snapshot: the first change of the day to a member's mix snapshots the mirror **before** the delta. Archived entries (D8) are listed and loadable, never overwritten, renamed or deleted.
- **Backups** (F19) are `{format: "iemmixer-backup", version: 2, timestamp, rev, state: MixState, customizations}` at 13:00, 21:00 and on demand, kept 60 days. **Restore with preview** (F31): a diff of the current mirror and the backup in the UI's names (level, group, output, EQ, limiter, input, customization; before → after), then one `ImportState`.
- **F29 engineer console** (settings, engineer's own page): per input mute, trim (−24…+24 dB) and processing; every mix's limiter active-seconds with reset; a link to the translator page; the login-failure counters (`LoginGuard::stats()`).
- **No band activity (#38, owner decision 2026-10-06).** This note had a detector over the input meters in `dev` (peak > −50 dBFS counted per second; active when ≥ 120 s of the last 300 s) → `BandActivity{active, can_switch}` to engineer pages (a banner with "Späť na REAPER") and one Web Push to the engineer's devices when it turned on. Removed: other devices on the Dante network feed the card's inputs, so no level says the band plays, and whether an event runs is the owner's to say. **"Back to REAPER"** (§4.3): engineer PIN → `POST /api/mode/event` → the server spawns `back_to_reaper` (argv, no shell, not awaited) and answers 202. Its only button was in that banner and went with it.
- **`iem-server notify <title> <body>`** (§4.2 "notify mode"): sends one Web Push to every stored engineer and alarm subscription and exits.
- Everything else (PINs, login guard, JWT, photos, push, SOS, tunnel watch, version, client errors, diagnostics with today's JSON fields) is unchanged.

## 7. Proof

- **Server tests on the real engine:** `iem-server` tests start `iem_engine::engine::run` on a thread with NullRt and a temp socket (Unix; dev-dependency only, the server binary stays permissive) and drive the client: hello/topology/state, a UI command becomes an engine change and a UI update, `rev` gap resync, reconnect after an engine restart, `Superseded`, Mute All batch, preset ramp steps, solo mask and 10 s clean-up, restore through `ImportState`, listen frames through Opus and back through a decoder, talkback frames arriving on stream 16.
- **Pure units:** mirror, view mapping (every command, permission and dB/pan edge), meter merge, solo janitor, talk lock, ramps, diff, config validation.
- **Mock E2E (CI):** `iem-engine run --sine 1000` + the server; the old suites without `REAPER_ABSENT`, and new suites for F4–F8, F10–F16, F20, F23, F29, F31, no band-activity banner on a loud stage (#38), the handshake, the talk-id binding, zero console errors.

## 8. What the band sees

Nothing new. Approved visible changes: X11 limiter after the listen boost (engineer's phone), the band-activity banner and "Back to REAPER" button (both removed: #38), the F29 console and the translator page, restore preview (all engineer only), and a one-time reload of an open tab after an upgrade. Kept on purpose (P9): every label, tab, colour and layout, including the disconnect banner text.

## 9. Deviations

- A mute click on a solo-masked channel changes the level's own mute but the channel stays silent until the solo ends (the predecessor emulated solo with REAPER mutes, so such a click un-muted the channel inside the solo). X2 already made solo a transient mask.
- Presets/snapshots no longer carry output or group EQ (band schema 3, S4); input EQ is metadata (Q2 fix).
- A tab of the predecessor's UI that stays open across the first switch cannot be reloaded by the server (it never asks); it shows the reconnect banner until reopened. Tabs of iemmixer's UI reload themselves (§5).

## 10. Not in S5

The ASIO backend, the guard, `iemmode`, the interlock and the PC (S6); live HIL listen/talkback quality (S7); the real site's `[[members]]`/`[[inputs]]` tables and categories (ops repo, S6/S8 — hand-off).
