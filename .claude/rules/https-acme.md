---
paths:
  - "crates/iem-server/src/acme.rs"
  - "crates/iem-server/src/acme/**"
  - "crates/iem-server/src/cloudflare.rs"
  - "crates/iem-server/src/cf_token.rs"
  - "crates/iem-server/src/sealed.rs"
  - "crates/iem-server/src/tls_cert.rs"
  - "crates/iem-server/tests/cloudflare_cli.rs"
---

# HTTPS certificate keeper (ACME DNS-01, #91)

- **Ported from fohmixer 9f3d440** (`crates/fohmixer-hub/src/{acme.rs,acme/,cloudflare.rs,cf_token.rs,sealed.rs,tls.rs}`, same owner and licence; ROZHODNUTÉ on #91: no shared crate for now). Each ported file's first line names its source. A fix found in one copy belongs in the other too: tell the fohmixer session (peer `fohmixer`).
- **Only with the site's `[acme]`** (`iem_core::config::AcmeConfig`: `directory` default Let's Encrypt production, `contact` optional, `propagation_s` default 20, at most 600; it needs `tls = true` and a DNS name in `https_domain`). Without it the server serves `tls_cert`/`tls_key` exactly as before; the plain-http listener and its redirect rule are never touched.
- **The keeper** (`acme::keep`, `tls` feature): keeps a stored certificate while ≥ 30 days remain (`RENEW_BEFORE_SECS`), looks again every 12 h (`CHECK_EVERY`); otherwise one attempt: zone by the longest parent the token sees (`cloudflare::zone_candidates`), this server's own leftover `_acme-challenge` TXT records removed first (only records with `TXT_COMMENT`; another client's are never touched), the record written, `propagation_s`, the challenge answered, the record removed **also on failure**, the order cut at `ATTEMPT_LIMIT` (30 min). Only the order is cut off: a certificate it got is always written (`CertFiles::save`: key then chain, each replaced whole) and then loaded with `RustlsConfig::reload_from_pem_file` (no restart). Failures retry after 1 min doubling to 6 h; a missing token is looked for every 5 min and warned of once (`NO_TOKEN`). The old certificate keeps being served meanwhile.
- **Start-up** (`lib.rs` `https_config`): the site's files when they load; with a keeper and no usable files, a self-signed stand-in (`tls_cert::stand_in`) so the listener exists for the keeper to reload; without a keeper, no files = no HTTPS listener (as before).
- **Status:** `GET /api/site` → `certificate: {expires, last_error, failures}` (`AppState.cert_status`, `iem_core::tunnel::CertStatus`), absent while no keeper runs. The route is public: errors name calls and HTTP statuses, never a token or key.
- **Secrets:** the Cloudflare token (`cf_token`, `iem-server cloudflare set-token`, read from stdin, shape-checked, never from argv) and the ACME account (`secrets/acme_account.<ext>`, tied to its directory URL; another directory or a file that does not parse makes a new account, an unreadable one is an error) are sealed for the server's user with DPAPI (`sealed.rs` → `pepper/dpapi.rs`), never in the site toml, never logged (`CfToken`'s and `Pem`'s `Debug` hide them; a stored account that does not parse is logged by kind and position only, never serde's text). **Linux has no sealed store yet** (Gen 3 later): `sealed::AVAILABLE` is false outside the unit tests, set-token exits 1 and stores nothing, the keeper logs `NO_SEALED_STORE`, shows it as `last_error` and does nothing.
- **Plain http only on loopback:** `[acme] directory` must be https or http to a loopback address (`iem_core::config::acme_url_allowed`), which only the tests' doubles use; the Cloudflare base is the constant `cloudflare::API` (tests override the field).
- **Tests:** the RFC 8555 double (`acme/double.rs`: JWS signatures, nonces, the DNS-01 record read from the Cloudflare double, a test CA signs the CSR) and the Cloudflare double (`cloudflare::double`); the keeper is tested against a real rustls listener with a TLS handshake (`tls_cert::test_certs::handshake`). Token-shaped test values stay low-entropy (`"c".repeat(24)`) so gitleaks stays quiet; names are `example.org` only. The windows CI job runs `pepper:: sealed:: cf_token::` and `tests/cloudflare_cli.rs` on DPAPI.
- **LAN DNS** (not code): the public name is a CNAME to a local-only name holding the server's A record on the router, never an A on the public name (fohmixer#26: iOS follows Cloudflare's HTTPS record hints otherwise). Real names live only in the ops repo.
