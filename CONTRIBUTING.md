# Contributing

- Open pull requests against `dev`; `main` only receives merges from `dev`.
- Workflows on pull requests from forks run only after a maintainer approves them (required for all external contributors). Fork pull requests never produce deployable builds.
- On fork pull requests the `secrets` check fails by design: the private denylist is not available to forks, so a maintainer runs the scan locally before merging, and the merge push to `dev` runs it again.
- Commits must use your GitHub noreply address (`<id>+<login>@users.noreply.github.com`): CI rejects author or committer emails outside `scripts/allowed-identities.txt`; a maintainer adds an accepted contributor's noreply address to that file (never a personal address).
- Every change needs tests that can fail; CI must be fully green: lint, unit tests with the coverage floor, WASM build, browser E2E with a clean console, diff-scoped mutation testing, supply chain, secret scans, Windows build.
- Never add site-specific data (names, hosts, addresses, PINs, keys) — see `.claude/rules/public-repo-hygiene.md`.
- Contributions are licensed MIT OR Apache-2.0 like the project (the future `iem-limiter-mga` crate: GPL-3.0-or-later).
