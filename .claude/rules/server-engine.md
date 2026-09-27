---
paths:
  - "crates/iem-server/src/**"
  - "crates/iem-ui/src/**"
  - "crates/iem-core/src/{ws,types,band,backup}.rs"
---

# Server ↔ engine (S5): ids, domains, echoes

- **The server is the engine's one controller** (`engine/client.rs`, role `control`). It connects to the control pipe (Unix socket path / Windows pipe name `engine_pipe`, override `IEMMIXER_ENGINE_PIPE`) and the media pipe `<pipe>.media` with plain tokio sockets — never with `interprocess`'s tokio feature: it would unify into the engine's dependency closure (`scripts/check_engine_deps.py`).
- **Ids, not indices:** UI protocol v2 (`iem_core::ws`, `UI_PROTO = 2`) keys every channel by the engine id (inputs and heard mixes share one namespace); `State{mix, group}` names the page's mix and the stems group. The site's `[[members]]` / `[[inputs]]` tables only add names, categories and owners; an id the topology lacks is left out, never guessed (`site_view.rs`).
- **Domains (`view.rs`):** fader −60 dB = off (`DB_OFF` −150 in the engine); UI pan 0…1 ↔ engine −1…1 (`ui_pan` / `engine_pan`), converted only there. EQ values are the engine's (freq_hz, gain_db, bw_oct); no normalised values anywhere.
- **Read-modify-write waits for the mirror:** an edit that builds on the current value (EQ band, preset ramp step) uses `request_applied` — `request` returns at the reply, before the delta reached the mirror.
- **Echoes:** a change carries its WS session as `origin`; that session gets no updates for it (it applied the change optimistically) except the channel mutes a solo change shows (`mixer_ws::own_echo`), because the mask is the server's view. A change with `origin: None` (preset ramp, Mute All, restore) reaches every session.
- **Solo** is the engine's transient mask, shown as each channel's mute (`muted = level.muted || masked`). A mute click on a masked channel changes the level's own mute but stays silent until the solo ends (UI `MuteClick::Masked`). 10 s after a member's last socket closed, the server clears that mix's solo.
- **Talk lock (X6):** `TalkStart` → `TalkAcquired{talk_id}`; `/ws/talkback` binds only with the held id within 1 s; released on `TalkStop`, the holder's mixer socket closing, or 2 s without frames.
- **Tests run the real engine:** `engine/testkit.rs` (`EngineHarness`, `#[cfg(test)] #[cfg(unix)]`) runs `iem_engine::engine::run` on NullRt with the test site; `engine_live_tests.rs` drives it through `AppState`, `routes_live_tests.rs` through the HTTP routes (`routes::api_tests::{router, call, token}`), and modules test their own private handlers in a `#[cfg(unix)] mod live` inside `tests` (`mixer_ws::handle`, `listen_ws::start`/`stop`). Engine tests are Unix-only: the `windows` job compiles every test (clippy `--all-targets`), so they and their imports stay behind `cfg(unix)`. `iem-engine` is a dev-dependency only — the server binary never links the GPL engine.
