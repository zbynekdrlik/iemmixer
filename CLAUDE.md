# iemmixer

An in-ear-monitor mixer for a band: a native Rust audio engine with its own Dante device, a server and a phone UI, on one Linux box (Gen 3). **Public repository** — site data never enters it (program spec P6).

- Design: `docs/superpowers/specs/2026-10-24-gen3-linux-dante-design.md`; the principles and invariants the code cites (P6, I7, I8 …) are in `docs/superpowers/specs/2026-09-24-iemmixer-gen2-program.md`. A new sub-project gets a design note and a plan in `docs/superpowers/` before code; a finished plan is removed.
- Tickets: this repository's issues. Predecessor issues are written `reaperiem#N`; `#N` always means this repository.

<!-- airuleset:tray=n/a a server on a headless Linux box, no desktop -->

## Playbook router

- Public-repo hygiene, denylist → `.claude/rules/public-repo-hygiene.md`
- Login protection, PIN hashing, pepper, secrets, provisioning → `.claude/rules/security-baseline.md`
- Playwright E2E (console guard, env PINs, login budgets) → `.claude/rules/e2e.md`
- CI Rust toolchain, `--locked`, mutation gate → `.claude/rules/ci-rust-toolchain.md`
- Leptos `view!` gotchas and disposal safety → `.claude/rules/leptos-view-macro.md`
- Engine core, RT contract at B = 32, site `[engine]` table, protocol, persistence, pipes, rtsan/bench → `.claude/rules/engine.md`
- Server ↔ engine: ids, pan/dB domains, echoes, solo, talk lock → `.claude/rules/server-engine.md`
- Cloudflare tunnel watchdog, LAN URL / public host → `.claude/rules/tunnel-watchdog.md`
- HTTPS certificate keeper (ACME DNS-01 via Cloudflare, `[acme]`, `cloudflare set-token`, sealed store) → `.claude/rules/https-acme.md`
- DSP kernels, EQ/pan/limiter parity, the frozen goldens and `iem-golden` → `.claude/rules/dsp-parity.md`
- Soak harness, verdict → `.claude/rules/soak.md`
- Live E2E on the band's public URL, live verdict → `.claude/rules/live.md`
- A box from zero (new hardware, OS, provisioning with the realtime kernel and isolated cores, the device config's timing, deploy, proving it) → `.claude/rules/gen3-new-box.md`
- The Gen 3 stack: our own Dante device and the engine on Linux (the engine's Dante backend, the units run as the service user `iemmixer`, `scripts/gen3/deploy.sh` into two slots with `--back`, `provision.sh`/`verify_box.py`, `iem-dante-dev health`, `iempc gen3-gate`, `sudo-lines.txt`, the event flag; real device and channel names only in the ops repo's device config) → `.claude/rules/gen3.md`

## Always-apply rules

**Owner event signals.** The owner's signals are the only gate: never ask whether an event is running. Nothing is ever force-killed. The box serves the band at every event, and a reboot serves the band. "ide event" → `python3 scripts/iem-pc/iempc.py event`: it writes `~/.config/iemmixer/EVENT-NOW`, which freezes dev-time work, and leaves the box alone. "event skončil" → remove the flag.

**Predecessor boundary.** Never push to `zbynekdrlik/reaperiem`, never change its code, config or deployment; read it only through `~/devel/reaperiem` at a pinned SHA.

**Site data (P6).** Never commit names, hosts, IPs, Dante channel numbers, real track names, PINs, keys or tokens. Real site values live only in the private `zbynekdrlik/iemmixer-ops` (credential strings not even there: only in `~/.config/iemmixer/` and the CI secret). Every clone installs the pre-push hook (gitleaks + private denylist + allowed commit identities); CI repeats all three.

**Dante.** Never change Dante subscriptions or any Dante device other than our own device on the box.

**Versions.** One version for the workspace: `[workspace.package].version` in `Cargo.toml` (`2.0.0-dev.N`). `scripts/check_version.py` enforces consistency, and dev > main on PRs.
