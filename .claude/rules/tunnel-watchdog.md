---
paths:
  - "crates/iem-server/src/tunnel_watch.rs"
  - "crates/iem-server/src/tunnel_watch/**"
  - "crates/iem-core/src/tunnel.rs"
  - "crates/iem-ui/src/components/tunnel_status.rs"
---

# Cloudflare tunnel watchdog

- **Health = cloudflared `/ready`, never the service state.** A RUNNING `cloudflared` with 0 edge connections is exactly the event failure (QUIC blocked). `http://127.0.0.1:20241/ready` → `{"status":200,"readyConnections":N}`, **HTTP 503** when N = 0 (treated as 0, like timeouts and garbage).
- **Pure state machine** (`TunnelWatch::observe(ready, now)`, `Instant` injected): Down on the first 0, restart after 120 s continuous, cooldown 600 s, `Restarting` for 60 s grace then back to `Down` (keeps `since`). Tests use synthetic instants — never sleep.
- **Tunnel flags live in the service ImagePath** (`tunnel --protocol http2 --metrics 127.0.0.1:20241 run --token …`), never an env var; `--metrics` MUST be pinned or the watchdog restarts a healthy tunnel every 10 min.
- **Stop takes up to 30 s** (`--grace-period`) → wait ≤ 45 s for STOPPED; `sc start` 1056 = still stopping → wait and retry once; `sc stop` 1062 = already stopped; 5 = access denied (service rights missing — the one-time elevated setup is S6 work). The outcome is published as `last_restart_ok`.
- **Never print the token.** `sc.exe qc` and the ImagePath carry `--token <secret>`; the code only uses `sc query/stop/start`.
- **Windows-only code is not compiled on Linux CI:** `*_sc_windows` and `restart_service_blocking` are excluded from mutation testing; their pure parts are unit-tested.
- **LAN URL and public host come from the site config** (`lan_url`, `https_domain` → `GET /api/site` → UI context; the tray's "Copy URL" uses `Config::share_url()`). Members on the public URL get no `TunnelStatus` while the tunnel is down; the client-side `LanHint` shows when the page host equals the configured public host.
- Server-side `sc.exe` runs with `CREATE_NO_WINDOW` (the app is a tray GUI).
