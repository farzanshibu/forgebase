# ForgeBase database

ForgeBase keeps its structured data in PostgreSQL by default: the standard
release embeds PostgreSQL 18 and runs it for you. SQLite (one file, WAL mode) is
the optional alternative, and the default of the `-lite` release, which leaves
the engine out. Other data types sit beside the database in the same data
directory. This page covers the backends, the storage engines, the
data-directory files, the multi-model layout, the system migrations, and the
backup pointers.

## 0. Backends

| | PostgreSQL (bundled or your server) | SQLite |
|---|---|---|
| Default | Standard release | `-lite` release; `forgebase init --database sqlite` |
| Choose at init | `forgebase init`, `--database postgres`, `--database-url postgres://…` | `forgebase init --database sqlite` |
| Data | `<data_dir>/postgres/data` (bundled) or your server | `<data_dir>/forgebase.db` |
| Access | Unix socket only; TCP is off unless `[database.postgres] listen` is set. Windows has no socket: loopback TCP | in-process |
| Extras | Extensions as accelerators with a portable fallback (ADR-0029), `forgebase db psql` | none |

Which engine runs each feature on each backend, and what differs: [feature parity](feature-parity.md) (ADR-0034).

`forgebase init` asks `Database: [1] PostgreSQL (default, bundled server) [2]
SQLite (one file)` on a terminal and takes the build's default otherwise. The
data directory's backend marker (`forgebase.backend`) wins over the default at
every start: an existing SQLite project stays SQLite, and `forgebase status`
and `forgebase doctor` say so with the command that moves it,
`forgebase db convert --to postgres` (ADR-0028 §6.4). PostgreSQL cannot run as
root. [Deployment §3.5](deployment.md#35-database-postgresql-or-sqlite) has the
details and [Upgrading to 0.3](release-notes/0.3.md#upgrading-to-03) the steps.

`forgebase db convert --to postgres|sqlite` moves a stopped project to the
other backend: a verified table-level copy (counts and SHA-256 per table), a
snapshot of the source first (`--no-backup` skips it), and the old database
is kept. `--to sqlite` first converts the data the source keeps in
extensions (pgvector vectors, pgmq deliveries, pg_cron schedules) to portable
rows, in the source itself. `--no-prepare` skips that step and refuses when
such data exists. A `--to sqlite` that fails after that step leaves the
source on portable rows. That is harmless: it stays a valid PostgreSQL
project. In `auto` mode pg_cron takes the schedules back at the next start;
vectors and deliveries stay on portable rows until the `vector.convert` and
`queue.convert` jobs move them back to pgvector and pgmq.

§1 describes the SQLite engine; the other sections cover both backends and
say where they differ. The PostgreSQL engine's settings and commands are in
the deployment guide.

Decisions: [ADR-0001 §2–5](adr/0001-architecture.md) (engine, concurrency,
migrations), [ADR-0003](adr/0003-data-core.md) (collections and records),
[ADR-0005](adr/0005-kv.md) (key-value), [ADR-0006](adr/0006-graph.md) (graph),
[ADR-0012](adr/0012-vector.md) (vector), [ADR-0018](adr/0018-object-storage.md)
(object storage).

## 1. Engine and concurrency (SQLite)

| Setting | Value | Config key / env var |
|---|---|---|
| Journal | WAL. Startup fails if WAL cannot be enabled. | fixed |
| Writer pool | Exactly one connection. Writers queue in process rather than retry on busy. | fixed |
| Read pool | Parallel connections with `query_only = ON`. | `database.read_pool_size` (1–64, default 8); `FORGEBASE_DB_READ_POOL_SIZE` |
| Durability | `full` fsyncs the WAL on every commit. `normal` is faster but can lose the latest commits on power loss. | `database.synchronous`; `FORGEBASE_DB_SYNCHRONOUS` |
| Busy timeout | How long a connection waits for another process's lock (CLI `migrate`, `doctor`, backups). Max 600000 ms. | `database.busy_timeout_ms` (default 5000); `FORGEBASE_DB_BUSY_TIMEOUT_MS` |
| Foreign keys | On for every connection. | fixed |

Multi-statement writes take the write lock up front (`BEGIN IMMEDIATE`). On
graceful shutdown the server runs `wal_checkpoint(TRUNCATE)`, so the data
directory is left as one self-contained database file.

Only one server may use a data directory. The running server holds
`.forgebase.lock`. `migrate` and `init` take the same lock. On SQLite,
`status` and `doctor` do not. On the bundled PostgreSQL, `status`, `doctor`,
`admin`, `export` and `db` use a running server's engine through its socket;
when no server runs they take the lock, start the engine for the command and
stop it afterwards.

## 2. Files in the data directory

```text
<data_dir>/
├── forgebase.backend             which engine this directory holds
├── forgebase.db (+ -wal, -shm)  SQLite only, canonical: structured data
├── postgres/                     bundled PostgreSQL only:
│   ├── engine/<version>-…/       extracted engine (read-only); the first start unpacks it
│   ├── data/                     canonical: the cluster (PGDATA, 0700)
│   ├── run/                      Unix socket (not on Windows)
│   ├── forgebase-postgres.conf   settings ForgeBase writes on every start
│   ├── roles.secret              passwords of the ForgeBase roles (0600)
│   └── nodes/<name>/             other clusters on this host (replicas, shards)
├── topology.json                 PostgreSQL: database sets and nodes, kept by the server
├── storage/                      canonical: object blobs (when storage is in use)
├── auth/master.key               canonical: root key for auth secrets (see §5)
├── indexes/                      derived: rebuildable indexes
├── backups/                      pre-migration snapshots; snapshots/ of `forgebase backup`
├── tmp/                          staging for in-flight writes; safe to delete when stopped
└── .forgebase.lock               held by the running server
```

`auth/master.key` is created by the auth service on first start. It is not
in the `DataLayout` directory list, so `init` does not create it. Losing it
makes encrypted signing keys and TOTP seeds unreadable. Keep a copy of it
apart from the backups (§5).

`roles.secret` holds the passwords ForgeBase logs in to its cluster with.
Without it the server cannot reach the data; a backup of the bundled engine
includes it. `topology.json` lets a restart find the control primary after a
promote.

## 3. Multi-model data

One database holds several data models. Each one is a set of tables with
the `_forgebase_` prefix, except user collections. On PostgreSQL the system
tables are in schema `forgebase` and collection tables in `public`.

| Model | Where it lives | Notes |
|---|---|---|
| Records (collections) | One SQL table per user collection; definitions in `_forgebase_collections` and `_forgebase_schema_history`; `_forgebase_indexes`; history in `_forgebase_record_versions` | Typed fields; filter language compiled to parameterised SQL. ADR-0003. |
| Key-value | `_forgebase_kv_entries`, one row per key within a namespace | Strings, hashes, lists, sets, sorted sets, streams, TTL, LRU eviction under a memory budget. ADR-0005. |
| Graph | `_forgebase_graph_nodes`, `_forgebase_graph_edges` (indexed both directions) | Tenant-scoped. ADR-0006. |
| Vector | `_forgebase_vector_collections`, `_forgebase_vector_fields`, `_forgebase_vector_items`, `_forgebase_vector_values` | Canonical vectors are rows in the database (`_forgebase_vector_values`). The portable engine keeps them as bytes and its ANN indexes as derived, rebuildable files in `indexes/`. On a new PostgreSQL database with a usable pgvector, `auto` mode stores them in pgvector's `vec` column with an HNSW index per field in PostgreSQL. `vector.convert` moves them between the two. ADR-0012, ADR-0029 §1.4. |
| Object storage | Metadata in `_forgebase_storage_*`; bytes in `storage/` | Blobs are not stored in the database. ADR-0018. |
| Jobs and workflows | `_forgebase_jobs`, `_forgebase_job_events`, `_forgebase_job_schedules`, `_forgebase_job_workers`, `_forgebase_job_queues`, `_forgebase_job_workflows`, `_forgebase_job_deps`, `_forgebase_job_attempts` | A job is a row; every state change is a committed transaction. ADR-0007, ADR-0023. |
| Realtime | `_forgebase_realtime_outbox` | One row per record change, written in the same transaction as the change. ADR-0011. |

Rules both engines keep alike, so a project can move between them:

- **No NUL in text.** PostgreSQL's `text` cannot hold U+0000, so a text value
  containing it is refused on both engines (`400 invalid_statement`). This
  covers record fields, graph and vector properties, and KV keys
  (`kv_invalid_key`). Bytes go in blobs (or base64).
- **Field definitions are checked key by key.** Type options sit beside
  `type` (`{"name": "title", "type": "text", "max_length": 200}`). A key that
  is not a field key or an option of that type (an `options` object, a
  misspelt option, another type's option) is refused with `400 invalid_body`
  naming the field, instead of being dropped.

## 4. System migrations

Migrations are SQL files in `migrations/system/` (SQLite) and
`migrations/postgres/` (PostgreSQL). They are embedded in the binary and
applied at startup or by `forgebase migrate`. Numbers are strictly
increasing; 0010, 0032, 0034, 0036, 0037 and 0039 have no file on either
backend.

| File | Adds |
|---|---|
| `0001_initial.sql` | Metadata table |
| `0002_data_core.sql` | Collections, schema history, indexes, record versions |
| `0003_auth.sql` | Admins, auth keys, sessions, refresh tokens, one-time codes, MFA, recovery codes, service accounts, API keys, lockouts |
| `0004_policy.sql` | Realms, orgs, projects, environments, roles, role sets, memberships, teams, overrides, resource ACLs |
| `0005_kv.sql` | KV entries |
| `0006_graph.sql` | Graph nodes and edges |
| `0007_jobs.sql` | Jobs, job events, schedules, workers |
| `0008_oauth_webauthn.sql` | OAuth states and identities, WebAuthn credentials and challenges |
| `0009_entitlements.sql` | Features, plans, add-ons, usage packs, quota policies, subscriptions, billing sync events |
| `0011_realtime.sql` | Realtime outbox |
| `0012_vector.sql` | Vector collections, fields, items, values |
| `0013_impersonation.sql` | Impersonation support |
| `0014_identity.sql` | Realm identity, phones, SMS codes, account status |
| `0015_credits.sql` | Credit accounts, ledger, lots, allocations, rates, usage, cursors |
| `0016_functions.sql` | Functions, versions, secrets, logs, cursors |
| `0017_storage.sql` | Buckets, ACLs, blobs, objects, uploads, parts, settings |
| `0018_oidc_provider.sql` | OAuth clients, OIDC keys, requests, consents, families, codes, tokens, device codes |
| `0019_customize.sql` | Domains, branding, email templates and deliveries, legal agreements and acceptances |
| `0020_billing.sql` | Products, prices, tax rates, coupons, customers, subscriptions, orders, invoices and more |
| `0021_queue.sql` | Queue topics, subscriptions, messages, deliveries, dead letters, deduplication, rates, snapshots |
| `0022_jobs_long_tasks.sql` | Job queues, workflows, dependencies, attempts |
| `0023_identity_admin.sql` | Login (OAuth/OIDC) providers and SMS gateway per realm |
| `0024_billing_stores.sql` | Store products and purchases (App Store, Google Play, Creem) |
| `0025_queue_ready_order.sql` | Index that returns ready deliveries in sequence order |
| `0026_resource_rules.sql` | Rules of named engine resources |
| `0027_functions_js.sql` | JavaScript and TypeScript function versions, function KV rules |
| `0028_key_tenant.sql` | Tenant binding of API keys |
| `0029_storage_rls.sql` | Bucket tenant, object owner and folder columns |
| `0030_job_tenant.sql` | Tenant and invocation context of jobs, schedules and workflows |
| `0031_queue_rls.sql` | Queue tenancy, subscription owner context, record-source rows |
| `0033_db_topology.sql` | Database sets, nodes, tenant placements, moves, join tokens, transaction journal |
| `0035_job_queue_tenant.sql` | Tenant and creator of job queues and workers |
| `0038_schedule_fire.sql` | PostgreSQL: `forgebase.fire_schedule()`, which pg_cron calls. SQLite: nothing |
| `0040_native_rls.sql` | PostgreSQL: roles, the `api` schema and the functions of native row-level security. SQLite: nothing |
| `0041_index_builds.sql` | Index build state per database set |
| `0042_db_logins.sql` | Named direct logins (PostgreSQL also creates their roles and grants; the SQLite table stays empty) |
| `0043_db_node_agents.sql` | Agents of managed replicas on other hosts and the commands queued for them |

`migrations/postgres/` holds the PostgreSQL counterparts. `0025_baseline.sql`
is the schema SQLite has after `0001`–`0025`, written once for PostgreSQL.
From `0026` on every file has a PostgreSQL twin with the same number and
name; a change for one backend only gets a twin that does nothing on the
other. The test `every_sqlite_migration_has_a_postgres_twin` fails on a
missing twin, and `crates/forgebase-migrations/tests/schema_parity.rs`
migrates both backends to the head and fails on any difference in tables,
columns, indexes, triggers or seed rows.

The `_forgebase_migrations` table (`forgebase._forgebase_migrations` on
PostgreSQL) records each applied file with its name, SHA-256 checksum, time
and duration. Applying the PostgreSQL baseline records versions 1–25 with the
SQLite names and the baseline's checksum, so a version means the same schema
on both backends. On SQLite, `PRAGMA user_version` also mirrors the latest
version for external tools. PostgreSQL has no such field: the highest
`version` in that table is the schema version. Check the current state with:

```sh
forgebase migrate --status
```

### Rules for migrations

These rules are in [`migrations/README.md`](../migrations/README.md).

1. File names are `NNNN_snake_case_name.sql`. Register every SQLite file in
   `SYSTEM_MIGRATIONS` in `crates/forgebase-migrations/src/lib.rs` and every
   PostgreSQL file in `POSTGRES_MIGRATIONS` in
   `crates/forgebase-migrations/src/postgres.rs`; a test fails otherwise.
2. **Never edit a migration that has shipped.** A changed checksum makes the
   server refuse to start. Fix forward with a new migration.
3. On SQLite each migration runs in one `BEGIN IMMEDIATE` transaction with its
   bookkeeping row, followed by `PRAGMA foreign_key_check`. Statements that
   cannot run in a transaction (`VACUUM`, `PRAGMA journal_mode`) are not
   allowed. On PostgreSQL each migration runs in one transaction under an
   advisory lock; DDL is transactional, so a failed migration leaves nothing
   behind.
4. Migrations are forward-only. A database migrated by a newer binary is refused.

## 5. Backups and rollback

Each backend is copied with its own tool:

| | SQLite | Bundled PostgreSQL | External PostgreSQL |
|---|---|---|---|
| Database copy | `VACUUM INTO` into `forgebase.db` | `pg_basebackup -Fp -X stream` as `forgebase_super` into `postgres/`, plus `roles.secret` | `pg_dump -Fc` of the `forgebase` and `public` schemas into `postgres.dump` |
| Pre-migration snapshot | `backups/pre-migrate-v<N>-<time>.db` | `backups/pre-migrate-v<N>-<time>/` (database only) | none |
| `backup verify` checks | `integrity_check`, `foreign_key_check`, migrations | `pg_verifybackup`; the manifest's schema version against this build | `pg_restore --list`; the manifest's schema version against this build |
| Restore | the file is swapped in | the cluster is swapped in; WAL is replayed at the next start | `pg_restore` into the server, after a `pg_dump` of what it replaces into `backups/pre-restore-<time>/postgres.dump`; a failed file swap loads that dump back |

- **Pre-migration snapshot.** Before pending migrations run on an existing
  database, the server writes a snapshot of the database to `backups/`.
  SQLite writes `pre-migrate-v<N>-<time>.db` with `VACUUM INTO`. The bundled
  PostgreSQL writes `pre-migrate-v<N>-<time>/`, a `pg_basebackup` of the
  cluster with its `MANIFEST.json` and no storage or indexes; if it fails,
  nothing is migrated. Restoring the snapshot is the rollback for a bad
  migration: copy the file over `forgebase.db`, or run
  `forgebase restore backups/pre-migrate-v<N>-<time> --force`, which puts the
  cluster back and leaves files and keys alone. An external PostgreSQL server
  gets no snapshot (the server logs a warning): take a backup before you
  upgrade.
- **Backup (online).** `forgebase backup` copies the whole data directory while
  the server keeps serving: the database with the method in the table (one
  consistent snapshot; readers are never blocked), then `storage/` blobs and
  the derived `indexes/`. The copy goes to `<dest>.partial` and is renamed after
  `MANIFEST.json` (every file with size and SHA-256) is on disk, so a crash
  never leaves something that looks complete. Without a destination it writes
  a **snapshot** to `backups/snapshots/<UTC time>`; `--keep-last N` and
  `--max-age-days D` apply a retention policy afterwards (`backup prune` and
  `backup delete <id>` do it on demand; `backup list` shows what exists).
  Snapshots are also available over REST: `/api/v1/system/backups`
  (administrators only; see [api.md](api.md)).
- **Verify.** `forgebase backup verify <dir|id|latest>` re-hashes every file and
  rejects missing and unlisted files. On SQLite it then opens the copied
  database read-only and runs `integrity_check`, `foreign_key_check` and the
  migration check. On PostgreSQL a physical backup must hold `PG_VERSION`,
  `backup_manifest` and `global/pg_control` of the major in the manifest, and
  passes `pg_verifybackup`; a dump must pass `pg_restore --list`. Where the
  engine's tools are not available only the hashes are checked, and the
  report says so.
- **Restore.** `forgebase restore <dir|snapshot id|latest|time> [--force]`,
  with the server stopped (it takes the data-directory lock). The backup is
  verified first, staged and hashed again, then swapped in. The data it
  replaces is **moved**, never deleted, to `backups/pre-restore-<time>/`. A
  crash mid-swap leaves a journal; the next `restore` (or
  `restore --recover`) rolls the previous data back, and `forgebase doctor`
  reports it. Giving a time restores the newest snapshot taken at or before it
  (point-in-time restore). A restore never changes the backend: a PostgreSQL
  backup is refused in a SQLite data directory and the reverse (use
  `db convert`, §0).
- **Restore on PostgreSQL.** The bundled engine's backup replaces
  `postgres/data` and `postgres/roles.secret`. It needs the same PostgreSQL
  major and refuses while a postmaster still runs on the cluster. PostgreSQL
  replays the copied WAL when the server next starts, so that start takes a
  little longer. A dump of an external server is loaded with
  `pg_restore --clean --single-transaction`: it replaces the objects of the
  ForgeBase schemas in that database (`--force` is required) and changes
  nothing if it fails.
- **Master key.** `auth/master.key` seals every stored secret. A backup does
  **not** contain it unless you pass `--include-master-key`, because a key
  stored next to the data it protects is no protection. The manifest records the
  key's fingerprint; restore refuses a different key and, when no key is
  available, refuses to continue unless you pass `--master-key FILE` (the
  original) or `--allow-missing-key` (sealed secrets become unreadable). Back
  the key up separately, once.
- **What is covered.** The system database (all collections, users, policies,
  jobs, queues, vectors, billing records), local object-storage blobs and the
  vector/other index files (rebuildable; `--no-indexes` leaves them out). Not
  covered: objects in an S3 bucket (`storage.driver = "s3"`; use the bucket's
  own versioning or replication) and in-flight temporary files.
- **Manual copy.** Stopping the server and copying the database
  (`forgebase.db` and `forgebase.db-wal` if present, or `postgres/data` and
  `postgres/roles.secret` on the bundled engine), `storage/` and
  `auth/master.key` still works. Copying the database while the server runs
  can produce an inconsistent copy; use `forgebase backup` instead.

### Export and import

`forgebase export collections <dir>` writes collection definitions and records
(NDJSON, ids preserved) with SHA-256 checksums; `import collections <dir>`
recreates them with the **same collection, field and record ids**, relation
targets first. `--on-conflict skip` merges into existing collections.
`export records <collection> <file> --format json|ndjson|csv` and
`import records <collection> <file> --format json|ndjson` work on one
collection (CSV import is the REST endpoint `POST /collections/{name}/import`).
`export config [file]` writes the effective configuration as TOML and
`import config <file>` validates it exactly as the server does before installing
it. Import needs the server stopped; export does not.

`import collections` checks every collection of the bundle before it writes
anything: with the default `--on-conflict fail`, any collection that already
exists stops the import with all of them named, and nothing is imported. The
server creates the auth collection `users` on its first start, so a bundle
(which always lists `users`) goes into a served project with
`--on-conflict skip`. When a later step fails (a record that does not
validate), the collections the run created are dropped again; records merged
into existing collections with `skip` stay. Records of auth
collections are not exported (password hashes stay in the database), and
`created`/`updated` are reassigned on import, as in the REST import.

### `forgebase doctor`

Prints one line per check (`[ok]`, `[warn]`, `[FAIL]`) or JSON with `--json`
(`{"ok", "failed", "warnings", "checks": [{"name", "status", "detail"}]}`) and
exits with code 1 when a check fails.

| Check | Backend | What it looks at |
|---|---|---|
| `config` | all | The configuration loads |
| `data_dir` | all | Exists, writable, owner-only |
| `backend` | all | The data directory's backend; on SQLite, the notice that PostgreSQL is the default |
| `engine` | bundled PostgreSQL | Engine files against the archive manifest; the `postgres` binary runs |
| `cluster` | bundled PostgreSQL | `PG_VERSION` against the engine, mode 0700 |
| `roles` | bundled PostgreSQL | `roles.secret` present |
| `postmaster` | bundled PostgreSQL | A postmaster left behind by a stopped server (warns only) |
| `database` | all | SQLite: `integrity_check`, `foreign_key_check`, WAL mode, file mode. PostgreSQL: `amcheck` (`bt_index_check` on every B-tree index, installing `amcheck` in `public` when the login may) and the foreign-key check; a server where `amcheck` cannot be installed gets a warning and the foreign-key check only |
| `migrations` | all | Applied, pending, checksum problems |
| `storage`, `indexes` | all | `storage/` present and writable; `indexes/` present (warns: recreated at startup) |
| `disk` | all | Free space |
| `master_key` | all | Present, owner-only |
| `restore` | all | An interrupted restore |
| `snapshots` | all | Count and age of the newest; `--deep` also verifies it |
| `pre_migration` | all | How many pre-migration snapshots are kept, and the newest (shown only when there are some) |
| `staging` | all | Leftovers of unfinished backups or restores |
| `index_files` | all | Vector index files readable |
| `wal` | SQLite | Warns when the WAL is over 256 MiB (shown only then) |
| `storage_blobs` | all, local storage | Referenced blobs that are missing: fail; files without metadata: warn; metadata that cannot be read: warn (the check did not run) |
| `two_phase` | PostgreSQL | In-doubt two-phase schema transactions; fails when there are shards and `max_prepared_transactions` is 0 |
| `port` | all | The HTTP port is free, or held by this data directory's server |

On the bundled PostgreSQL with no server running, `doctor` takes the
data-directory lock and starts the engine for the checks (§1).

## 6. Query language

Record filters use the ForgeBase filter language (ADR-0003 §5, implemented in
`crates/forgebase-query`). Values never go into the expression text; they are
passed as `{:name}` placeholders with `p.<name>` query parameters:

```text
filter=status = "live" && views > {:min}&p.min=10
sort=-created,title
```

A sort key list is comma-separated, and `-` before a key sorts descending.
The compiler produces parameterised SQL. User text is never concatenated into
SQL.

`~` (contains) and `!~` ignore case. PostgreSQL uses `ILIKE`, which folds what
the database's ctype folds (every letter with the bundled engine's
`C.UTF-8`). SQLite uses `LIKE` for an ASCII value and, for a value with other
letters, a case-insensitive `REGEXP` of the value taken literally, so
`title ~ "ÉCOLE"` finds `école` there too. The `REGEXP` is only used in
queries, never in stored index or view definitions.

Pagination, `fields` and `expand` are in [api.md §7](api.md#7-lists-and-pagination).
