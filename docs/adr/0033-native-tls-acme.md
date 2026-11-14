# ADR-0033: Native TLS with certificate files and ACME

- Status: accepted
- Date: 2026-11-14
- Work package: native TLS and ACME (roadmap, cache slug `tls`). No migration
  (version 0055 was reserved and is not used); ACME state lives in the data
  directory.
- Scope: optional HTTPS on the HTTP listener and TLS on the RESP listener,
  with a certificate from PEM files (hot reload) or from an RFC 8555
  certificate authority (ACME: Let's Encrypt, ZeroSSL, step-ca, ...);
  HSTS, minimum TLS version, ALPN, the HTTP to HTTPS redirect listener,
  status, metrics and `doctor`; what this means for several servers.
- Builds on: ADR-0001 (one binary, one data directory), ADR-0004 (keyring),
  ADR-0013 (RESP, whose decision 2 left TLS as a follow-up), ADR-0015 §2
  (FB-25: rustls with the ring provider, the outbound HTTPS client),
  ADR-0026 (feature registry), ADR-0029 (replicas and shards).

## Context

Until now the binary served plain HTTP, and docs/deployment.md §5 said to
terminate TLS in a reverse proxy. That stays the default and stays fully
supported: many deployments already have nginx, Caddy, a cloud load
balancer or a Kubernetes ingress. But a single-binary product should also
be able to stand on the internet alone: one host, one process, a domain
name, and a certificate that renews itself. The RESP listener had no TLS at
all, so it had to stay on loopback or behind a tunnel.

## Decisions

1. **Off by default; the proxy setup is unchanged.** `[tls] mode = "off"`
   (default) | `"files"` | `"acme"`. With TLS on, the existing listener
   (`[server] host:port`) speaks HTTPS; there is no second HTTPS port. With
   `off`, nothing in the request path changes (the router, middleware and
   `axum::serve` are the same as before).

2. **The TLS stack is the one already in the tree.** rustls 0.23 with the
   ring provider (FB-25, ADR-0015 §2) through tokio-rustls; no OpenSSL, no
   aws-lc. TLS 1.3 and 1.2 by default, `[tls] min_version = "1.3"` refuses
   1.2. ALPN offers `h2` then `http/1.1` (server preference), plus
   `acme-tls/1` only while the `tls-alpn-01` challenge is configured.

3. **Our own accept loop for HTTPS** (`forgebase-server/src/tls/serve.rs`).
   The TCP accept loop never waits on a handshake: each handshake runs in
   its own task, at most 1024 at a time, each bounded to 10 s (a silent or
   slow client cannot stall others; extra connections are closed and
   counted). A finished connection is served by `hyper-util`'s auto builder
   (HTTP/1.1 or HTTP/2) with the same router, `ConnectInfo` and graceful
   shutdown as the plain listener. `axum::serve` is not used for HTTPS
   because it advertises the HTTP/2 extended CONNECT protocol (RFC 8441):
   browsers would then open WebSockets over HTTP/2 as `CONNECT`, which the
   realtime route (`GET /api/v1/realtime/ws`) answers with 405. Without the
   setting, browsers open WebSockets over a separate HTTP/1.1 connection.

4. **One certificate store for every TLS listener.** `CertStore` is the
   rustls certificate resolver of HTTPS and RESP. A reload (files) or a
   renewal (ACME) swaps the certificate for the next handshake on both;
   open connections keep the certificate they were set up with and are
   never dropped. The store also holds pending ACME challenge answers.

5. **Files mode.** `cert_file` (PEM chain, leaf first) and `key_file` (PEM
   PKCS#8, PKCS#1 or SEC1; RSA, ECDSA or Ed25519) are read and checked
   **before the database opens**: a missing file, a file with no
   certificate, or a key that does not belong to the certificate
   (rustls `CertifiedKey::keys_match`, comparing the SubjectPublicKeyInfo)
   refuses to start with an error naming both paths
   (`tls_key_mismatch`, `tls_file_unreadable`, `tls_certificate_invalid`,
   `tls_key_invalid`), and nothing else is opened. While running, the files'
   **contents** are compared every `reload_interval_secs` (default 30; 0 =
   never) and on `SIGHUP` (Unix; the handler is only installed in files
   mode, so SIGHUP keeps its default meaning otherwise). Contents, not
   modification times: atomic replaces, Kubernetes secret symlink swaps and
   two writes in the same second are all seen. A changed pair that does not
   load (a key written before its certificate, a mismatch) is logged,
   counted and shown as `last_error`; the previous certificate keeps being
   served, and the pair is tried again when either file changes. An expired
   certificate is served with a warning (refusing to start would not help
   clients); `doctor` fails on it.

6. **ACME client: `instant-acme`, over our HTTPS client.**
   - The protocol (JWS signing, nonces and `badNonce` retries, orders,
     authorizations, polling) is `instant-acme` 0.8.5 (Apache-2.0),
     maintained alongside rustls. It is used with `default-features = false,
     features = ["ring"]`: no aws-lc-rs, no hyper-rustls, no platform
     verifier. Its `HttpClient` trait is implemented over
     `forgebase_auth::net::HttpsClient` (FB-25: rustls + ring, Mozilla roots
     from webpki-roots, certificates always verified), so there is one
     outbound TLS stack in the binary. `[tls.acme] ca_file` adds trust
     anchors for a private CA's own HTTPS endpoint (step-ca, Pebble).
   - The CSR and the `tls-alpn-01` certificate are written by a small DER
     writer (`tls/x509.rs`, like FB-153's certificate generator) with a
     fresh ECDSA P-256 key per issuance; the same module reads the
     validity, issuer, serial and SANs of certificates for scheduling, the
     status and `doctor`. rustls/webpki still decide trust.
   - Not chosen: `rustls-acme` (owns the accept loop and its own cache
     abstraction, built around its own TLS-ALPN-only flow and the
     `futures-rustls`/async-io stack; we need the store shared with RESP,
     sealed storage, http-01 and our metrics), hand-written JWS (more code
     to get exactly right than the dependency costs), `x509-parser` (a
     parser stack for four fields).
   - Account: created on first use with the contact `mailto:` address and
     `termsOfServiceAgreed = true` — **configuring `mode = "acme"` accepts
     the CA's terms of service** (docs/deployment.md §5 says so). External
     account binding (`eab_kid` + secret `FORGEBASE_TLS_ACME_EAB_HMAC_KEY`,
     base64url) for CAs that require it (ZeroSSL, Google, some step-ca).
   - Challenges, in the configured order of preference: `tls-alpn-01`
     (RFC 8737; answered on the HTTPS listener by the resolver, which hands
     the challenge certificate with the critical `acmeIdentifier` extension
     to `acme-tls/1` handshakes for that name and closes them after the
     handshake) and `http-01` (answered on the plain listener, decision 8).
     No `dns-01`, therefore **no wildcard certificates** (refused at
     configuration time; use files mode for a wildcard).
   - Domains: DNS names only (no IP addresses, no wildcards), validated at
     load.

7. **Renewal and storage.**
   - Renew at two thirds of the lifetime minus a random jitter of up to a
     fifteenth of it (90-day certificates: between day 54 and day 60), so
     several servers started together do not renew together. A missing
     certificate, one for a different domain list, or one past its renewal
     time is requested at once — **in the background**: the server starts
     and serves; until the first certificate is issued handshakes fail
     (except `tls-alpn-01` validation). Startup never blocks on a CA.
   - On failure the old certificate keeps being served; the next attempt
     waits 2 minutes, doubling up to 6 hours. Failures are counted,
     logged with the next retry time and shown in the status. ARI (RFC 9773
     renewal information) is not used yet.
   - `<data_dir>/tls/acme/<16 hex digits of SHA-256(directory URL)>/`:
     `account.sealed` (the account credentials) and `certificate.sealed`
     (chain, private key, domains, issue time), each base64 of
     `Keyring::seal` (XChaCha20-Poly1305 under the data directory's master
     key, ADR-0004) with the directory URL in the associated data, written
     atomically with mode 0600 in 0700 directories; `status.json` (domains,
     issuer, validity, last renewal, next renewal, last error: nothing
     secret) for `forgebase doctor` and operators. Staging and production
     accounts never mix. A store that cannot be unsealed (another master
     key) is logged and replaced by a new request.

8. **The plain HTTP listener.** Runs on `[server] host:[tls] http_port`
   (default 80) when `redirect_http = true` or the `http-01` challenge is
   configured. It answers `GET /.well-known/acme-challenge/<token>` from the
   store and redirects everything else to HTTPS with `308`. The target is
   `[server] public_url` when it is `https://`; otherwise the request's
   `Host` (in acme mode only one of the configured domains, else the first;
   in files mode any well-formed host name) with the HTTPS port (omitted
   when 443). A malformed `Host` gets `400`, never a redirect.

9. **HSTS** (`hsts_max_age_secs`, default 0 = off; `hsts_include_subdomains`)
   is added to every HTTPS response that does not set it. Off by default
   because it is hard to undo in browsers.

10. **RESP over TLS.** `[resp] tls = true` (requires `[tls] mode` other than
    `off`) serves RESP with the same certificate (`rediss://`); the
    handshake must finish within the pre-auth deadline (10 s). The
    clear-text warning for a non-loopback bind only fires without TLS.

11. **Status, metrics, doctor.** `GET /api/system/info` (administrators, or
    direct loopback) gains `tls`: `{"mode": "off"}` or mode, minimum
    version, ALPN, HSTS, the plain listener address, RESP TLS, the
    certificate (subject, names, issuer, serial, `not_before`, `not_after`,
    `expires_in_secs`), `last_error`/`last_error_at`, and per mode
    `cert_file`/`key_file`/`reload_interval_secs`/`last_reload` or
    `domains`/`directory_url`/`challenges`/`last_renewal`/`next_renewal`/
    `renewal_failures`. It is added through the new additive
    `AppState::with_status_section` hook. Metrics (names in
    `forgebase_observability::names`): `forgebase_tls_certificate_expiry_seconds`
    (gauge, refreshed at every scrape; negative once expired — the alerting
    work package alerts on it), `forgebase_tls_renewals_total`,
    `forgebase_tls_renewal_failures_total`, `forgebase_tls_reloads_total`,
    `forgebase_tls_reload_failures_total`,
    `forgebase_tls_handshake_failures_total{listener="https"|"resp"}`.
    `forgebase doctor` adds a `tls` line: ok with the expiry date, a warning
    under 14 days or before the first ACME certificate, a failure when
    expired or when the files do not load. It reads the files or
    `status.json`; it never needs the keyring.

12. **Several servers (decision for v1: keep it simple).** ADR-0029's
    replicas and shards are PostgreSQL nodes; `forgebase node` runs the
    engine only and serves no HTTP, so it needs no certificate. When
    several ForgeBase servers serve the same domain (behind a load balancer,
    Kubernetes replicas):
    - **ACME runs on one server only.** Each server keeps its ACME state in
      its own data directory; several servers requesting the same
      certificate would each order one (CA rate limits) and a CA's
      validation connection can land on a server that did not publish the
      challenge.
    - **For more than one server, use files mode with a certificate the
      platform provides** (cert-manager writing a Kubernetes Secret mounted
      into every pod; certbot or another ACME client on a shared volume)
      — the content check reloads it everywhere without restarts — or
      terminate TLS at the load balancer (`mode = "off"`).
    - Sharing the ACME account and certificate through the control
      database (one renewing server holding a lease, every server serving
      the sealed certificate and the challenge answers) is the planned
      follow-up; migration version 0055 is the slot for it.

13. **Proxy-related settings stay correct for both setups.** With native
    TLS and clients connecting directly, keep `[auth] trust_forwarded_for =
    false`: a client could otherwise choose its own address for rate
    limits. The server logs a warning when both are on, since a TLS
    pass-through or re-encrypting proxy that appends `X-Forwarded-For` is a
    valid (rare) setup. `[server] ops_access = "loopback"` keeps its
    meaning: a direct connection from the same host, without forwarding
    headers, over HTTP or HTTPS. The auth service's own public URL (used
    for OAuth callbacks and mail links when `[server] public_url` is unset)
    switches to `https://` with native TLS.

## Error codes

| Code | Kind | When |
|---|---|---|
| `tls_file_unreadable` | invalid input | A certificate or key file cannot be read. |
| `tls_certificate_invalid` | invalid input | No PEM certificate, or the leaf cannot be read. |
| `tls_key_invalid` | invalid input | No PEM private key, or a key type rustls cannot use. |
| `tls_key_mismatch` | invalid input | The key does not belong to the certificate. |
| `tls_setup_failed` | internal | Protocol versions, `ca_file`, key generation. |
| `tls_storage_failed` | internal | The sealed ACME files cannot be read or written. |
| `tls_acme_failed` | unavailable | The CA refused or failed (only in status and logs; never a startup error). |

These are startup errors (`ServerError::Tls`), not HTTP responses.

## Configuration

`[tls]`: `mode`, `cert_file`, `key_file`, `reload_interval_secs`,
`min_version`, `hsts_max_age_secs`, `hsts_include_subdomains`,
`redirect_http`, `http_port`; `[tls.acme]`: `domains`, `email`,
`directory_url` (`letsencrypt`, `letsencrypt-staging` or a URL), `challenges`,
`eab_kid`, `eab_hmac_key` (secret), `ca_file`; `[resp] tls`. Each has a
`FORGEBASE_TLS_*` / `FORGEBASE_TLS_ACME_*` / `FORGEBASE_RESP_TLS` variable
(lists comma-separated) and a registry entry under the always-on feature
`tls` (read-only in the dashboard: each changes how the server listens).
Paths resolve against the config file's directory. Details and examples:
docs/deployment.md §5 and §7, forgebase.example.toml.

## Changes by crate

- `forgebase-core`: `config/tls_config.rs` (`TlsConfig`, `AcmeConfig`,
  overrides, environment, validation), `[resp] tls`, registry entries, the
  init template.
- `forgebase-server`: `tls/` (`mod.rs` setup, status, doctor check;
  `certs.rs` loading and the resolver; `files.rs` watcher; `acme.rs`
  scheduler and sealed storage; `acme_client.rs` instant-acme transport and
  issuance; `serve.rs` HTTPS loop, plain listener, HSTS; `x509.rs`);
  `lib.rs` prepares TLS before the database, binds the listeners, stops
  them on shutdown; `RunningServer::{tls, http_redirect_addr, url}`;
  `ServerError::Tls`.
- `forgebase-resp`: `RespConfig::tls`; the connection loop is generic over
  the stream.
- `forgebase-api`: `AppState::with_status_section` (additive).
- `forgebase-observability`: six metric names.
- `forgebase-cli`: `doctor` `tls` line; `serve` prints `https://`, the
  redirect listener and `rediss://`.

## Dependencies

| Crate | Version | Licence | Where | Why |
|---|---|---|---|---|
| instant-acme | 0.8.5 (`default-features = false`, `ring`) | Apache-2.0 | server | ACME (RFC 8555) protocol |
| rcgen | 0.14.10 (`ring`, `pem`) | MIT OR Apache-2.0 | transitive | instant-acme's `ring` feature enables it unconditionally; ForgeBase does not call it |
| pem | 4.0.0 | MIT | transitive (rcgen) | |
| yasna | 0.6.0 | MIT OR Apache-2.0 | transitive (rcgen) | |
| h2 | 0.4.20 | MIT | transitive (hyper `http2`) | HTTP/2 over ALPN |
| hyper-util | 0.1.21, adds `server-auto`, `server-graceful`, `http2` | MIT | server | already in the tree (axum); the HTTPS serve loop |
| bytes, http, http-body-util, tower, tokio-rustls, rustls, ring, base64 | as in the workspace | MIT / Apache-2.0 / ISC | server, resp | already in the tree |

`Cargo.lock` also lists instant-acme's optional dependencies (hyper-rustls,
rustls-platform-verifier, aws-lc-related, x509-parser, jni, ...) because a
lockfile records every optional dependency of a package in the graph; they
are **not compiled** (`cargo tree -p forgebase-cli -e normal` shows only the
crates above). Test-only tools: Pebble 2.10.3 and pebble-challtestsrv (Go,
MPL-2.0), not linked.

## Testing

- `crates/forgebase-server/tests/tls.rs` (10 tests): HTTPS with a generated
  self-signed certificate and a client that trusts it (HTTP/1.1, ALPN h2,
  IP SAN, HSTS, untrusted client refused, plain HTTP gets nothing, the
  `tls` status entry, the expiry gauge), also on PostgreSQL when the engine
  is available; hot reload swaps the certificate for new handshakes while a
  connection opened before keeps working, a broken pair keeps the old
  certificate and reports why; SIGHUP reload with polling off; a mismatched
  key and a missing file refuse to start before the database opens; RESP
  over TLS with a real API key, plain RESP refused; `resp.tls` without TLS
  is a configuration error; the redirect listener (308 with path and query,
  malformed host 400, `http-01` answers); `tls-alpn-01` handshakes get the
  challenge certificate with the digest and are closed, unknown names fail,
  normal clients get the served certificate; `doctor` levels.
- `crates/forgebase-server/tests/tls_renewal.rs` (4 tests, fake clock and
  fake CA behind `CertificateAuthority`): first issuance, renewal exactly at
  two thirds without jitter and within the jitter window with it, failures
  keep the old certificate with 2/4/8-minute backoff and a 6-hour cap,
  recovery resets, sealed files and `status.json`, restart without asking
  the CA, another master key requests a new certificate, changed domains
  or an expired certificate are requested at once.
- `crates/forgebase-server/tests/tls_acme_pebble.rs` (2 tests, `#[ignore]`,
  env-gated, docs/qa.md): real ACME against Pebble with `tls-alpn-01` and
  `http-01`, the chain verified up to Pebble's root, restart serving the
  stored certificate; a CA requiring EAB refuses without the key (counted,
  retried, server keeps running) and issues with it.
- Unit tests: config parsing and validation (`forgebase-core`), the DER
  writer and reader against rustls/webpki, the CSR signature, the resolver
  and challenge board, renewal time and backoff, redirect targets, HSTS
  values, the `/api/system/info` hook.

## Limitations and follow-ups

- No `dns-01`, so no wildcard certificates over ACME; no ARI; no OCSP
  stapling; no client certificates (mutual TLS).
- ACME on one server per domain (decision 12); sharing through the database
  is a follow-up (migration 0055).
- `tls-alpn-01` needs the CA to reach `[server] port` as port 443, `http-01`
  needs `http_port` reachable as port 80 (port mapping or
  `CAP_NET_BIND_SERVICE`).
- The CLI's own HTTP probes (`forgebase status`, the `forgebase node`
  agent) speak plain HTTP only; with native TLS `status` reports the server
  as not answering. Their HTTPS client is a CLI follow-up.
- The dashboard does not show the `tls` status entry yet (fields in
  decision 11), and the feature registry's first `list` settings
  (`tls.acme.domains`, `tls.acme.challenges`) are read-only there.
- ~~`forgebase backup` snapshots do not include `<data_dir>/tls/`; a restored
  server requests a new certificate.~~ Resolved by ADR-0041: `[backup]
  include_tls` (on by default with `mode = "acme"`) puts the sealed `tls/`
  directory in every backup and a restore brings it back. Cluster backups and
  the pre-migration snapshot still do not carry it.
