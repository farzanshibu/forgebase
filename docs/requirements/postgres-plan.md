# Plan: bundled PostgreSQL and row-level security (FB-130 to FB-157)

Design: [ADR-0028](../adr/0028-bundled-postgres.md) (PostgreSQL backend) and
[`rls-coverage.md`](rls-coverage.md) (gaps G1–G19 and native RLS).
Written by FB-95 on 2026-11-11.

Human rule: build in parallel. Every package below names the files it owns.
Two packages in the same wave never own the same file; where a one-line
shared edit is unavoidable it is called out under "Shared edits".

Model tier: **High** = `claude-opus-5-5` (design or security critical),
**Normal** = `claude-sonnet-5-5`. Each package is sized for one temp in one
session (one crate or a tight group of files, its tests, its ADR addendum).

## Rules for every package

1. Work in your worktree; never push; god merges.
2. A package is done when its crates' tests pass on **both backends** once
   the backend exists (from wave 2 on: `FORGEBASE_TEST_BACKEND=postgres`),
   `cargo clippy --all-targets -- -D warnings` is clean, and its ADR addendum
   is written.
3. **Migrations**: take the next free number when god merges (god renumbers if
   two packages collide; in-flight FB-91/FB-93 may take numbers first). Before
   FB-133 merges, add the SQLite file only; FB-133 writes PostgreSQL twins for
   every SQLite migration on `main` when it merges. After FB-133, every
   migration is a pair (`migrations/system/NNNN_x.sql` and
   `migrations/postgres/NNNN_x.sql`); a PostgreSQL-only change gets a SQLite
   twin containing only a comment.
4. New SQL uses `Dialect` methods (ADR-0028 §7.4) or SQL that is valid on
   both engines; never a new SQLite-only construct.
5. Every RLS fix ships a failing-before test in
   `crates/forgebase-server/tests/security_bypass.rs` (add your test in a new
   file `crates/forgebase-server/tests/security_<package>.rs` that reuses
   `security_common`, so parallel packages do not edit the same test file).
6. New third-party crates: declare versions in your own crate's
   `Cargo.toml` rather than in the root `[workspace.dependencies]` (god
   normalises at merge), so parallel packages do not collide there; god
   regenerates `Cargo.lock` at merge. Builds are `--offline`: `sqlx-postgres
   0.9.0`, `tar`, `zstd`, `object` and `rustix` are already in the local
   registry cache (checked 2026-11-11); anything else needs one online
   `cargo fetch`, so ask god before adding it.
7. The PostgreSQL engine for local tests comes from
   `FORGEBASE_PG_ENGINE_DIR` (any PostgreSQL 18 with pgvector, e.g. a theseus
   archive plus a local pgvector build, or Homebrew) until FB-130's archives
   exist.

## Waves at a glance

| Wave | Starts when | Packages (all parallel inside a wave unless noted) |
|---|---|---|
| 1 | now | FB-130, FB-131, FB-132, FB-133*, FB-134, FB-135, FB-136, FB-137 |
| 1b | FB-134 merged | FB-138, FB-139, FB-140, FB-141, FB-142** |
| 2 | FB-131 and FB-133 merged, and the wave-1/1b package of the same crate merged | FB-143, FB-144, FB-145, FB-146, FB-147, and FB-148 (needs FB-132, FB-90, FB-91) |
| 3 | wave 2 merged (FB-149 also needs FB-130) | FB-149, FB-150, FB-151, FB-152 |
| 4 | wave 3 merged | FB-153, FB-154, FB-155, FB-156, FB-157 |

\* FB-133 starts in wave 1 (it writes SQL and its parity test) but merges after
FB-131. \*\* FB-142's functions part waits for FB-93 to merge.

Critical path: FB-131 → FB-143/FB-144 → FB-152 → FB-153. FB-130 (engine
builds) runs beside everything and must be done before FB-149.

## v0.3 cut (god, 2026-11-11)

The human asked for PostgreSQL in the v0.3 release.

- **In v0.3:** FB-130 to FB-153 and FB-155: waves 1, 1b, 2 and 3, direct
  logins and extensions, and the Database Studio for PostgreSQL. Together these
  make PostgreSQL a supported, bundled backend with row rules everywhere, CI on
  both backends, backups, `db convert` and native RLS.
- **Deferred to v0.4:** FB-154 engine major upgrades (needed only when moving
  from PostgreSQL 18 to 19), FB-156 point-in-time recovery, and FB-157
  concurrent writers (it runs alone, last, and changes the writer model).

**Human decisions (2026-11-11):**
1. The default download embeds the engine, and a `-lite` download ships
   without it.
2. Bundle PostgreSQL 18 now, and move to 19 after 19.1 (FB-154, in v0.4).
3. Direct psql and driver logins ship in v0.3 with `native_rls=logins`,
   through the Unix socket and `forgebase db psql` only. TCP stays off until an
   operator turns it on.

4. **PostgreSQL is the default backend** for new projects. SQLite stays as an
   option: a config switch and the `-lite` download.
5. On PostgreSQL, features run on **PostgreSQL extensions**: vector on
   pgvector, graph on Apache AGE (openCypher), queues on pgmq, scheduled jobs
   on pg_cron, and full-text and fuzzy search on built-in full-text search
   plus pg_trgm. All of them are bundled in the engine.
6. **Read replicas and sharding ship in v0.3.** ForgeBase manages read
   replicas: reads go to replicas and writes to the primary. Sharding is
   **ForgeBase tenant sharding**: each tenant or project is placed on one of
   several PostgreSQL servers, and ForgeBase routes requests to it. Citus is
   not used (AGPL). Design: ADR-0029 (FB-158), with packages FB-160 onward.

**Disk.** The build machine has about 35 GB free.
- Rust temps build with `CARGO_PROFILE_DEV_DEBUG=0` and
  `CARGO_PROFILE_TEST_DEBUG=0`, and delete their `target/` at done.
- At most 4 Rust temps run at once. Dashboard-only temps do not count.
- Batches inside a wave follow the critical path:
  - first FB-131 and FB-134 (they gate wave 2 and wave 1b);
  - then FB-132 and FB-133;
  - then FB-130, FB-135, FB-136 and FB-137.

**Local engine** until FB-130's archives exist: `FORGEBASE_PG_ENGINE_DIR=/opt/homebrew/opt/postgresql@18`
(PostgreSQL 18.3, without pgvector). Vector tests skip when `vector` is not
available, and FB-130 or FB-139 adds a pgvector build.

## Wave 1

### FB-130 — Engine build pipeline and relocation spike (High)

- **Scope**: `postgres-engine.yml` workflow building PostgreSQL 18 + pgvector +
  the contrib list for the five targets (ADR-0028 §1, §2.2); Linux on Alpine
  with the padded `PT_INTERP` (§2.3), macOS with `@loader_path` and ad-hoc
  re-signing, Windows with meson/MSVC; archive layout with an inner file
  manifest (path, size, SHA-256, mode); `engine.lock`; licence files
  (§2.4); release `engine-pg18.x-r1` with `SHA256SUMS` and provenance
  attestations; smoke tests per target and the Linux distro matrix
  (Debian 12, Ubuntu 22.04, UBI 9, Alpine 3.20, distroless). The spike
  relocates with `patchelf --set-interpreter` in the smoke test; the Rust
  patcher is FB-132's.
- **Decides**: whether the musl-loader relocation works on every host, or
  the glibc 2.28 fallback is needed. Replaces the size estimates in ADR-0028
  §2.2 and §3.2 with measured numbers (ADR addendum).
- **Owns**: `.github/workflows/postgres-engine.yml`, `deploy/postgres-engine/**`,
  `engine.lock`, `THIRD_PARTY_LICENSES/postgres/**`.
- **Depends on**: nothing. No Rust.

### FB-131 — `forgebase-db`: PostgreSQL backend (High)

- **Scope**: `DbTarget`, `Database` as a facade over `SqliteDatabase` and
  `PgDatabase` (ADR-0028 §7.2); `PgDatabase` (§7.3): one-connection writer and
  read pool, `TxMode`, drop-without-commit, placeholder translator with
  tests (strings, dollar quotes, comments, bare `?`), describe-driven
  binding and decoding, SQLSTATE → `DbError` (new `Unavailable`), `explain`,
  `statement_writes`, slow-query log shared with SQLite, `info`/`ping`/
  `integrity_check` (amcheck)/`meta_*`/`ensure_instance`; every `Dialect`
  method of §7.4 with SQLite implementations returning today's SQL and
  PostgreSQL implementations; `PgDialect`; `testing` module
  (`test_backend()`, one engine per test binary, template databases,
  ADR-0028 §7.7). `migrate()` on PostgreSQL calls
  `forgebase_migrations::postgres::migrate` (FB-133) and returns
  `Unsupported` until FB-133 merges.
- **Owns**: `crates/forgebase-db/**`, workspace `Cargo.toml` (`sqlx`
  `postgres` feature, `rustls`), `deny.toml`.
- **Depends on**: nothing (tests use `FORGEBASE_PG_ENGINE_DIR` or
  `FORGEBASE_TEST_PG_URL`).

### FB-132 — `forgebase-pgengine`: run the bundled engine (High)

- **Scope**: new crate. `bundled-postgres` feature and `build.rs` (embed
  `FORGEBASE_PG_ENGINE_ARCHIVE`, verify against `engine.lock`, no network);
  extraction with the `zstd` + `tar` crates, manifest verification, `.partial` +
  rename, read-only files; `PT_INTERP` patcher (`object` crate) and the
  long-path fallback (ADR-0028 §2.3); `initdb` with the flags of §4.2;
  bootstrap roles and `roles.secret`; `forgebase-postgres.conf` with the deny
  list; spawn, readiness, stderr → `tracing`, fast/immediate shutdown,
  stale `postmaster.pid` handling, Linux parent-death signal (`rustix`),
  Windows job object, restart with backoff (§4.3); root refusal; minor
  upgrade by engine swap (§4.5). API: `Engine::prepare(layout, cfg) →
  EngineHandle { url(role), stop(), health() }`.
- **Owns**: `crates/forgebase-pgengine/**` (new).
- **Depends on**: nothing to start (tests use `FORGEBASE_PG_ENGINE_DIR`);
  FB-130 archives to test embedding end to end. Shared edit: one line in
  `deny.toml` if new crates need licences listed — coordinate with FB-131 by
  adding it in your own `crates/forgebase-pgengine/deny-notes.md` and letting
  god merge.

### FB-133 — PostgreSQL system schema, migration runner, parity test (High)

- **Scope**: `migrations/postgres/0025_baseline.sql` (ADR-0028 §7.6) with the
  type mapping of §7.4, schemas `forgebase`/`public`, PL/pgSQL functions for
  the 22 triggers, the same seed rows; PostgreSQL twins for every SQLite
  migration that reached `main` before this merges; the PostgreSQL runner in
  `forgebase-migrations` (`postgres::migrate(conn, …)`, bookkeeping,
  checksums, one transaction per migration, `SET ROLE forgebase_owner`); the
  pairing check; the schema-parity test (§7.6). Uses `sqlx` with the
  `postgres` feature at crate level (`crates/forgebase-migrations/Cargo.toml`).
- **Owns**: `migrations/postgres/**`, `crates/forgebase-migrations/**`.
- **Depends on**: starts now; merges after FB-131. Shared edit: the one-line
  call in `PgDatabase::migrate` (`crates/forgebase-db/src/`), done after
  rebasing on FB-131.

### FB-134 — Policy: resource-scoped engine permissions and rules (High)

- **Scope**: the framework the engine RLS fixes use (rls-coverage G7, G9,
  G10, G14, G16). Permission keys with a resource segment
  (`engine:kv_read:<ns>`, `engine:vector_read:<collection>`,
  `engine:queue_consume:<subscription>`, …), with `engine:<action>` still
  granting every resource; `Authorizer::authorize_resource(ctx, action,
  &ResourceRef) -> Decision` (default method in `forgebase-data::access`,
  falls back to `authorize`), which may return `Filter` with a predicate over
  the resource's item fields; resource rules stored in a new table
  (`_forgebase_resource_rules`: kind, name, action, rule) with validation at
  save time against a field list the engine supplies; HTTP routes under
  `/api/v1/policies/resources/…`; OpenAPI.
- **Owns**: `crates/forgebase-policy/**`, `crates/forgebase-data/src/access.rs`,
  a new SQLite migration.
- **Depends on**: nothing.

### FB-135 — Data core and SQL editor fixes (High)

- **Scope**: G12 (`describe_table` hides system tables from non-privileged
  callers); G13 (versions always visibility-checked, including tombstones;
  cascade semantics documented); G11 data-core part (SQL editor result
  masking of `password`/`token_key` columns unless `show_secrets`, audited).
- **Owns**: `crates/forgebase-data/src/{sql.rs,records.rs,guard.rs}`,
  `crates/forgebase-api/src/v1/sql.rs`.
- **Depends on**: nothing.

### FB-136 — Billing, credits and realtime scoping (High)

- **Scope**: G18 (realm-level fallback only for subjects in the caller's
  realm; realm filter on "all customers" lists) in billing and credits; G17
  (implicit `t:<tenant>:` channel namespace for tenant-bound callers,
  `@request.tenant` in channel rules, `global:` prefix).
- **Owns**: `crates/forgebase-billing/src/{service.rs,subscription.rs,billing.rs}`,
  `crates/forgebase-credits/src/service.rs`, `crates/forgebase-realtime/**`.
- **Depends on**: nothing.

### FB-137 — Realm-scoped administrators (High)

- **Scope**: G19. Realm administrators through realm bindings with an
  `identity:admin` key for the identity admin APIs (users, keys, sessions,
  realm identity, audit) limited to their realms; impersonation limited to
  the admin's realms; platform administrators unchanged.
- **Owns**: `crates/forgebase-auth/src/{impersonation.rs,identity_admin.rs}`,
  `crates/forgebase-auth/src/service/mod.rs` (`require_admin` and callers).
- **Depends on**: nothing.

## Wave 1b (after FB-134)

### FB-138 — KV and RESP tenancy and namespace rules (High)

- **Scope**: G6 (optional tenant binding on API keys and PATs, validated by
  policy step 2; RESP sessions use it; `tenant_required` for non-privileged
  callers without a tenant; explicit `shared.<ns>` namespaces), G7
  (namespace permissions through FB-134), G8 (per-tenant stats, optional
  `kv.tenant_max_bytes`).
- **Owns**: `crates/forgebase-kv/**`, `crates/forgebase-resp/**`,
  `crates/forgebase-auth/src/machine.rs` (key tenant binding), a new SQLite
  migration.
- **Depends on**: FB-134.

### FB-139 — Vector and graph rules (High)

- **Scope**: G9 (vector collection rules as a search pre-filter;
  `source_collection` re-check against the record View rule and field mask;
  `tenant_required`), G10 (graph label rules in traversal SQL;
  `tenant_required`).
- **Owns**: `crates/forgebase-vector/**`, `crates/forgebase-graph/**`.
- **Depends on**: FB-134.

### FB-140 — Object storage RLS (High)

- **Scope**: G1 (bucket-level ACL matching), G14 (tenant-scoped buckets,
  object rules over `key`, `created_by`, `size`, `content_type`, path
  segments, via FB-134), G15 (docs), G11 storage part (seal the URL signing
  key with the auth keyring; migrate the existing key).
- **Owns**: `crates/forgebase-storage/**`, a new SQLite migration.
- **Depends on**: FB-134.

### FB-141 — Queues RLS (High)

- **Scope**: G2 (record-source subscriptions evaluated at delivery as their
  owner, field mask applied), G16 (tenant-scoped topics, per-topic and
  per-subscription permissions via FB-134, subscription ownership).
- **Owns**: `crates/forgebase-queue/**`, a new SQLite migration.
- **Depends on**: FB-134.

### FB-142 — Jobs RLS and internal invocation contexts (High)

- **Scope**: G3 (job `tenant`, list and channel filtered by tenant and
  creator, `engine:read_jobs:all`), G4 (reserved job kinds; `function.invoke`
  checks invoke access at enqueue and runs as the recorded enqueuer), G5
  follow-up (hooks and jobs invoke functions with an explicit recorded
  context).
- **Owns**: `crates/forgebase-jobs/**`, `crates/forgebase-functions/src/jobs.rs`,
  `crates/forgebase-functions/src/hooks.rs`, a new SQLite migration.
- **Depends on**: FB-134; the functions part after FB-93 merges (FB-93 owns
  the host-call API).

## Wave 2: everything on PostgreSQL

Each port package switches its crates' `tests/common` helpers to
`forgebase_db::testing::test_backend()` and makes those suites green on both
backends.

### FB-143 — Query compiler, records, lists and SQL editor on PostgreSQL (High)

- **Scope**: `forgebase-query` through the dialect (ILIKE, NULLS order, JSON
  paths, `json_each` patterns, keyset cursors); records, lists, aggregates,
  expand, versions, change snapshots on PostgreSQL; SQL editor: PostgreSQL
  tokenizer deny list, read/write classification, read path on the reader
  role (ADR-0028 §7.5); semantic tests of §7.4.
- **Owns**: `crates/forgebase-query/**`,
  `crates/forgebase-data/src/{list.rs,records.rs,sql.rs,changes.rs,guard.rs}`,
  `crates/forgebase-data/tests/{common/mod.rs,scenarios.rs,security.rs,send_futures.rs}`.
- **Depends on**: FB-131, FB-133, FB-135.

### FB-144 — Collections, schema changes, indexes and full text on PostgreSQL (High)

- **Scope**: DDL generation from the type mapping; catalog introspection via
  `pg_catalog`; schema changes as `ALTER TABLE` in one transaction instead
  of rebuilds; views with `security_invoker`; verification step; index
  manager (all index kinds, stats, sizes, rebuild, analyze, explain,
  recommendations); full text with `tsvector` + GIN; identifier length
  hashing; `forgebase-schema` name rules for PostgreSQL.
- **Owns**: `crates/forgebase-data/src/{ddl.rs,collections.rs,indexes.rs,catalog.rs,lib.rs}`,
  `crates/forgebase-data/tests/schema_changes.rs` (calls `test_backend()`
  directly if FB-143's helper switch has not merged), `crates/forgebase-schema/**`.
- **Depends on**: FB-131, FB-133. Parallel with FB-143 (disjoint files; both use
  FB-131's dialect).

### FB-145 — Auth, policy, customize, credits, entitlements, billing on PostgreSQL (Normal)

- **Scope**: replace `INSERT OR IGNORE/REPLACE` with portable `ON CONFLICT`,
  `json_each` with the dialect, fix type mismatches the PostgreSQL suite
  shows; no behaviour change.
- **Owns**: `src/**` and `tests/**` of `forgebase-auth`, `forgebase-policy`,
  `forgebase-customize`, `forgebase-credits`, `forgebase-entitlements`,
  `forgebase-billing`.
- **Depends on**: FB-131, FB-133, and FB-134, FB-136, FB-137, FB-138 (they edit
  these crates first).

### FB-146 — Jobs, queues and realtime on PostgreSQL (High)

- **Scope**: `rowid` tie-breaks → explicit sequence columns; queue delivery
  history through the dialect's JSON array functions (or in Rust);
  `last_insert_id` → `RETURNING`; commit-order cursor invariants asserted
  under the single writer (ADR-0011, ADR-0022), with a test that fails if a
  second writer is ever configured.
- **Owns**: `src/**` and `tests/**` of `forgebase-jobs`, `forgebase-queue`,
  `forgebase-realtime`.
- **Depends on**: FB-131, FB-133, FB-136, FB-141, FB-142.

### FB-147 — KV, graph, vector, storage and functions on PostgreSQL (Normal)

- **Scope**: id lists through `in_json_list`; graph walk rewritten for
  PostgreSQL (one recursive reference, `LIMIT` outside the CTE, cycle
  guard); vector hybrid search with `tsvector` instead of FTS5; storage
  `INSERT OR IGNORE`; functions' SQL.
- **Owns**: `src/**` and `tests/**` of `forgebase-kv`, `forgebase-graph`,
  `forgebase-vector`, `forgebase-storage`, `forgebase-functions`.
- **Depends on**: FB-131, FB-133, FB-138, FB-139, FB-140, FB-142, FB-93.

### FB-148 — Configuration, CLI and server wiring (Normal)

- **Scope**: `[database] backend`, `[database.postgres]` keys and env
  variables (ADR-0028 §6.1) registered in FB-91's feature registry;
  backend marker (§6.3); `forgebase init --database`, `--database-url`
  (§6.2); server start/stop of the engine around `Database::open`;
  `status`, `doctor` (engine checks, amcheck), `/api/system/info`,
  `/health/ready` for PostgreSQL; `forgebase db status`, `db psql`;
  `forgebase-bench` on PostgreSQL.
- **Owns**: `crates/forgebase-core/src/config.rs`, `crates/forgebase-cli/**`
  (except backup and convert commands), `crates/forgebase-server/src/lib.rs`,
  `crates/forgebase-api/src/{lib.rs,routes.rs}`, `crates/forgebase-bench/**`,
  `forgebase.example.toml`, `docs/deployment.md`.
- **Depends on**: FB-131, FB-132; FB-90 and FB-91 merged (they edit `init` and
  the config registry). Parallel with FB-143 to FB-147.

## Wave 3

### FB-149 — CI on both backends, release embedding, Docker (Normal)

- **Scope**: `test-postgres` job (Linux every push; macOS arm64 and Windows
  nightly); black-box suites (`tests/integration`, the 25 SPEC scenarios)
  with `--database postgres`; switch `forgebase-api/tests` and
  `forgebase-server/tests` helpers; release workflow downloads the pinned
  engine and builds with `bundled-postgres`, plus `-lite` archives;
  Dockerfile and compose example (`shm_size` note).
- **Owns**: `.github/workflows/{ci.yml,release.yml}`, `Dockerfile`,
  `docker-compose.yml`, `tests/**`, `crates/forgebase-api/tests/**`,
  `crates/forgebase-server/tests/**` (helpers only).
- **Depends on**: wave 2, FB-130.

### FB-150 — Backups, restore and snapshots on PostgreSQL (Normal)

- **Scope**: ADR-0028 §5: `pg_basebackup` inside the backup directory,
  manifest `engine` field, `pg_verifybackup`, restore swap of
  `postgres/data` with the journal, snapshots, pre-migration snapshots,
  `doctor` checks, `forgebase db dump`, logical backup for external mode.
- **Owns**: `crates/forgebase-backup/**`, the backup/restore/snapshot
  subcommands in `crates/forgebase-cli/src/dataops.rs`.
- **Depends on**: FB-148. Shared edit: one `Command` variant in
  `crates/forgebase-cli/src/main.rs` (for `db dump`); rebase on FB-151's if
  both land together.

### FB-151 — `forgebase db convert` (High)

- **Scope**: ADR-0028 §6.4 both directions: preconditions, `COPY`, type
  conversion, identity resets, index recreation and the raw-SQL index report,
  per-table count and SHA-256 verification, marker flip, keep the old file;
  refusal list for `--to sqlite`. Test: convert a database built by every
  crate's fixtures and run the suite against the result.
- **Owns**: `crates/forgebase-convert/**` (new), `crates/forgebase-cli/src/convert.rs`
  (new).
- **Depends on**: FB-148, wave 2. Shared edit: one `Command` variant in
  `crates/forgebase-cli/src/main.rs`.

### FB-152 — Native RLS: policy compiler, roles, `enforce` mode (High)

- **Scope**: rls-coverage §3.1–3.4 and §3.7: roles and schema privileges
  (PostgreSQL migration pair); `forgebase.claims()` and helper functions;
  native render mode in `forgebase-query`; policy generation and
  regeneration hook inside the data core's schema transaction; claims
  computed from the policy snapshot; relation-path `SECURITY DEFINER`
  helpers; `api` views and `INSTEAD OF` triggers; `Backend::begin_as` (default
  method, no-op on SQLite) for `SET LOCAL ROLE` + claims; modes
  `off|logins|enforce`; differential and spoofing tests; overhead measured.
- **Owns**: `crates/forgebase-policy/src/native/**` (new),
  `crates/forgebase-query/src/native.rs` (new), the hook call in
  `crates/forgebase-data/src/collections.rs`, `begin_as` in
  `crates/forgebase-db/src/{backend.rs,postgres/**}`.
- **Depends on**: wave 2 (FB-143, FB-144, FB-145), FB-134.

## Wave 4

### FB-153 — Direct logins, SQL editor roles, extensions (High)

- **Scope**: rls-coverage §3.5–3.6: `_forgebase_db_logins` and claims refresh
  on every snapshot reload; `forgebase db login …` and
  `/api/v1/system/db-logins`; `listen`, generated `pg_hba.conf`, TLS
  certificate; `db psql --as`; SQL editor as `forgebase_sql_admin` with "run
  as"; secret-column privileges (G11); `/api/v1/system/extensions` with the
  allow list.
- **Owns**: `crates/forgebase-policy/src/logins.rs` (new),
  `crates/forgebase-api/src/v1/{db_logins.rs,extensions.rs}` (new),
  `crates/forgebase-pgengine/src/hba.rs` (new), `crates/forgebase-data/src/sql.rs`
  (run-as path), a migration pair.
- **Depends on**: FB-152, FB-148.

### FB-154 — Engine major upgrades (High)

- **Scope**: ADR-0028 §4.6: `forgebase db upgrade-engine` with backup,
  `initdb`, `pg_upgrade --link`/`--copy`, `ANALYZE`, journalled swap,
  pgvector check; CI fixture from the previous major (FB-130 workflow built
  for PostgreSQL 17, or 18 → 19 once 19.1 exists).
- **Owns**: `crates/forgebase-pgengine/src/upgrade.rs` (new),
  `crates/forgebase-cli/src/upgrade.rs` (new).
- **Depends on**: FB-132, FB-150. Shared edit: one `Command` variant in
  `crates/forgebase-cli/src/main.rs`.

### FB-155 — Database Studio for PostgreSQL (Normal)

- **Scope**: dashboard: backend and engine version, connections,
  `pg_stat_statements` top queries, generated vs hand-written policies with
  the widening warning, direct logins (create, rotate, revoke), extensions,
  SQL editor role selector ("run as").
- **Owns**: `dashboard/src/**` (database studio routes only).
- **Depends on**: FB-153.

### FB-156 — Point-in-time recovery (Normal)

- **Scope**: WAL archiving to `backups/wal/`, retention, `restore --to <time>`
  from the newest base backup before it, `doctor` checks.
- **Owns**: `crates/forgebase-backup/src/pitr.rs` (new), engine settings in
  `crates/forgebase-pgengine/src/conf.rs`.
- **Depends on**: FB-150.

### FB-157 — Concurrent writers on PostgreSQL (High)

- **Scope**: audit every read-then-write transaction in every crate; `FOR
  UPDATE` / `SKIP LOCKED` for queue and job claims; commit-ordered cursors
  for realtime and queues (outbox numbered under an advisory lock, or a
  `pg_current_xact_id` watermark); `[database.postgres] writers` (default 1);
  stress tests comparing results with one writer.
- **Owns**: touches many crates; runs alone, after everything above.
- **Depends on**: all of the above.

## Parallel groups

| Group | Run together | Why it is safe |
|---|---|---|
| A (now) | FB-130, FB-131, FB-132, FB-133, FB-134, FB-135, FB-136, FB-137 | disjoint crates or files; FB-133 merges after FB-131; FB-134 and FB-135 split `forgebase-data` by file (`access.rs` vs `sql.rs`/`records.rs`/`guard.rs`) |
| B (after FB-134) | FB-138, FB-139, FB-140, FB-141, FB-142 | one crate group each; FB-138 touches only `machine.rs` in `forgebase-auth` (FB-137 owns the rest) |
| C (after FB-131, FB-133 and group B) | FB-143, FB-144, FB-145, FB-146, FB-147, FB-148 | ports split by crate, data core split by file; FB-148 owns core/cli/server/api |
| D (after C) | FB-149, FB-150, FB-151, FB-152 | CI vs backup vs convert vs native RLS; the one-line `main.rs` edits are rebased |
| E (after D) | FB-153, FB-154, FB-155, FB-156 | new files each; FB-157 runs alone at the end |

High tier: FB-130–FB-144, FB-146, FB-151–FB-154, FB-157 (21). Normal: FB-145,
FB-147–FB-150, FB-155, FB-156 (7).

## Open questions for the human

1. **Release binary**: embed the engine in the default download (+10–14 MB
   estimated) and publish `-lite` without it, or the reverse? Recommended:
   embed by default (SPEC's single-executable rule; "no separate install").
2. **PostgreSQL major**: start on 18 (stable, supported to 2030) or wait for
   19 (GA planned 2026-11-19)? Recommended: 18 now, move to 19 after 19.1 via
   FB-154, which proves the upgrade path.
3. **Direct logins**: ship them (FB-153) in the first PostgreSQL release, or
   API-only first? Recommended: ship with `native_rls = "logins"`, TCP off by
   default (socket and `db psql` only), so direct access is there but never
   network-exposed unless an operator opts in.
