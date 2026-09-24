---
paths:
  - "crates/iem-server/src/auth.rs"
  - "crates/iem-server/src/login_guard.rs"
  - "crates/iem-server/src/pin_hash.rs"
  - "crates/iem-server/src/pin_store.rs"
  - "crates/iem-server/src/pepper.rs"
  - "crates/iem-server/src/pepper/**"
  - "crates/iem-server/src/secrets.rs"
  - "crates/iem-server/src/provision.rs"
  - "crates/iem-server/src/bin/server.rs"
  - "crates/iem-server/src/lib.rs"
  - "crates/iem-server/src/routes.rs"
  - "crates/iem-core/src/config.rs"
---

# Security baseline (program spec §5.3)

- **No compiled-in credentials.** No default PIN, no default JWT key. `jwt_secret` and `vapid_private` are generated on first run in `<config dir>/secrets/` (owner-only, created exclusively, never overwritten). The site config rejects secret and PIN keys (`deny_unknown_fields`; secret fields are `#[serde(skip)]`).
- **PINs** are argon2id PHC strings (m=19456 KiB, t=2, p=1) keyed with a 32-byte pepper (`Argon2::new_with_secret`). `pin_hashes.json` holds only `$argon2id$` values — anything else is a load error. The pepper is DPAPI-protected on Windows (`pepper.dpapi`), a plain owner-only test file elsewhere (Linux is test-only). An unreadable pepper is an error, never regenerated (that would void every PIN). Backups never contain PINs.
- **Start-up fails loud:** `start_server` builds its state with `AppState::try_new(…)?`, so a bad pepper or PIN store returns an error before any port is bound, and the tray exits when its server fails before it is ready. `AppState::new` (panicking) exists only for tests (`cfg(test)` / `test-helpers`).
- **No raw REAPER passthrough** (X10): `/api/reaper/*` is gone (404); never add a generic proxy to an engine or REAPER control surface.
- **Provisioning:** first engineer PIN via `iem-server pin set-engineer` (PIN on stdin, server stopped); members get PINs by engineer reset in the UI (F3) or `iem-server pin set-member <id>`.
- **Login protection** (`login_guard.rs`): admission before any hashing; failures only; separate LAN/tunnel origins; `CF-Connecting-IP` trusted only from a loopback peer (the tunnel ingress must target 127.0.0.1 — S6 checks); IPv6 clients keyed by their /64; admission does not reserve (in-flight attempts are bounded by the hashing gate); per (client, member): 3 free failures then 1, 2, 4 … s, forgotten after 15 quiet minutes; per client: 20 failures / 10 min → 60 s spacing; engineer budget per origin: > 30 failures / h → 5 s spacing for the whole origin (every failure may be an engineer-PIN guess); every delay ≤ 60 s, never a lockout; 429 + `Retry-After`; hashing gate 2 running + 8 queued, else 429; issued JWTs untouched. `LoginGuard::stats()` feeds the engineer page in S5.
- **Keep these tests:** limiter tables, forged header from a non-loopback peer, IPv6 keyed per /64, the intended LAN-origin spacing, in-flight attempts bounded by the gate, bounded memory, gate cancellation, plaintext store rejected, pepper never regenerated, start-up refuses a bad pepper or PIN store, the raw REAPER passthrough stays gone, the CLI never reads the PIN from argv.
- `pepper/dpapi.rs` is Windows-only: excluded from mutation testing and run by the `windows` CI job (`cargo test -p iem-server --lib pepper::`).
