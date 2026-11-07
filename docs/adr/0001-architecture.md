# ADR-0001: Phase 1 architecture

- Status: accepted
- Date: 2026-11-07
- Work package: FB-01 (Phase 1 foundation)
- Scope: crate boundaries, storage, SQLite concurrency, configuration, HTTP API policy, error model, data directory.

## Context

ForgeBase ships as one executable (SPEC §2, §24) built from a modular Cargo
workspace (SPEC §3, §25). Phase 1 must start from an empty folder, expose a
persistent embedded database and a basic admin dashboard (SPEC §26), and every
later phase builds on the decisions below. Correctness beats benchmark numbers
(SPEC §20).

## 1. Crates and dependency direction

Phase 1 creates only the crates it needs. Later crates (`forgebase-sql`,
`-documents`, `-auth`, ...) are added by their work packages; there are no
empty stubs.

```text
forgebase-cli ──► forgebase-server ──► forgebase-api ──► forgebase-db ──► forgebase-migrations ──► forgebase-core
      │                  │                   │                                                         ▲
      └──────────────────┴───────────────────┴──► forgebase-observability ────────────────────────────┘
```

| Crate | Owns | Must not |
|---|---|---|
| `forgebase-core` | config model + precedence, error model (`ErrorKind`, `ErrorCode`), data-dir layout, build info | depend on any other ForgeBase crate, or on sqlx/axum/tokio |
| `forgebase-observability` | `tracing` subscriber, Prometheus recorder, metric names | know about HTTP or SQL |
| `forgebase-migrations` | embedded system migrations, checksums, bookkeeping, atomic apply | open databases itself (it receives a connection) |
| `forgebase-db` | SQLite pools, pragmas, WAL, integrity/stats, system metadata, pre-migration snapshots | leak `sqlx` pools or SQLite types to callers |
| `forgebase-api` | Axum router, handlers, error envelope, request ids, metrics middleware, embedded dashboard | bind sockets or own process lifecycle |
| `forgebase-server` | startup sequence, data-dir lock, listener, graceful shutdown | parse CLI flags |
| `forgebase-cli` | the `forgebase` binary, clap commands, `doctor` | contain business logic other crates need |

Rules: dependencies point one way (towards `core`); no cycles; a crate only
uses another crate's public API. Workspace lints forbid `unsafe` and require
docs on public items.

## 2. Storage abstraction (SQLite now, PostgreSQL later)

- Embedded mode stores all canonical structured data in one SQLite file.
- Every SQLite detail stays inside `forgebase-db`. Callers use `Database`
  methods; no crate outside `forgebase-db` (and `forgebase-migrations`, which
  receives a connection from it) imports `sqlx`.
- Phase 2 (FB-10) puts a backend trait in front of `Database` (records,
  collections, transactions, query plans). Its SQL is generated per dialect;
  user filters compile to parameterised SQL, never string concatenation.
  A PostgreSQL backend (server mode) implements the same trait later; nothing
  above `forgebase-db` changes.
- Engines that are not relational (object blobs, ANN vector indexes) are not
  forced into SQLite (SPEC §2): blobs live in `storage/`, derived indexes in
  `indexes/` (see §8).

## 3. SQLite concurrency: WAL, one writer, read pool

| Setting | Value | Why |
|---|---|---|
| `journal_mode` | `WAL` | readers never block the writer and vice versa; startup fails if WAL cannot be enabled |
| writer | pool of exactly **1** connection | serialises writes in-process; writers queue on the pool (30 s acquire timeout) instead of spinning on `SQLITE_BUSY` |
| readers | pool of `read_pool_size` (default 8) connections with `query_only = ON` | parallel consistent snapshots; a reader can never write by mistake |
| `foreign_keys` | `ON` on every connection | referential integrity (SPEC §20) |
| `busy_timeout` | 5000 ms (configurable) | covers other processes (CLI `migrate`, `doctor`, backups) |
| `synchronous` | **`FULL`** (configurable to `NORMAL`) | see below |
| `optimize_on_close` | writer only | keeps planner statistics fresh |

**Why `synchronous = FULL`.** With WAL, `NORMAL` survives application
crashes but can lose the last committed transactions on power loss or an OS
crash, because the WAL is fsynced only at checkpoints. SPEC §20 says *never
report an operation as successful before its required durable state is
committed*, so the default fsyncs the WAL on every commit. Operators who accept
that window for throughput can set `database.synchronous = "normal"`.

Multi-statement writes use `BEGIN IMMEDIATE` so the write lock is taken up
front and a transaction never fails half-way on lock upgrade. On graceful
shutdown the server runs `wal_checkpoint(TRUNCATE)` so the data directory is
left as one self-contained database file.

One server per data directory: a running server holds an exclusive OS lock on
`.forgebase.lock` (released by the OS even on crash). `migrate` and `init`
take the same lock; read-only tools (`status`, `doctor`) do not.

## 4. Migrations

- System migrations are SQL files in `migrations/system/NNNN_name.sql`,
  embedded at build time and registered in `SYSTEM_MIGRATIONS` (a test fails if
  a file is not registered).
- `_forgebase_migrations(version, name, checksum, applied_at, execution_ms)`
  records each applied migration with the SHA-256 of its text.
  `PRAGMA user_version` mirrors the version for external tools.
- Each migration runs in its own `BEGIN IMMEDIATE` transaction together with
  its bookkeeping row and a `PRAGMA foreign_key_check`: all or nothing.
  Re-checked inside the transaction, so two processes cannot apply it twice.
- Refusals: an edited shipped migration (checksum mismatch) and a database
  migrated by a newer binary (unknown version) stop startup with a clear error.
- Forward-only. Rollback strategy: before applying pending migrations to a
  database that already has some, a consistent snapshot is written with
  `VACUUM INTO backups/pre-migrate-v<N>-<time>.db`; restoring it is the
  rollback. Full backup/restore commands come with FB-83.
- `serve` and `init` apply pending migrations automatically; `migrate` does it
  explicitly; `migrate --status` lists state without changing anything.

## 5. Configuration precedence

**CLI flags > `FORGEBASE_*` environment > `forgebase.toml` > defaults.**

- Config file: `--config`, else `FORGEBASE_CONFIG`, else `./forgebase.toml`
  if present. An explicitly named file that is missing is an error.
- Unknown keys in the file and unknown `FORGEBASE_*` variables are errors, so
  typos never pass silently. Values are range-checked.
- Relative `data_dir` resolves against the config file's directory when it
  comes from the file, against the working directory when it comes from a flag
  or the environment.
- Variables: `FORGEBASE_DATA_DIR`, `_HOST`, `_PORT`, `_REQUEST_TIMEOUT_SECS`,
  `_SHUTDOWN_TIMEOUT_SECS`, `_DB_SYNCHRONOUS`, `_DB_BUSY_TIMEOUT_MS`,
  `_DB_READ_POOL_SIZE`, `_LOG_LEVEL`, `_LOG_FORMAT`, `_CONFIG`.
- Default bind is `127.0.0.1:8090`. Binding elsewhere logs a warning until
  admin authentication lands (FB-23).

## 6. HTTP API paths and versioning

| Prefix | Use | Versioned |
|---|---|---|
| `/health`, `/health/ready` | liveness / readiness probes | no (stable forever) |
| `/metrics` | Prometheus text format | no |
| `/api/system/*` | instance management (info now; backups, settings later) | no prefix; additive changes only |
| `/api/v1/*` | data APIs from Phase 2 (collections, records, auth, files, ...) | yes |
| `/admin/*` | embedded dashboard SPA | n/a |

Policy: inside a version, changes are additive only (new fields, new
endpoints, new optional parameters). Removing or changing the meaning of a
field needs `/api/v2/*`, and `/api/v1/*` stays served for at least one minor
release with a `Deprecation` header. Clients must ignore unknown fields. The
Phase 1 contract (PLAN §5) is fixed; changing it requires telling god first.

## 7. Error model

- Every crate has an explicit error enum (`thiserror`), implementing
  `ErrorCode`: a stable snake_case `code` and an `ErrorKind`.
- `ErrorKind` → HTTP status: `InvalidInput` 400, `Unauthenticated` 401,
  `Forbidden` 403, `NotFound` 404, `Conflict` 409, `Unavailable` 503,
  `Internal` 500.
- Every error response, including router 404/405, timeouts (408) and caught
  panics (500), is
  `{"error":{"code":"…","message":"…","request_id":"…"}}` with the same id in
  the `x-request-id` header.
- `Internal` errors are logged in full but reach clients only as
  `internal server error` (no SQL, paths or stack traces).
- Request ids: a client-sent `x-request-id` is kept only if it is 1–128 chars
  of `[A-Za-z0-9._:-]`; otherwise a UUID v4 is generated. The id is on the
  request's tracing span, so every log line of a request carries it.
- Codes are part of the API and never renamed once released.

## 8. Data directory layout; canonical vs derived data

```text
<data_dir>/                    created 0700 on Unix
├── forgebase.db (+ -wal/-shm) canonical: all structured data; created 0600
├── storage/                   canonical: object blobs (Phase 4)
├── indexes/                   derived: ANN/vector and other rebuildable indexes (Phase 6)
├── backups/                   pre-migration snapshots (and FB-83 backups)
├── tmp/                       staging for in-flight writes; safe to delete when stopped
└── .forgebase.lock            held by the running server
```

- **Canonical** data (the database file and `storage/`) is the source of
  truth and must be backed up.
- **Derived** data (`indexes/`) is never the only copy of anything; it can be
  deleted and rebuilt from canonical data (SPEC §27 #23). A missing `indexes/`
  is a warning in `doctor`, not a failure.
- Multi-component operations that touch external storage use staging in
  `tmp/` plus commit markers and background reconciliation (Phase 4).

## 9. Observability

- `tracing` with a compact human format or JSON lines (`log.format`),
  filter from `log.level`; request spans carry method, path (no query string)
  and request id.
- Prometheus via the `metrics` facade: `forgebase_http_requests_total`,
  `forgebase_http_request_duration_seconds` (histogram, labelled by route
  template to bound cardinality), `forgebase_http_requests_in_flight`,
  `forgebase_build_info`, `forgebase_uptime_seconds`,
  `forgebase_db_size_bytes`, `forgebase_db_wal_size_bytes`,
  `forgebase_db_schema_version`.
- `/health` = liveness, `/health/ready` = database answers and is fully
  migrated, and `storage/` is writable.

## 10. Admin dashboard embedding

`dashboard/dist` (Vite, `base: "/admin/"`) is compiled into the binary with
`rust-embed` (`allow_missing`), so `cargo build` succeeds without it and
`/admin/` then serves a short "not built" page. Hashed files under `assets/`
are cached immutably; `index.html` revalidates via a strong ETag; unknown
extension-less paths return `index.html` for client-side routing. A build
script watches `dashboard/dist` (or `dashboard/` until `dist` exists) so a new
dashboard build is re-embedded without `cargo clean`.

## Consequences

- One writer bounds write throughput to one connection; acceptable for
  embedded mode and measured in FB-81. Server mode (PostgreSQL) lifts it.
- `synchronous = FULL` costs one fsync per commit; operators can opt into
  `NORMAL`.
- Because `forgebase-db` hides `sqlx`, Phase 2 extends `forgebase-db`'s API
  (or the FB-10 trait) instead of reaching for pools.
