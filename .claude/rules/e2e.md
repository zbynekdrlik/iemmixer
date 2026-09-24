---
paths:
  - "e2e/**"
---

# E2E (Playwright, mock — no audio hardware)

- Specs import `test`/`expect` from `./support/fixtures`: an auto fixture fails any test whose browser console shows an error, a warning or a page error. A test that deliberately provokes a failed request declares exactly that message with `test.use({ allowedConsole: [/…/] })` in its own `describe`, with a comment saying why. Never a global allowance; where the message is an app bug, fix the app.
- **The one documented environment class — REAPER absent until S5:** the server runs without REAPER, so pages that load REAPER-era mixer state get the failures listed in `REAPER_ABSENT` (`support/fixtures.ts`). A `describe` whose pages call those endpoints declares `test.use({ allowedConsole: REAPER_ABSENT })` with the comment `// REAPER absent in mock E2E until S5`. Add a message to the list only as an anchored exact pattern copied from a CI log line that shows it, and only for a REAPER-era endpoint; S5 deletes the list.
- PINs come from `E2E_ENGINEER_PIN` / `E2E_MEMBER_PIN` via `./support/pins` (CI generates them per run and provisions them with `iem-server pin …`). No credential is committed.
- The server runs with `config/test-site.toml`. REAPER is absent (the REAPER-era poller fails fast against `127.0.0.1:1`); cloudflared is absent (the tunnel is Down, so members see the tunnel banner).
- Login budgets are shared by the whole run: tests that fail logins act as tunnel clients from their own `203.0.113.N` (`CF-Connecting-IP` from a loopback peer is trusted) and keep the total under 30 failures per origin.
- **UI login attempts wait for their response:** `const r = page.waitForResponse(…/api/auth POST…)` before typing, `(await r).status()` after, and the PIN dots cleared before the next attempt — asserting on a still-visible "Invalid PIN" races the request.
- Live specs (real PC, audio) come back in S6/S7 through HIL in the private ops repo — never in public CI.
