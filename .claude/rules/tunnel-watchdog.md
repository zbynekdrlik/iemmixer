---
paths:
  - "crates/iem-server/src/tunnel_watch.rs"
  - "crates/iem-server/src/tunnel_watch/**"
  - "crates/iem-core/src/tunnel.rs"
  - "crates/iem-ui/src/components/tunnel_status.rs"
---

# Cloudflare tunnel watchdog

- **Health = cloudflared `/ready`, never the service state.** A RUNNING `cloudflared` with 0 edge connections is exactly the event failure (QUIC blocked). `http://127.0.0.1:20241/ready` → `{"status":200,"readyConnections":N}`, **HTTP 503** when N = 0 (treated as 0, like timeouts and garbage).
- **Pure state machine** (`TunnelWatch::observe(ready, now)`, `Instant` injected): `Down` on the first 0, `Ok` on any connection. It only detects, logs and reports (`ServerMsg::TunnelStatus`, `GET /api/tunnel`) and never touches the tunnel: the server's user holds no right over `iem-tunnel.service`, whose own unit restarts cloudflared when it ends. Tests use synthetic instants — never sleep.
- **Tunnel flags live in the box's unit** `scripts/gen3/iem-tunnel.service` (`tunnel --no-autoupdate --protocol http2 --metrics 127.0.0.1:20241 run --token-file %d/tunnel`), never an env var; `--metrics` MUST stay pinned or the watch reads no `/ready` and reports a healthy tunnel `Down`.
- **Never print the token.** It is the systemd credential `tunnel` of the unit, never in a file in the clear or on a command line.
- **LAN URL and public host come from the site config** (`lan_url`, `https_domain` → `GET /api/site` → UI context). Members on the public URL get no `TunnelStatus` while the tunnel is down; the client-side `LanHint` shows when the page host equals the configured public host.
