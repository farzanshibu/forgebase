# ADR-0002: Dependencies (Phase 1)

- Status: accepted
- Date: 2026-11-07
- Work package: FB-01

## Policy

- Licences allowed without asking (PLAN §2): MIT, Apache-2.0, BSD, ISC, Zlib,
  MPL-2.0. Anything else is listed under *Needs a decision* below.
- Every new dependency is recorded here with its checked version and licence.
  Versions were checked with `cargo search` / `cargo info` against crates.io on
  2026-11-07.
- Advisories and licences of the full tree were checked with
  `cargo deny check advisories licenses` (cargo-deny 0.20.2, RustSec database
  fetched 2026-11-07): **advisories ok, licences ok** (with Unicode-3.0
  allowed, see below). FB-03 adds the permanent `deny.toml` and CI job.
- Features are trimmed: `default-features = false` where the defaults pull in
  things Phase 1 does not use (sqlx macros/any/migrate, metrics HTTP
  listener + push gateway, axum form/query).

## Direct dependencies

| Crate | Version | Licence | Used by | Purpose |
|---|---|---|---|---|
| tokio | 1.54.1 | MIT | all async crates | runtime, net, fs, signals |
| axum | 0.8.9 | MIT | api, server | HTTP server and routing |
| tower | 0.5.3 | MIT | api (dev) | `ServiceExt::oneshot` in router tests |
| tower-http | 0.7.1 | MIT | api | trace, timeout, catch-panic layers |
| sqlx | 0.9.0 (`sqlite`, `runtime-tokio`) | MIT OR Apache-2.0 | db, migrations | SQLite driver, pools, transactions |
| libsqlite3-sys (via sqlx) | 0.37.0, bundled SQLite 3.51.3 | MIT (SQLite itself: public domain) | db | statically linked SQLite, so no system SQLite is needed |
| serde | 1.0.229 | MIT OR Apache-2.0 | core, db, api, cli | (de)serialisation |
| serde_json | 1.0.154 | MIT OR Apache-2.0 | api, cli | JSON bodies and CLI `--json` output |
| toml | 1.2.1 | MIT OR Apache-2.0 | core | config file parsing |
| thiserror | 2.0.21 | MIT OR Apache-2.0 | all library crates | explicit error enums |
| anyhow | 1.0.104 | MIT OR Apache-2.0 | cli | top-level error reporting in the binary only |
| clap | 4.6.7 (`derive`) | MIT OR Apache-2.0 | cli | command-line parsing |
| tracing | 0.1.44 | MIT | all | structured logging |
| tracing-subscriber | 0.3.23 (`env-filter`, `fmt`, `json`) | MIT | observability | log formatting and filtering |
| metrics | 0.24.6 | MIT | observability, api | metrics facade |
| metrics-exporter-prometheus | 0.18.3 (no default features) | MIT | observability, api | Prometheus text rendering |
| rust-embed | 8.13.0 (`debug-embed`) | MIT | api | embeds `dashboard/dist`; `allow_missing` keeps builds working without it |
| mime_guess | 2.0.5 | MIT | api | content types for dashboard assets |
| sha2 | 0.11.1 | MIT OR Apache-2.0 | migrations | migration checksums |
| time | 0.3.55 (`formatting`) | MIT OR Apache-2.0 | core | RFC 3339 timestamps |
| uuid | 1.29.1 (`v4`) | Apache-2.0 OR MIT | db, api | instance ids, request ids |
| fs4 | 1.1.0 (`sync`) | MIT OR Apache-2.0 | cli | free disk space for `doctor` (cross-platform, no libc/unsafe in our code) |
| tempfile | 3.27.0 | MIT OR Apache-2.0 | dev only | test directories |

Deliberately **not** added:

- An HTTP client (reqwest/hyper client): `status`, `doctor` and the tests use
  a 40-line HTTP/1.1 GET over `TcpStream`. reqwest 0.13 defaults would pull
  rustls + aws-lc-rs (C, extra licences) into the tree.
- `fs2`/`fs4` locking: the data-directory lock uses `std::fs::File::try_lock`
  (stable since Rust 1.89).
- `figment`/`config`: precedence is ~150 tested lines in `forgebase-core`.
- `tower-http` request-id layer: replaced by our own middleware so unsafe
  client-supplied ids can be rejected.

## Transitive tree (binary, aarch64-apple-darwin)

205 third-party crates. Licence counts: 118 `MIT OR Apache-2.0`, 41 `MIT`,
18 `Unicode-3.0`, 13 `Apache-2.0 OR MIT`, 4 `Unlicense OR MIT`, 4 other
MIT/Apache combinations, 2 `Apache-2.0`, 1 `Zlib`, 1 `MIT AND BSD-3-Clause`,
1 `MIT AND Apache-2.0`, 1 `(MIT OR Apache-2.0) AND Unicode-3.0`
(`unicode-ident`), 1 `BSD-2-Clause OR Apache-2.0 OR MIT`.

## Needs a decision (flagged to god)

1. **Unicode-3.0** (18 ICU crates: `icu_*`, `zerovec`, `yoke`, `litemap`,
   `tinystr`, `writeable`, `zerofrom`, `zerotrie`, `potential_utf`, plus
   `unicode-ident`). They come in through `sqlx-core → url → idna`, which sqlx
   needs for connection URLs; sqlx is mandated by SPEC §3, so they cannot be
   removed without replacing sqlx. Unicode-3.0 is a permissive, OSI-approved
   licence (attribution only), allowed by default in most cargo-deny setups.
   Recommendation: add it to the allow list in FB-03's `deny.toml`.
2. **SQLite public-domain dedication** (bundled C source in
   `libsqlite3-sys`). Required by SPEC §3 (primary embedded database); no
   obligations. Recommendation: record as accepted.

## Toolchain

- Rust 1.99.0 / Cargo 1.99.0 used to build and test.
- Workspace `rust-version = 1.94` (sqlx 0.9 MSRV); edition 2024; resolver 3.

## Phase 2 additions (FB-10, 2026-11-07)

New internal crates: `forgebase-schema`, `forgebase-query`, `forgebase-data`
(ADR-0003). **No new third-party crates enter the tree**: `Cargo.lock` gains
only the three internal packages. Two crates already in the tree become direct
dependencies, and one feature is enabled:

| Crate | Version | Licence | Used by | Purpose |
|---|---|---|---|---|
| url | 2.5.8 (`std` only) | MIT OR Apache-2.0 | schema | URL field validation (already in the tree via `sqlx-core`) |
| base64 | 0.22.1 | MIT OR Apache-2.0 | data | pagination cursors, BLOB values in JSON (already in the tree via `metrics-exporter-prometheus`) |
| time | 0.3.55, adds feature `parsing` | MIT OR Apache-2.0 | schema | RFC 3339 parsing of `date` fields |
| tokio | 1.54.1 (workspace features) | MIT | data | `sync::Mutex` serialising schema changes |

`cargo deny check advisories licenses` (cargo-deny 0.20.2, RustSec database
fetched 2026-11-07) on the full tree: **advisories ok, licences ok** (with the
policy licences plus Unicode-3.0 allowed, as above).

The bundled SQLite (libsqlite3-sys 0.37.0) is compiled with FTS5, JSON1,
RTREE, DBSTAT and STAT4, which the data core uses for full-text indexes, JSON
paths, index sizes and planner statistics; no extra C code is added.

## Phase 2 REST API additions (FB-11, 2026-11-07)

| Crate | Version | Licence | Used by | Purpose |
|---|---|---|---|---|
| utoipa (+ utoipa-gen 6.0.1, feature `macros`) | 6.0.0 | MIT OR Apache-2.0 | api | OpenAPI 3 document at `/api/openapi.json`; new to the tree (the only new third-party packages in `Cargo.lock`) |
| futures-util | 0.3.34 (`std`) | MIT OR Apache-2.0 | api | streamed export bodies (`stream::try_unfold`); already in the tree |
| http-body-util | 0.1.5 | MIT | api | reading import bodies frame by frame; already in the tree via axum |
| url | 2.5.8 | MIT OR Apache-2.0 | api | `form_urlencoded` query-string parsing; already in the tree |
| tower-http | 0.7.1, adds features `cors`, `limit` | MIT | api | CORS, request body size limits |

`cargo deny check advisories licenses bans` (cargo-deny 0.20.2): ok.

## Phase 3 authentication additions (FB-20, 2026-11-07)

Versions checked with `cargo info` against crates.io on 2026-11-07; all are
RustCrypto crates (pure Rust, no C, no `unsafe` in our code).

| Crate | Version | Licence | Used by | Purpose |
|---|---|---|---|---|
| argon2 | 0.6.0 (no default features; `alloc`, `password-hash`) | MIT OR Apache-2.0 | auth | Argon2id password hashing, PHC strings |
| hmac | 0.13.0 | MIT OR Apache-2.0 | auth | HS256 access tokens, keyed token hashing, key derivation, TOTP |
| sha1 | 0.11.0 (no default features) | MIT OR Apache-2.0 | auth | HMAC-SHA1 for TOTP (RFC 6238 default, what authenticator apps use) |
| chacha20poly1305 | 0.11.0 (no default features; `alloc`) | Apache-2.0 OR MIT | auth | XChaCha20-Poly1305 encryption of signing keys and TOTP seeds at rest |
| subtle | 2.6.1 (no default features) | BSD-3-Clause | auth | constant-time comparisons |
| getrandom | 0.4.3 | MIT OR Apache-2.0 | auth | OS CSPRNG for salts, tokens, codes, keys (already in the tree via uuid) |
| sha2 | 0.11.1 | MIT OR Apache-2.0 | auth | HMAC-SHA256 (already a direct dependency of migrations) |

New packages in `Cargo.lock` (transitive): `aead` 0.6.1, `base64ct` 1.8.3,
`blake2` 0.11.0, `chacha20` 0.10.2, `cipher` 0.5.2, `cmov` 0.5.4,
`ctutils` 0.4.8, `inout` 0.2.6, `password-hash` 0.6.1, `phc` 0.6.1,
`poly1305` 0.9.1, `universal-hash` 0.6.1, all `MIT OR Apache-2.0`.

Deliberately **not** added: a JWT library (`jsonwebtoken` pulls `ring` or
`aws-lc`; the strict HS256 subset we need is ~60 tested lines that accept only
`alg=HS256` with a known `kid`), a TOTP crate (RFC 6238 is ~30 lines, tested
against the RFC vectors), `rand` (getrandom is enough), an SMTP client (the
`Mailer` trait has a dev mailer; a provider is a later card).

`cargo deny check advisories licenses` (cargo-deny 0.20.2, RustSec database
fetched 2026-11-07) on the full tree: **advisories ok, licences ok**.

## FB-21 (policy engine and tenancy, ADR-0008)

No new third-party crate. The new workspace crate `forgebase-policy` uses only
dependencies already recorded here: `axum`, `serde`, `serde_json`, `sha2`
(invitation-token digests), `thiserror`, `tokio` (audit writer task),
`tracing`, `url` (`form_urlencoded` for query strings, already used by
`forgebase-api`) and `utoipa`. The property tests use a 10-line SplitMix64
generator instead of `proptest` (which would add ~15 packages for one test).
`Cargo.lock` gains only the `forgebase-policy` entry.
## Phase 6 graph (FB-51, 2026-11-07)

New crate `forgebase-graph` (graph model on SQLite, recursive-CTE traversal,
shortest path, REST under `/api/v1/graph`). **No new third-party crates.** It
uses packages already in the tree: axum, serde, serde_json, thiserror,
tower-http (`limit`), tracing, url, utoipa, uuid. The bundled SQLite's recursive
CTEs and JSON1 functions are enough; no graph library is added.

## FB-61 (jobs)

`forgebase-jobs` adds no third-party dependencies (uses tokio, axum, tower-http, utoipa, uuid, time, serde already listed). Cron parsing is in-crate.

## Phase 6 vector (FB-50, 2026-11-09)

Native code approved by god for this card because SPEC §3/§6 name USearch. Versions from `Cargo.lock`, licences from each crate's manifest; `cargo deny check` on the full tree: see the FB-50 report.

| Crate | Version | Licence | Used by | Purpose |
|---|---|---|---|---|
| usearch | 2.26.6 (`default-features = false`: no `numkong` SIMD library, no OpenMP) | Apache-2.0 | vector | HNSW approximate nearest-neighbour index (C++ built with `cc` through `cxx`) |

New packages in `Cargo.lock` (transitive, build-time code generation and the C++ bridge): `cxx` 1.0.205, `cxx-build` 1.0.205, `cxxbridge-flags` 1.0.205, `cxxbridge-macro` 1.0.205, `cxxbridge-cmd` 1.0.205 (lock entry only, not built), `link-cplusplus` 1.0.12, `scratch` 1.0.9, `unicode-width` 0.2.3 (all `MIT OR Apache-2.0`), `codespan-reporting` 0.13.1 (`Apache-2.0`), `termcolor` 1.4.1 (`Unlicense OR MIT`, used under MIT). Building needs a C++17 compiler (Xcode clang on macOS, g++/clang++ on Linux, MSVC on Windows), which the Rust toolchain targets already assume for `cc`.

## FIX-1 (test reliability and the SQLite advisory, 2026-11-09)

No new dependencies.

**RUSTSEC-2026-0459 (bundled SQLite older than 3.54).** `libsqlite3-sys` 0.37
bundles SQLite 3.51.3. `sqlx-sqlite` 0.9.0 requires `libsqlite3-sys < 0.38`, so
the fixed SQLite cannot be pulled in yet. Two measures, both to be removed
when `sqlx-sqlite` allows `libsqlite3-sys >= 0.39`:

1. `deny.toml` ignores the advisory with a reason and a review date
   (**2026-12-09**; re-check then whether a newer `sqlx` is out).
2. Mitigation in `forgebase-db`: `Database::open` reads the 32-byte header of
   a sibling `<db>-wal` file and the 100-byte database header and refuses to open
   (`DbError::WalPageSizeMismatch`, code `database_wal_mismatch`) when the two
   page sizes differ, before SQLite can replay that WAL. A missing or empty
   WAL, a WAL without the WAL magic number (SQLite ignores it) and a new or
   empty database are not affected. Tested with a hand-crafted pair of files.
   This is a mitigation for one attack shape (a mismatched database/WAL pair
   planted next to the database file), not a fix: the real fix is the SQLite upgrade.

**Test clocks.** `forgebase-jobs` and `forgebase-auth` read time through an
injectable `Clock` (`SystemClock` in production, `ManualClock` in tests), so
lease, backoff, cron, delay and token-expiry tests advance virtual time
instead of sleeping and no longer depend on machine load.

## Phase 3 social login and passkeys additions (FB-22, 2026-11-09)

Versions checked with `cargo info` on 2026-11-09. All pure Rust, no C.

| Crate | Version | Licence | Used by | Purpose |
|---|---|---|---|---|
| p256 | 0.14.0 (`ecdsa`, `arithmetic`, `pkcs8`, `pem`, `std`) | Apache-2.0 OR MIT | auth | ES256 verification (ID tokens, passkeys), Apple client-secret signing |
| num-bigint | 0.5.1 (`std`) | MIT OR Apache-2.0 | auth | RS256 verification (`modpow` on public data) |
| url | 2.5.8 | MIT OR Apache-2.0 | auth | provider URLs, query/form parsing (already in the tree) |
| utoipa | 6.0.0 | MIT OR Apache-2.0 | auth | OpenAPI fragment (already in the tree) |
| uuid | 1.29.1 | Apache-2.0 OR MIT | auth (dev) | test codes (already in the tree) |

New transitive packages in `Cargo.lock`: `base16ct`, `cpubits`, `crypto-bigint`,
`der`, `ecdsa`, `elliptic-curve`, `ff`, `group`, `num-bigint`, `num-integer`,
`pkcs8`, `primefield`, `primeorder`, `rand_core`, `rfc6979`, `sec1`, `serdect`,
`signature`, `spki`, `wnaf`, `zeroize` (RustCrypto, MIT/Apache-2.0).

Deliberately **not** added: `webauthn-rs` (OpenSSL), `jsonwebtoken` (ring/aws-lc),
`rsa` (RUSTSEC-2023-0071), `openidconnect`/`oauth2`/`reqwest` (TLS stack),
a CBOR crate (a 130-line strict reader covers WebAuthn). **Open decision**:
a TLS-capable outbound HTTP client (ADR-0009 §9).

## FB-32 (RESP listener, ADR-0013)

New crate `forgebase-resp`. Its default build adds **no third-party crates** to the `forgebase` binary (it uses tokio, metrics, tracing, thiserror, already listed). One new third-party crate for tests and an optional feature:

| Crate | Version | Licence | Used by | Purpose |
|---|---|---|---|---|
| redis | =0.32.7 (`tokio-comp`, `connection-manager`; no default features) | BSD-3-Clause | dev-dependency of `forgebase-resp`, `forgebase-server`; optional dependency behind cargo feature `external-redis` (off by default) | real Redis client for the conformance tests; external Redis/Valkey backend |

New packages in `Cargo.lock` (all via `redis`): `combine` 4.6.9 (MIT), `backon` 1.6.0 (Apache-2.0), `arc-swap` 1.9.4, `num-bigint` 0.4.8, `num-integer` 0.1.47 (MIT OR Apache-2.0), `tokio-util` 0.7.20 (MIT), `ryu` 1.0.23 (Apache-2.0 OR BSL-1.0; taken under Apache-2.0). `cargo deny check licenses` passes.

`redis` is pinned to 0.32.7 on purpose: 1.x depends on `xxhash-rust` (BSL-1.0, not on the allow list). Revisit if BSL-1.0 is approved.

## Phase 5 realtime (FB-40, 2026-11-09)

New crate `forgebase-realtime` (ADR-0011). One feature and one dev-dependency:

| Crate | Version | Licence | Used by | Purpose |
|---|---|---|---|---|
| axum feature `ws` | 0.8.9 | MIT | realtime | WebSocket upgrade (pulls `tokio-tungstenite`) |
| futures-util feature `sink` | 0.3.34 | MIT OR Apache-2.0 | realtime | split a WebSocket into reader and writer |
| tokio-tungstenite | 0.29.0 (no default features; `connect`, `handshake`) | MIT | realtime (dev) | WebSocket client in tests; same version axum's `ws` uses |

New packages in `Cargo.lock` (transitive, via axum `ws`): `tokio-tungstenite`
0.29.0 (MIT), `tungstenite` 0.29.0 (MIT OR Apache-2.0), `data-encoding`
2.11.0 (MIT), `sha1` 0.10.7 (MIT OR Apache-2.0; tungstenite's handshake hash,
next to the 0.11 used by auth). `rand`, `httparse`, `bytes` and `http` were
already in the tree. No TLS stack, no native code. Also uses `time` and `url`
(already workspace dependencies).

Deliberately **not** added: a Redis client (`RedisFanOut` speaks the few RESP2
commands pub/sub needs over `tokio::net::TcpStream`), `tokio-stream` /
`tokio-util` (bounded `mpsc` + `futures_util::stream::unfold` cover SSE).

## FB-24 (entitlements and quotas, ADR-0010)

No new third-party crate. The new workspace crate `forgebase-entitlements` uses
only dependencies already recorded here: `axum`, `serde`, `serde_json`,
`thiserror`, `time` (RFC 3339 on the wire), `tokio`, `tower-http` (`limit`),
`tracing`, `utoipa` and `uuid`. Calendar arithmetic (months, billing-cycle
anchors) is a 30-line civil-date conversion in the crate, not a date library.
`Cargo.lock` gains only the `forgebase-entitlements` entry.

## Addendum (MDF-52): stale statements after DDL

Two caches go stale on a pooled reader after another connection runs DDL:
sqlx's per-connection statement cache (it keeps the column list seen at prepare
time; `sqlx-sqlite` then panics `index out of bounds` decoding `SELECT t.*`
after `ADD COLUMN`) and SQLite's own in-memory schema, which is reloaded only
when a transaction starts, so even a fresh prepare sees the old columns.
Appending a version marker to the SQL text fixes only the first (tried, test
still failed).

Fix (`forgebase-db`): a shared schema generation counter, bumped before and
after every committed `TxMode::SchemaChange` and around migrations. The reader
pool's `before_acquire` hook compares it with the generation that connection
last saw; if behind it calls `clear_cached_statements()` and runs
`SELECT 1 FROM sqlite_schema LIMIT 1` to make SQLite reload the schema. Cost
when nothing changed: one atomic load and a map lookup per acquire, no database
round trip, and statement caching stays on for normal traffic. Rejected:
non-persistent star selects (does not fix SQLite's schema cache, loses caching)
and `PRAGMA schema_version` per acquire (round trip on every acquire).
The writer needs nothing: it runs the DDL itself. Regression test:
`crates/forgebase-db/tests/stale_statements.rs`.

## Phase 3 identity extensions (FB-25, 2026-11-09)

Outbound HTTPS for identity providers, Turnstile and SMS gateways, and LDAPS
(ADR-0009 §9 option b, decided by god; ADR-0015 §2). Behind the
`forgebase-auth` cargo feature `https` (on by default).

| Crate | Version | Licence | Used by | Purpose |
|---|---|---|---|---|
| rustls | 0.23.46 (no default features; `ring`, `std`, `tls12`) | Apache-2.0 OR ISC OR MIT | auth | TLS 1.2/1.3 client (no aws-lc, no logging) |
| ring | 0.17.14 | Apache-2.0 AND ISC | auth (via rustls) | crypto provider; C and assembly built with `cc`, no system libraries |
| rustls-webpki | 0.103.16 | ISC | auth (via rustls) | certificate path validation |
| rustls-pki-types | 1.15.1 | MIT OR Apache-2.0 | auth (via rustls) | certificate/key types, PEM parsing |
| untrusted | 0.9.0 | ISC | auth (via ring, webpki) | safe parsing of untrusted input |
| tokio-rustls | 0.26.6 (no default features; `ring`, `tls12`) | MIT OR Apache-2.0 | auth | async TLS streams over tokio |
| webpki-roots | 1.0.11 | CDLA-Permissive-2.0 | auth | Mozilla trust anchors (data); `deny.toml` exception for this crate only |
| getrandom | 0.2.17 | MIT OR Apache-2.0 | auth (via ring) | second getrandom major in the tree (ring 0.17 needs 0.2) |
| windows-sys / windows-targets | 0.52.x | MIT OR Apache-2.0 | (via ring, Windows targets only) | lock entries for the Windows build targets |

No LDAP crate: the LDAPv3 subset (simple bind, search, StartTLS) is
implemented in `forgebase-auth::ldap` (ADR-0015 §5).

## FB-52 (Vector Studio + Graph Studio, 2026-11-09)

Dashboard only. **No new npm or Rust dependencies.** The graph explorer draws an
inline SVG with a small in-house force-directed layout
(`dashboard/src/pages/graph/layout.ts`, ~70 lines, deterministic), so no
graph-drawing library (cytoscape, d3-force, sigma, ...) was added and no licence
review was needed. Revisit if graphs of more than a few hundred on-screen nodes
are needed; the likely pick then is `d3-force` (ISC).

Rust: one rename, no new crates. `forgebase-vector` named a response schema
`CollectionList`, which collided with the data API's `CollectionList` in the
merged `/api/openapi.json` (the vector list route was documented with the data
schema). It is now `VectorCollectionList`.
