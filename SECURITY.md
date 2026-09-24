# Security policy

## Reporting a vulnerability

Report vulnerabilities privately with GitHub's "Report a vulnerability" button on this repository (private vulnerability reporting). Please do not open a public issue. You will get an answer within 7 days.

## Scope

The server, web UI and tray in this repository, and later the engine and guard. Site configuration and deployment live in a private repository and are out of scope.

## Baseline

- No credentials in the code or the repository; secrets are generated on the target PC.
- PINs are stored as argon2id hashes keyed with a DPAPI-protected pepper.
- The API grants no cross-origin access (no CORS); the UI is served by the same server.
- Login attempts are rate-limited per client and per origin and never locked out.
- CI: SHA-pinned actions, read-only default token, secret scanning with push protection, gitleaks, a private denylist scan, cargo-deny.
- No self-hosted runner is registered on this repository; pull requests from forks never produce deployable builds.

## Supported versions

Only the latest `main`.
