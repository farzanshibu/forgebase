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
