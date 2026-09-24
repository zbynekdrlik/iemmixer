# iemmixer

A personal in-ear-monitor (IEM) mixer for a church band: every musician mixes their own in-ear feed from a phone. Gen 2 replaces the REAPER-based predecessor with a native Rust audio engine; until the cutover the predecessor serves every event.

**Status:** S0 (bootstrap). The imported web app — server, Leptos UI, Windows tray — builds and is tested in CI; the engine arrives in S1–S3. See the program spec in `docs/superpowers/specs/`.

## Layout

| Path | Contents |
|---|---|
| `crates/iem-core` | Shared types, site config, protocol (WASM-safe) |
| `crates/iem-server` | HTTPS/WebSocket server, login protection, push, backups (REAPER control plane until S5) |
| `crates/iem-ui` | Leptos progressive web app |
| `crates/iem-tray` | Windows tray shell (Tauri 2) |
| `e2e` | Playwright browser tests against a synthetic site |
| `config` | `iemmixer.example.toml` (every key documented), `test-site.toml` (synthetic site for CI) |
| `scripts` | CI and repository-security tooling |

## Building and testing

All compilation and tests run in GitHub Actions on hosted runners (`.github/workflows/ci.yml`): formatting and clippy, unit tests with a coverage floor, the WASM build, browser E2E with a clean console, diff-scoped mutation testing, supply-chain and secret scans, and a Windows build.

## Security

See `SECURITY.md`. No credentials are compiled in; PINs are argon2id hashes keyed with a DPAPI-protected pepper; logins are rate-limited per client and origin and never locked out.

## Provenance

Imported from the private predecessor repository `zbynekdrlik/reaperiem` at commit `03be5b97deafdc5d765516014c3f59370edd534b`, scrubbed of site data; its history stays private (`docs/provenance/import-manifest.txt`).

## License

Licensed under either of `LICENSE-APACHE` or `LICENSE-MIT` at your option. A future limiter crate (a port of a GPL-3.0-or-later JSFX limiter) will be GPL-3.0-or-later and make the engine binary GPL (program spec D1).
