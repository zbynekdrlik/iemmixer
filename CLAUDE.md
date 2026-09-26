# iemmixer

Gen 2 of the band's in-ear-monitor mixer: a native Rust audio engine replacing REAPER. **Public repository** — site data never enters it (program spec P6).

- Program spec: `docs/superpowers/specs/2026-09-24-iemmixer-gen2-program.md` (approved 2026-09-24). Each sub-project gets a design note and a plan in `docs/superpowers/` before code.
- Tickets: this repository's issues (#1 program, one ticket per sub-project). Predecessor issues are written `reaperiem#N`; `#N` always means this repository.

## Playbook router

- Public-repo hygiene, denylist, scrubbing, imports from the predecessor → `.claude/rules/public-repo-hygiene.md`
- Login protection, PIN hashing, pepper, secrets, provisioning → `.claude/rules/security-baseline.md`
- Playwright E2E (console guard, env PINs, login budgets) → `.claude/rules/e2e.md`
- CI Rust toolchain, `--locked`, mutation gate → `.claude/rules/ci-rust-toolchain.md`
- Leptos `view!` gotchas and disposal safety → `.claude/rules/leptos-view-macro.md`
- Engine core, RT contract at B = 32, site `[engine]` table, protocol, persistence, pipes, rtsan/bench → `.claude/rules/engine.md`
- Pan domains + send_index (REAPER-era server code) → `.claude/rules/pan-and-send-domains.md`
- Cloudflare tunnel watchdog, LAN URL / public host → `.claude/rules/tunnel-watchdog.md`
- DSP kernels, EQ/pan/limiter parity, golden tolerances → `.claude/rules/dsp-parity.md`
- Golden renders on the IEM PC (generator, window driver, analysis) → `.claude/rules/golden-renders.md`

## Always-apply rules

**Owner event signals (D2).** "ide event" (an event is coming) → immediately stop everything iemmixer on the IEM PC, start REAPER and the predecessor app, verify the handover, and confirm back to the owner. "event skončil" (the event ended) → save and quit REAPER, stop the predecessor app gracefully, start iemmixer, continue development. Never switch on your own and never ask whether an event is running. A reboot always comes back in event mode. Nothing is ever force-killed.
**Until S1a's interim switch script (S6: `iemmode`)** follow the "Owner event signals — S0 interim runbook" in the private `iemmixer-ops` `CLAUDE.md` (on the dev box also `~/.config/iemmixer/event-runbook.md`, which exists before the ops repo does): PC access path, read-only checks of REAPER, the predecessor app and the handover, graceful starts only. Nothing of iemmixer runs on the PC yet, so "event skončil" needs no PC action: confirm to the owner that development continues.

**Predecessor boundary.** Never push to `zbynekdrlik/reaperiem`, never change its code, config or deployment; read it only through `~/devel/reaperiem` at a pinned SHA.

**Site data (P6).** Never commit names, hosts, IPs, Dante channel numbers, real track names, PINs, keys or tokens. Real site values live only in the private `zbynekdrlik/iemmixer-ops` (credential strings not even there: only in `~/.config/iemmixer/` and the CI secret). Every clone installs the pre-push hook (gitleaks + private denylist + allowed commit identities); CI repeats all three.

**Dante.** Never change Dante subscriptions or any Dante device other than the IEM PC's own card.

**Builds.** Tier 0: no local cargo compilation — push `dev` and verify in CI. Local `cargo fmt`, `cargo metadata`, `cargo tree`, `cargo update -p` are fine; `cargo deny` and `cargo mutants` run in CI only.

**Versions.** One version for the workspace: `[workspace.package].version` in `Cargo.toml` (`2.0.0-dev.N` until cutover). `scripts/check_version.py` enforces consistency, and dev > main on PRs.
