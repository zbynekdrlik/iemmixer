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
- Server ↔ engine: ids, pan/dB domains, echoes, solo, talk lock → `.claude/rules/server-engine.md`
- Cloudflare tunnel watchdog, LAN URL / public host → `.claude/rules/tunnel-watchdog.md`
- ASIO host, the S1a spike, the PC window driver → `.claude/rules/asio-spike.md`
- PC tuning window (S1c), tuning/measure modules, bundle sync, graceful reboot / I8 → `.claude/rules/pc-tuning.md`
- Importer, exporter, band data migration (`iem-migrate`, iem-rpp import/export) → `.claude/rules/migration.md`
- DSP kernels, EQ/pan/limiter parity, golden tolerances → `.claude/rules/dsp-parity.md`
- Gen 1 test parity manifest (`docs/parity/gen1-tests.tsv`, its CI check, cutover gate) → `.claude/rules/parity.md`
- Golden renders on the IEM PC (generator, window driver, analysis) → `.claude/rules/golden-renders.md`
- Guard, iemmode, PC install → `.claude/rules/guard.md`
- Soak harness, verdict, dispatch-soak → `.claude/rules/soak.md`
- Live E2E on the PC, live verdict, dispatch-live → `.claude/rules/live.md`
- Roster onto the PC (`iempc roster-install`) → `.claude/rules/roster-install.md`
- Dante labels of the IEM card (`iempc dante-labels`, the sheet) → `.claude/rules/dante-labels.md`

## Always-apply rules

**Owner event signals (D2).** The owner's signals are the only gate: never switch on your own and never ask whether an event is running. Nothing is ever force-killed. **Since the cutover (prod since 2026-10-19, #11) iemmixer serves every event**, from `~/devel/iemmixer-event-main` at the PC's pin: "ide event" → `python3 scripts/iem-pc/iempc.py event` (it writes `~/.config/iemmixer/EVENT-NOW` first, which freezes all PC work, then sends the owner's signal: a maintenance session ends live on the pin, a healthy live stays, an engine that does not play is started live on the pin again: `lifecycle::on_event`); "prepni na reaper" → plain `iemmode event` (the event plan; the PC stays prod and the next boot goes live on the pin); "event skončil" → remove the flag, PC work then runs as maintenance (`iempc dev`). A reboot in prod goes live on the pin. **In prod nothing goes to REAPER on its own** (owner, 2026-10-19 on #11: "na reaper sa už nevraciame"): a failed entry stops with an alarm, a crash loop stays down with an alarm; only an explicit `iemmode event` brings REAPER. While the flag exists, read the PC's process list (ssh) before any `iemmode status`: a status call with no guard running starts one, and a guard's start runs its boot plan. Before the cutover a reboot came back in event and "ide event" brought REAPER (history on #9 and #11).

**Predecessor boundary.** Never push to `zbynekdrlik/reaperiem`, never change its code, config or deployment; read it only through `~/devel/reaperiem` at a pinned SHA.

**Site data (P6).** Never commit names, hosts, IPs, Dante channel numbers, real track names, PINs, keys or tokens. Real site values live only in the private `zbynekdrlik/iemmixer-ops` (credential strings not even there: only in `~/.config/iemmixer/` and the CI secret). Every clone installs the pre-push hook (gitleaks + private denylist + allowed commit identities); CI repeats all three.

**Dante.** Never change Dante subscriptions or any Dante device other than the IEM PC's own card.

**Builds.** Tier 0: no local cargo compilation — push `dev` and verify in CI. Local `cargo fmt`, `cargo metadata`, `cargo tree`, `cargo update -p` are fine; `cargo deny` and `cargo mutants` run in CI only.

**Versions.** One version for the workspace: `[workspace.package].version` in `Cargo.toml` (`2.0.0-dev.N` until cutover). `scripts/check_version.py` enforces consistency, and dev > main on PRs.
