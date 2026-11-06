# iemmixer

A personal in-ear-monitor (IEM) mixer for a church band: every musician mixes their own in-ear feed from a phone. One Linux box runs it all: a native Rust audio engine at a 32-sample block, its own Dante device on the band's network, and a server with the web app.

## Layout

| Path | Contents |
|---|---|
| `crates/iem-engine` | The audio engine: real-time mixing, its Dante backend, persistence, the control and media sockets |
| `crates/iem-engine-proto` | The protocol between the engine and the server (also WASM) |
| `crates/iem-audio-io` | The engine's audio types and its null backend |
| `crates/iem-dsp` | EQ, pan law, smoothers, meters and the sanitiser |
| `crates/iem-limiter-mga` | Port of the MGA JS limiter (GPL-3.0-or-later) |
| `crates/iem-server` | HTTP(S)/WebSocket server, login protection, push, backups, the band's lineups, the certificate keeper, the tunnel watchdog |
| `crates/iem-core` | Shared types, site config, the UI protocol (WASM-safe) |
| `crates/iem-ui` | Leptos progressive web app |
| `crates/iem-dante-dev` | The standalone Dante device binary and its health check |
| `crates/iem-soakclient` | Read-only load client for the soak runs, and its token mint |
| `crates/iem-golden` | Dev-only: the reader of `goldens/s1b` and the limiter's stimulus |
| `goldens/s1b` | The frozen DSP baseline |
| `e2e` | Playwright browser tests against a synthetic site, and the live specs |
| `fuzz` | Coverage-guided fuzzing of the engine's request parser and control core |
| `config` | `iemmixer.example.toml` (every key documented), `test-site.toml` and `test-roster.toml` (synthetic, for CI) |
| `scripts/gen3` | The box: provision, deploy into two slots, systemd units, installers |
| `scripts/iem-pc` | `iempc`, the dev box's commands: the event flag, deploy, new box, the gates and their verdicts |
| `scripts` | CI and repository-security tooling |

The site's real values live in a private repository.

## Building and testing

CI (`.github/workflows/ci.yml`) is the merge gate: formatting and clippy, unit tests with a coverage floor, randomised and coverage-guided fuzzing, the DSP golden tests, RealtimeSanitizer and a CPU benchmark over the engine's `process()`, the WASM build, browser E2E with a clean console, a soak client run, the attested box bundle, diff-scoped mutation testing, and supply-chain and secret scans. The box itself is proven by `iempc gen3-gate hil`, `live` and `soak`.

## Security

See `SECURITY.md`. No credentials are compiled in; PINs are argon2id hashes keyed with a pepper held as a systemd credential; logins are rate-limited per client and origin and never locked out.

## Provenance

The web app was imported from the private predecessor repository `zbynekdrlik/reaperiem` at commit `03be5b97deafdc5d765516014c3f59370edd534b`, scrubbed of site data; its history stays private.

## License

Licensed under either of `LICENSE-APACHE` or `LICENSE-MIT` at your option, except `iem-limiter-mga`, `iem-engine` and `iem-dante-dev` and what links them: GPL-3.0-or-later (`deny.toml` lists them).
